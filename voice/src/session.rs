use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use note_voice_proto::{CallBody, Direction, Floor, LiveState, VoiceProfile};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use crate::audio::engines::{SpeechEngines, SpeechToText, TurnDetector, Vad};
use crate::audio::language::{confident, Choice, MIN_SECONDS};
use crate::audio::lines::{Line, Lines};
use crate::audio::playout::{Clip, Playout};
use crate::audio::speech::SpeechQueue;
use crate::audio::tts::{speaker, Mute, Speaker, SpeechBackend};
use crate::audio::turn::{backchannels, Action, Input, TurnConfig, TurnMachine};
use crate::media::{Gone, MediaIo};

const RATE: usize = 16_000;
const VAD_WINDOW: usize = 512;
const STT_CHUNK: usize = RATE * 160 / 1000;
const TURN_SPAN: usize = RATE * 8;
const TICK: Duration = Duration::from_millis(10);
const LOST_NOTICE: Duration = Duration::from_secs(1);
/// How long an ending waits on audio that makes no progress.
const DRAIN_CAP: Duration = Duration::from_secs(5);
/// The longest an ending waits on audio that keeps playing.
const DRAIN_LIMIT: Duration = Duration::from_secs(60);
/// What the media path still holds once playout is empty, played out before leaving.
const DRAIN_TAIL: Duration = Duration::from_millis(500);
const FILLER_AFTER: Duration = Duration::from_millis(1500);
/// The voiced audio the caller's language is first judged from, unless their first turn ends sooner.
const ID_SPAN: usize = RATE * 5 / 2;
/// A turn with less voiced audio than this does not count toward `ID_TURNS`.
const ID_MIN: usize = RATE * 3 / 10;
/// The voiced audio that has the language judged at a pause, ahead of the turn's end.
const ID_AT_PAUSE: usize = RATE;
/// Turns, and voiced audio, heard without a confident language before the call keeps the user's.
const ID_TURNS: u32 = 3;
const ID_CAP: usize = RATE * 8;
/// An inbound caller silent this long from the start is asked if they are there.
const HELLO_AFTER: Duration = Duration::from_secs(8);
/// Silent this long after that, the call ends.
const NO_ONE_AFTER: Duration = Duration::from_secs(20);

/// 48 kHz mono audio for the call's cues; an empty one plays nothing.
pub struct Cues {
    pub ready: Arc<Vec<i16>>,
    pub heard: Arc<Vec<i16>>,
}

impl Cues {
    /// Each cue is the first of its candidates that reads: WAV at any rate, or raw 48 kHz mono s16le.
    /// A cue none of whose files read is logged and left silent.
    pub fn load(ready: &[PathBuf], heard: &[PathBuf]) -> Cues {
        Cues { ready: Arc::new(first_cue(ready)), heard: Arc::new(first_cue(heard)) }
    }
}

fn first_cue(candidates: &[PathBuf]) -> Vec<i16> {
    for path in candidates {
        match read_cue(path) {
            Ok(pcm) => return pcm,
            Err(e) => eprintln!("voice: the cue {} is unusable: {e:#}", path.display()),
        }
    }
    Vec::new()
}

fn read_cue(path: &Path) -> anyhow::Result<Vec<i16>> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("wav") => read_wav(path),
        Some("ogg" | "oga" | "opus") => anyhow::bail!("Ogg cues are not supported"),
        _ => Ok(std::fs::read(path)?.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect()),
    }
}

fn read_wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|s| s as f32 / scale)).collect::<Result<_, _>>()?
        }
    };
    let channels = usize::from(spec.channels.max(1));
    let mono: Vec<f32> = samples.chunks(channels).map(|c| c.iter().sum::<f32>() / c.len() as f32).collect();
    Ok(to_48k(&mono, spec.sample_rate))
}

/// Linear interpolation from `rate` to 48 kHz.
fn to_48k(samples: &[f32], rate: u32) -> Vec<i16> {
    let pcm = |x: f32| (x.clamp(-1.0, 1.0) * 32767.0).round() as i16;
    if samples.is_empty() || rate == 0 {
        return Vec::new();
    }
    let step = f64::from(rate) / 48_000.0;
    let len = (samples.len() as f64 / step).round() as usize;
    (0..len)
        .map(|i| {
            let pos = i as f64 * step;
            let at = pos.floor() as usize;
            let a = samples[at.min(samples.len() - 1)];
            let b = samples.get(at + 1).copied().unwrap_or(a);
            pcm(a + (b - a) * (pos - pos.floor()) as f32)
        })
        .collect()
}

pub struct SessionDeps {
    pub engines: Arc<dyn SpeechEngines>,
    pub lines: Arc<Lines>,
    pub cues: Arc<Cues>,
    pub profile: VoiceProfile,
    /// The speech sidecars up as the call starts.
    pub sidecars: Vec<Arc<dyn SpeechBackend>>,
    /// On an inbound call Note waits for the caller's first words, prompting once if they stay silent.
    pub direction: Direction,
    pub max_len: Duration,
    pub link_grace: Duration,
}

pub enum SessionIn {
    Frame(CallBody),
    LinkUp(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEnd {
    HungUp,
    UserLeft,
    TimedOut,
    LinkLost,
    /// The language's base voice was down as the call started.
    VoiceDown,
    /// A reply could not be spoken by any backend mid-call.
    VoiceLost,
    /// Speech recognition stopped mid-call.
    EarsLost,
    /// The caller never spoke.
    NoOneThere,
    MediaFailed(String),
}

impl SessionEnd {
    /// Why Note should take over by message, for an end that leaves the user unanswered.
    pub fn failure(&self) -> Option<&'static str> {
        match self {
            SessionEnd::VoiceDown => Some("the call's voice is down"),
            SessionEnd::VoiceLost => Some("the call's voice was lost"),
            SessionEnd::EarsLost => Some("the call's speech recognition stopped"),
            _ => None,
        }
    }
}

enum Event {
    Vad { at: Duration, speech: bool },
    Partial(String),
    Score { at: Duration, p: f32 },
    Finished { turn: u64, text: String },
    /// The caller's first words settled the call's language.
    Language(Choice),
    /// `pcm` is None for a line that failed to render.
    Line { language: String, line: Line, pcm: Option<Arc<Vec<i16>>> },
}

enum SttCmd {
    /// `voiced`: the VAD heard speech in the chunk.
    Accept { samples: Vec<f32>, voiced: bool },
    Finish { turn: u64 },
    /// The caller paused.
    Pause,
    Reset,
}

enum SttOut {
    Partial(Option<Choice>, String),
    Finished(Option<Choice>, Option<(u64, String)>),
}

/// Runs one answered call until it ends. Outgoing frames go through `send` (journaled by the caller).
pub async fn run_session(
    deps: SessionDeps,
    media: Box<dyn MediaIo>,
    inbox: mpsc::UnboundedReceiver<SessionIn>,
    send: impl Fn(CallBody) + Send + Sync + 'static,
) -> SessionEnd {
    let media: Arc<dyn MediaIo> = Arc::from(media);
    let end = match Live::start(&deps, media.clone(), send) {
        Ok((mut live, mut tasks)) => {
            let end = live.run(inbox, &mut tasks, deps.max_len, deps.link_grace).await;
            tasks.audio.abort();
            tasks.stt.abort();
            end
        }
        Err(e) => SessionEnd::MediaFailed(format!("{e:#}")),
    };
    media.leave().await;
    end
}

struct Tasks {
    audio: JoinHandle<()>,
    stt: JoinHandle<()>,
}

struct Ending {
    end: SessionEnd,
    stalled_by: Instant,
    limit: Instant,
    drained_at: Option<Instant>,
}

impl Ending {
    fn new(end: SessionEnd) -> Self {
        let now = Instant::now();
        Self { end, stalled_by: now + DRAIN_CAP, limit: now + DRAIN_LIMIT, drained_at: None }
    }

    /// The end, once every sound has played out or the audio has stopped moving.
    fn due(&mut self, drained: bool, now: Instant) -> Option<SessionEnd> {
        if !drained {
            self.drained_at = None;
        } else if self.drained_at.is_none() {
            self.drained_at = Some(now);
        }
        let played_out = self.drained_at.is_some_and(|at| now.duration_since(at) >= DRAIN_TAIL);
        (played_out || now >= self.stalled_by || now >= self.limit).then(|| self.end.clone())
    }
}

struct Live<S> {
    send: S,
    engines: Arc<dyn SpeechEngines>,
    sidecars: Vec<Arc<dyn SpeechBackend>>,
    line_store: Arc<Lines>,
    voice: String,
    /// The language the call speaks, settled by what the caller says.
    language: String,
    direction: Direction,
    /// The caller has made a sound, or Note has started to speak.
    broken_silence: bool,
    hello_at: Option<Instant>,
    media: Arc<dyn MediaIo>,
    playout: Playout,
    speech: SpeechQueue,
    turn: TurnMachine,
    detector: Arc<dyn TurnDetector>,
    recent: Arc<Mutex<VecDeque<f32>>>,
    stt: mpsc::UnboundedSender<SttCmd>,
    events_tx: mpsc::UnboundedSender<Event>,
    events: mpsc::UnboundedReceiver<Event>,
    heard_cue: Option<Arc<Vec<i16>>>,
    /// The lines rendered so far.
    lines: HashMap<Line, Option<Arc<Vec<i16>>>>,
    /// The line the ending waits to say once rendered.
    closing: Option<Line>,
    /// "One moment" by `FILLER_AFTER` past the Commit at the instant, unless a reply is speaking.
    awaiting_reply: Option<Instant>,
    drafted: Option<u64>,
    start: Instant,
    playing: bool,
    played_reply: bool,
    link_down_since: Option<Instant>,
    lost_played: bool,
    ending: Option<Ending>,
    /// The caller is speaking.
    hearing: bool,
    /// A turn is committed and Note's reply has not started playing.
    thinking: bool,
    shown: Option<LiveState>,
}

impl<S: Fn(CallBody)> Live<S> {
    fn start(deps: &SessionDeps, media: Arc<dyn MediaIo>, send: S) -> anyhow::Result<(Self, Tasks)> {
        let languages = deps.engines.languages();
        let language = if languages.contains(&deps.profile.language) {
            deps.profile.language.clone()
        } else {
            languages.first().cloned().ok_or_else(|| anyhow::anyhow!("no voice models"))?
        };
        let vad = deps.engines.vad(&language)?;
        let speakable: Vec<String> =
            languages.iter().filter(|l| deps.engines.tts(l).live(&deps.sidecars).is_some()).cloned().collect();
        let ears = Ears::new(deps.engines.clone(), &speakable, &language)?;
        let mut turn = TurnMachine::new(TurnConfig::default(), backchannels(&language));
        turn.set_blind(ears.chosen.is_none());
        let base = deps.engines.tts(&language);
        let voice_up = base.live(&deps.sidecars);
        let base: Arc<dyn SpeechBackend> = voice_up.clone().unwrap_or_else(|| Arc::new(Mute::new(base.id())));
        let speaker = speaker(&deps.profile.voice, &language, &base, &deps.sidecars);
        let start = Instant::now();
        let (events_tx, events) = mpsc::unbounded_channel();
        let (stt_tx, stt_rx) = mpsc::unbounded_channel();
        let recent = Arc::new(Mutex::new(VecDeque::with_capacity(TURN_SPAN)));
        let tasks = Tasks {
            stt: tokio::spawn(stt_worker(ears, stt_rx, events_tx.clone())),
            audio: tokio::spawn(audio_in(media.clone(), vad, stt_tx.clone(), events_tx.clone(), recent.clone(), start)),
        };
        let speech = SpeechQueue::new(speaker.clone(), base.clone());
        let order: &'static [Line] = match (voice_up.is_some(), deps.direction) {
            (false, _) => &[Line::NoVoice],
            (true, Direction::Inbound) => &[
                Line::OneMoment,
                Line::Hello,
                Line::NoOneThere,
                Line::LostNotes,
                Line::Goodbye,
                Line::NoVoice,
                Line::NoEars,
            ],
            (true, Direction::Outbound) => SPOKEN_TO,
        };
        {
            let (lines, rendering, language, events) =
                (deps.lines.clone(), speech.rendering(), language.clone(), events_tx.clone());
            tokio::task::spawn_blocking(move || {
                render_lines(&lines, &speaker, &base, &language, order, &rendering, &events);
            });
        }
        let mut playout = Playout::default();
        if deps.profile.cue {
            playout.push(Clip { reply: None, chars: 0, pcm: deps.cues.ready.to_vec() });
        }
        if voice_up.is_none() {
            eprintln!("voice: the {language} voice is down; the call ends after its kept line");
        }
        let live = Live {
            send,
            engines: deps.engines.clone(),
            sidecars: deps.sidecars.clone(),
            line_store: deps.lines.clone(),
            voice: deps.profile.voice.clone(),
            language: language.clone(),
            direction: deps.direction,
            broken_silence: false,
            hello_at: None,
            media,
            playout,
            speech,
            turn,
            detector: deps.engines.turn(&language),
            recent,
            stt: stt_tx,
            events_tx,
            events,
            heard_cue: deps.profile.cue.then(|| deps.cues.heard.clone()),
            lines: HashMap::new(),
            closing: voice_up.is_none().then_some(Line::NoVoice),
            awaiting_reply: None,
            drafted: None,
            start,
            playing: false,
            played_reply: false,
            link_down_since: None,
            lost_played: false,
            ending: voice_up.is_none().then(|| Ending::new(SessionEnd::VoiceDown)),
            hearing: false,
            thinking: false,
            shown: None,
        };
        Ok((live, tasks))
    }

    /// Ends with `MediaFailed` if the audio-in task panics, and says why before ending if the STT worker
    /// stops; audio-in running out of audio is left to `left()`.
    async fn run(
        &mut self,
        mut inbox: mpsc::UnboundedReceiver<SessionIn>,
        tasks: &mut Tasks,
        max_len: Duration,
        link_grace: Duration,
    ) -> SessionEnd {
        let media = self.media.clone();
        let left = media.left();
        tokio::pin!(left);
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Burst);
        let mut inbox_open = true;
        let mut audio_running = true;
        let mut stt_running = true;
        loop {
            tokio::select! {
                gone = &mut left => return match gone {
                    Gone::Left => SessionEnd::UserLeft,
                    Gone::Failed(reason) => SessionEnd::MediaFailed(reason),
                },
                got = &mut tasks.stt, if stt_running => {
                    stt_running = false;
                    eprintln!("voice: speech recognition stopped: {got:?}");
                    self.close_with(Line::NoEars, SessionEnd::EarsLost);
                }
                got = &mut tasks.audio, if audio_running => match got {
                    Ok(()) => audio_running = false,
                    Err(e) => return task_died("the audio input", Some(e)),
                },
                _ = tick.tick() => {
                    if let Some(end) = self.tick(max_len, link_grace).await {
                        return end;
                    }
                }
                Some(ev) = self.events.recv() => self.event(ev),
                got = inbox.recv(), if inbox_open => match got {
                    Some(SessionIn::Frame(body)) => self.frame(body),
                    Some(SessionIn::LinkUp(up)) => self.link(up),
                    None => inbox_open = false,
                },
            }
        }
    }

    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    async fn tick(&mut self, max_len: Duration, link_grace: Duration) -> Option<SessionEnd> {
        if self.ending.is_none() {
            let actions = self.turn.tick(self.now());
            self.act(actions);
        }
        self.speech.pump(&mut self.playout);
        for reply in self.speech.take_unspeakable() {
            eprintln!("voice: reply {reply} cannot be spoken; ending the call");
            self.speech.drop_reply(reply);
            self.close_with(Line::NoVoice, SessionEnd::VoiceLost);
        }
        if let Some(frame) = self.playout.next_frame() {
            if let Err(e) = self.media.send(&frame).await {
                return Some(SessionEnd::MediaFailed(format!("{e:#}")));
            }
            if let Some(ending) = self.ending.as_mut() {
                ending.stalled_by = Instant::now() + DRAIN_CAP;
            }
        }
        for reply in self.speech.take_finished(&mut self.playout) {
            (self.send)(CallBody::Played { reply });
            self.played_reply = true;
        }
        if self.played_reply && self.playout.is_empty() && self.speech.is_idle() {
            (self.send)(CallBody::Floor { floor: Floor::Drained });
            self.played_reply = false;
        }
        let playing = !self.playout.is_empty();
        if playing != self.playing {
            self.playing = playing;
            let actions = self.turn.input(Input::Playing { playing });
            self.act(actions);
        }
        if let Some(since) = self.link_down_since {
            if !self.lost_played && self.lines.contains_key(&Line::LostNotes) && since.elapsed() >= LOST_NOTICE {
                self.lost_played = true;
                self.play_line(Line::LostNotes);
            }
            if since.elapsed() >= link_grace {
                self.say_goodbye(SessionEnd::LinkLost);
            }
        }
        if self.start.elapsed() >= max_len {
            self.say_goodbye(SessionEnd::TimedOut);
        }
        if let Some(at) = self.awaiting_reply {
            if self.lines.contains_key(&Line::OneMoment) && at.elapsed() >= FILLER_AFTER {
                self.awaiting_reply = None;
                self.play_line(Line::OneMoment);
            }
        }
        self.wait_for_the_caller();
        if let Some(line) = self.closing.filter(|line| self.lines.contains_key(line)) {
            self.closing = None;
            self.play_line(line);
        }
        if self.playout.playing_reply() {
            self.thinking = false;
        }
        let state = shown(self.playout.is_playing(), self.hearing, self.thinking);
        if self.shown != Some(state) {
            self.shown = Some(state);
            self.media.show(state);
        }
        let drained = self.closing.is_none() && self.playout.is_empty() && self.speech.is_idle();
        self.ending.as_mut()?.due(drained, Instant::now())
    }

    /// An inbound caller who says nothing is asked once if they are there, then let go.
    fn wait_for_the_caller(&mut self) {
        if self.direction != Direction::Inbound || self.broken_silence || self.ending.is_some() {
            return;
        }
        match self.hello_at {
            None if self.start.elapsed() >= HELLO_AFTER && self.lines.contains_key(&Line::Hello) => {
                self.hello_at = Some(Instant::now());
                self.play_line(Line::Hello);
            }
            Some(at) if at.elapsed() >= NO_ONE_AFTER => self.close_with(Line::NoOneThere, SessionEnd::NoOneThere),
            _ => {}
        }
    }

    /// Speaks `choice.language` from here on: its recognizer is already listening, and replies not yet
    /// opened, the canned lines and the backchannels follow.
    fn settle(&mut self, choice: Choice) {
        if choice.language == self.language {
            return;
        }
        let base = self.engines.tts(&choice.language);
        let Some(base) = base.live(&self.sidecars) else {
            eprintln!("voice: the {} voice is down; the call keeps speaking {}", choice.language, self.language);
            return;
        };
        let speaker = speaker(&self.voice, &choice.language, &base, &self.sidecars);
        self.speech.set_voice(speaker.clone(), base.clone());
        self.turn.set_backchannels(backchannels(&choice.language));
        self.detector = self.engines.turn(&choice.language);
        self.language = choice.language;
        self.lines.clear();
        let (lines, rendering, language, events) =
            (self.line_store.clone(), self.speech.rendering(), self.language.clone(), self.events_tx.clone());
        tokio::task::spawn_blocking(move || {
            render_lines(&lines, &speaker, &base, &language, SPOKEN_TO, &rendering, &events);
        });
    }

    fn play_line(&mut self, line: Line) {
        if let Some(Some(pcm)) = self.lines.get(&line) {
            self.playout.push(Clip { reply: None, chars: 0, pcm: pcm.to_vec() });
        }
    }

    fn act(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::ScoreTurn => self.score_turn(),
                Action::Draft { turn, text } => {
                    self.drafted = Some(turn);
                    (self.send)(CallBody::Draft { turn, text, language: Some(self.language.clone()) });
                }
                Action::Commit { turn, .. } => {
                    let _ = self.stt.send(SttCmd::Finish { turn });
                }
                Action::Retract { turn } => {
                    self.drafted = self.drafted.filter(|&d| d != turn);
                    (self.send)(CallBody::Retract { turn });
                }
                Action::PausePlayout => self.playout.pause(),
                Action::ResumePlayout => {
                    self.playout.resume();
                    let _ = self.stt.send(SttCmd::Reset);
                }
                Action::FlushPlayout => {
                    self.media.clear();
                    for (reply, heard_chars) in self.speech.flush(&mut self.playout) {
                        (self.send)(CallBody::BargeIn { reply, heard_chars });
                    }
                    self.playout.resume();
                }
                Action::Floor(floor) => {
                    match floor {
                        Floor::UserSpeaking => {
                            self.hearing = true;
                            self.thinking = false;
                        }
                        Floor::UserQuiet => self.hearing = false,
                        Floor::Drained => {}
                    }
                    if floor == Floor::UserQuiet {
                        let _ = self.stt.send(SttCmd::Pause);
                    }
                    (self.send)(CallBody::Floor { floor });
                }
            }
        }
    }

    /// The score is stamped with the time it was asked for, so the machine can tell it from a stale one.
    fn score_turn(&self) {
        let at = self.now();
        let samples: Vec<f32> = self.recent.lock().expect("recent audio lock").iter().copied().collect();
        let detector = self.detector.clone();
        let events = self.events_tx.clone();
        tokio::spawn(async move {
            if let Ok(p) = tokio::task::spawn_blocking(move || detector.complete(&samples)).await {
                let _ = events.send(Event::Score { at, p });
            }
        });
    }

    fn event(&mut self, ev: Event) {
        let input = match ev {
            Event::Line { language, line, pcm } => {
                if language == self.language {
                    self.lines.insert(line, pcm);
                }
                return;
            }
            Event::Language(choice) => {
                self.turn.set_blind(false);
                self.settle(choice);
                return;
            }
            _ if self.ending.is_some() => return,
            Event::Vad { at, speech } => {
                self.broken_silence |= speech;
                Input::Vad { at, speech }
            }
            Event::Partial(text) => Input::Partial { text },
            Event::Score { at, p } => Input::TurnScore { at, p },
            Event::Finished { turn, text } => {
                let drafted = self.drafted.take_if(|&mut d| d == turn).is_some();
                if text.is_empty() {
                    if drafted {
                        (self.send)(CallBody::Retract { turn });
                    }
                } else {
                    (self.send)(CallBody::Commit { turn, text, language: Some(self.language.clone()) });
                    self.thinking = true;
                    self.awaiting_reply = Some(Instant::now());
                    if let Some(cue) = &self.heard_cue {
                        self.playout.push_front(Clip { reply: None, chars: 0, pcm: cue.to_vec() });
                    }
                }
                return;
            }
        };
        let actions = self.turn.input(input);
        self.act(actions);
    }

    fn frame(&mut self, body: CallBody) {
        if self.ending.is_some() {
            return;
        }
        let reply = match &body {
            CallBody::Speak { reply, .. } | CallBody::Play { reply } => Some(*reply),
            _ => None,
        };
        if matches!(body, CallBody::Speak { .. }) {
            self.broken_silence = true;
        }
        match body {
            CallBody::Speak { reply, idx, text } => self.speech.speak(reply, idx, text),
            CallBody::SpeakDone { reply } => self.speech.speak_done(reply),
            CallBody::Play { reply } => self.speech.play(reply),
            CallBody::Drop { reply } => self.speech.drop_reply(reply),
            CallBody::HangUp => {
                self.playout.resume();
                self.ending = Some(Ending::new(SessionEnd::HungUp));
            }
            _ => {}
        }
        if reply.is_some_and(|r| self.speech.is_speaking(r)) {
            self.awaiting_reply = None;
        }
    }

    fn link(&mut self, up: bool) {
        if up {
            self.link_down_since = None;
            self.lost_played = false;
        } else if self.link_down_since.is_none() {
            self.link_down_since = Some(Instant::now());
        }
    }

    /// Cuts whatever is playing and ends the call once the goodbye line has played.
    fn say_goodbye(&mut self, end: SessionEnd) {
        self.close_with(Line::Goodbye, end);
    }

    fn close_with(&mut self, line: Line, end: SessionEnd) {
        if self.ending.is_some() {
            return;
        }
        self.media.clear();
        self.speech.flush(&mut self.playout);
        self.playout.resume();
        self.closing = Some(line);
        self.ending = Some(Ending::new(end));
    }
}

/// The lines a call needs once the caller has spoken.
const SPOKEN_TO: &[Line] = &[Line::OneMoment, Line::LostNotes, Line::Goodbye, Line::NoVoice, Line::NoEars];

/// Gives the call each line at once: in `speaker` if already rendered, else in the base voice. A sidecar
/// voice's lines are then rendered in it one by one, only while no reply is rendering, and replace the
/// base's. A base that is down gives only what it rendered before.
fn render_lines(
    lines: &Lines,
    speaker: &Speaker,
    base: &Arc<dyn SpeechBackend>,
    language: &str,
    order: &[Line],
    rendering: &AtomicBool,
    events: &mpsc::UnboundedSender<Event>,
) {
    let on_base = speaker.backend.id() == base.id();
    let quick = if on_base { speaker.clone() } else { Speaker::new(base.clone(), "") };
    let mut upgrades = Vec::new();
    for &line in order {
        let pcm = lines.cached(speaker, language, line).or_else(|| {
            if !on_base {
                upgrades.push(line);
            }
            lines.get(&quick, &**base, language, line).map_err(|e| eprintln!("voice: rendering {line:?} failed: {e:#}")).ok()
        });
        if events.send(Event::Line { language: language.to_owned(), line, pcm }).is_err() {
            return;
        }
    }
    let busy = || rendering.load(Ordering::SeqCst) || events.is_closed();
    for line in upgrades {
        loop {
            while busy() {
                if events.is_closed() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            match lines.upgrade(speaker, language, line, &busy) {
                Ok(Some(pcm)) => {
                    if events.send(Event::Line { language: language.to_owned(), line, pcm: Some(pcm) }).is_err() {
                        return;
                    }
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("voice: rendering {line:?} in {} failed, keeping the base voice's: {e:#}", speaker.backend.id());
                    return;
                }
            }
        }
    }
}

fn task_died(what: &str, err: Option<tokio::task::JoinError>) -> SessionEnd {
    let reason = match err {
        Some(e) => format!("{what} failed: {e}"),
        None => format!("{what} stopped"),
    };
    eprintln!("voice: {reason}");
    SessionEnd::MediaFailed(reason)
}

/// Note's voice wins, then the caller's, then a reply on its way; a cue during that wait is not Note speaking.
fn shown(playing: bool, hearing: bool, thinking: bool) -> LiveState {
    if playing && !thinking {
        LiveState::Speaking
    } else if hearing {
        LiveState::Hearing
    } else if thinking {
        LiveState::Thinking
    } else {
        LiveState::Listening
    }
}

/// Splits the user's audio into VAD windows and STT chunks, and keeps the last 8 s for Smart Turn.
async fn audio_in(
    media: Arc<dyn MediaIo>,
    mut vad: Box<dyn Vad>,
    stt: mpsc::UnboundedSender<SttCmd>,
    events: mpsc::UnboundedSender<Event>,
    recent: Arc<Mutex<VecDeque<f32>>>,
    start: Instant,
) {
    let mut window = Vec::with_capacity(VAD_WINDOW * 2);
    let mut chunk = Vec::with_capacity(STT_CHUNK * 2);
    let mut voiced = false;
    while let Some(frame) = media.recv().await {
        {
            let mut recent = recent.lock().expect("recent audio lock");
            recent.extend(&frame);
            let excess = recent.len().saturating_sub(TURN_SPAN);
            recent.drain(..excess);
        }
        window.extend_from_slice(&frame);
        chunk.extend_from_slice(&frame);
        while window.len() >= VAD_WINDOW {
            let speech = vad.push(&window[..VAD_WINDOW]);
            window.drain(..VAD_WINDOW);
            voiced |= speech;
            let _ = events.send(Event::Vad { at: start.elapsed(), speech });
        }
        if chunk.len() >= STT_CHUNK {
            let _ = stt.send(SttCmd::Accept { samples: std::mem::take(&mut chunk), voiced: std::mem::take(&mut voiced) });
        }
    }
}

/// The call's recognizers. Until the caller's language is settled, every language the call can
/// speak listens at once with no partials shown, and the voiced audio of each turn is kept. It is
/// judged at `ID_SPAN`, at a pause with `ID_AT_PAUSE` heard, and at the end of each turn, over all
/// the voiced audio so far; the recognizer of a confident language, which heard the whole turn,
/// carries on alone. A turn that ends unsettled is heard in the fallback language and the language
/// stays open, until `ID_TURNS` such turns or `ID_CAP` of voiced audio settle it on the fallback.
struct Ears {
    engines: Arc<dyn SpeechEngines>,
    speakable: Vec<String>,
    fallback: String,
    listening: Vec<(String, Box<dyn SpeechToText>)>,
    voiced: Vec<f32>,
    /// Where the current turn's voiced audio starts in `voiced`.
    turn_from: usize,
    /// The voiced audio the last judgement saw.
    judged: usize,
    /// Turns that ended unsettled.
    turns: u32,
    chosen: Option<Box<dyn SpeechToText>>,
}

impl Ears {
    /// Settled on `fallback` from the start when there is nothing to choose between.
    fn new(engines: Arc<dyn SpeechEngines>, speakable: &[String], fallback: &str) -> anyhow::Result<Ears> {
        let choosing = engines.identifies() && speakable.len() > 1 && speakable.iter().any(|l| l == fallback);
        let mut ears = Ears {
            speakable: speakable.to_vec(),
            fallback: fallback.to_owned(),
            listening: Vec::new(),
            voiced: Vec::new(),
            turn_from: 0,
            judged: 0,
            turns: 0,
            chosen: None,
            engines,
        };
        if choosing {
            for language in speakable {
                ears.listening.push((language.clone(), ears.engines.stt(language)?));
            }
        } else {
            ears.chosen = Some(ears.engines.stt(fallback)?);
        }
        Ok(ears)
    }

    fn accept(&mut self, samples: &[f32], voiced: bool) -> Option<Choice> {
        if let Some(stt) = &mut self.chosen {
            stt.accept(samples);
            return None;
        }
        for (_, stt) in &mut self.listening {
            stt.accept(samples);
        }
        if voiced {
            let room = ID_CAP.saturating_sub(self.voiced.len());
            self.voiced.extend_from_slice(&samples[..samples.len().min(room)]);
        }
        if self.voiced.len() >= ID_CAP {
            return self.judge().or_else(|| Some(self.give_up()));
        }
        (self.voiced.len() >= ID_SPAN && self.judged < ID_SPAN).then(|| self.judge()).flatten()
    }

    fn partial(&mut self) -> String {
        self.chosen.as_mut().map(|stt| stt.partial()).unwrap_or_default()
    }

    fn pause(&mut self) -> Option<Choice> {
        let fresh = self.voiced.len() >= ID_AT_PAUSE && self.voiced.len() >= self.judged + ID_AT_PAUSE / 2;
        (self.chosen.is_none() && fresh).then(|| self.judge()).flatten()
    }

    fn finish(&mut self) -> (Option<Choice>, String) {
        let mut settled = None;
        if self.chosen.is_none() {
            settled = self.judge();
            if settled.is_none() && self.voiced.len() - self.turn_from >= ID_MIN {
                self.turns += 1;
                if self.turns >= ID_TURNS {
                    settled = Some(self.give_up());
                }
            }
        }
        if let Some(stt) = &mut self.chosen {
            return (settled, stt.finish());
        }
        let mut text = String::new();
        for (language, stt) in &mut self.listening {
            let heard = stt.finish();
            if *language == self.fallback {
                text = heard;
            }
        }
        self.turn_from = self.voiced.len();
        (None, text)
    }

    /// Forgets the current turn, its voiced audio with it.
    fn reset(&mut self) {
        if let Some(stt) = &mut self.chosen {
            stt.reset();
            return;
        }
        for (_, stt) in &mut self.listening {
            stt.reset();
        }
        self.voiced.truncate(self.turn_from);
        self.judged = self.judged.min(self.turn_from);
    }

    /// Settles on the language of the voiced audio so far, if it is confident.
    fn judge(&mut self) -> Option<Choice> {
        let seconds = self.voiced.len() as f32 / RATE as f32;
        if self.voiced.len() == self.judged || seconds < MIN_SECONDS {
            return None;
        }
        self.judged = self.voiced.len();
        let started = std::time::Instant::now();
        let scores = match self.engines.identify(&self.voiced)? {
            Ok(scores) => scores,
            Err(e) => {
                eprintln!("voice: identifying the caller's language failed: {e:#}");
                return None;
            }
        };
        let took = started.elapsed();
        let language = confident(&scores, seconds, &self.speakable);
        let top: Vec<String> = scores.iter().take(3).map(|(l, p)| format!("{l} {p:.2}")).collect();
        eprintln!("voice: over {seconds:.1} s voiced the caller sounds like {top:?} (judged in {took:.0?}); {language:?}");
        Some(self.settle(Choice { language: language?, identified: true }))
    }

    fn give_up(&mut self) -> Choice {
        eprintln!("voice: the caller's language stays unclear; the call speaks {}", self.fallback);
        self.settle(Choice { language: self.fallback.clone(), identified: false })
    }

    fn settle(&mut self, choice: Choice) -> Choice {
        let at = self.listening.iter().position(|(l, _)| *l == choice.language).expect("the choice is speakable");
        self.chosen = Some(self.listening.swap_remove(at).1);
        self.listening.clear();
        self.voiced = Vec::new();
        choice
    }
}

/// Owns the call's recognizers; each step runs on the blocking pool, in order. Reports the language
/// once settled, then partial changes, and an empty partial once a finish or reset clears the stream.
async fn stt_worker(
    mut ears: Ears,
    mut cmds: mpsc::UnboundedReceiver<SttCmd>,
    events: mpsc::UnboundedSender<Event>,
) {
    let mut last = String::new();
    while let Some(cmd) = cmds.recv().await {
        let step = tokio::task::spawn_blocking(move || {
            let out = match cmd {
                SttCmd::Accept { samples, voiced } => {
                    let settled = ears.accept(&samples, voiced);
                    SttOut::Partial(settled, ears.partial())
                }
                SttCmd::Finish { turn } => {
                    let (settled, text) = ears.finish();
                    SttOut::Finished(settled, Some((turn, text)))
                }
                SttCmd::Pause => {
                    let settled = ears.pause();
                    SttOut::Partial(settled, ears.partial())
                }
                SttCmd::Reset => {
                    ears.reset();
                    SttOut::Finished(None, None)
                }
            };
            (ears, out)
        });
        let (back, out) = match step.await {
            Ok(done) => done,
            Err(e) => {
                eprintln!("voice: an STT step failed: {e}");
                return;
            }
        };
        ears = back;
        match out {
            SttOut::Partial(settled, partial) => {
                if let Some(choice) = settled {
                    let _ = events.send(Event::Language(choice));
                }
                if partial != last {
                    last.clone_from(&partial);
                    let _ = events.send(Event::Partial(partial));
                }
            }
            SttOut::Finished(settled, finished) => {
                if let Some(choice) = settled {
                    let _ = events.send(Event::Language(choice));
                }
                if let Some((turn, text)) = finished {
                    let _ = events.send(Event::Finished { turn, text });
                }
                if !last.is_empty() {
                    last.clear();
                    let _ = events.send(Event::Partial(String::new()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::engines::BaseVoice;
    use crate::audio::lines::text;
    use crate::audio::playout::FRAME;
    use crate::audio::tts::{ChunkedBackend, Renderer};
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

    #[test]
    fn an_ending_waits_out_the_audio_and_its_tail() {
        let mut ending = Ending::new(SessionEnd::HungUp);
        let t0 = ending.stalled_by - DRAIN_CAP;
        assert_eq!(ending.due(false, t0 + Duration::from_secs(4)), None);
        ending.stalled_by = t0 + Duration::from_secs(12);
        assert_eq!(ending.due(false, t0 + Duration::from_secs(10)), None, "audio still playing past the stall cap");
        assert_eq!(ending.due(true, t0 + Duration::from_secs(11)), None, "the tail is still in flight");
        assert_eq!(ending.due(true, t0 + Duration::from_secs(11) + DRAIN_TAIL), Some(SessionEnd::HungUp));
    }

    #[test]
    fn an_ending_gives_up_on_audio_that_stops_moving() {
        let mut ending = Ending::new(SessionEnd::HungUp);
        let t0 = ending.stalled_by - DRAIN_CAP;
        assert_eq!(ending.due(false, t0 + DRAIN_CAP), Some(SessionEnd::HungUp));
        let mut ending = Ending::new(SessionEnd::HungUp);
        let t0 = ending.limit - DRAIN_LIMIT;
        ending.stalled_by = t0 + DRAIN_LIMIT * 2;
        assert_eq!(ending.due(false, t0 + DRAIN_LIMIT), Some(SessionEnd::HungUp));
    }

    struct FakeVad;

    impl Vad for FakeVad {
        fn push(&mut self, window: &[f32]) -> bool {
            window[0] != 0.0
        }

        fn reset(&mut self) {}
    }

    /// The partial is the last script entry whose count of speech chunks has been reached.
    struct FakeStt {
        script: Vec<(usize, &'static str)>,
        heard: usize,
        panics: bool,
        blank_finish: bool,
    }

    impl SpeechToText for FakeStt {
        fn accept(&mut self, samples_16k: &[f32]) {
            assert!(!self.panics, "the recognizer crashed");
            if samples_16k.iter().any(|&s| s != 0.0) {
                self.heard += 1;
            }
        }

        fn partial(&mut self) -> String {
            self.script.iter().rev().find(|(n, _)| self.heard >= *n).map_or("", |(_, t)| t).to_string()
        }

        fn finish(&mut self) -> String {
            let text = if self.blank_finish { String::new() } else { self.partial() };
            self.heard = 0;
            text
        }
    }

    struct FakeTurn(f32);

    impl TurnDetector for FakeTurn {
        fn complete(&self, _samples_16k: &[f32]) -> f32 {
            self.0
        }
    }

    /// `text.len()` frames, each sample equal to `text.len()`.
    #[derive(Default)]
    struct FakeTts {
        said: Mutex<Vec<String>>,
    }

    impl Renderer for FakeTts {
        fn render(&self, text: &str, _voice: &str) -> anyhow::Result<Vec<i16>> {
            self.said.lock().unwrap().push(text.into());
            Ok(vec![text.len() as i16; text.len() * FRAME])
        }
    }

    struct FakeEngines {
        stt: Vec<(usize, &'static str)>,
        tts: Arc<FakeTts>,
        stt_panics: bool,
        blank_finish: bool,
        /// The first language loaded, and the sidecar it speaks through if not Kokoro.
        language: &'static str,
        sidecar_base: Option<&'static str>,
        second: Option<Second>,
    }

    /// A second language, which makes the call identify its caller's language.
    struct Second {
        language: &'static str,
        stt: Vec<(usize, &'static str)>,
        sidecar: &'static str,
        /// What identification says of each clip in turn, the last for every clip after.
        heard: Vec<Vec<(String, f32)>>,
        /// The length of every clip identified.
        clips: Arc<Mutex<Vec<usize>>>,
    }

    impl SpeechEngines for FakeEngines {
        fn languages(&self) -> Vec<String> {
            std::iter::once(self.language).chain(self.second.as_ref().map(|s| s.language)).map(String::from).collect()
        }

        fn vad(&self, _language: &str) -> anyhow::Result<Box<dyn Vad>> {
            Ok(Box::new(FakeVad))
        }

        fn stt(&self, language: &str) -> anyhow::Result<Box<dyn SpeechToText>> {
            let script = match &self.second {
                Some(second) if second.language == language => second.stt.clone(),
                _ => self.stt.clone(),
            };
            Ok(Box::new(FakeStt { script, heard: 0, panics: self.stt_panics, blank_finish: self.blank_finish }))
        }

        fn turn(&self, _language: &str) -> Arc<dyn TurnDetector> {
            Arc::new(FakeTurn(0.9))
        }

        fn tts(&self, language: &str) -> BaseVoice {
            match (&self.second, self.sidecar_base) {
                (Some(second), _) if second.language == language => BaseVoice::Sidecar(second.sidecar.into()),
                (_, Some(id)) => BaseVoice::Sidecar(id.into()),
                (_, None) => BaseVoice::Kokoro(Arc::new(ChunkedBackend::new("kokoro", "Kokoro", self.tts.clone(), Vec::new()))),
            }
        }

        fn identifies(&self) -> bool {
            self.second.is_some()
        }

        fn identify(&self, samples_16k: &[f32]) -> Option<anyhow::Result<crate::audio::language::Scores>> {
            let second = self.second.as_ref()?;
            let mut clips = second.clips.lock().unwrap();
            let heard = &second.heard[clips.len().min(second.heard.len() - 1)];
            clips.push(samples_16k.len());
            Some(Ok(heard.clone()))
        }
    }

    #[derive(Clone, Default)]
    struct Probe {
        /// The first sample of every frame sent.
        sent: Arc<Mutex<Vec<i16>>>,
        sent_at: Arc<Mutex<Vec<std::time::Instant>>>,
        gone: Arc<Notify>,
        left_room: Arc<AtomicBool>,
        shown: Arc<Mutex<Vec<LiveState>>>,
    }

    impl Probe {
        fn frames_of(&self, marker: usize) -> usize {
            self.sent.lock().unwrap().iter().filter(|&&s| s == marker as i16).count()
        }
    }

    /// Plays its script one frame per 10 ms, then goes quiet forever.
    struct FakeMedia {
        script: Mutex<VecDeque<Vec<f32>>>,
        probe: Probe,
    }

    #[async_trait::async_trait]
    impl MediaIo for FakeMedia {
        async fn recv(&self) -> Option<Vec<f32>> {
            let next = self.script.lock().unwrap().pop_front();
            match next {
                Some(frame) => {
                    tokio::time::sleep(TICK).await;
                    Some(frame)
                }
                None => std::future::pending().await,
            }
        }

        async fn send(&self, frame: &[i16; FRAME]) -> anyhow::Result<()> {
            self.probe.sent.lock().unwrap().push(frame[0]);
            self.probe.sent_at.lock().unwrap().push(std::time::Instant::now());
            Ok(())
        }

        fn clear(&self) {}

        async fn left(&self) -> Gone {
            self.probe.gone.notified().await;
            Gone::Left
        }

        async fn leave(&self) {
            self.probe.left_room.store(true, Ordering::SeqCst);
        }

        fn show(&self, state: LiveState) {
            self.probe.shown.lock().unwrap().push(state);
        }
    }

    fn audio(parts: &[(f32, u64)]) -> Vec<Vec<f32>> {
        parts.iter().flat_map(|&(level, ms)| (0..ms / 10).map(move |_| vec![level; 160])).collect()
    }

    const READY: i16 = 1000;
    const HEARD: i16 = 2000;

    struct Call {
        inbox: mpsc::UnboundedSender<SessionIn>,
        log: Arc<Mutex<Vec<CallBody>>>,
        probe: Probe,
        end: JoinHandle<SessionEnd>,
        tts: Arc<FakeTts>,
    }

    impl Call {
        fn log(&self) -> Vec<CallBody> {
            self.log.lock().unwrap().clone()
        }

        fn frame(&self, body: CallBody) {
            self.inbox.send(SessionIn::Frame(body)).unwrap();
        }

        /// Waits, without advancing the paused clock, for the queue's TTS thread to render `text`.
        async fn synthesized(&self, text: &str) {
            for _ in 0..1000 {
                if self.tts.said.lock().unwrap().iter().any(|t| t == text) {
                    return;
                }
                tokio::task::yield_now().await;
                std::thread::sleep(Duration::from_millis(1));
            }
            panic!("{text:?} was never synthesized");
        }
    }

    fn engines(stt: Vec<(usize, &'static str)>, tts: &Arc<FakeTts>) -> FakeEngines {
        FakeEngines { stt, tts: tts.clone(), stt_panics: false, blank_finish: false, language: "en", sidecar_base: None, second: None }
    }

    fn deps(stt: Vec<(usize, &'static str)>, cue: bool, tts: &Arc<FakeTts>) -> SessionDeps {
        SessionDeps {
            engines: Arc::new(engines(stt, tts)),
            lines: Arc::new(Lines::default()),
            cues: Arc::new(Cues { ready: Arc::new(vec![READY; FRAME]), heard: Arc::new(vec![HEARD; FRAME]) }),
            profile: VoiceProfile { language: "en".into(), voice: String::new(), cue },
            sidecars: Vec::new(),
            direction: Direction::Outbound,
            max_len: Duration::from_mins(30),
            link_grace: Duration::from_secs(10),
        }
    }

    fn call(script: Vec<Vec<f32>>, stt: Vec<(usize, &'static str)>, cue: bool) -> Call {
        let tts = Arc::default();
        call_with(deps(stt, cue, &tts), script, tts)
    }

    fn call_with(deps: SessionDeps, script: Vec<Vec<f32>>, tts: Arc<FakeTts>) -> Call {
        let probe = Probe::default();
        let media = FakeMedia { script: Mutex::new(script.into()), probe: probe.clone() };
        let (inbox, rx) = mpsc::unbounded_channel();
        let log: Arc<Mutex<Vec<CallBody>>> = Arc::default();
        let sink = log.clone();
        let end = tokio::spawn(run_session(deps, Box::new(media), rx, move |b| sink.lock().unwrap().push(b)));
        Call { inbox, log, probe, end, tts }
    }

    async fn sleep_ms(ms: u64) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_spoken_turn_reaches_note_as_draft_then_commit() {
        let c = call(
            audio(&[(0.1, 1000), (0.0, 1000)]),
            vec![(2, "move"), (4, "move my"), (6, "move my run")],
            true,
        );
        sleep_ms(3000).await;
        let log = c.log();
        assert_eq!(log.len(), 4, "{log:?}");
        assert_eq!(log[0], CallBody::Floor { floor: Floor::UserSpeaking });
        assert_eq!(log[1], CallBody::Floor { floor: Floor::UserQuiet });
        assert!(matches!(&log[2], CallBody::Draft { turn: 1, .. }), "{log:?}");
        assert_eq!(log[3], CallBody::Commit { turn: 1, text: "move my run".into(), language: Some("en".into()) });
        assert_eq!(c.probe.frames_of(READY as usize), 1, "the ready cue plays once");
        assert_eq!(c.probe.frames_of(HEARD as usize), 1, "the heard cue follows the commit");
    }

    async fn until_committed(c: &Call) {
        for _ in 0..1000 {
            if c.log().iter().any(|b| matches!(b, CallBody::Commit { .. })) {
                return;
            }
            sleep_ms(10).await;
        }
        panic!("never committed: {:?}", c.log());
    }

    #[tokio::test(start_paused = true)]
    async fn no_speech_within_a_beat_plays_one_moment() {
        let moment = text(Line::OneMoment, "en").len();
        let turn = || audio(&[(0.1, 1000), (0.0, 1000)]);
        let words = || vec![(2, "move"), (6, "move my run")];

        let c = call(turn(), words(), false);
        until_committed(&c).await;
        sleep_ms(1400).await;
        assert_eq!(c.probe.frames_of(moment), 0, "not before 1.5 s");
        sleep_ms(4000).await;
        assert_eq!(c.probe.frames_of(moment), moment, "the line plays once");

        let c = call(turn(), words(), false);
        until_committed(&c).await;
        c.frame(CallBody::Speak { reply: 2, idx: 0, text: "ok".into() });
        c.frame(CallBody::Play { reply: 2 });
        sleep_ms(4000).await;
        assert_eq!(c.probe.frames_of(moment), 0, "a reply on its way needs no filler");

        let c = call(turn(), words(), false);
        until_committed(&c).await;
        c.frame(CallBody::Speak { reply: 2, idx: 0, text: "ok".into() });
        sleep_ms(4000).await;
        assert_eq!(c.probe.frames_of(moment), moment, "a held draft's words are not on their way");

        let c = call(turn(), words(), false);
        until_committed(&c).await;
        c.frame(CallBody::Speak { reply: 2, idx: 0, text: "ok".into() });
        c.frame(CallBody::Drop { reply: 2 });
        c.frame(CallBody::Play { reply: 2 });
        sleep_ms(4000).await;
        assert_eq!(c.probe.frames_of(moment), moment, "a dropped reply says nothing");

        let c = call(turn(), words(), false);
        until_committed(&c).await;
        c.frame(CallBody::Play { reply: 2 });
        c.frame(CallBody::SpeakDone { reply: 2 });
        sleep_ms(1400).await;
        assert_eq!(c.probe.frames_of(moment), 0, "not before 1.5 s");
        sleep_ms(4000).await;
        assert_eq!(c.probe.frames_of(moment), moment, "a tool-only reply still gets the filler");
    }

    fn inbound(script: Vec<Vec<f32>>) -> Call {
        let tts: Arc<FakeTts> = Arc::default();
        call_with(SessionDeps { direction: Direction::Inbound, ..deps(Vec::new(), false, &tts) }, script, tts)
    }

    #[tokio::test(start_paused = true)]
    async fn an_inbound_call_is_silent_until_the_caller_speaks() {
        let hello = text(Line::Hello, "en").len();
        let c = inbound(Vec::new());
        sleep_ms(7900).await;
        assert!(c.probe.sent.lock().unwrap().is_empty(), "no word and no filler before the caller speaks");
        assert!(c.log().is_empty());
        sleep_ms(3000).await;
        assert_eq!(c.probe.frames_of(hello), hello, "a caller silent for 8 s is asked once");
        assert_eq!(c.probe.sent.lock().unwrap().len(), hello);

        let c = inbound(audio(&[(0.0, 2000), (0.1, 500), (0.0, 1000)]));
        sleep_ms(30_000).await;
        assert_eq!(c.probe.frames_of(hello), 0, "a caller who spoke is not asked");
        assert!(!c.end.is_finished());

        let c = inbound(Vec::new());
        sleep_ms(500).await;
        c.frame(CallBody::Speak { reply: 1, idx: 0, text: "hello there".into() });
        sleep_ms(10_000).await;
        assert_eq!(c.probe.frames_of(hello), 0, "Note speaking first is not met with a prompt");

        let c = call(Vec::new(), Vec::new(), false);
        sleep_ms(30_000).await;
        assert!(c.probe.sent.lock().unwrap().is_empty(), "an outbound call waits on Note's words");
        assert!(!c.end.is_finished());
    }

    #[tokio::test(start_paused = true)]
    async fn an_inbound_caller_who_never_speaks_is_let_go_after_the_line() {
        let gone = text(Line::NoOneThere, "en").len();
        let mut c = inbound(Vec::new());
        sleep_ms(27_000).await;
        assert!(!c.end.is_finished());
        let end = tokio::time::timeout(Duration::from_secs(20), &mut c.end).await.unwrap().unwrap();
        assert_eq!(end, SessionEnd::NoOneThere);
        assert_eq!(end.failure(), None);
        assert_eq!(c.probe.frames_of(gone), gone, "the line plays out whole before the end");
        assert!(c.probe.left_room.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn the_line_a_call_needs_first_renders_first() {
        let tts: Arc<FakeTts> = Arc::default();
        let c = call_with(SessionDeps { direction: Direction::Inbound, ..deps(Vec::new(), false, &tts) }, Vec::new(), tts);
        c.synthesized(text(Line::Goodbye, "en")).await;
        assert_eq!(c.tts.said.lock().unwrap()[0], text(Line::OneMoment, "en"));

        let tts: Arc<FakeTts> = Arc::default();
        let c = call_with(deps(Vec::new(), false, &tts), Vec::new(), tts);
        c.synthesized(text(Line::Goodbye, "en")).await;
        assert_eq!(c.tts.said.lock().unwrap()[0], text(Line::OneMoment, "en"));
    }

    /// Renders one frame valued `SIDE` per text, once opened.
    #[derive(Default)]
    struct Gated {
        open: Mutex<bool>,
        opened: std::sync::Condvar,
        said: Mutex<Vec<String>>,
    }

    const SIDE: i16 = 7000;

    impl Renderer for Gated {
        fn render(&self, text: &str, _voice: &str) -> anyhow::Result<Vec<i16>> {
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.opened.wait(open).unwrap();
            }
            self.said.lock().unwrap().push(text.into());
            Ok(vec![SIDE; FRAME])
        }
    }

    /// In real time: a render blocked on the gate would hold a paused clock still.
    #[tokio::test]
    async fn a_sidecar_voices_lines_start_in_kokoro_and_switch_once_rendered() {
        let moment = text(Line::OneMoment, "en").len();
        let goodbye = text(Line::Goodbye, "en").len();
        let gate = Arc::new(Gated::default());
        let voices = vec![crate::audio::engines::VoiceInfo { id: "v".into(), label: "V".into(), languages: Vec::new(), credit: None }];
        let side: Arc<dyn SpeechBackend> = Arc::new(ChunkedBackend::new("side", "Side", gate.clone(), voices));
        let tts: Arc<FakeTts> = Arc::default();
        let mut d = deps(vec![(2, "move my run")], false, &tts);
        d.sidecars = vec![side];
        d.profile.voice = "side:v".into();
        d.max_len = Duration::from_secs(6);
        let c = call_with(d, audio(&[(0.1, 600), (0.0, 1000)]), tts);
        until_committed(&c).await;
        sleep_ms(2500).await;
        let early = c.probe.frames_of(moment);
        *gate.open.lock().unwrap() = true;
        gate.opened.notify_all();
        assert_eq!(early, moment, "Kokoro's filler plays while the sidecar is busy");
        let end = tokio::time::timeout(Duration::from_secs(10), c.end).await.unwrap().unwrap();
        assert_eq!(end, SessionEnd::TimedOut);
        assert!(c.probe.frames_of(SIDE as usize) > 0, "the goodbye is in the sidecar's voice");
        assert_eq!(c.probe.frames_of(goodbye), 0);
    }

    /// Speaks each text as `text.len()` frames of `JA`, until told to fail.
    #[derive(Default)]
    struct Japanese {
        fail: AtomicBool,
        said: Mutex<Vec<String>>,
    }

    const JA: i16 = 9000;

    impl Renderer for Japanese {
        fn render(&self, text: &str, _voice: &str) -> anyhow::Result<Vec<i16>> {
            if self.fail.load(Ordering::SeqCst) {
                anyhow::bail!("the sidecar is gone");
            }
            self.said.lock().unwrap().push(text.into());
            Ok(vec![JA; text.len() * FRAME])
        }
    }

    /// Streams of a sidecar: one that errors is over, like a closed socket.
    struct Sidecar(ChunkedBackend);

    struct SidecarStream(Box<dyn crate::audio::tts::SpeechStream>, bool);

    impl SpeechBackend for Sidecar {
        fn id(&self) -> &str {
            self.0.id()
        }

        fn label(&self) -> &str {
            self.0.label()
        }

        fn input(&self) -> crate::audio::tts::TextInput {
            self.0.input()
        }

        fn voices(&self) -> Vec<crate::audio::engines::VoiceInfo> {
            self.0.voices()
        }

        fn open(&self, voice: &str) -> anyhow::Result<Box<dyn crate::audio::tts::SpeechStream>> {
            Ok(Box::new(SidecarStream(self.0.open(voice)?, true)))
        }
    }

    impl crate::audio::tts::SpeechStream for SidecarStream {
        fn push(&mut self, text: &str) -> anyhow::Result<()> {
            self.0.push(text)
        }

        fn finish(&mut self) -> anyhow::Result<()> {
            self.0.finish()
        }

        fn next(&mut self, ahead: Duration) -> anyhow::Result<crate::audio::tts::Next> {
            let next = self.0.next(ahead);
            self.1 &= next.is_ok();
            next
        }

        fn cancel(&mut self) {
            self.0.cancel();
        }

        fn alive(&self) -> bool {
            self.1
        }
    }

    fn japanese_sidecar(japanese: &Arc<Japanese>) -> Arc<dyn SpeechBackend> {
        Arc::new(Sidecar(ChunkedBackend::new("ja", "Japanese", japanese.clone(), Vec::new())))
    }

    fn japanese_deps(tts: &Arc<FakeTts>, lines: Arc<Lines>, sidecars: Vec<Arc<dyn SpeechBackend>>) -> SessionDeps {
        let engines = Arc::new(FakeEngines { language: "ja", sidecar_base: Some("ja"), ..engines(Vec::new(), tts) });
        let profile = VoiceProfile { language: "ja".into(), voice: String::new(), cue: false };
        SessionDeps { engines, lines, sidecars, profile, direction: Direction::Inbound, ..deps(Vec::new(), false, tts) }
    }

    #[tokio::test(start_paused = true)]
    async fn a_japanese_call_speaks_through_its_sidecar_and_keeps_its_lines() {
        let japanese = Arc::new(Japanese::default());
        let ja = japanese_sidecar(&japanese);
        let dir = tempfile::tempdir().unwrap();
        let lines = Arc::new(Lines::new(Some(dir.path().to_path_buf())));
        let tts: Arc<FakeTts> = Arc::default();
        let c = call_with(japanese_deps(&tts, lines.clone(), vec![ja]), Vec::new(), tts);
        c.frame(CallBody::Speak { reply: 1, idx: 0, text: "はい。".into() });
        c.frame(CallBody::SpeakDone { reply: 1 });
        c.frame(CallBody::Play { reply: 1 });
        sleep_ms(3000).await;
        assert_eq!(c.probe.frames_of(JA as usize), "はい。".len(), "the reply is in the sidecar's voice");
        assert!(c.tts.said.lock().unwrap().is_empty(), "Kokoro says nothing on a Japanese call");
        let said = japanese.said.lock().unwrap().concat();
        for line in [Line::Hello, Line::OneMoment, Line::LostNotes, Line::Goodbye, Line::NoVoice] {
            assert!(said.contains(text(line, "ja")), "{line:?} in {said:?}");
            assert!(lines.kept("ja", "", "ja", line).is_some(), "{line:?} is kept");
        }
    }

    /// An English user's call with Japanese loaded beside English, speaking through `japanese`; the
    /// identifier answers each clip with the next of `heard`, the last one repeating.
    fn bilingual(heard: &[&[(&str, f32)]], japanese: &Arc<Japanese>, direction: Direction) -> (SessionDeps, Arc<FakeTts>, Arc<Mutex<Vec<usize>>>) {
        let tts: Arc<FakeTts> = Arc::default();
        let clips: Arc<Mutex<Vec<usize>>> = Arc::default();
        let second = Second {
            language: "ja",
            stt: vec![(2, "明日"), (5, "明日の朝走る")],
            sidecar: "ja",
            heard: heard.iter().map(|h| h.iter().map(|(l, p)| ((*l).to_owned(), *p)).collect()).collect(),
            clips: clips.clone(),
        };
        let engines = Arc::new(FakeEngines { second: Some(second), ..engines(vec![(2, "move"), (5, "move my run")], &tts) });
        let d = SessionDeps { engines, sidecars: vec![japanese_sidecar(japanese)], direction, ..deps(Vec::new(), false, &tts) };
        (d, tts, clips)
    }

    fn committed(c: &Call) -> Vec<CallBody> {
        c.log().into_iter().filter(|b| matches!(b, CallBody::Commit { .. } | CallBody::Draft { .. })).collect()
    }

    fn commits(c: &Call) -> Vec<(String, Option<String>)> {
        c.log()
            .into_iter()
            .filter_map(|b| match b {
                CallBody::Commit { text, language, .. } => Some((text, language)),
                _ => None,
            })
            .collect()
    }

    async fn until_commits(c: &Call, n: usize) {
        for _ in 0..3000 {
            if commits(c).len() >= n {
                return;
            }
            sleep_ms(10).await;
        }
        panic!("{n} commits never came: {:?}", c.log());
    }

    fn turns(parts: &[u64]) -> Vec<Vec<f32>> {
        audio(&parts.iter().flat_map(|&ms| [(0.1, ms), (0.0, 1500)]).collect::<Vec<_>>())
    }

    #[tokio::test(start_paused = true)]
    async fn a_caller_heard_speaking_japanese_is_transcribed_and_answered_in_japanese() {
        let japanese = Arc::new(Japanese::default());
        let (d, tts, clips) = bilingual(&[&[("ja", 0.9), ("en", 0.05)]], &japanese, Direction::Inbound);
        let c = call_with(d, audio(&[(0.0, 300), (0.1, 1000), (0.0, 1000)]), tts);
        until_committed(&c).await;
        let turns = committed(&c);
        assert_eq!(
            turns.last(),
            Some(&CallBody::Commit { turn: 1, text: "明日の朝走る".into(), language: Some("ja".into()) }),
            "the buffered first turn is transcribed by the Japanese recognizer: {turns:?}"
        );
        let clips = clips.lock().unwrap().clone();
        assert_eq!(clips.len(), 1, "identified once");
        assert!((ID_MIN..=ID_SPAN).contains(&clips[0]), "only the voiced audio: {clips:?}");

        c.frame(CallBody::Speak { reply: 1, idx: 0, text: "はい。".into() });
        c.frame(CallBody::SpeakDone { reply: 1 });
        c.frame(CallBody::Play { reply: 1 });
        sleep_ms(3000).await;
        assert_eq!(c.probe.frames_of(JA as usize), "はい。".len(), "the reply is in the Japanese voice");
        assert!(c.tts.said.lock().unwrap().iter().all(|t| t.is_ascii()), "Kokoro speaks no Japanese");
        let said = japanese.said.lock().unwrap().concat();
        assert!(said.contains(text(Line::OneMoment, "ja")), "the lines follow the language: {said}");
    }

    #[tokio::test(start_paused = true)]
    async fn japanese_that_whisper_leans_to_chinese_is_answered_in_japanese() {
        let japanese = Arc::new(Japanese::default());
        let (d, tts, _) = bilingual(&[&[("zh", 0.85), ("ja", 0.09), ("en", 0.014)]], &japanese, Direction::Inbound);
        let c = call_with(d, turns(&[1500]), tts);
        until_commits(&c, 1).await;
        assert_eq!(commits(&c), vec![("明日の朝走る".into(), Some("ja".into()))]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_long_first_turn_is_identified_from_its_first_seconds() {
        let japanese = Arc::new(Japanese::default());
        let (d, tts, clips) = bilingual(&[&[("ja", 0.9)]], &japanese, Direction::Inbound);
        let c = call_with(d, audio(&[(0.1, 4000), (0.0, 1000)]), tts);
        sleep_ms(3500).await;
        let clips = clips.lock().unwrap().clone();
        assert!(matches!(clips[..], [n] if (ID_SPAN..ID_SPAN + STT_CHUNK).contains(&n)), "settled while the caller still speaks: {clips:?}");
        until_committed(&c).await;
        assert!(matches!(committed(&c).last(), Some(CallBody::Commit { language: Some(l), .. }) if l == "ja"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_short_first_word_is_heard_in_the_setting_and_the_next_turn_settles_the_language() {
        let japanese = Arc::new(Japanese::default());
        let (d, tts, clips) = bilingual(&[&[("ja", 0.99)]], &japanese, Direction::Inbound);
        let c = call_with(d, turns(&[500, 1500]), tts);
        until_commits(&c, 2).await;
        assert_eq!(
            commits(&c),
            vec![("move".into(), Some("en".into())), ("明日の朝走る".into(), Some("ja".into()))],
            "「うん」 alone is not judged; the second turn is, over both turns' voice"
        );
        let clips = clips.lock().unwrap().clone();
        assert!(matches!(clips[..], [n] if n > RATE * 3 / 2), "{clips:?}");
        c.frame(CallBody::Speak { reply: 1, idx: 0, text: "うん。".into() });
        c.frame(CallBody::SpeakDone { reply: 1 });
        c.frame(CallBody::Play { reply: 1 });
        sleep_ms(2000).await;
        assert_eq!(c.probe.frames_of(JA as usize), "うん。".len(), "the voice switched with the language");
    }

    #[tokio::test(start_paused = true)]
    async fn an_unsure_or_unspoken_language_is_heard_in_the_setting_and_stays_open() {
        for heard in [&[("ja", 0.4), ("en", 0.3)][..], &[("ko", 0.97), ("ja", 0.02)]] {
            let japanese = Arc::new(Japanese::default());
            let (d, tts, clips) = bilingual(&[heard], &japanese, Direction::Outbound);
            let c = call_with(d, audio(&[(0.1, 1000), (0.0, 1000)]), tts);
            until_committed(&c).await;
            assert_eq!(clips.lock().unwrap().len(), 1);
            assert_eq!(commits(&c), vec![("move my run".into(), Some("en".into()))], "{heard:?}");
            c.frame(CallBody::Speak { reply: 1, idx: 0, text: "ok".into() });
            c.frame(CallBody::SpeakDone { reply: 1 });
            c.frame(CallBody::Play { reply: 1 });
            c.synthesized("ok").await;
            assert!(japanese.said.lock().unwrap().iter().all(|t| !t.contains("ok")));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn three_unsure_turns_settle_on_the_setting() {
        let japanese = Arc::new(Japanese::default());
        let (d, tts, clips) = bilingual(&[&[("ja", 0.6), ("en", 0.4)]], &japanese, Direction::Inbound);
        let c = call_with(d, turns(&[1000, 1000, 1000, 1500]), tts);
        until_commits(&c, 3).await;
        let judged = clips.lock().unwrap().len();
        until_commits(&c, 4).await;
        assert_eq!(clips.lock().unwrap().len(), judged, "nothing is judged once the language is settled");
        assert!(commits(&c).iter().all(|(_, l)| l.as_deref() == Some("en")), "{:?}", commits(&c));
    }

    #[tokio::test(start_paused = true)]
    async fn eight_seconds_of_unsure_voice_settle_on_the_setting() {
        let japanese = Arc::new(Japanese::default());
        let (d, tts, clips) = bilingual(&[&[("ja", 0.6), ("en", 0.4)]], &japanese, Direction::Inbound);
        let c = call_with(d, turns(&[9500, 1500]), tts);
        until_commits(&c, 2).await;
        let clips = clips.lock().unwrap().clone();
        assert_eq!(clips.last(), Some(&ID_CAP), "{clips:?}");
        assert!(commits(&c).iter().all(|(_, l)| l.as_deref() == Some("en")), "{:?}", commits(&c));
    }

    #[tokio::test(start_paused = true)]
    async fn a_blip_before_the_first_words_leaves_the_language_open() {
        let japanese = Arc::new(Japanese::default());
        let (d, tts, clips) = bilingual(&[&[("ja", 0.9)]], &japanese, Direction::Inbound);
        let c = call_with(d, audio(&[(0.0, 200), (0.1, 100), (0.0, 2000), (0.1, 1000), (0.0, 1500)]), tts);
        until_committed(&c).await;
        assert_eq!(clips.lock().unwrap().len(), 1, "the blip is not identified");
        assert!(matches!(committed(&c).last(), Some(CallBody::Commit { language: Some(l), .. }) if l == "ja"));
    }

    #[tokio::test(start_paused = true)]
    async fn the_language_holds_once_settled() {
        let japanese = Arc::new(Japanese::default());
        let (d, tts, clips) = bilingual(&[&[("ja", 0.9)], &[("en", 0.99)]], &japanese, Direction::Inbound);
        let c = call_with(d, turns(&[1000, 1000]), tts);
        until_commits(&c, 2).await;
        assert!(commits(&c).iter().all(|(_, l)| l.as_deref() == Some("ja")), "{:?}", commits(&c));
        assert_eq!(clips.lock().unwrap().len(), 1, "identified only from the first turn");
    }

    #[tokio::test(start_paused = true)]
    async fn a_japanese_call_without_its_sidecar_says_the_kept_line_and_ends() {
        let japanese = Arc::new(Japanese::default());
        let ja = japanese_sidecar(&japanese);
        let dir = tempfile::tempdir().unwrap();
        let lines = Arc::new(Lines::new(Some(dir.path().to_path_buf())));
        lines.get(&Speaker::new(ja.clone(), ""), &*ja, "ja", Line::NoVoice).unwrap();
        let rendered = japanese.said.lock().unwrap().len();
        let tts: Arc<FakeTts> = Arc::default();
        let c = call_with(japanese_deps(&tts, lines.clone(), Vec::new()), Vec::new(), tts);
        c.frame(CallBody::Speak { reply: 1, idx: 0, text: "はい。".into() });
        c.frame(CallBody::SpeakDone { reply: 1 });
        c.frame(CallBody::Play { reply: 1 });
        let end = tokio::time::timeout(Duration::from_secs(10), c.end).await.unwrap().unwrap();
        assert_eq!(end, SessionEnd::VoiceDown);
        assert_eq!(c.probe.frames_of(JA as usize), text(Line::NoVoice, "ja").len(), "the kept line, nothing else");
        assert_eq!(japanese.said.lock().unwrap().len(), rendered, "nothing was rendered during the call");
        assert!(c.tts.said.lock().unwrap().is_empty());
        assert_eq!(end.failure(), Some("the call's voice is down"));

        let tts: Arc<FakeTts> = Arc::default();
        let bare = call_with(japanese_deps(&tts, Arc::new(Lines::default()), Vec::new()), Vec::new(), tts);
        let end = tokio::time::timeout(Duration::from_secs(10), bare.end).await.unwrap().unwrap();
        assert_eq!(end, SessionEnd::VoiceDown, "with nothing kept the call still ends");
        assert!(bare.probe.sent.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn losing_the_sidecar_mid_call_ends_the_call_after_the_kept_line() {
        let japanese = Arc::new(Japanese::default());
        let ja = japanese_sidecar(&japanese);
        let lines = Arc::new(Lines::default());
        let tts: Arc<FakeTts> = Arc::default();
        let c = call_with(japanese_deps(&tts, lines.clone(), vec![ja]), Vec::new(), tts);
        for _ in 0..1000 {
            if lines.kept("ja", "", "ja", Line::NoVoice).is_some() {
                break;
            }
            tokio::task::yield_now().await;
            std::thread::sleep(Duration::from_millis(1));
        }
        japanese.fail.store(true, Ordering::SeqCst);
        c.frame(CallBody::Speak { reply: 1, idx: 0, text: "明日です。".into() });
        c.frame(CallBody::SpeakDone { reply: 1 });
        c.frame(CallBody::Play { reply: 1 });
        let mut c = c;
        let end = tokio::time::timeout(Duration::from_secs(10), &mut c.end).await.unwrap().unwrap();
        assert_eq!(end, SessionEnd::VoiceLost);
        assert!(c.probe.frames_of(JA as usize) >= text(Line::NoVoice, "ja").len(), "the kept line plays");
        assert!(!c.log().contains(&CallBody::Played { reply: 1 }));
    }

    #[tokio::test(start_paused = true)]
    async fn an_empty_commit_retracts_its_draft() {
        let tts: Arc<FakeTts> = Arc::default();
        let engines = Arc::new(FakeEngines { blank_finish: true, ..engines(vec![(2, "move"), (6, "move my run")], &tts) });
        let c = call_with(
            SessionDeps { engines, ..deps(Vec::new(), false, &tts) },
            audio(&[(0.1, 1000), (0.0, 1000)]),
            tts,
        );
        sleep_ms(3000).await;
        let log = c.log();
        let Some(CallBody::Draft { turn, .. }) = log.iter().find(|b| matches!(b, CallBody::Draft { .. })) else {
            panic!("no draft: {log:?}")
        };
        assert!(!log.iter().any(|b| matches!(b, CallBody::Commit { .. })), "{log:?}");
        assert_eq!(log.iter().filter(|b| **b == CallBody::Retract { turn: *turn }).count(), 1, "{log:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn speak_play_reaches_the_media_and_reports_played() {
        let c = call(Vec::new(), Vec::new(), true);
        c.frame(CallBody::Speak { reply: 7, idx: 0, text: "hi".into() });
        c.frame(CallBody::SpeakDone { reply: 7 });
        c.frame(CallBody::Play { reply: 7 });
        c.synthesized("hi").await;
        sleep_ms(500).await;
        assert_eq!(c.probe.frames_of(2), 2);
        assert_eq!(c.log(), vec![CallBody::Played { reply: 7 }, CallBody::Floor { floor: Floor::Drained }]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_user_leaving_ends_the_call_once() {
        let mut c = call(Vec::new(), Vec::new(), false);
        let long = "x".repeat(300);
        c.frame(CallBody::Speak { reply: 1, idx: 0, text: long.clone() });
        c.frame(CallBody::SpeakDone { reply: 1 });
        c.frame(CallBody::Play { reply: 1 });
        c.synthesized(&long).await;
        sleep_ms(500).await;
        assert!(c.probe.frames_of(300) > 0);
        c.probe.gone.notify_one();
        let end = (&mut c.end).await.unwrap();
        assert_eq!(end, SessionEnd::UserLeft);
        assert!(c.probe.left_room.load(Ordering::SeqCst), "the bot leaves the room");
        let (frames, sent) = (c.probe.sent.lock().unwrap().len(), c.log().len());
        sleep_ms(5000).await;
        assert_eq!(c.probe.sent.lock().unwrap().len(), frames, "playout stopped");
        assert_eq!(c.log().len(), sent);
        assert!(!c.log().contains(&CallBody::Played { reply: 1 }));
    }

    #[tokio::test(start_paused = true)]
    async fn a_link_drop_mid_call_is_bridged_or_ended() {
        let lost = text(Line::LostNotes, "en").len();
        let goodbye = text(Line::Goodbye, "en").len();

        let c = call(Vec::new(), Vec::new(), false);
        sleep_ms(500).await;
        c.inbox.send(SessionIn::LinkUp(false)).unwrap();
        sleep_ms(3000).await;
        c.inbox.send(SessionIn::LinkUp(true)).unwrap();
        sleep_ms(12_000).await;
        assert!(!c.end.is_finished(), "the call goes on");
        assert_eq!(c.probe.frames_of(lost), lost, "the lost-notes line plays once");
        assert_eq!(c.probe.frames_of(goodbye), 0);

        let c = call(Vec::new(), Vec::new(), false);
        sleep_ms(500).await;
        c.inbox.send(SessionIn::LinkUp(false)).unwrap();
        let end = tokio::time::timeout(Duration::from_secs(20), c.end).await.unwrap().unwrap();
        assert_eq!(end, SessionEnd::LinkLost);
        assert_eq!(c.probe.frames_of(lost), lost);
        assert_eq!(c.probe.frames_of(goodbye), goodbye, "the goodbye plays out before the end");
    }

    #[tokio::test(start_paused = true)]
    async fn barge_in_reports_heard_chars_and_drops_the_reply() {
        let c = call(audio(&[(0.0, 500), (0.1, 1000), (0.0, 1000)]), vec![(2, "wait actually")], false);
        c.frame(CallBody::Speak { reply: 3, idx: 0, text: "x".repeat(300) });
        c.frame(CallBody::SpeakDone { reply: 3 });
        c.frame(CallBody::Play { reply: 3 });
        c.synthesized(&"x".repeat(300)).await;
        sleep_ms(1200).await;
        let barge = c.log().into_iter().find_map(|b| match b {
            CallBody::BargeIn { reply, heard_chars } => Some((reply, heard_chars)),
            _ => None,
        });
        let Some((3, heard)) = barge else { panic!("no barge-in for reply 3: {:?}", c.log()) };
        let played = c.probe.frames_of(300);
        assert!(heard > 0 && (heard as usize) <= played, "heard {heard} of {played} frames");
        c.frame(CallBody::Speak { reply: 3, idx: 1, text: "more".into() });
        sleep_ms(3000).await;
        assert!(!c.tts.said.lock().unwrap().iter().any(|t| t == "more"), "a dropped reply is not synthesized");
        assert_eq!(c.probe.frames_of(300), played, "the cut reply stays silent");
        assert_eq!(c.probe.frames_of(4), 0, "a late clause of the cut reply is ignored");
        assert!(!c.log().contains(&CallBody::Played { reply: 3 }));
        assert!(c.log().contains(&CallBody::Commit { turn: 1, text: "wait actually".into(), language: Some("en".into()) }));
    }

    #[tokio::test(start_paused = true)]
    async fn note_hanging_up_lets_the_last_reply_finish() {
        let c = call(Vec::new(), Vec::new(), false);
        c.frame(CallBody::Speak { reply: 2, idx: 0, text: "bye".into() });
        c.frame(CallBody::SpeakDone { reply: 2 });
        c.frame(CallBody::Play { reply: 2 });
        c.synthesized("bye").await;
        c.frame(CallBody::HangUp);
        let end = tokio::time::timeout(Duration::from_secs(6), c.end).await.unwrap().unwrap();
        assert_eq!(end, SessionEnd::HungUp);
        assert_eq!(c.probe.frames_of(3), 3);
        assert!(c.probe.left_room.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn a_call_at_its_length_cap_says_goodbye_and_ends() {
        let goodbye = text(Line::Goodbye, "en").len();
        let tts = Arc::default();
        let c = call_with(SessionDeps { max_len: Duration::from_secs(2), ..deps(Vec::new(), false, &tts) }, Vec::new(), tts);
        let end = tokio::time::timeout(Duration::from_secs(5), c.end).await.unwrap().unwrap();
        assert_eq!(end, SessionEnd::TimedOut);
        assert_eq!(c.probe.frames_of(goodbye), goodbye);
    }

    #[tokio::test(start_paused = true)]
    async fn a_crashed_recognizer_says_so_and_ends_the_call() {
        let tts: Arc<FakeTts> = Arc::default();
        let engines = Arc::new(FakeEngines { stt_panics: true, ..engines(Vec::new(), &tts) });
        let c = call_with(SessionDeps { engines, ..deps(Vec::new(), false, &tts) }, audio(&[(0.1, 1000)]), tts);
        let end = tokio::time::timeout(Duration::from_secs(10), c.end).await.unwrap().unwrap();
        assert_eq!(end, SessionEnd::EarsLost);
        let line = text(Line::NoEars, "en");
        assert_eq!(c.probe.frames_of(line.len()), line.len(), "the line plays out whole");
        assert_eq!(end.failure(), Some("the call's speech recognition stopped"));
        assert!(c.probe.left_room.load(Ordering::SeqCst));
    }

    /// An outbox whose every append takes as long as a slow fsync.
    struct SlowOutbox(Arc<Mutex<note_voice_proto::MemOutbox>>);

    impl note_voice_proto::Outbox for SlowOutbox {
        fn append(&mut self, call_id: &str, body: &CallBody) -> std::io::Result<u64> {
            std::thread::sleep(Duration::from_millis(50));
            self.0.append(call_id, body)
        }
        fn unacked(&self, call_id: &str, after: u64) -> std::io::Result<Vec<(u64, CallBody)>> {
            self.0.unacked(call_id, after)
        }
        fn ack(&mut self, call_id: &str, upto: u64) -> std::io::Result<()> {
            self.0.ack(call_id, upto)
        }
        fn pending_calls(&self) -> std::io::Result<Vec<String>> {
            self.0.pending_calls()
        }
        fn forget(&mut self, call_id: &str) -> std::io::Result<()> {
            self.0.forget(call_id)
        }
    }

    #[tokio::test]
    async fn a_slow_journal_neither_gaps_the_playout_nor_reorders_frames() {
        use crate::outgoing::CallWriter;
        use note_voice_proto::{testkit::Recording, Dir, MemOutbox, Outbox, Peer, PeerConfig, Role};

        let journal: Arc<Mutex<MemOutbox>> = Arc::default();
        let peer = Peer::new(
            PeerConfig::new(Role::Voice),
            Dir::ToNote,
            Arc::new(Recording::default()),
            Box::new(SlowOutbox(journal.clone())),
        );
        let writer = CallWriter::spawn(move |body| {
            peer.send_call("c1", body).unwrap();
        })
        .unwrap();
        let tts: Arc<FakeTts> = Arc::default();
        let probe = Probe::default();
        let media = FakeMedia { script: Mutex::new(VecDeque::new()), probe: probe.clone() };
        let (inbox, rx) = mpsc::unbounded_channel();
        let _session = tokio::spawn(run_session(deps(Vec::new(), false, &tts), Box::new(media), rx, writer.sender()));
        let send = |body| inbox.send(SessionIn::Frame(body)).unwrap();

        let texts: Vec<String> = (1..=8).map(|n| "x".repeat(n)).collect();
        for (reply, text) in (1u64..).zip(&texts) {
            send(CallBody::Speak { reply, idx: 0, text: text.clone() });
            send(CallBody::SpeakDone { reply });
        }
        for text in &texts {
            for _ in 0..1000 {
                if tts.said.lock().unwrap().contains(text) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
        for reply in 1..=8 {
            send(CallBody::Play { reply });
        }

        let mut expected: Vec<CallBody> = (1..=8).map(|reply| CallBody::Played { reply }).collect();
        expected.push(CallBody::Floor { floor: Floor::Drained });
        let journaled = || journal.unacked("c1", 0).unwrap().into_iter().map(|(_, b)| b).collect::<Vec<_>>();
        for _ in 0..300 {
            if journaled().len() >= expected.len() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(journaled(), expected, "every frame is journaled, in order");

        let at = probe.sent_at.lock().unwrap().clone();
        assert_eq!(at.len(), 36, "every frame of every reply played");
        let worst = at.windows(2).map(|w| w[1] - w[0]).max().unwrap();
        assert!(worst < Duration::from_millis(40), "a {worst:?} gap between media frames");
    }

    fn write_wav(path: &Path, spec: hound::WavSpec, samples: &[i16]) {
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for &s in samples {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
    }

    #[test]
    fn a_wav_cue_is_mixed_down_and_resampled_to_48k() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("ready.wav");
        let spec = hound::WavSpec { channels: 2, sample_rate: 24_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        write_wav(&wav, spec, &[1000, 3000, 4000, 6000]);
        let cues = Cues::load(&[wav], &[]);
        assert_eq!(*cues.ready, vec![2000, 3500, 5000, 5000]);
        assert!(cues.heard.is_empty());
    }

    #[test]
    fn an_ogg_or_unreadable_override_falls_back_to_the_default_cue() {
        let dir = tempfile::tempdir().unwrap();
        let ogg = dir.path().join("ready.ogg");
        std::fs::write(&ogg, b"OggS").unwrap();
        let pcm = dir.path().join("ready.pcm");
        std::fs::write(&pcm, [0x10, 0x00, 0x20, 0x00]).unwrap();
        let wav = dir.path().join("heard.wav");
        let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        write_wav(&wav, spec, &[7, 8, 9]);
        let cues = Cues::load(&[ogg, pcm.clone()], &[dir.path().join("missing.wav"), wav]);
        assert_eq!(*cues.ready, vec![0x10, 0x20]);
        assert_eq!(*cues.heard, vec![7, 8, 9]);
    }

    /// Plays clips of `NOTE_VOICE_LID_CLIPS` (16 kHz WAVs named `en-…`/`ja-…`) into calls on the real
    /// models, each as a user set to the other language, and prints the first turn's commit and how
    /// long after the speech it came, with the identifier and without it.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs NOTE_VOICE_MODELS and NOTE_VOICE_LID_CLIPS; prints timings"]
    async fn the_real_models_settle_the_language_of_the_first_turn() {
        use crate::audio::engines::{Device, Engines};
        use crate::config::{language_id_from_dir, models_from_dir, TtsConfig};
        let models = PathBuf::from(std::env::var_os("NOTE_VOICE_MODELS").unwrap());
        let clips = PathBuf::from(std::env::var_os("NOTE_VOICE_LID_CLIPS").unwrap());
        let load = || Engines::load(&models_from_dir(&models, &TtsConfig::default()), Device::Cpu).unwrap();
        let with: Arc<dyn SpeechEngines> = Arc::new(load().with_identifier(language_id_from_dir(&models).as_ref()));
        let without: Arc<dyn SpeechEngines> = Arc::new(load());
        let mut names: Vec<String> =
            std::fs::read_dir(&clips).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        let (mut right, mut lag_with, mut lag_without) = (0, Vec::new(), Vec::new());
        for name in names.iter().filter(|n| !n.contains("-caro-") && !n.contains("-himari-") && !n.contains("-default-")) {
            let want = &name[..2];
            let mut reader = hound::WavReader::open(clips.join(name)).unwrap();
            let speech: Vec<f32> = reader.samples::<i16>().map(|s| f32::from(s.unwrap()) / 32768.0).collect();
            let lead = vec![0.0; RATE * 3 / 10];
            let tail = vec![0.0; RATE * 3];
            let script: Vec<Vec<f32>> = [lead.as_slice(), &speech, &tail].concat().chunks(160).map(<[f32]>::to_vec).collect();
            let spoken = Duration::from_millis(((lead.len() + speech.len()) / 16) as u64);
            let setting = if want == "ja" { "en" } else { "ja" };
            for (identifying, engines, lags, profile) in [(true, &with, &mut lag_with, setting), (false, &without, &mut lag_without, want)] {
                let japanese = Arc::new(Japanese::default());
                let tts: Arc<FakeTts> = Arc::default();
                let d = SessionDeps {
                    engines: engines.clone(),
                    sidecars: vec![japanese_sidecar(&japanese)],
                    profile: VoiceProfile { language: profile.into(), voice: String::new(), cue: false },
                    direction: Direction::Inbound,
                    ..deps(Vec::new(), false, &tts)
                };
                let started = std::time::Instant::now();
                let c = call_with(d, script.clone(), tts);
                let commit = loop {
                    if let Some(b) = c.log().into_iter().find(|b| matches!(b, CallBody::Commit { .. })) {
                        break Some((b, started.elapsed()));
                    }
                    if started.elapsed() > spoken + Duration::from_secs(5) {
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                };
                c.inbox.send(SessionIn::Frame(CallBody::HangUp)).unwrap();
                let Some((CallBody::Commit { text, language, .. }, at)) = commit else {
                    println!("{name:<18} set to {profile}: nothing committed");
                    continue;
                };
                let lag = at.saturating_sub(spoken);
                lags.push(lag);
                if identifying {
                    right += usize::from(language.as_deref() == Some(want));
                    println!("{name:<18} set to {setting}: {language:?} {text:?}, committed {lag:?} after the speech");
                }
            }
        }
        let median = |v: &mut Vec<Duration>| {
            v.sort();
            v[v.len() / 2]
        };
        println!(
            "{right}/{} calls settled on the clip's language; median commit lag {:?} identifying, {:?} on the right language without",
            lag_with.len(),
            median(&mut lag_with),
            median(&mut lag_without)
        );
    }

    #[test]
    fn notes_voice_wins_then_the_callers_then_a_reply_on_its_way() {
        assert_eq!(shown(true, true, false), LiveState::Speaking);
        assert_eq!(shown(true, false, true), LiveState::Thinking, "a cue while the reply is coming");
        assert_eq!(shown(false, true, true), LiveState::Hearing);
        assert_eq!(shown(false, false, true), LiveState::Thinking);
        assert_eq!(shown(false, false, false), LiveState::Listening);
    }

    #[tokio::test(start_paused = true)]
    async fn the_caller_sees_listening_hearing_thinking_and_speaking_in_turn() {
        let c = call(audio(&[(0.1, 1000), (0.0, 1000)]), vec![(2, "move"), (4, "move my"), (6, "move my run")], false);
        until_committed(&c).await;
        c.frame(CallBody::Speak { reply: 2, idx: 0, text: "ok".into() });
        c.frame(CallBody::SpeakDone { reply: 2 });
        c.frame(CallBody::Play { reply: 2 });
        c.synthesized("ok").await;
        sleep_ms(1000).await;
        let shown = c.probe.shown.lock().unwrap().clone();
        let first = |s: LiveState| shown.iter().position(|&x| x == s).unwrap_or_else(|| panic!("{s:?} never shown: {shown:?}"));
        assert_eq!(shown[0], LiveState::Listening, "{shown:?}");
        assert!(first(LiveState::Hearing) < first(LiveState::Thinking), "{shown:?}");
        assert!(first(LiveState::Thinking) < first(LiveState::Speaking), "{shown:?}");
        assert_eq!(shown.last(), Some(&LiveState::Listening), "{shown:?}");
        assert!(shown.windows(2).all(|w| w[0] != w[1]), "only changes are shown: {shown:?}");
    }
}
