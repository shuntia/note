use crate::calls::{ring_once, Ring};
use crate::config::VoiceServiceConfig;
use crate::matrix::{Matrix, RoomEvent};
use crate::state::{CallState, LinkState, StateFile};
use note_voice_proto::{
    dial_forever, AppliedFile, BoxFuture, CallBody, Dir, FileOutbox, Handler, Outcome, Peer, PeerConfig,
    Refusal, RefusalCode, Reply, Request, Role,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{broadcast, watch};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

struct Service {
    cfg: VoiceServiceConfig,
    matrix: Arc<Matrix>,
    state: Mutex<StateFile>,
    applied: AppliedFile,
    events: broadcast::Sender<RoomEvent>,
    hang_ups: Mutex<HashMap<String, watch::Sender<bool>>>,
    peer: OnceLock<Peer>,
}

impl Service {
    fn peer(&self) -> &Peer {
        self.peer.get().expect("the peer is set before anything runs")
    }

    fn send(&self, call_id: &str, body: CallBody) -> Option<u64> {
        match self.peer().send_call(call_id, body) {
            Ok(seq) => Some(seq),
            Err(e) => {
                eprintln!("voice: journaling a frame for {call_id} failed: {e}");
                None
            }
        }
    }

    fn finish(&self, call_id: &str, outcome: Outcome) {
        self.send(call_id, CallBody::Outcome { outcome });
        let ended = self.send(call_id, CallBody::Ended);
        let mut st = lock(&self.state);
        if let Some(c) = st.data.calls.get_mut(call_id) {
            c.done = true;
            c.ended_seq = ended;
        }
        let _ = st.save();
        drop(st);
        lock(&self.hang_ups).remove(call_id);
        if ended.is_some() && !self.peer().pending_calls().iter().any(|c| c == call_id) {
            self.drop_call(call_id);
        }
    }

    /// Once Note holds the `Ended` frame, nothing of the call is kept here.
    fn drop_call(&self, call_id: &str) {
        let _ = self.peer().forget(call_id);
        let _ = self.applied.forget(call_id);
        let mut st = lock(&self.state);
        st.data.calls.remove(call_id);
        let _ = st.save();
    }

    fn begin_ring(self: &Arc<Self>, call_id: String, room_id: String, mxid: String, ring_secs: u32) {
        let (tx, rx) = watch::channel(false);
        lock(&self.hang_ups).insert(call_id.clone(), tx);
        let events = self.events.subscribe();
        let svc = self.clone();
        tokio::spawn(async move {
            let ring = Ring {
                matrix: &svc.matrix,
                room_id: &room_id,
                mxid: &mxid,
                livekit_url: &svc.cfg.livekit_service_url,
                ring_secs,
            };
            let id = call_id.clone();
            let s = svc.clone();
            let outcome = ring_once(ring, events, rx, move || {
                s.send(&id, CallBody::Ringing);
            })
            .await;
            svc.finish(&call_id, outcome);
        });
    }

    /// Calls a crash left open cannot be resumed; each is closed and reported.
    async fn recover(self: &Arc<Self>) {
        let open: Vec<(String, String)> = lock(&self.state)
            .data
            .calls
            .iter()
            .filter(|(_, c)| !c.done)
            .map(|(id, c)| (id.clone(), c.room_id.clone()))
            .collect();
        for (call_id, room_id) in open {
            let _ = self.matrix.clear_member(&room_id).await;
            self.finish(&call_id, Outcome::Failed { reason: "the voice service restarted".into() });
        }
    }

    async fn open_dm(&self, link_id: i64, mxid: String) -> Result<Reply, Refusal> {
        let known = lock(&self.state).data.links.get(&link_id).cloned();
        if let Some(link) = known.filter(|l| l.mxid == mxid) {
            let _ = self.matrix.invite(&link.room_id, &mxid).await;
            return Ok(Reply::Dm { room_id: link.room_id });
        }
        let room_id = self
            .matrix
            .create_dm(&mxid)
            .await
            .map_err(|e| Refusal::new(RefusalCode::Failed, format!("{e:#}")))?;
        let mut st = lock(&self.state);
        st.data.links.insert(link_id, LinkState { mxid, room_id: room_id.clone(), reported: false });
        st.save().map_err(|e| Refusal::new(RefusalCode::Failed, e.to_string()))?;
        Ok(Reply::Dm { room_id })
    }

    async fn report_join(self: &Arc<Self>, room: &str, user: &str) {
        let pending: Vec<i64> = lock(&self.state)
            .data
            .links
            .iter()
            .filter(|(_, l)| l.room_id == room && l.mxid == user && !l.reported)
            .map(|(id, _)| *id)
            .collect();
        for link_id in pending {
            let svc = self.clone();
            let room = room.to_string();
            tokio::spawn(async move {
                loop {
                    let got = svc.peer().request(Request::DmJoined { link_id, room_id: room.clone() }).await;
                    if got.is_ok() {
                        let mut st = lock(&svc.state);
                        if let Some(l) = st.data.links.get_mut(&link_id) {
                            l.reported = true;
                        }
                        let _ = st.save();
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            });
        }
    }

    async fn sync_forever(self: Arc<Self>) {
        loop {
            let since = lock(&self.state).data.since.clone();
            match self.matrix.sync(since.as_deref(), 30_000).await {
                Ok(batch) => {
                    for ev in batch.events {
                        if let RoomEvent::Joined { room, user } = &ev {
                            self.report_join(room, user).await;
                        }
                        let _ = self.events.send(ev);
                    }
                    let mut st = lock(&self.state);
                    st.data.since = Some(batch.next_batch);
                    let _ = st.save();
                }
                Err(e) => {
                    eprintln!("voice: sync failed: {e:#}");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    }
}

struct VoiceHandler {
    svc: OnceLock<Arc<Service>>,
}

impl VoiceHandler {
    fn svc(&self) -> &Arc<Service> {
        self.svc.get().expect("the service is set before the link runs")
    }
}

impl Handler for VoiceHandler {
    fn applied(&self, call_id: &str) -> u64 {
        self.svc().applied.applied(call_id)
    }

    fn apply(&self, call_id: &str, seq: u64, body: CallBody) -> Result<(), String> {
        let svc = self.svc().clone();
        match body {
            CallBody::Start { room_id, mxid, ring_secs, ring_by_ms, .. } => {
                {
                    let mut st = lock(&svc.state);
                    st.data.calls.insert(
                        call_id.to_string(),
                        CallState { room_id: room_id.clone(), done: false, ended_seq: None },
                    );
                    st.save().map_err(|e| e.to_string())?;
                }
                svc.applied.set_applied(call_id, seq).map_err(|e| e.to_string())?;
                if now_ms() > ring_by_ms {
                    svc.finish(call_id, Outcome::Failed { reason: "late".into() });
                } else {
                    svc.begin_ring(call_id.to_string(), room_id, mxid, ring_secs);
                }
            }
            CallBody::HangUp => {
                if let Some(tx) = lock(&svc.hang_ups).get(call_id) {
                    let _ = tx.send(true);
                }
                svc.applied.set_applied(call_id, seq).map_err(|e| e.to_string())?;
            }
            _ => svc.applied.set_applied(call_id, seq).map_err(|e| e.to_string())?,
        }
        Ok(())
    }

    fn request(&self, body: Request) -> BoxFuture<Result<Reply, Refusal>> {
        let svc = self.svc().clone();
        Box::pin(async move {
            match body {
                Request::OpenDm { link_id, mxid } => svc.open_dm(link_id, mxid).await,
                Request::DmJoined { .. } => Err(Refusal::new(RefusalCode::BadRequest, "the voice side reports joins")),
            }
        })
    }

    fn acked(&self, call_id: &str, upto: u64) {
        let svc = self.svc();
        let done = lock(&svc.state).data.calls.get(call_id).and_then(|c| c.ended_seq).is_some_and(|s| upto >= s);
        if done {
            svc.drop_call(call_id);
        }
    }
}

pub async fn run(cfg: VoiceServiceConfig) -> anyhow::Result<()> {
    run_with(cfg, PeerConfig::new(Role::Voice)).await
}

pub async fn run_with(cfg: VoiceServiceConfig, peer_cfg: PeerConfig) -> anyhow::Result<()> {
    let token = std::fs::read_to_string(&cfg.token_file)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", cfg.token_file.display()))?;
    let matrix = Arc::new(Matrix::connect(&cfg.homeserver, &token).await?);
    eprintln!("voice: signed in as {} ({})", matrix.user_id, matrix.device_id);
    let journal = FileOutbox::open(&cfg.state_dir.join("journal"))?;
    let applied = AppliedFile::new(&cfg.state_dir.join("journal"));
    let state = StateFile::open(&cfg.state_dir)?;
    let svc = Arc::new(Service {
        cfg: cfg.clone(),
        matrix,
        state: Mutex::new(state),
        applied,
        events: broadcast::channel(256).0,
        hang_ups: Mutex::new(HashMap::new()),
        peer: OnceLock::new(),
    });
    let handler = Arc::new(VoiceHandler { svc: OnceLock::new() });
    let _ = handler.svc.set(svc.clone());
    let peer = Peer::new(peer_cfg, Dir::ToNote, handler, Box::new(journal));
    let _ = svc.peer.set(peer.clone());
    svc.recover().await;
    tokio::spawn(svc.clone().sync_forever());
    dial_forever(peer, cfg.socket.clone()).await;
    Ok(())
}
