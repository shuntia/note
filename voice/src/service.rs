use crate::audio::engines::{Engines, SpeechEngines};
use crate::audio::lines::{Line, Lines};
use crate::audio::sidecar::Sidecars;
use crate::audio::tts::{find_speaker, Speaker, SpeechBackend, RATE};
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
use std::path::{Path, PathBuf};
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
        .map_or(0, |d| d.as_millis() as i64)
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

/// English stands in for any language without its own wording.
fn preview_text(language: &str) -> &'static str {
    match language {
        "ja" => "こんにちは、ノートです。こんな声だよ。",
        _ => "Hi, it's Note. This is how I sound.",
    }
}

type Slot = Arc<Mutex<Option<Arc<Vec<u8>>>>>;

/// Voice samples as WAV, rendered once per language, offered voice and
/// `preview_text`, and kept in `dir` across restarts; a render in progress
/// holds its slot, so a second ask for it waits.
#[derive(Default)]
struct Previews {
    slots: Mutex<HashMap<(String, String), Slot>>,
    dir: Option<PathBuf>,
}

impl Previews {
    /// `voice` empty is the base voice's default; any other id no live backend offers is refused, as
    /// is the default while `base` is down.
    fn get(
        &self,
        base: Option<&Arc<dyn SpeechBackend>>,
        sidecars: &[Arc<dyn SpeechBackend>],
        language: &str,
        voice: &str,
    ) -> Result<Arc<Vec<u8>>, Refusal> {
        let speaker = match (voice.is_empty(), base) {
            (true, Some(base)) => Some(Speaker::new(base.clone(), "")),
            (true, None) => return Err(Refusal::new(RefusalCode::Failed, format!("the {language} voice is down"))),
            (false, Some(base)) => find_speaker(voice, language, base, sidecars),
            (false, None) => {
                let mute: Arc<dyn SpeechBackend> = Arc::new(crate::audio::tts::Mute::new(language));
                find_speaker(voice, language, &mute, sidecars)
            }
        };
        let Some(speaker) = speaker else {
            return Err(Refusal::new(RefusalCode::BadRequest, format!("no voice {voice:?}")));
        };
        let slot = lock(&self.slots).entry((language.to_string(), voice.to_string())).or_default().clone();
        let mut held = lock(&slot);
        if let Some(wav) = &*held {
            return Ok(wav.clone());
        }
        let file = self.dir.as_ref().map(|dir| dir.join(preview_file(language, voice)));
        let kept = file.as_ref().and_then(|f| std::fs::read(f).ok());
        let rendered = if let Some(wav) = kept {
            wav
        } else {
            let wav = speaker
                .backend
                .render(preview_text(language), &speaker.voice)
                .and_then(|pcm| wav(&pcm, RATE))
                .map_err(|e| Refusal::new(RefusalCode::Failed, format!("rendering the sample failed: {e:#}")))?;
            if let Some(file) = &file {
                if let Err(e) = keep(file, &wav) {
                    eprintln!("voice: keeping the sample at {} failed: {e:#}", file.display());
                }
            }
            wav
        };
        let wav = Arc::new(rendered);
        *held = Some(wav.clone());
        Ok(wav)
    }
}

/// A name stable across builds: FNV-1a over the language, voice and text.
fn preview_file(language: &str, voice: &str) -> String {
    let hash = [language, voice, preview_text(language)].iter().flat_map(|part| part.bytes().chain([0])).fold(
        0xcbf2_9ce4_8422_2325_u64,
        |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3),
    );
    format!("{hash:016x}.wav")
}

fn keep(file: &Path, wav: &[u8]) -> anyhow::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = file.with_extension("partial");
    std::fs::write(&partial, wav)?;
    std::fs::rename(&partial, file)?;
    Ok(())
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

/// Incoming calls being answered, one per room. Each takes only the `Start`
/// of the call Note opened for it; one that comes before Note names that call
/// is held until it does.
#[derive(Default)]
struct Answering(Mutex<HashMap<String, Pending>>);

#[derive(Default)]
struct Pending {
    call_id: Option<String>,
    early: Vec<InboundStart>,
    waiter: Option<oneshot::Sender<InboundStart>>,
}

/// An attempt's hold on its room. Dropping it frees the room and turns away
/// any `Start` it still holds.
struct Attempt {
    answering: Arc<Answering>,
    room: String,
    turn_away: Box<dyn Fn(InboundStart) + Send + Sync>,
}

impl Answering {
    fn begin(self: &Arc<Self>, room: &str, turn_away: impl Fn(InboundStart) + Send + Sync + 'static) -> Attempt {
        lock(&self.0).insert(room.to_string(), Pending::default());
        Attempt { answering: self.clone(), room: room.to_string(), turn_away: Box::new(turn_away) }
    }

    fn busy(&self, room: &str) -> bool {
        lock(&self.0).contains_key(room)
    }

    /// Hands `start` to the attempt in its room; `Err` gives back a `Start` no attempt is waiting for.
    fn start(&self, room: &str, start: InboundStart) -> Result<(), InboundStart> {
        let mut rooms = lock(&self.0);
        let Some(pending) = rooms.get_mut(room) else { return Err(start) };
        match &pending.call_id {
            None => {
                pending.early.push(start);
                Ok(())
            }
            Some(id) if *id == start.call_id => match pending.waiter.take() {
                Some(waiter) => waiter.send(start),
                None => Err(start),
            },
            Some(_) => Err(start),
        }
    }
}

impl Attempt {
    /// From now on only `call_id`'s `Start` is taken; any other held is turned away.
    fn expect(&self, call_id: &str) -> oneshot::Receiver<InboundStart> {
        let (tx, rx) = oneshot::channel();
        let others = {
            let mut rooms = lock(&self.answering.0);
            let pending = rooms.entry(self.room.clone()).or_default();
            pending.call_id = Some(call_id.to_string());
            let (mine, others): (Vec<_>, Vec<_>) =
                std::mem::take(&mut pending.early).into_iter().partition(|s| s.call_id == call_id);
            match mine.into_iter().next() {
                Some(start) => drop(tx.send(start)),
                None => pending.waiter = Some(tx),
            }
            others
        };
        for start in others {
            (self.turn_away)(start);
        }
        rx
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        let held = lock(&self.answering.0).remove(&self.room).map(|p| p.early).unwrap_or_default();
        for start in held {
            (self.turn_away)(start);
        }
    }
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
    sidecars: Arc<Sidecars>,
    lines: Arc<Lines>,
    cues: Arc<Cues>,
    reporting: Mutex<HashSet<i64>>,
    answering: Arc<Answering>,
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
        let live = self.sidecars.live();
        let base = self.backends.engines.tts(language).live(&live);
        Ok(Reply::Voices { voices: voice_options(language, base.as_ref(), &live) })
    }

    async fn preview(self: Arc<Self>, language: String, voice: String) -> Result<Reply, Refusal> {
        use base64::Engine as _;
        if let Some(refusal) = self.no_models() {
            return Err(refusal);
        }
        let wav = tokio::task::spawn_blocking(move || {
            let live = self.sidecars.live();
            let base = self.backends.engines.tts(&language).live(&live);
            self.previews.get(base.as_ref(), &live, &language, &voice)
        })
        .await
        .map_err(|e| Refusal::new(RefusalCode::Failed, format!("rendering the sample failed: {e}")))??;
        Ok(Reply::Audio { wav_base64: base64::engine::general_purpose::STANDARD.encode(&*wav) })
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
        {
            let mut st = lock(&self.state);
            st.data.profiles.insert(mxid.to_string(), profile.clone());
            let _ = st.save();
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
            sidecars: self.sidecars.live(),
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
        match end.failure() {
            Some(reason) => self.finish(call_id, Outcome::Failed { reason: reason.into() }),
            None => self.end(call_id),
        }
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
        self.answering.busy(room)
            || lock(&self.state).data.calls.values().any(|c| c.room_id == room && !c.done)
    }

    /// Asks Note to open the user's call; with no Note to take it, answers to say so.
    async fn answer_incoming(self: Arc<Self>, attempt: Attempt, mxid: String, key: String, device: Option<String>) {
        let room = attempt.room.clone();
        let ask = Request::IncomingCall { room_id: room.clone(), mxid: mxid.clone(), key };
        let why = match tokio::time::timeout(INCOMING_CALL_WAIT, self.peer().request(ask)).await {
            Ok(Ok(Reply::Call { call_id })) => {
                let mut started = attempt.expect(&call_id);
                let start = if let Ok(start) = tokio::time::timeout(INBOUND_START_WAIT, &mut started).await {
                    start.ok()
                } else {
                    started.close();
                    started.try_recv().ok()
                };
                if let Some(start) = start {
                    return self.answer_for_note(start, &room, &mxid, device.as_deref()).await;
                }
                format!("the Start for {call_id} never came")
            }
            Ok(Ok(reply)) => format!("Note answered {reply:?}"),
            Ok(Err(refusal)) => refusal.to_string(),
            Err(_) => "no answer in time".into(),
        };
        eprintln!("voice: Note cannot take {mxid}'s call in {room} ({why}); answering to say so");
        self.cant_reach(&room, &mxid, device.as_deref()).await;
    }

    fn turn_away(&self, start: &InboundStart) {
        self.finish(&start.call_id, Outcome::Failed { reason: "no incoming call to answer".into() });
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
            Ok(media) => say_and_leave(&*media, self.cant_reach_clips(mxid).await).await,
            Err(e) => eprintln!("voice: joining {mxid}'s call in {room} failed: {e:#}"),
        }
        clear_member(&self.matrix, room).await;
    }

    /// The ready cue and `Line::CantReach` as `mxid` last heard them: in the language's base voice, or
    /// its kept rendering while that voice is down.
    async fn cant_reach_clips(&self, mxid: &str) -> Vec<Vec<i16>> {
        let profile = lock(&self.state).data.profiles.get(mxid).cloned().unwrap_or_default();
        let mut clips = Vec::new();
        if profile.cue {
            clips.push(self.cues.ready.to_vec());
        }
        let languages = self.backends.engines.languages();
        let Some(language) = languages.iter().find(|l| **l == profile.language).or(languages.first()).cloned() else {
            return clips;
        };
        let (engines, lines, live) = (self.backends.engines.clone(), self.lines.clone(), self.sidecars.live());
        let line = tokio::task::spawn_blocking(move || {
            let base = engines.tts(&language);
            match base.live(&live) {
                Some(backend) => lines.get(&Speaker::new(backend.clone(), ""), &*backend, &language, Line::CantReach),
                None => lines
                    .kept(base.id(), "", &language, Line::CantReach)
                    .ok_or_else(|| anyhow::anyhow!("the {language} voice is down and never rendered it")),
            }
        })
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
        let start = InboundStart { call_id: call_id.to_string(), profile, hang_up };
        if let Err(start) = self.answering.start(room_id, start) {
            self.turn_away(&start);
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
        let svc = self.clone();
        let attempt = self.answering.begin(&room, move |start| svc.turn_away(&start));
        eprintln!("voice: {mxid} is calling in {room}");
        tokio::spawn(self.clone().answer_incoming(attempt, mxid, key, device));
    }

    async fn sync_forever(self: Arc<Self>) {
        let mut detector = Detector::default();
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

/// Renders each loaded language's call lines in its base voice's default, off the runtime, so they
/// are kept for when that voice is down; a language whose base is down now is left for the next time.
fn warm_lines(engines: &Arc<dyn SpeechEngines>, lines: &Arc<Lines>, sidecars: &[Arc<dyn SpeechBackend>]) {
    let (engines, lines, sidecars) = (engines.clone(), lines.clone(), sidecars.to_vec());
    tokio::task::spawn_blocking(move || {
        for language in engines.languages() {
            let Some(base) = engines.tts(&language).live(&sidecars) else { continue };
            let speaker = Speaker::new(base.clone(), "");
            for line in crate::audio::lines::ALL {
                if let Err(e) = lines.get(&speaker, &*base, &language, line) {
                    eprintln!("voice: rendering {line:?} in {language} failed: {e:#}");
                }
            }
        }
    });
}

/// The base voice's voices, then each other live sidecar's as `<sidecar>:<voice>`, keeping only
/// those that speak `language`.
fn voice_options(
    language: &str,
    base: Option<&Arc<dyn SpeechBackend>>,
    sidecars: &[Arc<dyn SpeechBackend>],
) -> Vec<VoiceOption> {
    let base_voices = base.into_iter().flat_map(|b| b.voices().into_iter().map(move |v| (b, v.id.clone(), v)));
    let sidecar_voices = sidecars
        .iter()
        .filter(|b| base.is_none_or(|base| base.id() != b.id()))
        .flat_map(|b| b.voices().into_iter().map(move |v| (b, format!("{}:{}", b.id(), v.id), v)));
    base_voices
        .chain(sidecar_voices)
        .filter(|(_, _, v)| v.languages.is_empty() || v.languages.iter().any(|l| l == language))
        .map(|(backend, id, v)| VoiceOption {
            id,
            label: v.label,
            language: language.to_owned(),
            backend: backend.label().to_owned(),
            slow: backend.slow(),
            credit: v.credit,
        })
        .collect()
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
    let lines = Arc::new(Lines::new(Some(cfg.state_dir.join("lines"))));
    warm_lines(&backends.engines, &lines, &[]);
    let sidecars = Arc::new(Sidecars::new(cfg.tts.sidecars.clone()));
    {
        let (engines, lines, sidecars) = (backends.engines.clone(), lines.clone(), sidecars.clone());
        sidecars.clone().watch(move || warm_lines(&engines, &lines, &sidecars.live()));
    }
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
        sidecars,
        lines,
        cues: Arc::new(cues),
        reporting: Mutex::new(HashSet::new()),
        answering: Arc::default(),
        previews: Previews { dir: Some(cfg.state_dir.join("previews")), ..Previews::default() },
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
    use crate::audio::tts::{ChunkedBackend, Renderer};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_failed_model_load_leaves_no_languages_instead_of_an_error() {
        assert!(loaded_or_empty(Err(anyhow::anyhow!("libonnxruntime.so not found"))).languages().is_empty());
    }

    #[test]
    fn the_wav_header_is_44_bytes_at_48_khz() {
        let bytes = wav(&[0, 1, -1], RATE).unwrap();
        assert_eq!(bytes.len(), 44 + 3 * 2);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 48_000);
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 1, "mono");
    }

    fn inbound(call_id: &str) -> InboundStart {
        InboundStart { call_id: call_id.into(), profile: VoiceProfile::default(), hang_up: watch::channel(false).1 }
    }

    fn attempt(answering: &Arc<Answering>) -> (Attempt, Arc<Mutex<Vec<String>>>) {
        let turned_away = Arc::new(Mutex::new(Vec::new()));
        let seen = turned_away.clone();
        (answering.begin("!dm:t", move |s| lock(&seen).push(s.call_id)), turned_away)
    }

    #[test]
    fn a_start_before_note_names_the_call_is_held_for_it() {
        let answering = Arc::new(Answering::default());
        let (attempt, turned_away) = attempt(&answering);
        assert!(answering.start("!dm:t", inbound("b")).is_ok());
        assert_eq!(attempt.expect("b").try_recv().unwrap().call_id, "b");
        assert!(lock(&turned_away).is_empty());
    }

    #[test]
    fn a_late_start_from_an_earlier_attempt_takes_nothing() {
        let answering = Arc::new(Answering::default());
        let (attempt, turned_away) = attempt(&answering);
        assert!(answering.start("!dm:t", inbound("a")).is_ok(), "held until Note names the call");
        let mut started = attempt.expect("b");
        assert_eq!(*lock(&turned_away), ["a"]);
        assert_eq!(answering.start("!dm:t", inbound("a")).unwrap_err().call_id, "a");
        assert!(started.try_recv().is_err(), "still waiting");
        assert!(answering.start("!dm:t", inbound("b")).is_ok());
        assert_eq!(started.try_recv().unwrap().call_id, "b");
    }

    #[test]
    fn a_start_with_no_attempt_is_turned_away() {
        let answering = Answering::default();
        assert_eq!(answering.start("!dm:t", inbound("a")).unwrap_err().call_id, "a");
    }

    #[test]
    fn a_panicking_attempt_frees_its_room() {
        let answering = Arc::new(Answering::default());
        let turned_away = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (_attempt, turned_away) = attempt(&answering);
            assert!(answering.busy("!dm:t"));
            assert!(answering.start("!dm:t", inbound("a")).is_ok());
            std::panic::panic_any(turned_away)
        }))
        .unwrap_err()
        .downcast::<Arc<Mutex<Vec<String>>>>()
        .unwrap();
        assert!(!answering.busy("!dm:t"));
        assert_eq!(*lock(&turned_away), ["a"]);
    }

    #[derive(Default)]
    struct CountingTts(AtomicUsize);

    impl Renderer for CountingTts {
        fn render(&self, text: &str, _voice: &str) -> anyhow::Result<Vec<i16>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
            Ok(vec![1; text.len()])
        }
    }

    /// Voice ids may carry a language after '@' ("yui@ja"); Kokoro's speak English.
    fn backend(id: &str, label: &str, tts: &Arc<CountingTts>, ids: &[&str]) -> Arc<dyn SpeechBackend> {
        let voices = ids
            .iter()
            .map(|v| {
                let (v, languages) = match v.split_once('@') {
                    Some((v, language)) => (v, vec![language.to_owned()]),
                    None if id == "kokoro" => (*v, vec!["en".to_owned()]),
                    None => (*v, Vec::new()),
                };
                VoiceInfo { id: v.into(), label: v.into(), languages, credit: None }
            })
            .collect();
        Arc::new(ChunkedBackend::new(id, label, tts.clone(), voices))
    }

    fn kokoro(tts: &Arc<CountingTts>) -> Arc<dyn SpeechBackend> {
        backend("kokoro", "Kokoro", tts, &["af_heart", "bm_george"])
    }

    #[test]
    fn a_voice_not_offered_is_refused_and_not_cached() {
        let (tts, previews) = (Arc::new(CountingTts::default()), Previews::default());
        let sidecars = [backend("kyutai", "Natural", &tts, &["alba"])];
        for voice in ["a1", "kyutai:a1", "gone:alba", "kokoro:af_heart"] {
            let refused = previews.get(Some(&kokoro(&tts)), &sidecars, "en", voice).unwrap_err();
            assert_eq!(refused.code, RefusalCode::BadRequest, "{voice}");
        }
        assert_eq!(tts.0.load(Ordering::SeqCst), 0, "nothing is rendered");
        assert!(lock(&previews.slots).is_empty(), "nothing is cached");
    }

    #[test]
    fn concurrent_asks_for_one_voice_render_it_once() {
        let (tts, previews) = (Arc::new(CountingTts::default()), Arc::new(Previews::default()));
        let asks: Vec<_> = (0..4)
            .map(|_| {
                let (tts, previews) = (tts.clone(), previews.clone());
                std::thread::spawn(move || previews.get(Some(&kokoro(&tts)), &[], "en", "af_heart").unwrap())
            })
            .collect();
        for ask in asks {
            ask.join().unwrap();
        }
        assert_eq!(tts.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_preview_is_rendered_once_per_voice() {
        let (tts, previews) = (Arc::new(CountingTts::default()), Previews::default());
        let sidecars = [backend("kyutai", "Natural", &tts, &["alba"])];
        let first = previews.get(Some(&kokoro(&tts)), &sidecars, "en", "af_heart").unwrap();
        assert_eq!(previews.get(Some(&kokoro(&tts)), &sidecars, "en", "af_heart").unwrap(), first);
        previews.get(Some(&kokoro(&tts)), &sidecars, "en", "bm_george").unwrap();
        previews.get(Some(&kokoro(&tts)), &sidecars, "en", "kyutai:alba").unwrap();
        assert_eq!(tts.0.load(Ordering::SeqCst), 3);
        assert_eq!(first.len(), 44 + preview_text("en").len() * 2);
    }

    #[test]
    fn a_kept_preview_outlives_a_restart() {
        let (tts, dir) = (Arc::new(CountingTts::default()), tempfile::tempdir().unwrap());
        let previews = || Previews { dir: Some(dir.path().to_path_buf()), ..Previews::default() };
        let first = previews().get(Some(&kokoro(&tts)), &[], "en", "af_heart").unwrap();
        assert_eq!(previews().get(Some(&kokoro(&tts)), &[], "en", "af_heart").unwrap(), first);
        previews().get(Some(&kokoro(&tts)), &[], "ja", "").unwrap();
        assert_eq!(tts.0.load(Ordering::SeqCst), 2, "each language renders once");
    }

    #[test]
    fn a_japanese_preview_speaks_japanese_through_a_live_sidecar_only() {
        let (tts, previews) = (Arc::new(CountingTts::default()), Previews::default());
        let ja = backend("ja", "Japanese", &tts, &["yui@ja"]);
        let down = previews.get(None, &[], "ja", "").unwrap_err();
        assert_eq!(down.code, RefusalCode::Failed);
        assert_eq!(previews.get(None, &[], "ja", "ja:yui").unwrap_err().code, RefusalCode::BadRequest);
        let live = std::slice::from_ref(&ja);
        let sample = previews.get(Some(&ja), live, "ja", "").unwrap();
        assert_eq!(sample.len(), 44 + preview_text("ja").len() * 2, "the Japanese wording");
        assert_eq!(previews.get(None, live, "ja", "ja:yui").unwrap().len(), sample.len());
    }

    #[test]
    fn the_voice_list_is_kokoro_then_each_sidecar_under_its_label() {
        let tts = Arc::new(CountingTts::default());
        let sidecars = [backend("kyutai", "Natural", &tts, &["alba"])];
        let listed: Vec<(String, String, String)> =
            voice_options("en", Some(&kokoro(&tts)), &sidecars).into_iter().map(|v| (v.id, v.backend, v.language)).collect();
        let expected = [("af_heart", "Kokoro"), ("bm_george", "Kokoro"), ("kyutai:alba", "Natural")]
            .map(|(id, backend)| (id.to_owned(), backend.to_owned(), "en".to_owned()));
        assert_eq!(listed, expected);
    }

    #[test]
    fn the_voice_list_keeps_the_voices_that_speak_the_language() {
        let tts = Arc::new(CountingTts::default());
        let ja = backend("ja", "Japanese", &tts, &["yui@ja", "ren@ja"]);
        let sidecars = [backend("chatterbox", "Best", &tts, &["alba", "sora@ja"]), ja.clone()];
        let ids = |language: &str, base: Option<&Arc<dyn SpeechBackend>>| {
            voice_options(language, base, &sidecars).into_iter().map(|v| v.id).collect::<Vec<_>>()
        };
        assert_eq!(ids("ja", Some(&ja)), ["yui", "ren", "chatterbox:alba", "chatterbox:sora"], "the base is listed bare");
        assert_eq!(ids("ja", None), ["chatterbox:alba", "chatterbox:sora", "ja:yui", "ja:ren"], "the base voice is down");
        assert_eq!(ids("en", Some(&kokoro(&tts))), ["af_heart", "bm_george", "chatterbox:alba"]);
    }

    #[test]
    #[ignore = "needs NOTE_VOICE_MODELS"]
    fn preview_is_a_wav_and_cached() {
        let dir = std::env::var_os("NOTE_VOICE_MODELS").unwrap();
        let models = crate::config::models_from_dir(std::path::Path::new(&dir), &crate::config::TtsConfig::default());
        let engines = Engines::load(&models, crate::audio::engines::Device::Auto).unwrap();
        let tts = engines.tts("en").live(&[]).unwrap();
        let previews = Previews::default();
        let first = previews.get(Some(&tts), &[], "en", "").unwrap();
        let reader = hound::WavReader::new(std::io::Cursor::new(first.to_vec())).unwrap();
        assert_eq!((reader.spec().sample_rate, reader.spec().channels), (48_000, 1));
        assert!(reader.duration() > 48_000, "at least a second of speech");
        assert!(Arc::ptr_eq(&previews.get(Some(&tts), &[], "en", "").unwrap(), &first));
    }
}
