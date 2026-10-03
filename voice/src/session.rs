use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use note_voice_proto::{CallBody, Direction, Floor, VoiceProfile};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use crate::audio::engines::{SpeechEngines, SpeechToText, TurnDetector, Vad};
use crate::audio::lines::{Line, Lines};
use crate::audio::playout::{Clip, Playout};
use crate::audio::speech::{Gate, SpeechQueue};
use crate::audio::turn::{backchannels, Action, Input, TurnConfig, TurnMachine};
use crate::media::{Gone, MediaIo};

const RATE: usize = 16_000;
const VAD_WINDOW: usize = 512;
const STT_CHUNK: usize = RATE * 160 / 1000;
const TURN_SPAN: usize = RATE * 8;
const TICK: Duration = Duration::from_millis(10);
const LOST_NOTICE: Duration = Duration::from_secs(1);
const DRAIN_CAP: Duration = Duration::from_secs(5);
const FILLER_AFTER: Duration = Duration::from_millis(1500);

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
        _ => Ok(std::fs::read(path)?.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()),
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
    /// An inbound call is greeted with `Line::Hi` if Note is quiet at first.
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
    MediaFailed(String),
}

enum Event {
    Vad { at: Duration, speech: bool },
    Partial(String),
    Score { at: Duration, p: f32 },
    Finished { turn: u64, text: String },
    /// `pcm` is None for a line that failed to render.
    Line { line: Line, pcm: Option<Arc<Vec<i16>>> },
}

enum SttCmd {
    Accept(Vec<f32>),
    Finish { turn: u64 },
    Reset,
}

enum SttOut {
    Partial(String),
    Finished(Option<(u64, String)>),
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
    by: Instant,
}

struct Live<S> {
    send: S,
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
    goodbye_pending: bool,
    /// The line to fill with by `FILLER_AFTER` past the instant: "hi" from the start of an inbound
    /// call unless Note's words arrive, "one moment" from a Commit unless a reply is speaking.
    awaiting_reply: Option<(Instant, Line)>,
    drafted: Option<u64>,
    start: Instant,
    playing: bool,
    played_reply: bool,
    link_down_since: Option<Instant>,
    lost_played: bool,
    ending: Option<Ending>,
}

impl<S: Fn(CallBody)> Live<S> {
    fn start(deps: &SessionDeps, media: Arc<dyn MediaIo>, send: S) -> anyhow::Result<(Self, Tasks)> {
        let languages = deps.engines.languages();
        let language = if languages.contains(&deps.profile.language) {
            deps.profile.language.clone()
        } else {
            languages.first().cloned().ok_or_else(|| anyhow::anyhow!("no voice models"))?
        };
        let voice = deps.profile.voice.clone();
        let vad = deps.engines.vad(&language)?;
        let stt = deps.engines.stt(&language)?;
        let tts = deps.engines.tts(&language);
        let start = Instant::now();
        let (events_tx, events) = mpsc::unbounded_channel();
        let (stt_tx, stt_rx) = mpsc::unbounded_channel();
        let recent = Arc::new(Mutex::new(VecDeque::with_capacity(TURN_SPAN)));
        let tasks = Tasks {
            stt: tokio::spawn(stt_worker(stt, stt_rx, events_tx.clone())),
            audio: tokio::spawn(audio_in(media.clone(), vad, stt_tx.clone(), events_tx.clone(), recent.clone(), start)),
        };
        {
            let (lines, tts, language, voice, events) =
                (deps.lines.clone(), tts.clone(), language.clone(), voice.clone(), events_tx.clone());
            let order: &[Line] = match deps.direction {
                Direction::Inbound => &[Line::Hi, Line::OneMoment, Line::LostNotes, Line::Goodbye],
                Direction::Outbound => &[Line::OneMoment, Line::LostNotes, Line::Goodbye],
            };
            tokio::task::spawn_blocking(move || {
                for &line in order {
                    let pcm = lines
                        .get(&*tts, &language, &voice, line)
                        .map_err(|e| eprintln!("voice: rendering {line:?} failed: {e:#}"))
                        .ok();
                    if events.send(Event::Line { line, pcm }).is_err() {
                        return;
                    }
                }
            });
        }
        let mut playout = Playout::default();
        if deps.profile.cue {
            playout.push(Clip { reply: None, chars: 0, pcm: deps.cues.ready.to_vec() });
        }
        let live = Live {
            send,
            media,
            playout,
            speech: SpeechQueue::new(tts, voice),
            turn: TurnMachine::new(TurnConfig::default(), backchannels(&language)),
            detector: deps.engines.turn(&language),
            recent,
            stt: stt_tx,
            events_tx,
            events,
            heard_cue: deps.profile.cue.then(|| deps.cues.heard.clone()),
            lines: HashMap::new(),
            goodbye_pending: false,
            awaiting_reply: (deps.direction == Direction::Inbound).then_some((start, Line::Hi)),
            drafted: None,
            start,
            playing: false,
            played_reply: false,
            link_down_since: None,
            lost_played: false,
            ending: None,
        };
        Ok((live, tasks))
    }

    /// Ends with `MediaFailed` if the STT worker stops or the audio-in task panics; audio-in running out of
    /// audio is left to `left()`.
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
        loop {
            tokio::select! {
                gone = &mut left => return match gone {
                    Gone::Left => SessionEnd::UserLeft,
                    Gone::Failed(reason) => SessionEnd::MediaFailed(reason),
                },
                got = &mut tasks.stt => return task_died("speech recognition", got.err()),
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
        if let Some(frame) = self.playout.next_frame() {
            if let Err(e) = self.media.send(&frame).await {
                return Some(SessionEnd::MediaFailed(format!("{e:#}")));
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
        if let Some((at, line)) = self.awaiting_reply {
            if self.lines.contains_key(&line) && at.elapsed() >= FILLER_AFTER {
                self.awaiting_reply = None;
                self.play_line(line);
            }
        }
        if self.goodbye_pending && self.lines.contains_key(&Line::Goodbye) {
            self.goodbye_pending = false;
            self.play_line(Line::Goodbye);
        }
        let ending = self.ending.as_ref()?;
        let drained = !self.goodbye_pending && self.playout.is_empty() && self.speech.is_idle();
        (drained || Instant::now() >= ending.by).then(|| ending.end.clone())
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
                    (self.send)(CallBody::Draft { turn, text });
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
                Action::Floor(floor) => (self.send)(CallBody::Floor { floor }),
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
            Event::Line { line, pcm } => {
                self.lines.insert(line, pcm);
                return;
            }
            _ if self.ending.is_some() => return,
            Event::Vad { at, speech } => Input::Vad { at, speech },
            Event::Partial(text) => Input::Partial { text },
            Event::Score { at, p } => Input::TurnScore { at, p },
            Event::Finished { turn, text } => {
                let drafted = self.drafted.take_if(|&mut d| d == turn).is_some();
                if text.is_empty() {
                    if drafted {
                        (self.send)(CallBody::Retract { turn });
                    }
                } else {
                    (self.send)(CallBody::Commit { turn, text });
                    self.awaiting_reply = Some((Instant::now(), Line::OneMoment));
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
        let spoke = match &body {
            CallBody::Speak { reply, .. } => Some(*reply),
            _ => None,
        };
        match body {
            CallBody::Speak { reply, idx, text } => self.speech.speak(reply, idx, text),
            CallBody::SpeakDone { reply } => self.speech.speak_done(reply),
            CallBody::Play { reply } => self.speech.play(reply),
            CallBody::Drop { reply } => self.speech.drop_reply(reply),
            CallBody::HangUp => {
                self.playout.resume();
                self.ending = Some(Ending { end: SessionEnd::HungUp, by: Instant::now() + DRAIN_CAP });
            }
            _ => {}
        }
        let greeted = matches!(self.awaiting_reply, Some((_, Line::Hi)))
            && spoke.is_some_and(|r| self.speech.gate(r) != Gate::Dropped);
        if greeted || reply.is_some_and(|r| self.speech.is_speaking(r)) {
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
        if self.ending.is_some() {
            return;
        }
        self.media.clear();
        self.speech.flush(&mut self.playout);
        self.playout.resume();
        self.goodbye_pending = true;
        self.ending = Some(Ending { end, by: Instant::now() + DRAIN_CAP });
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
            let _ = events.send(Event::Vad { at: start.elapsed(), speech });
        }
        if chunk.len() >= STT_CHUNK {
            let _ = stt.send(SttCmd::Accept(std::mem::take(&mut chunk)));
        }
    }
}

/// Owns the call's STT stream; each step runs on the blocking pool, in order. Reports partial changes,
/// and an empty partial once a finish or reset clears the stream.
async fn stt_worker(
    mut stt: Box<dyn SpeechToText>,
    mut cmds: mpsc::UnboundedReceiver<SttCmd>,
    events: mpsc::UnboundedSender<Event>,
) {
    let mut last = String::new();
    while let Some(cmd) = cmds.recv().await {
        let step = tokio::task::spawn_blocking(move || {
            let out = match cmd {
                SttCmd::Accept(samples) => {
                    stt.accept(&samples);
                    SttOut::Partial(stt.partial())
                }
                SttCmd::Finish { turn } => SttOut::Finished(Some((turn, stt.finish()))),
                SttCmd::Reset => {
                    stt.finish();
                    SttOut::Finished(None)
                }
            };
            (stt, out)
        });
        let (back, out) = match step.await {
            Ok(done) => done,
            Err(e) => {
                eprintln!("voice: an STT step failed: {e}");
                return;
            }
        };
        stt = back;
        match out {
            SttOut::Partial(partial) => {
                if partial != last {
                    last.clone_from(&partial);
                    let _ = events.send(Event::Partial(partial));
                }
            }
            SttOut::Finished(finished) => {
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
    use crate::audio::engines::{TextToSpeech, VoiceInfo};
    use crate::audio::lines::text;
    use crate::audio::playout::FRAME;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

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

    impl TextToSpeech for FakeTts {
        fn synthesize(&self, text: &str, _voice: &str) -> anyhow::Result<Vec<i16>> {
            self.said.lock().unwrap().push(text.into());
            Ok(vec![text.len() as i16; text.len() * FRAME])
        }

        fn voices(&self) -> Vec<VoiceInfo> {
            Vec::new()
        }
    }

    struct FakeEngines {
        stt: Vec<(usize, &'static str)>,
        tts: Arc<FakeTts>,
        stt_panics: bool,
        blank_finish: bool,
    }

    impl SpeechEngines for FakeEngines {
        fn languages(&self) -> Vec<String> {
            vec!["en".into()]
        }

        fn vad(&self, _language: &str) -> anyhow::Result<Box<dyn Vad>> {
            Ok(Box::new(FakeVad))
        }

        fn stt(&self, _language: &str) -> anyhow::Result<Box<dyn SpeechToText>> {
            Ok(Box::new(FakeStt { script: self.stt.clone(), heard: 0, panics: self.stt_panics, blank_finish: self.blank_finish }))
        }

        fn turn(&self, _language: &str) -> Arc<dyn TurnDetector> {
            Arc::new(FakeTurn(0.9))
        }

        fn tts(&self, _language: &str) -> Arc<dyn TextToSpeech> {
            self.tts.clone()
        }
    }

    #[derive(Clone, Default)]
    struct Probe {
        /// The first sample of every frame sent.
        sent: Arc<Mutex<Vec<i16>>>,
        sent_at: Arc<Mutex<Vec<std::time::Instant>>>,
        gone: Arc<Notify>,
        left_room: Arc<AtomicBool>,
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

        /// Waits, without advancing the paused clock, for the queue's TTS thread to synthesize `text`.
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

    fn deps(stt: Vec<(usize, &'static str)>, cue: bool, tts: &Arc<FakeTts>) -> SessionDeps {
        SessionDeps {
            engines: Arc::new(FakeEngines { stt, tts: tts.clone(), stt_panics: false, blank_finish: false }),
            lines: Arc::new(Lines::default()),
            cues: Arc::new(Cues { ready: Arc::new(vec![READY; FRAME]), heard: Arc::new(vec![HEARD; FRAME]) }),
            profile: VoiceProfile { language: "en".into(), voice: String::new(), cue },
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
        assert_eq!(log[3], CallBody::Commit { turn: 1, text: "move my run".into() });
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

    #[tokio::test(start_paused = true)]
    async fn a_quiet_start_to_an_inbound_call_says_hi() {
        let hi = text(Line::Hi, "en").len();
        let inbound = || {
            let tts: Arc<FakeTts> = Arc::default();
            call_with(SessionDeps { direction: Direction::Inbound, ..deps(Vec::new(), false, &tts) }, Vec::new(), tts)
        };

        let c = inbound();
        sleep_ms(1400).await;
        assert_eq!(c.probe.frames_of(hi), 0, "not before 1.5 s");
        sleep_ms(2000).await;
        assert_eq!(c.probe.frames_of(hi), hi, "the line plays once");
        sleep_ms(3000).await;
        assert_eq!(c.probe.frames_of(hi), hi);

        let c = inbound();
        sleep_ms(500).await;
        c.frame(CallBody::Speak { reply: 1, idx: 0, text: "hello there".into() });
        sleep_ms(3000).await;
        assert_eq!(c.probe.frames_of(hi), 0, "Note's own greeting needs no filler, even held");

        let c = inbound();
        sleep_ms(500).await;
        c.frame(CallBody::Drop { reply: 1 });
        c.frame(CallBody::Speak { reply: 1, idx: 0, text: "hello there".into() });
        sleep_ms(3000).await;
        assert_eq!(c.probe.frames_of(hi), hi, "a dropped reply's words are no greeting");

        let c = call(Vec::new(), Vec::new(), false);
        sleep_ms(3000).await;
        assert_eq!(c.probe.frames_of(hi), 0, "an outbound call opens with Note's words");
    }

    #[tokio::test(start_paused = true)]
    async fn the_line_a_call_needs_first_renders_first() {
        let tts: Arc<FakeTts> = Arc::default();
        let c = call_with(SessionDeps { direction: Direction::Inbound, ..deps(Vec::new(), false, &tts) }, Vec::new(), tts);
        c.synthesized(text(Line::Goodbye, "en")).await;
        assert_eq!(c.tts.said.lock().unwrap()[0], text(Line::Hi, "en"));

        let tts: Arc<FakeTts> = Arc::default();
        let c = call_with(deps(Vec::new(), false, &tts), Vec::new(), tts);
        c.synthesized(text(Line::Goodbye, "en")).await;
        assert_eq!(c.tts.said.lock().unwrap()[0], text(Line::OneMoment, "en"));
    }

    #[tokio::test(start_paused = true)]
    async fn an_empty_commit_retracts_its_draft() {
        let tts: Arc<FakeTts> = Arc::default();
        let engines = Arc::new(FakeEngines {
            stt: vec![(2, "move"), (6, "move my run")],
            tts: tts.clone(),
            stt_panics: false,
            blank_finish: true,
        });
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
        assert!(c.log().contains(&CallBody::Commit { turn: 1, text: "wait actually".into() }));
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
    async fn a_crashed_recognizer_ends_the_call() {
        let tts: Arc<FakeTts> = Arc::default();
        let engines = Arc::new(FakeEngines { stt: Vec::new(), tts: tts.clone(), stt_panics: true, blank_finish: false });
        let c = call_with(SessionDeps { engines, ..deps(Vec::new(), false, &tts) }, audio(&[(0.1, 1000)]), tts);
        let end = tokio::time::timeout(Duration::from_secs(2), c.end).await.unwrap().unwrap();
        assert!(matches!(end, SessionEnd::MediaFailed(_)), "{end:?}");
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
}
