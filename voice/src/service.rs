use crate::audio::engines::{Engines, SpeechEngines};
use crate::audio::lines::{Line, Lines};
use crate::calls::{clear_member, ring_once, Ring};
use crate::config::VoiceServiceConfig;
use crate::matrix::{HomeserverError, Matrix, RoomEvent};
use crate::media::{LiveKitJoin, MediaJoin};
use crate::session::{run_session, Cues, SessionDeps, SessionEnd, SessionIn};
use crate::state::{CallState, LinkState, StateFile};
use note_voice_proto::{
    dial_forever, AppliedFile, BoxFuture, CallBody, Dir, FileOutbox, Handler, Outcome, Peer, PeerConfig,
    Refusal, RefusalCode, Reply, Request, Role, VoiceProfile,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, watch};

const APPLIED_KEEP: Duration = Duration::from_hours(7 * 24);
/// Element X treats an expired membership as left, so a live call's outlasts the longest call.
const LIVE_MEMBER_MS: u64 = 60 * 60 * 1000;
const MAX_CALL: Duration = Duration::from_mins(30);
const LINK_GRACE: Duration = Duration::from_secs(10);
const JOIN_WAIT: Duration = Duration::from_secs(10);

/// What a live call runs on: the speech engines, and how to join a room's media.
pub struct Backends {
    pub engines: Arc<dyn SpeechEngines>,
    pub media: Arc<dyn MediaJoin>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A client error other than auth or rate limiting, which is how a
/// homeserver refuses a `since` token it no longer knows.
fn rejects_since(e: &anyhow::Error) -> bool {
    use reqwest::StatusCode;
    e.downcast_ref::<HomeserverError>().is_some_and(|h| {
        h.status.is_client_error()
            && ![StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN, StatusCode::TOO_MANY_REQUESTS].contains(&h.status)
    })
}

struct Service {
    cfg: VoiceServiceConfig,
    matrix: Arc<Matrix>,
    state: Mutex<StateFile>,
    applied: AppliedFile,
    events: broadcast::Sender<RoomEvent>,
    hang_ups: Mutex<HashMap<String, watch::Sender<bool>>>,
    sessions: Mutex<HashMap<String, mpsc::UnboundedSender<SessionIn>>>,
    backends: Backends,
    lines: Arc<Lines>,
    cues: Arc<Cues>,
    reporting: Mutex<HashSet<i64>>,
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
        self.end(call_id);
    }

    /// A call whose `Ended` could not be journaled stays open, so the next
    /// start's recovery closes it again.
    fn end(&self, call_id: &str) {
        let ended = self.send(call_id, CallBody::Ended);
        let mut st = lock(&self.state);
        if let (Some(c), Some(seq)) = (st.data.calls.get_mut(call_id), ended) {
            c.done = true;
            c.live = false;
            c.ended_seq = Some(seq);
        }
        let _ = st.save();
        drop(st);
        lock(&self.hang_ups).remove(call_id);
        if ended.is_some() && !self.peer().pending_calls().iter().any(|c| c == call_id) {
            self.drop_call(call_id);
        }
    }

    /// Once Note holds the `Ended` frame, nothing of the call is kept here
    /// but its applied seq, which `run_with` prunes once it is old.
    fn drop_call(&self, call_id: &str) {
        let _ = self.peer().forget(call_id);
        let mut st = lock(&self.state);
        st.data.calls.remove(call_id);
        let _ = st.save();
    }

    fn begin_ring(
        self: &Arc<Self>,
        call_id: String,
        room_id: String,
        mxid: String,
        ring_secs: u32,
        profile: VoiceProfile,
    ) {
        let (tx, rx) = watch::channel(false);
        match lock(&self.hang_ups).entry(call_id.clone()) {
            std::collections::hash_map::Entry::Occupied(_) => return,
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(tx);
            }
        }
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
            let outcome = ring_once(ring, events, rx.clone(), move || {
                s.send(&id, CallBody::Ringing);
            })
            .await;
            if outcome == Outcome::Answered {
                svc.go_live(&call_id, &room_id, &mxid, profile, &rx).await;
            } else {
                svc.finish(&call_id, outcome);
            }
        });
    }

    /// Joins the answered call's media and runs its session to the end. A
    /// `HangUp` during the join abandons it; the session's inbox is open from
    /// the answer on, so one after the join is not lost.
    async fn go_live(
        self: &Arc<Self>,
        call_id: &str,
        room_id: &str,
        mxid: &str,
        profile: VoiceProfile,
        hang_up: &watch::Receiver<bool>,
    ) {
        if self.backends.engines.languages().is_empty() {
            clear_member(&self.matrix, room_id).await;
            return self.finish(call_id, Outcome::Failed { reason: "no voice models".into() });
        }
        let (tx, inbox) = mpsc::unbounded_channel();
        {
            let mut sessions = lock(&self.sessions);
            if !self.peer().is_up() {
                let _ = tx.send(SessionIn::LinkUp(false));
            }
            sessions.insert(call_id.to_string(), tx);
        }
        let join = async {
            self.matrix.put_member(room_id, LIVE_MEMBER_MS, &self.cfg.livekit_service_url).await?;
            self.backends.media.join(&self.matrix, &self.cfg.livekit_service_url, room_id, mxid).await
        };
        let mut hang_up = hang_up.clone();
        let joined = tokio::select! {
            joined = join => joined.map_err(|e| format!("{e:#}")),
            Ok(_) = hang_up.wait_for(|h| *h) => Err("hung up by Note".to_string()),
        };
        let media = match joined {
            Ok(media) => media,
            Err(reason) => {
                lock(&self.sessions).remove(call_id);
                clear_member(&self.matrix, room_id).await;
                return self.finish(call_id, Outcome::Failed { reason });
            }
        };
        self.send(call_id, CallBody::Outcome { outcome: Outcome::Answered });
        self.set_live(call_id);
        let deps = SessionDeps {
            engines: self.backends.engines.clone(),
            lines: self.lines.clone(),
            cues: self.cues.clone(),
            profile,
            max_len: MAX_CALL,
            link_grace: LINK_GRACE,
        };
        let (svc, id) = (self.clone(), call_id.to_string());
        let session = tokio::spawn(run_session(deps, media, inbox, move |body| {
            svc.send(&id, body);
        }));
        let end = session.await.unwrap_or_else(|e| SessionEnd::MediaFailed(format!("the session failed: {e}")));
        eprintln!("voice: call {call_id} ended: {end:?}");
        lock(&self.sessions).remove(call_id);
        clear_member(&self.matrix, room_id).await;
        self.end(call_id);
    }

    fn set_live(&self, call_id: &str) {
        let mut st = lock(&self.state);
        if let Some(c) = st.data.calls.get_mut(call_id) {
            c.live = true;
        }
        let _ = st.save();
    }

    fn to_session(&self, call_id: &str, msg: SessionIn) {
        if let Some(tx) = lock(&self.sessions).get(call_id) {
            let _ = tx.send(msg);
        }
    }

    /// Calls a crash left open, ringing or live, cannot be resumed; each is closed and reported.
    async fn recover(self: &Arc<Self>) {
        let open: Vec<(String, String)> = lock(&self.state)
            .data
            .calls
            .iter()
            .filter(|(_, c)| !c.done)
            .map(|(id, c)| (id.clone(), c.room_id.clone()))
            .collect();
        for (call_id, room_id) in open {
            clear_member(&self.matrix, &room_id).await;
            self.finish(&call_id, Outcome::Failed { reason: "the voice service restarted".into() });
        }
    }

    /// Finished calls Note already holds every frame of.
    fn drop_delivered(&self) {
        let pending = self.peer().pending_calls();
        let delivered: Vec<String> = lock(&self.state)
            .data
            .calls
            .iter()
            .filter(|(id, c)| c.done && !pending.contains(id))
            .map(|(id, _)| id.clone())
            .collect();
        for call_id in delivered {
            self.drop_call(&call_id);
        }
    }

    /// Reports the link's join if its user is in the room already, which
    /// covers joins a restart kept this side from seeing.
    async fn check_joined(self: &Arc<Self>, link_id: i64) {
        let Some(link) = lock(&self.state).data.links.get(&link_id).cloned().filter(|l| !l.reported) else {
            return;
        };
        match self.matrix.joined_members(&link.room_id).await {
            Ok(members) if members.contains(&link.mxid) => self.report_join(&link.room_id, &link.mxid),
            Ok(_) => {}
            Err(e) => eprintln!("voice: reading the members of {} failed: {e:#}", link.room_id),
        }
    }

    async fn check_unreported(self: Arc<Self>) {
        let unreported: Vec<i64> =
            lock(&self.state).data.links.iter().filter(|(_, l)| !l.reported).map(|(id, _)| *id).collect();
        for link_id in unreported {
            self.check_joined(link_id).await;
        }
    }

    async fn open_dm(self: &Arc<Self>, link_id: i64, mxid: String) -> Result<Reply, Refusal> {
        let known = lock(&self.state).data.links.get(&link_id).cloned();
        if let Some(link) = known.filter(|l| l.mxid == mxid) {
            {
                let mut st = lock(&self.state);
                if let Some(l) = st.data.links.get_mut(&link_id) {
                    l.reported = false;
                }
                st.save().map_err(|e| Refusal::new(RefusalCode::Failed, e.to_string()))?;
            }
            let _ = self.matrix.invite(&link.room_id, &mxid).await;
            tokio::spawn({
                let svc = self.clone();
                async move { svc.check_joined(link_id).await }
            });
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

    fn report_join(self: &Arc<Self>, room: &str, user: &str) {
        let pending: Vec<i64> = lock(&self.state)
            .data
            .links
            .iter()
            .filter(|(_, l)| l.room_id == room && l.mxid == user && !l.reported)
            .map(|(id, _)| *id)
            .collect();
        for link_id in pending {
            if !lock(&self.reporting).insert(link_id) {
                continue;
            }
            let svc = self.clone();
            let room = room.to_string();
            let user = user.to_string();
            tokio::spawn(async move {
                let mut up = svc.peer().up_watch();
                loop {
                    let _ = up.wait_for(|up| *up).await;
                    let got = svc.peer().request(Request::DmJoined { link_id, room_id: room.clone() }).await;
                    if got.is_ok() {
                        let mut st = lock(&svc.state);
                        if let Some(l) =
                            st.data.links.get_mut(&link_id).filter(|l| l.room_id == room && l.mxid == user)
                        {
                            l.reported = true;
                        }
                        let _ = st.save();
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
                lock(&svc.reporting).remove(&link_id);
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
                            self.report_join(room, user);
                        }
                        let _ = self.events.send(ev);
                    }
                    let mut st = lock(&self.state);
                    st.data.since = Some(batch.next_batch);
                    let _ = st.save();
                }
                Err(e) => {
                    eprintln!("voice: sync failed: {e:#}");
                    if rejects_since(&e) {
                        let mut st = lock(&self.state);
                        st.data.since = None;
                        let _ = st.save();
                    }
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
            CallBody::Start { room_id, mxid, ring_secs, ring_by_ms, voice, .. } => {
                {
                    let mut st = lock(&svc.state);
                    if st.data.calls.contains_key(call_id) {
                        drop(st);
                        return svc.applied.set_applied(call_id, seq).map_err(|e| e.to_string());
                    }
                    st.data.calls.insert(
                        call_id.to_string(),
                        CallState { room_id: room_id.clone(), done: false, live: false, ended_seq: None },
                    );
                    st.save().map_err(|e| e.to_string())?;
                }
                svc.applied.set_applied(call_id, seq).map_err(|e| e.to_string())?;
                if now_ms() > ring_by_ms {
                    svc.finish(call_id, Outcome::Failed { reason: "late".into() });
                } else {
                    svc.begin_ring(call_id.to_string(), room_id, mxid, ring_secs, voice);
                }
            }
            CallBody::HangUp => {
                if let Some(tx) = lock(&svc.hang_ups).get(call_id) {
                    let _ = tx.send(true);
                }
                svc.to_session(call_id, SessionIn::Frame(CallBody::HangUp));
                svc.applied.set_applied(call_id, seq).map_err(|e| e.to_string())?;
            }
            body @ (CallBody::Speak { .. } | CallBody::SpeakDone { .. } | CallBody::Play { .. } | CallBody::Drop { .. }) => {
                svc.to_session(call_id, SessionIn::Frame(body));
                svc.applied.set_applied(call_id, seq).map_err(|e| e.to_string())?;
            }
            CallBody::Ringing
            | CallBody::Outcome { .. }
            | CallBody::Ended
            | CallBody::Draft { .. }
            | CallBody::Commit { .. }
            | CallBody::Retract { .. }
            | CallBody::Floor { .. }
            | CallBody::BargeIn { .. }
            | CallBody::Played { .. } => svc.applied.set_applied(call_id, seq).map_err(|e| e.to_string())?,
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

    fn link_changed(&self, up: bool) {
        let Some(svc) = self.svc.get() else { return };
        for tx in lock(&svc.sessions).values() {
            let _ = tx.send(SessionIn::LinkUp(up));
        }
    }

    fn acked(&self, call_id: &str, upto: u64) {
        let svc = self.svc();
        let done = lock(&svc.state).data.calls.get(call_id).and_then(|c| c.ended_seq).is_some_and(|s| upto >= s);
        if done {
            svc.drop_call(call_id);
        }
    }
}

/// Renders each loaded language's call lines in its default voice, off the runtime.
fn warm_lines(engines: &Arc<dyn SpeechEngines>, lines: &Arc<Lines>) {
    let (engines, lines) = (engines.clone(), lines.clone());
    tokio::task::spawn_blocking(move || {
        for language in engines.languages() {
            let tts = engines.tts(&language);
            for line in [Line::LostNotes, Line::Goodbye] {
                if let Err(e) = lines.get(&*tts, &language, "", line) {
                    eprintln!("voice: rendering {line:?} in {language} failed: {e:#}");
                }
            }
        }
    });
}

pub async fn run(cfg: VoiceServiceConfig) -> anyhow::Result<()> {
    let (models, device) = (cfg.model_sets(), cfg.device);
    let engines = tokio::task::spawn_blocking(move || Engines::load(&models, device)).await??;
    let backends = Backends { engines: Arc::new(engines), media: Arc::new(LiveKitJoin { wait: JOIN_WAIT }) };
    run_with(cfg, PeerConfig::new(Role::Voice), backends).await
}

pub async fn run_with(cfg: VoiceServiceConfig, peer_cfg: PeerConfig, backends: Backends) -> anyhow::Result<()> {
    if backends.engines.languages().is_empty() {
        eprintln!("voice: no voice models are loaded; answered calls will fail with \"no voice models\"");
    }
    let cues = Cues::load(cfg.ready_cue().as_deref(), cfg.heard_cue().as_deref());
    let lines = Arc::new(Lines::default());
    warm_lines(&backends.engines, &lines);
    let token = std::fs::read_to_string(&cfg.token_file)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", cfg.token_file.display()))?;
    let matrix = Arc::new(Matrix::connect(&cfg.homeserver, &token).await?);
    eprintln!("voice: signed in as {} ({})", matrix.user_id, matrix.device_id);
    let journal = FileOutbox::open(&cfg.state_dir.join("journal"))?;
    let applied = AppliedFile::new(&cfg.state_dir.join("journal"));
    if let Err(e) = applied.prune(APPLIED_KEEP) {
        eprintln!("voice: pruning old applied records failed: {e}");
    }
    let state = StateFile::open(&cfg.state_dir)?;
    let svc = Arc::new(Service {
        cfg: cfg.clone(),
        matrix,
        state: Mutex::new(state),
        applied,
        events: broadcast::channel(256).0,
        hang_ups: Mutex::new(HashMap::new()),
        sessions: Mutex::new(HashMap::new()),
        backends,
        lines,
        cues: Arc::new(cues),
        reporting: Mutex::new(HashSet::new()),
        peer: OnceLock::new(),
    });
    let handler = Arc::new(VoiceHandler { svc: OnceLock::new() });
    let _ = handler.svc.set(svc.clone());
    let peer = Peer::new(peer_cfg, Dir::ToNote, handler, Box::new(journal));
    let _ = svc.peer.set(peer.clone());
    svc.drop_delivered();
    svc.recover().await;
    tokio::spawn(svc.clone().check_unreported());
    tokio::spawn(svc.clone().sync_forever());
    dial_forever(peer, cfg.socket.clone()).await;
    Ok(())
}
