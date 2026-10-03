use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use note_voice_proto::{CallBody, Floor, VoiceProfile};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use crate::audio::engines::{SpeechEngines, SpeechToText, TurnDetector, Vad};
use crate::audio::lines::{Line, Lines};
use crate::audio::playout::{Clip, Playout};
use crate::audio::speech::SpeechQueue;
use crate::audio::turn::{backchannels, Action, Input, TurnConfig, TurnMachine};
use crate::media::MediaIo;

const RATE: usize = 16_000;
const VAD_WINDOW: usize = 512;
const STT_CHUNK: usize = RATE * 160 / 1000;
const TURN_SPAN: usize = RATE * 8;
const TICK: Duration = Duration::from_millis(10);
const LOST_NOTICE: Duration = Duration::from_secs(1);
const DRAIN_CAP: Duration = Duration::from_secs(5);

/// 48 kHz mono audio for the call's cues; an empty one plays nothing.
pub struct Cues {
    pub ready: Arc<Vec<i16>>,
    pub heard: Arc<Vec<i16>>,
}

impl Cues {
    /// Raw s16le files; a missing or unreadable one is logged and left silent.
    pub fn load(ready: Option<&Path>, heard: Option<&Path>) -> Cues {
        Cues { ready: Arc::new(read_pcm(ready)), heard: Arc::new(read_pcm(heard)) }
    }
}

fn read_pcm(path: Option<&Path>) -> Vec<i16> {
    let Some(path) = path else { return Vec::new() };
    match std::fs::read(path) {
        Ok(bytes) => bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect(),
        Err(e) => {
            eprintln!("voice: reading the cue {} failed: {e}", path.display());
            Vec::new()
        }
    }
}

pub struct SessionDeps {
    pub engines: Arc<dyn SpeechEngines>,
    pub lines: Arc<Lines>,
    pub cues: Arc<Cues>,
    pub profile: VoiceProfile,
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
    Lines { lost: Option<Arc<Vec<i16>>>, goodbye: Option<Arc<Vec<i16>>> },
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
    lines_ready: bool,
    lost_line: Option<Arc<Vec<i16>>>,
    goodbye_line: Option<Arc<Vec<i16>>>,
    goodbye_pending: bool,
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
            tokio::task::spawn_blocking(move || {
                let render = |line| {
                    lines
                        .get(&*tts, &language, &voice, line)
                        .map_err(|e| eprintln!("voice: rendering {line:?} failed: {e:#}"))
                        .ok()
                };
                let _ = events.send(Event::Lines { lost: render(Line::LostNotes), goodbye: render(Line::Goodbye) });
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
            lines_ready: false,
            lost_line: None,
            goodbye_line: None,
            goodbye_pending: false,
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
                () = &mut left => return SessionEnd::UserLeft,
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
            if !self.lost_played && self.lines_ready && since.elapsed() >= LOST_NOTICE {
                self.lost_played = true;
                if let Some(pcm) = &self.lost_line {
                    self.playout.push(Clip { reply: None, chars: 0, pcm: pcm.to_vec() });
                }
            }
            if since.elapsed() >= link_grace {
                self.say_goodbye(SessionEnd::LinkLost);
            }
        }
        if self.start.elapsed() >= max_len {
            self.say_goodbye(SessionEnd::TimedOut);
        }
        if self.goodbye_pending && self.lines_ready {
            self.goodbye_pending = false;
            if let Some(pcm) = &self.goodbye_line {
                self.playout.push(Clip { reply: None, chars: 0, pcm: pcm.to_vec() });
            }
        }
        let ending = self.ending.as_ref()?;
        let drained = !self.goodbye_pending && self.playout.is_empty() && self.speech.is_idle();
        (drained || Instant::now() >= ending.by).then(|| ending.end.clone())
    }

    fn act(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::ScoreTurn => self.score_turn(),
                Action::Draft { turn, text } => (self.send)(CallBody::Draft { turn, text }),
                Action::Commit { turn, .. } => {
                    let _ = self.stt.send(SttCmd::Finish { turn });
                }
                Action::Retract { turn } => (self.send)(CallBody::Retract { turn }),
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
            Event::Lines { lost, goodbye } => {
                self.lines_ready = true;
                self.lost_line = lost;
                self.goodbye_line = goodbye;
                return;
            }
            _ if self.ending.is_some() => return,
            Event::Vad { at, speech } => Input::Vad { at, speech },
            Event::Partial(text) => Input::Partial { text },
            Event::Score { at, p } => Input::TurnScore { at, p },
            Event::Finished { turn, text } => {
                if !text.is_empty() {
                    (self.send)(CallBody::Commit { turn, text });
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
            let text = self.partial();
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
    }

    impl SpeechEngines for FakeEngines {
        fn languages(&self) -> Vec<String> {
            vec!["en".into()]
        }

        fn vad(&self, _language: &str) -> anyhow::Result<Box<dyn Vad>> {
            Ok(Box::new(FakeVad))
        }

        fn stt(&self, _language: &str) -> anyhow::Result<Box<dyn SpeechToText>> {
            Ok(Box::new(FakeStt { script: self.stt.clone(), heard: 0, panics: self.stt_panics }))
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
            Ok(())
        }

        fn clear(&self) {}

        async fn left(&self) {
            self.probe.gone.notified().await;
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
            engines: Arc::new(FakeEngines { stt, tts: tts.clone(), stt_panics: false }),
            lines: Arc::new(Lines::default()),
            cues: Arc::new(Cues { ready: Arc::new(vec![READY; FRAME]), heard: Arc::new(vec![HEARD; FRAME]) }),
            profile: VoiceProfile { language: "en".into(), voice: String::new(), cue },
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
        let engines = Arc::new(FakeEngines { stt: Vec::new(), tts: tts.clone(), stt_panics: true });
        let c = call_with(SessionDeps { engines, ..deps(Vec::new(), false, &tts) }, audio(&[(0.1, 1000)]), tts);
        let end = tokio::time::timeout(Duration::from_secs(2), c.end).await.unwrap().unwrap();
        assert!(matches!(end, SessionEnd::MediaFailed(_)), "{end:?}");
        assert!(c.probe.left_room.load(Ordering::SeqCst));
    }
}
