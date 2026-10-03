use crate::audio::engines::{Engines, SpeechEngines, TextToSpeech, NATIVE_RATE};
use crate::audio::lines::{Line, Lines};
use crate::calls::{clear_member, ring_once, Ring};
use crate::config::VoiceServiceConfig;
use crate::inbound::{say_and_leave, Detect, Detector};
use crate::matrix::{HomeserverError, Matrix, RoomEvent};
use crate::media::{LiveKitJoin, MediaJoin};
use crate::outgoing::CallWriter;
use crate::session::{run_session, Cues, SessionDeps, SessionEnd, SessionIn};
use crate::state::{CallState, LinkState, StateFile};
use note_voice_proto::{
    dial_forever, AppliedFile, BoxFuture, CallBody, Dir, Direction, FileOutbox, Handler, Outcome, Peer, PeerConfig,
    Refusal, RefusalCode, Reply, Request, Role, VoiceOption, VoiceProfile,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

const APPLIED_KEEP: Duration = Duration::from_hours(7 * 24);
/// Element X treats an expired membership as left, so a live call's outlasts the longest call.
const LIVE_MEMBER_MS: u64 = 60 * 60 * 1000;
const MAX_CALL: Duration = Duration::from_mins(30);
const LINK_GRACE: Duration = Duration::from_secs(10);
const JOIN_WAIT: Duration = Duration::from_secs(20);
const INBOUND_JOIN_WAIT: Duration = Duration::from_secs(10);
const INCOMING_CALL_WAIT: Duration = Duration::from_secs(3);
const INBOUND_START_WAIT: Duration = Duration::from_secs(5);
const CANT_REACH_MEMBER_MS: u64 = 60 * 1000;

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

const PREVIEW_TEXT: &str = "Hi, it's Note. This is how I sound.";

/// Voice samples as WAV, rendered once per language and voice.
#[derive(Default)]
struct Previews(Mutex<HashMap<(String, String), Arc<Vec<u8>>>>);

impl Previews {
    fn get(&self, tts: &dyn TextToSpeech, language: &str, voice: &str) -> anyhow::Result<Arc<Vec<u8>>> {
        let key = (language.to_string(), voice.to_string());
        if let Some(wav) = lock(&self.0).get(&key) {
            return Ok(wav.clone());
        }
        let wav = Arc::new(wav(&tts.synthesize_native(PREVIEW_TEXT, voice)?, NATIVE_RATE)?);
        lock(&self.0).insert(key, wav.clone());
        Ok(wav)
    }
}

/// Mono 16-bit WAV.
fn wav(samples: &[i16], sample_rate: u32) -> anyhow::Result<Vec<u8>> {
    let spec = hound::WavSpec { channels: 1, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut out = std::io::Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(&mut out, spec)?;
    for &s in samples {
        writer.write_sample(s)?;
    }
    writer.finalize()?;
    Ok(out.into_inner())
}

/// The `Start` Note sent for an incoming call it opened.
struct InboundStart {
    call_id: String,
    profile: VoiceProfile,
    hang_up: watch::Receiver<bool>,
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
    /// Rooms with an incoming call being answered, each with the way to its `Start` until it arrives.
    answering: Mutex<HashMap<String, Option<oneshot::Sender<InboundStart>>>>,
    previews: Previews,
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

    fn no_models(&self) -> Option<Refusal> {
        self.backends.engines.languages().is_empty().then(|| Refusal::new(RefusalCode::Failed, "no voice models"))
    }

    fn voices(&self, language: &str) -> Result<Reply, Refusal> {
        if let Some(refusal) = self.no_models() {
            return Err(refusal);
        }
        let voices = self.backends.engines.tts(language).voices();
        Ok(Reply::Voices {
            voices: voices.into_iter().map(|v| VoiceOption { id: v.id, label: v.label, language: v.language }).collect(),
        })
    }

    async fn preview(self: Arc<Self>, language: String, voice: String) -> Result<Reply, Refusal> {
        use base64::Engine as _;
        if let Some(refusal) = self.no_models() {
            return Err(refusal);
        }
        let rendered = tokio::task::spawn_blocking(move || {
            self.previews.get(&*self.backends.engines.tts(&language), &language, &voice)
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
        match rendered {
            Ok(wav) => Ok(Reply::Audio { wav_base64: base64::engine::general_purpose::STANDARD.encode(&*wav) }),
            Err(e) => Err(Refusal::new(RefusalCode::Failed, format!("rendering the sample failed: {e:#}"))),
        }
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
                svc.join_and_run(&call_id, &room_id, &mxid, profile, &rx, Direction::Outbound).await;
            } else {
                svc.finish(&call_id, outcome);
            }
        });
    }

    /// Puts the bot's membership, which answers an inbound call, joins the
    /// media and runs the session to the end. A `HangUp` during the join
    /// abandons it; the session's inbox is open from the answer on, so one
    /// after the join is not lost.
    async fn join_and_run(
        self: &Arc<Self>,
        call_id: &str,
        room_id: &str,
        mxid: &str,
        profile: VoiceProfile,
        hang_up: &watch::Receiver<bool>,
        direction: Direction,
    ) {
        if self.backends.engines.languages().is_empty() {
            return self.fail_before_join(call_id, room_id, mxid, direction, "no voice models".into()).await;
        }
        let (svc, id) = (self.clone(), call_id.to_string());
        let writer = match CallWriter::spawn(move |body| {
            svc.send(&id, body);
        }) {
            Ok(writer) => writer,
            Err(e) => {
                let reason = format!("starting the frame writer: {e}");
                return self.fail_before_join(call_id, room_id, mxid, direction, reason).await;
            }
        };
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
            let wait = match direction {
                Direction::Outbound => JOIN_WAIT,
                Direction::Inbound => INBOUND_JOIN_WAIT,
            };
            self.backends.media.join(&self.matrix, &self.cfg.livekit_service_url, room_id, mxid, wait).await
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
            direction,
            max_len: MAX_CALL,
            link_grace: LINK_GRACE,
        };
        let session = tokio::spawn(run_session(deps, media, inbox, writer.sender()));
        let end = session.await.unwrap_or_else(|e| SessionEnd::MediaFailed(format!("the session failed: {e}")));
        writer.close().await;
        eprintln!("voice: call {call_id} ended: {end:?}");
        lock(&self.sessions).remove(call_id);
        clear_member(&self.matrix, room_id).await;
        self.end(call_id);
    }

    /// An inbound caller is still answered and told, rather than left ringing.
    async fn fail_before_join(&self, call_id: &str, room_id: &str, mxid: &str, direction: Direction, reason: String) {
        self.finish(call_id, Outcome::Failed { reason });
        match direction {
            Direction::Outbound => clear_member(&self.matrix, room_id).await,
            Direction::Inbound => self.cant_reach(room_id, mxid, None).await,
        }
    }

    fn set_live(&self, call_id: &str) {
        let mut st = lock(&self.state);
        if let Some(c) = st.data.calls.get_mut(call_id) {
            c.live = true;
        }
        let _ = st.save();
    }

    /// A room with an incoming call being answered, or a call of its own not yet done.
    fn busy(&self, room: &str) -> bool {
        lock(&self.answering).contains_key(room)
            || lock(&self.state).data.calls.values().any(|c| c.room_id == room && !c.done)
    }

    /// Asks Note to open the user's call; with no Note to take it, answers to say so.
    async fn answer_incoming(
        self: Arc<Self>,
        room: String,
        mxid: String,
        key: String,
        device: Option<String>,
        started: oneshot::Receiver<InboundStart>,
    ) {
        let ask = Request::IncomingCall { room_id: room.clone(), mxid: mxid.clone(), key };
        let why = match tokio::time::timeout(INCOMING_CALL_WAIT, self.peer().request(ask)).await {
            Ok(Ok(Reply::Call { call_id })) => match tokio::time::timeout(INBOUND_START_WAIT, started).await {
                Ok(Ok(start)) if start.call_id == call_id => {
                    self.answer_for_note(start, &room, &mxid, device.as_deref()).await;
                    lock(&self.answering).remove(&room);
                    return;
                }
                Ok(Ok(start)) => {
                    self.finish(&start.call_id, Outcome::Failed { reason: format!("Note opened {call_id} instead") });
                    format!("a Start for {} came for {call_id}", start.call_id)
                }
                _ => format!("the Start for {call_id} never came"),
            },
            Ok(Ok(reply)) => format!("Note answered {reply:?}"),
            Ok(Err(refusal)) => refusal.to_string(),
            Err(_) => "no answer in time".into(),
        };
        eprintln!("voice: Note cannot take {mxid}'s call in {room} ({why}); answering to say so");
        self.cant_reach(&room, &mxid, device.as_deref()).await;
        lock(&self.answering).remove(&room);
    }

    async fn answer_for_note(self: &Arc<Self>, start: InboundStart, room: &str, mxid: &str, device: Option<&str>) {
        if !self.still_calling(room, mxid, device).await {
            return self.finish(&start.call_id, Outcome::Failed { reason: "the caller hung up".into() });
        }
        self.join_and_run(&start.call_id, room, mxid, start.profile, &start.hang_up, Direction::Inbound).await;
    }

    /// Whether the user's call membership from `device` is still set; an
    /// unknown device or an unreadable state counts as still calling.
    async fn still_calling(&self, room: &str, mxid: &str, device: Option<&str>) -> bool {
        let Some(device) = device else { return true };
        match self.matrix.member_active(room, mxid, device).await {
            Ok(active) => active,
            Err(e) => {
                eprintln!("voice: reading {mxid}'s call membership in {room} failed: {e:#}");
                true
            }
        }
    }

    /// Answers, plays the ready cue and `Line::CantReach`, and leaves; Note hears nothing of it.
    async fn cant_reach(&self, room: &str, mxid: &str, device: Option<&str>) {
        if !self.still_calling(room, mxid, device).await {
            return eprintln!("voice: {mxid}'s call in {room} ended before it was answered");
        }
        if let Err(e) = self.matrix.put_member(room, CANT_REACH_MEMBER_MS, &self.cfg.livekit_service_url).await {
            eprintln!("voice: answering {mxid}'s call in {room} failed: {e:#}");
            return clear_member(&self.matrix, room).await;
        }
        let url = &self.cfg.livekit_service_url;
        match self.backends.media.join(&self.matrix, url, room, mxid, INBOUND_JOIN_WAIT).await {
            Ok(media) => say_and_leave(&*media, self.cant_reach_clips().await).await,
            Err(e) => eprintln!("voice: joining {mxid}'s call in {room} failed: {e:#}"),
        }
        clear_member(&self.matrix, room).await;
    }

    async fn cant_reach_clips(&self) -> Vec<Vec<i16>> {
        let profile = VoiceProfile::default();
        let mut clips = Vec::new();
        if profile.cue {
            clips.push(self.cues.ready.to_vec());
        }
        let languages = self.backends.engines.languages();
        let Some(language) = languages.iter().find(|l| **l == profile.language).or(languages.first()).cloned() else {
            return clips;
        };
        let (engines, lines) = (self.backends.engines.clone(), self.lines.clone());
        let line = tokio::task::spawn_blocking(move || lines.get(&*engines.tts(&language), &language, "", Line::CantReach))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
        match line {
            Ok(pcm) => clips.push(pcm.to_vec()),
            Err(e) => eprintln!("voice: rendering {:?} failed: {e:#}", Line::CantReach),
        }
        clips
    }

    /// Opens the inbound call Note started for the attempt in `room`.
    fn begin_answer(&self, call_id: &str, room_id: &str, profile: VoiceProfile) {
        let (tx, hang_up) = watch::channel(false);
        match lock(&self.hang_ups).entry(call_id.to_string()) {
            std::collections::hash_map::Entry::Occupied(_) => return,
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(tx);
            }
        }
        let waiting = lock(&self.answering).get_mut(room_id).and_then(Option::take);
        let start = InboundStart { call_id: call_id.to_string(), profile, hang_up };
        if waiting.is_none_or(|w| w.send(start).is_err()) {
            self.finish(call_id, Outcome::Failed { reason: "no incoming call to answer".into() });
        }
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

    /// Answers the linked users' calls as they start. A user's ring carries
    /// no device, so it is checked against one of their devices in a call.
    fn detect(self: &Arc<Self>, detector: &mut Detector, in_call: &mut HashSet<(String, String, String)>, ev: &RoomEvent) {
        if let RoomEvent::CallMember { room, user, device, active, .. } = ev {
            let at = (room.clone(), user.clone(), device.clone());
            if *active {
                in_call.insert(at);
            } else {
                in_call.remove(&at);
            }
        }
        let links: Vec<(String, String)> =
            lock(&self.state).data.links.values().map(|l| (l.room_id.clone(), l.mxid.clone())).collect();
        let Detect::Answer { room, mxid, key } = detector.on_event(ev, &links, &|room| self.busy(room), now_ms()) else {
            return;
        };
        let device = match ev {
            RoomEvent::CallMember { device, .. } => Some(device.clone()),
            _ => in_call.iter().find(|(r, u, _)| *r == room && *u == mxid).map(|(_, _, d)| d.clone()),
        }
        .filter(|d| !d.is_empty());
        let (tx, started) = oneshot::channel();
        lock(&self.answering).insert(room.clone(), Some(tx));
        eprintln!("voice: {mxid} is calling in {room}");
        tokio::spawn(self.clone().answer_incoming(room, mxid, key, device, started));
    }

    async fn sync_forever(self: Arc<Self>) {
        let mut detector = Detector::new(lock(&self.state).data.since.is_none(), now_ms());
        let mut in_call = HashSet::new();
        loop {
            let since = lock(&self.state).data.since.clone();
            match self.matrix.sync(since.as_deref(), 30_000).await {
                Ok(batch) => {
                    for ev in batch.events {
                        if let RoomEvent::Joined { room, user } = &ev {
                            self.report_join(room, user);
                        }
                        self.detect(&mut detector, &mut in_call, &ev);
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
            CallBody::Start { room_id, mxid, ring_secs, ring_by_ms, voice, direction, .. } => {
                let room_busy = direction == Direction::Outbound && svc.busy(&room_id);
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
                if direction == Direction::Inbound {
                    svc.begin_answer(call_id, &room_id, voice);
                } else if room_busy {
                    svc.finish(call_id, Outcome::Failed { reason: "a call is already up in this room".into() });
                } else if now_ms() > ring_by_ms {
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
                Request::IncomingCall { .. } => {
                    Err(Refusal::new(RefusalCode::BadRequest, "the voice side reports incoming calls"))
                }
                Request::ListVoices { language } => svc.voices(&language),
                Request::Preview { language, voice } => svc.preview(language, voice).await,
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
            for line in [Line::LostNotes, Line::Goodbye, Line::CantReach, Line::Hi] {
                if let Err(e) = lines.get(&*tts, &language, "", line) {
                    eprintln!("voice: rendering {line:?} in {language} failed: {e:#}");
                }
            }
        }
    });
}

/// A load failure leaves ringing up: answered calls fail and fall through.
fn loaded_or_empty(loaded: anyhow::Result<Engines>) -> Engines {
    loaded.unwrap_or_else(|e| {
        eprintln!("voice: loading the speech models failed: {e:#}; answered calls will fail");
        Engines::empty()
    })
}

pub async fn run(cfg: VoiceServiceConfig) -> anyhow::Result<()> {
    let (models, device) = (cfg.model_sets(), cfg.device);
    let loaded = tokio::task::spawn_blocking(move || Engines::load(&models, device)).await;
    let engines = loaded_or_empty(loaded.map_err(anyhow::Error::from).and_then(|r| r));
    let backends = Backends { engines: Arc::new(engines), media: Arc::new(LiveKitJoin) };
    run_with(cfg, PeerConfig::new(Role::Voice), backends).await
}

pub async fn run_with(cfg: VoiceServiceConfig, peer_cfg: PeerConfig, backends: Backends) -> anyhow::Result<()> {
    if backends.engines.languages().is_empty() {
        eprintln!("voice: no voice models are loaded; answered calls will fail with \"no voice models\"");
    }
    let cues = Cues::load(&cfg.ready_cue(), &cfg.heard_cue());
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
        answering: Mutex::new(HashMap::new()),
        previews: Previews::default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::audio::engines::VoiceInfo;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_failed_model_load_leaves_no_languages_instead_of_an_error() {
        assert!(loaded_or_empty(Err(anyhow::anyhow!("libonnxruntime.so not found"))).languages().is_empty());
    }

    #[test]
    fn the_wav_header_is_44_bytes_at_24_khz() {
        let bytes = wav(&[0, 1, -1], NATIVE_RATE).unwrap();
        assert_eq!(bytes.len(), 44 + 3 * 2);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 24_000);
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 1, "mono");
    }

    #[derive(Default)]
    struct CountingTts(AtomicUsize);

    impl TextToSpeech for CountingTts {
        fn synthesize(&self, _text: &str, _voice: &str) -> anyhow::Result<Vec<i16>> {
            anyhow::bail!("a preview is rendered at the native rate")
        }

        fn synthesize_native(&self, text: &str, _voice: &str) -> anyhow::Result<Vec<i16>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(vec![1; text.len()])
        }

        fn voices(&self) -> Vec<VoiceInfo> {
            Vec::new()
        }
    }

    #[test]
    fn a_preview_is_rendered_once_per_voice() {
        let (tts, previews) = (CountingTts::default(), Previews::default());
        let first = previews.get(&tts, "en", "af_heart").unwrap();
        assert_eq!(previews.get(&tts, "en", "af_heart").unwrap(), first);
        previews.get(&tts, "en", "bm_george").unwrap();
        assert_eq!(tts.0.load(Ordering::SeqCst), 2);
        assert_eq!(first.len(), 44 + PREVIEW_TEXT.len() * 2);
    }

    #[test]
    #[ignore = "needs NOTE_VOICE_MODELS"]
    fn preview_is_a_wav_and_cached() {
        let dir = std::env::var_os("NOTE_VOICE_MODELS").unwrap();
        let models = crate::config::models_from_dir(std::path::Path::new(&dir));
        let engines = Engines::load(&models, crate::audio::engines::Device::Auto).unwrap();
        let tts = engines.tts("en");
        let previews = Previews::default();
        let first = previews.get(&*tts, "en", "").unwrap();
        let reader = hound::WavReader::new(std::io::Cursor::new(first.to_vec())).unwrap();
        assert_eq!((reader.spec().sample_rate, reader.spec().channels), (24_000, 1));
        assert!(reader.duration() > 24_000, "at least a second of speech");
        assert!(Arc::ptr_eq(&previews.get(&*tts, "en", "").unwrap(), &first));
    }
}
