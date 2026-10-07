use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use super::playout::{Clip, Playout};
use super::tts::{Next, Speaker, SpeechBackend, SpeechStream, RATE};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Gate {
    #[default]
    Held,
    Playing,
    Dropped,
}

enum Cmd {
    Push { reply: u64, text: String },
    Finish { reply: u64 },
    Cancel { reply: u64 },
    Voice { speaker: Speaker, fallback: Arc<dyn SpeechBackend> },
}

enum Out {
    Piece { reply: u64, clip: Clip },
    /// The reply's stream ended after `pushes` of its texts.
    Ended { reply: u64, pushes: u32 },
    /// Neither the speaker nor the fallback could speak the reply.
    Unspeakable { reply: u64 },
}

#[derive(Clone)]
struct Queued {
    reply: u64,
    /// Samples queued to play up to and including this reply's.
    through: u64,
    received: u64,
}

/// What the worker reads of the queue's state.
#[derive(Default)]
struct Shared {
    /// The playing replies still producing audio, in play order, as of the last pump.
    playing: Mutex<Vec<Queued>>,
    cmds_sent: AtomicU64,
    /// Commands applied when the worker last found nothing to do.
    quiet_at: AtomicU64,
}

fn duration(samples: u64) -> Duration {
    Duration::from_micros(samples * 1_000_000 / u64::from(RATE))
}

#[derive(Default)]
struct Reply {
    gate: Gate,
    /// Any clause arrived while not dropped.
    spoken: bool,
    done: bool,
    next_idx: u32,
    waiting: BTreeMap<u32, String>,
    opened: bool,
    pushes: u32,
    ended: bool,
    ready: VecDeque<Clip>,
    received: u64,
}

impl Reply {
    fn produced_all(&self) -> bool {
        self.done && (self.ended || !self.opened) && self.ready.is_empty()
    }
}

pub struct SpeechQueue {
    cmds: Sender<Cmd>,
    dropped: Arc<Mutex<HashSet<u64>>>,
    shared: Arc<Shared>,
    to_remove: Vec<u64>,
    replies: HashMap<u64, Reply>,
    play_order: VecDeque<u64>,
    playing: Vec<u64>,
    rx: mpsc::UnboundedReceiver<Out>,
    rendering: Arc<AtomicBool>,
    unspeakable: Vec<u64>,
}

impl SpeechQueue {
    /// Starts one worker thread that drives each reply's stream on `speaker`, in reply order, one
    /// render at a time. A reply whose stream fails goes on in `fallback`'s default voice.
    pub fn new(speaker: Speaker, fallback: Arc<dyn SpeechBackend>) -> Self {
        let (cmds, queued) = std::sync::mpsc::channel::<Cmd>();
        let (tx, rx) = mpsc::unbounded_channel();
        let dropped = Arc::new(Mutex::new(HashSet::new()));
        let shared = Arc::new(Shared::default());
        let worker = Worker {
            speaker,
            fallback,
            streams: Vec::new(),
            dropped: dropped.clone(),
            shared: shared.clone(),
            applied: 0,
            pushes: HashMap::new(),
            tx,
        };
        std::thread::Builder::new()
            .name("note-voice-tts".into())
            .spawn(move || worker.run(&queued))
            .expect("spawning the TTS thread");
        Self {
            cmds,
            dropped,
            shared,
            to_remove: Vec::new(),
            replies: HashMap::new(),
            play_order: VecDeque::new(),
            playing: Vec::new(),
            rx,
            rendering: Arc::default(),
            unspeakable: Vec::new(),
        }
    }

    /// Replies no backend could speak, each reported once; the call has no voice while this happens.
    pub fn take_unspeakable(&mut self) -> Vec<u64> {
        self.receive();
        std::mem::take(&mut self.unspeakable)
    }

    fn send(&self, cmd: Cmd) {
        self.shared.cmds_sent.fetch_add(1, Ordering::SeqCst);
        self.cmds.send(cmd).expect("the TTS thread outlives the queue");
    }

    fn push(&mut self, reply: u64, texts: Vec<String>) {
        if texts.is_empty() {
            return;
        }
        let state = self.replies.get_mut(&reply).expect("a speaking reply has state");
        state.opened = true;
        state.ended = false;
        state.pushes += texts.len() as u32;
        for text in texts {
            self.send(Cmd::Push { reply, text });
        }
        self.note_rendering();
    }

    /// Set while any reply has text not yet fully rendered.
    pub fn rendering(&self) -> Arc<AtomicBool> {
        self.rendering.clone()
    }

    fn note_rendering(&self) {
        let busy = self.replies.values().any(|s| s.opened && !s.ended && s.gate != Gate::Dropped);
        self.rendering.store(busy, Ordering::SeqCst);
    }

    /// Replies that open from here on speak in `speaker`, falling back to `fallback`.
    pub fn set_voice(&mut self, speaker: Speaker, fallback: Arc<dyn SpeechBackend>) {
        self.send(Cmd::Voice { speaker, fallback });
    }

    /// Clauses reach the stream in idx order.
    pub fn speak(&mut self, reply: u64, idx: u32, text: String) {
        let state = self.replies.entry(reply).or_default();
        if state.gate == Gate::Dropped || state.done || idx < state.next_idx {
            return;
        }
        state.spoken = true;
        state.waiting.insert(idx, text);
        let mut texts = Vec::new();
        while let Some(text) = state.waiting.remove(&state.next_idx) {
            texts.push(text);
            state.next_idx += 1;
        }
        self.push(reply, texts);
    }

    /// Clauses still waiting on a missing idx are spoken in order.
    pub fn speak_done(&mut self, reply: u64) {
        let state = self.replies.entry(reply).or_default();
        if state.done {
            return;
        }
        state.done = true;
        if state.gate == Gate::Dropped {
            return;
        }
        let texts: Vec<String> = std::mem::take(&mut state.waiting).into_values().collect();
        self.push(reply, texts);
        if self.replies[&reply].opened {
            self.send(Cmd::Finish { reply });
        }
    }

    pub fn play(&mut self, reply: u64) {
        let state = self.replies.entry(reply).or_default();
        if state.gate == Gate::Held {
            state.gate = Gate::Playing;
            self.play_order.push_back(reply);
            self.playing.push(reply);
        }
    }

    /// A reply already in the playout leaves it on the next `pump`.
    pub fn drop_reply(&mut self, reply: u64) {
        self.dropped.lock().expect("dropped lock").insert(reply);
        if self.playing.contains(&reply) {
            self.to_remove.push(reply);
        }
        let state = self.replies.entry(reply).or_default();
        let cancel = state.opened && !state.ended && state.gate != Gate::Dropped;
        state.gate = Gate::Dropped;
        state.waiting.clear();
        state.ready.clear();
        self.play_order.retain(|&r| r != reply);
        self.playing.retain(|&r| r != reply);
        if cancel {
            self.send(Cmd::Cancel { reply });
        }
        self.note_rendering();
    }

    /// Moves the audio of playing replies, in play order, into the playout.
    pub fn pump(&mut self, playout: &mut Playout) {
        self.receive();
        for reply in self.to_remove.drain(..) {
            playout.remove(reply);
        }
        while let Some(&reply) = self.play_order.front() {
            let state = self.replies.get_mut(&reply).expect("a playing reply has state");
            for clip in state.ready.drain(..) {
                playout.push(clip);
            }
            if !state.produced_all() {
                break;
            }
            self.play_order.pop_front();
        }
        let mut through = playout.queued_samples() as u64;
        let playing = self
            .play_order
            .iter()
            .map(|&reply| {
                let state = &self.replies[&reply];
                through += state.ready.iter().map(|c| c.pcm.len() as u64).sum::<u64>();
                Queued { reply, through, received: state.received }
            })
            .collect();
        *self.shared.playing.lock().expect("playing lock") = playing;
    }

    /// Told to play, not dropped, and has words to say.
    pub fn is_speaking(&self, reply: u64) -> bool {
        self.replies.get(&reply).is_some_and(|s| s.gate == Gate::Playing && s.spoken)
    }

    pub fn gate(&self, reply: u64) -> Gate {
        self.replies.get(&reply).map_or(Gate::Held, |s| s.gate)
    }

    /// A reply that has been told to play and is fully played out.
    pub fn is_done(&self, reply: u64, playout: &Playout) -> bool {
        self.replies
            .get(&reply)
            .is_some_and(|s| s.gate == Gate::Playing && s.produced_all() && !playout.holds(reply))
    }

    /// No reply told to play is still waiting to finish.
    pub fn is_idle(&self) -> bool {
        self.playing.is_empty()
    }

    /// Replies told to play whose every piece has been rendered and played out, each reported once.
    pub fn take_finished(&mut self, playout: &mut Playout) -> Vec<u64> {
        playout.take_finished();
        let (finished, playing) = self.playing.iter().partition(|&&r| self.is_done(r, playout));
        self.playing = playing;
        finished
    }

    /// Flushes the playout and drops every reply still playing, with the characters heard of each.
    /// A reply already fully played is left for `take_finished`.
    pub fn flush(&mut self, playout: &mut Playout) -> Vec<(u64, u32)> {
        let between_clips: Vec<(u64, u32)> = self
            .playing
            .iter()
            .filter(|&&r| !self.is_done(r, playout))
            .map(|&r| (r, playout.heard(r)))
            .collect();
        let cut = playout.flush();
        let heard: Vec<(u64, u32)> = between_clips
            .into_iter()
            .map(|(r, h)| cut.iter().find(|&&(c, _)| c == r).copied().unwrap_or((r, h)))
            .collect();
        for &(reply, _) in &heard {
            self.drop_reply(reply);
        }
        heard
    }

    fn receive(&mut self) {
        while let Ok(out) = self.rx.try_recv() {
            match out {
                Out::Piece { reply, clip } => {
                    if let Some(state) = self.replies.get_mut(&reply).filter(|s| s.gate != Gate::Dropped) {
                        state.received += clip.pcm.len() as u64;
                        state.ready.push_back(clip);
                    }
                }
                Out::Ended { reply, pushes } => {
                    if let Some(state) = self.replies.get_mut(&reply).filter(|s| pushes >= s.pushes) {
                        state.ended = true;
                    }
                }
                Out::Unspeakable { reply } => {
                    if self.replies.get(&reply).is_some_and(|s| s.gate != Gate::Dropped) {
                        self.unspeakable.push(reply);
                    }
                }
            }
        }
        self.note_rendering();
    }
}

const POLL: Duration = Duration::from_millis(10);

struct Active {
    reply: u64,
    stream: Box<dyn SpeechStream>,
    on_fallback: bool,
    /// The fallback itself failed once and was reopened.
    retried: bool,
    /// The text pushed to the current stream.
    pushed: Vec<String>,
    /// Characters of `pushed` already covered by audio.
    covered: u32,
    finished: bool,
    /// Samples sent so far.
    sent: u64,
}

struct Worker {
    speaker: Speaker,
    fallback: Arc<dyn SpeechBackend>,
    streams: Vec<Active>,
    dropped: Arc<Mutex<HashSet<u64>>>,
    shared: Arc<Shared>,
    applied: u64,
    /// Texts pushed per reply, across its streams.
    pushes: HashMap<u64, u32>,
    tx: mpsc::UnboundedSender<Out>,
}

impl Worker {
    fn run(mut self, cmds: &Receiver<Cmd>) {
        loop {
            let progressed = self.step();
            if !progressed {
                self.shared.quiet_at.store(self.applied, Ordering::SeqCst);
            }
            let first = if progressed {
                match cmds.try_recv() {
                    Ok(cmd) => Some(cmd),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return,
                }
            } else {
                match cmds.recv_timeout(POLL) {
                    Ok(cmd) => Some(cmd),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            };
            for cmd in first.into_iter().chain(std::iter::from_fn(|| cmds.try_recv().ok())) {
                self.apply(cmd);
                self.applied += 1;
            }
            if self.tx.is_closed() {
                return;
            }
        }
    }

    fn is_dropped(&self, reply: u64) -> bool {
        self.dropped.lock().expect("dropped lock").contains(&reply)
    }

    fn apply(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Push { reply, text } => {
                if self.is_dropped(reply) {
                    return;
                }
                *self.pushes.entry(reply).or_default() += 1;
                let at = match self.streams.iter().position(|a| a.reply == reply) {
                    Some(at) => at,
                    None => match self.open(reply) {
                        Some(active) => {
                            self.streams.push(active);
                            self.streams.len() - 1
                        }
                        None => return,
                    },
                };
                let active = &mut self.streams[at];
                active.pushed.push(text.clone());
                if let Err(e) = active.stream.push(&text) {
                    self.fail(at, &e);
                }
            }
            Cmd::Finish { reply } => {
                let Some(at) = self.streams.iter().position(|a| a.reply == reply) else {
                    self.ended(reply);
                    return;
                };
                self.streams[at].finished = true;
                if let Err(e) = self.streams[at].stream.finish() {
                    self.fail(at, &e);
                }
            }
            Cmd::Cancel { reply } => {
                if let Some(at) = self.streams.iter().position(|a| a.reply == reply) {
                    self.streams.remove(at).stream.cancel();
                }
            }
            Cmd::Voice { speaker, fallback } => {
                self.speaker = speaker;
                self.fallback = fallback;
            }
        }
    }

    fn open(&self, reply: u64) -> Option<Active> {
        let on_fallback = self.speaker.backend.id() == self.fallback.id();
        let opened = self.speaker.backend.open(&self.speaker.voice).map(|s| (s, on_fallback)).or_else(|e| {
            eprintln!("voice: opening {} for reply {reply} failed: {e:#}", self.speaker.backend.id());
            self.fallback.open("").map(|s| (s, true))
        });
        match opened {
            Ok((stream, on_fallback)) => Some(Active {
                reply,
                stream,
                on_fallback,
                retried: false,
                pushed: Vec::new(),
                covered: 0,
                finished: false,
                sent: 0,
            }),
            Err(e) => {
                eprintln!("voice: reply {reply} cannot be spoken: {e:#}");
                let _ = self.tx.send(Out::Unspeakable { reply });
                None
            }
        }
    }

    /// Hands the text not yet covered by audio to the fallback, which speaks the rest of the reply. On
    /// the fallback a live stream only loses the piece; a dead one is reopened once, and failing
    /// again the reply is reported unspeakable.
    fn fail(&mut self, at: usize, err: &anyhow::Error) {
        let active = &mut self.streams[at];
        eprintln!("voice: speaking reply {} failed: {err:#}", active.reply);
        if active.on_fallback && active.stream.alive() {
            return;
        }
        if active.on_fallback && active.retried {
            let reply = self.streams.remove(at).reply;
            let _ = self.tx.send(Out::Unspeakable { reply });
            self.ended(reply);
            return;
        }
        active.stream.cancel();
        let mut skip = active.covered as usize;
        let mut rest = Vec::new();
        for text in &active.pushed {
            let len = text.chars().count();
            if skip >= len {
                skip -= len;
            } else {
                rest.push(text.chars().skip(skip).collect::<String>());
                skip = 0;
            }
        }
        let resumed = self.fallback.open("").and_then(|mut stream| {
            for text in &rest {
                stream.push(text)?;
            }
            if active.finished {
                stream.finish()?;
            }
            Ok(stream)
        });
        match resumed {
            Ok(stream) => {
                active.stream = stream;
                active.retried = active.on_fallback;
                active.on_fallback = true;
                active.pushed = rest;
                active.covered = 0;
            }
            Err(e) => {
                eprintln!("voice: reply {} cannot go on: {e:#}", active.reply);
                let reply = self.streams.remove(at).reply;
                let _ = self.tx.send(Out::Unspeakable { reply });
                self.ended(reply);
            }
        }
    }

    fn ended(&self, reply: u64) {
        let pushes = self.pushes.get(&reply).copied().unwrap_or(0);
        let _ = self.tx.send(Out::Ended { reply, pushes });
    }

    /// Takes one piece from the first stream that has one, playing replies first in play order, then
    /// held ones; false when none moved. A playing reply's audio ahead is everything queued to play
    /// before its next piece; a held reply's is its own audio.
    fn step(&mut self) -> bool {
        let playing = self.shared.playing.lock().expect("playing lock").clone();
        let mut order: Vec<(usize, usize)> = (0..self.streams.len())
            .map(|at| (playing.iter().position(|q| q.reply == self.streams[at].reply).unwrap_or(usize::MAX), at))
            .collect();
        order.sort_unstable();
        self.step_in(&playing, &order)
    }

    fn step_in(&mut self, playing: &[Queued], order: &[(usize, usize)]) -> bool {
        for &(_, at) in order {
            let reply = self.streams[at].reply;
            if self.is_dropped(reply) {
                self.streams.remove(at).stream.cancel();
                return true;
            }
            let sent = self.streams[at].sent;
            let ahead = duration(match playing.iter().find(|q| q.reply == reply) {
                Some(q) => q.through + sent.saturating_sub(q.received),
                None => sent,
            });
            match self.streams[at].stream.next(ahead) {
                Ok(Next::Pending) => {}
                Ok(Next::Audio(audio)) => {
                    let active = &mut self.streams[at];
                    active.covered += audio.chars;
                    active.sent += audio.pcm.len() as u64;
                    let clip = Clip { reply: Some(reply), chars: audio.chars, pcm: audio.pcm };
                    let _ = self.tx.send(Out::Piece { reply, clip });
                    return true;
                }
                Ok(Next::Done) => {
                    self.streams.remove(at);
                    self.ended(reply);
                    return true;
                }
                Err(e) => {
                    self.fail(at, &e);
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::super::engines::VoiceInfo;
    use super::super::playout::FRAME;
    use super::super::tts::{Audio, ChunkedBackend, Renderer, TextInput};
    use super::*;

    /// Speaks each push as one piece: `text.len()` frames, each sample equal to `text.len()`; text
    /// starting "slow" takes 50 ms, "fail" errors and "stop" ends the stream. Records the order of
    /// renders and whether two ever overlap.
    #[derive(Default)]
    struct FakeBackend {
        busy: AtomicBool,
        overlapped: AtomicBool,
        said: Mutex<Vec<String>>,
    }

    struct FakeStream {
        backend: Arc<FakeBackend>,
        texts: VecDeque<String>,
        finished: bool,
    }

    impl SpeechBackend for Arc<FakeBackend> {
        fn id(&self) -> &'static str {
            "fake"
        }

        fn label(&self) -> &'static str {
            "Fake"
        }

        fn input(&self) -> TextInput {
            TextInput::Incremental
        }

        fn voices(&self) -> Vec<VoiceInfo> {
            Vec::new()
        }

        fn open(&self, _voice: &str) -> anyhow::Result<Box<dyn SpeechStream>> {
            Ok(Box::new(FakeStream { backend: self.clone(), texts: VecDeque::new(), finished: false }))
        }
    }

    impl SpeechStream for FakeStream {
        fn push(&mut self, text: &str) -> anyhow::Result<()> {
            self.texts.push_back(text.into());
            Ok(())
        }

        fn finish(&mut self) -> anyhow::Result<()> {
            self.finished = true;
            Ok(())
        }

        fn next(&mut self, _ahead: Duration) -> anyhow::Result<Next> {
            let Some(text) = self.texts.pop_front() else {
                return Ok(if self.finished { Next::Done } else { Next::Pending });
            };
            self.backend.said.lock().unwrap().push(text.clone());
            if text == "stop" {
                self.finished = true;
            }
            if self.backend.busy.swap(true, Ordering::SeqCst) {
                self.backend.overlapped.store(true, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(if text.starts_with("slow") { 50 } else { 1 }));
            self.backend.busy.store(false, Ordering::SeqCst);
            if text == "fail" {
                anyhow::bail!("cannot say {text}");
            }
            Ok(Next::Audio(Audio { pcm: vec![text.len() as i16; text.len() * FRAME], chars: text.len() as u32 }))
        }

        fn cancel(&mut self) {
            self.texts.clear();
            self.finished = true;
        }
    }

    fn queue() -> SpeechQueue {
        let fake: Arc<dyn SpeechBackend> = Arc::new(Arc::new(FakeBackend::default()));
        SpeechQueue::new(Speaker::new(fake.clone(), ""), fake)
    }

    /// Waits until the worker has applied every command and has nothing left to do.
    async fn settle(q: &mut SpeechQueue) {
        while q.shared.quiet_at.load(Ordering::SeqCst) < q.shared.cmds_sent.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
        while q.shared.quiet_at.load(Ordering::SeqCst) < q.shared.cmds_sent.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        q.receive();
    }

    async fn until_pieces(q: &mut SpeechQueue, reply: u64, n: usize) {
        while q.replies.get(&reply).map_or(0, |s| s.ready.len()) < n {
            tokio::time::sleep(Duration::from_millis(1)).await;
            q.receive();
        }
    }

    fn drain(p: &mut Playout) -> Vec<i16> {
        std::iter::from_fn(|| p.next_frame()).map(|f| f[0]).collect()
    }

    #[tokio::test]
    async fn a_held_reply_plays_only_after_play_and_a_dropped_one_never() {
        let mut q = queue();
        let mut p = Playout::default();
        q.speak(1, 0, "draft".into());
        q.speak(2, 0, "wake".into());
        settle(&mut q).await;
        q.pump(&mut p);
        assert!(p.next_frame().is_none(), "nothing plays before Play");
        q.play(2);
        q.drop_reply(1);
        settle(&mut q).await;
        q.pump(&mut p);
        let mut n = 0;
        while p.next_frame().is_some() {
            n += 1;
        }
        assert_eq!(n, "wake".len());
        assert_eq!(q.gate(1), Gate::Dropped);
        assert_eq!(q.gate(2), Gate::Playing);
    }

    #[tokio::test]
    async fn clauses_play_in_idx_order_even_when_they_arrive_out_of_order() {
        let mut q = queue();
        let mut p = Playout::default();
        q.speak(5, 1, "ok".into());
        q.speak(5, 0, "slow start".into());
        q.speak_done(5);
        q.play(5);
        tokio::time::sleep(Duration::from_millis(20)).await;
        q.pump(&mut p);
        assert!(p.next_frame().is_none(), "idx 1 waits for idx 0");
        settle(&mut q).await;
        q.pump(&mut p);
        let mut expected = vec![10; 10];
        expected.extend([2, 2]);
        assert_eq!(drain(&mut p), expected);
        assert!(q.is_done(5, &p));
    }

    #[tokio::test]
    async fn a_missing_idx_is_skipped_once_the_reply_is_done() {
        let mut q = queue();
        let mut p = Playout::default();
        q.speak(6, 0, "a".into());
        q.speak(6, 2, "ccc".into());
        q.speak_done(6);
        q.play(6);
        settle(&mut q).await;
        q.pump(&mut p);
        assert_eq!(drain(&mut p), vec![1, 3, 3, 3]);
        assert_eq!(q.take_finished(&mut p), vec![6]);
    }

    #[tokio::test]
    async fn replies_play_in_play_order_and_a_failed_clause_is_skipped() {
        let mut q = queue();
        let mut p = Playout::default();
        q.speak(1, 0, "aaa".into());
        q.speak(2, 0, "b".into());
        q.speak(2, 1, "fail".into());
        q.speak(2, 2, "cc".into());
        q.speak_done(2);
        q.play(2);
        q.play(1);
        settle(&mut q).await;
        q.pump(&mut p);
        assert!(!q.is_done(2, &p));
        assert_eq!(drain(&mut p), vec![1, 2, 2, 3, 3, 3]);
        assert!(q.is_done(2, &p));
        assert!(!q.is_done(1, &p));
        q.speak(1, 1, "dddd".into());
        q.speak_done(1);
        settle(&mut q).await;
        q.pump(&mut p);
        assert_eq!(drain(&mut p), vec![4, 4, 4, 4]);
        assert!(q.is_done(1, &p));
    }

    #[tokio::test]
    async fn a_render_gap_mid_reply_is_not_reported_played() {
        let mut q = queue();
        let mut p = Playout::default();
        q.speak(3, 0, "abc".into());
        q.speak(3, 1, "slow tail".into());
        q.speak_done(3);
        q.play(3);
        until_pieces(&mut q, 3, 1).await;
        q.pump(&mut p);
        assert_eq!(drain(&mut p), vec![3, 3, 3]);
        assert!(q.take_finished(&mut p).is_empty(), "clause 1 is still rendering");
        settle(&mut q).await;
        q.pump(&mut p);
        assert_eq!(drain(&mut p), vec![9; 9]);
        assert_eq!(q.take_finished(&mut p), vec![3]);
        assert!(q.take_finished(&mut p).is_empty());
    }

    #[tokio::test]
    async fn a_flush_in_a_render_gap_cuts_the_reply_with_what_was_heard() {
        let mut q = queue();
        let mut p = Playout::default();
        q.speak(3, 0, "abc".into());
        q.speak(3, 1, "slow tail".into());
        q.speak_done(3);
        q.play(3);
        until_pieces(&mut q, 3, 1).await;
        q.pump(&mut p);
        assert_eq!(drain(&mut p), vec![3, 3, 3]);
        assert_eq!(q.flush(&mut p), vec![(3, 3)]);
        assert_eq!(q.gate(3), Gate::Dropped);
        q.speak(3, 2, "more".into());
        settle(&mut q).await;
        q.pump(&mut p);
        assert!(p.next_frame().is_none());
        assert!(q.take_finished(&mut p).is_empty());
    }

    #[tokio::test]
    async fn a_flush_mid_clip_reports_the_partial_and_spares_held_replies() {
        let mut q = queue();
        let mut p = Playout::default();
        q.speak(1, 0, "abcd".into());
        q.speak(2, 0, "held".into());
        q.speak_done(1);
        q.play(1);
        settle(&mut q).await;
        q.pump(&mut p);
        p.next_frame().unwrap();
        p.next_frame().unwrap();
        assert_eq!(q.flush(&mut p), vec![(1, 2)]);
        assert_eq!(q.gate(2), Gate::Held);
        assert!(p.next_frame().is_none());
    }

    #[tokio::test]
    async fn dropping_a_playing_reply_stops_its_audio() {
        let mut q = queue();
        let mut p = Playout::default();
        q.speak(4, 0, "abcdef".into());
        q.speak_done(4);
        q.play(4);
        settle(&mut q).await;
        q.pump(&mut p);
        p.next_frame().unwrap();
        q.drop_reply(4);
        q.pump(&mut p);
        assert!(p.next_frame().is_none());
        assert!(q.take_finished(&mut p).is_empty());
    }

    #[tokio::test]
    async fn a_flush_after_a_reply_played_out_reports_it_played_not_cut() {
        let mut q = queue();
        let mut p = Playout::default();
        q.speak(8, 0, "ab".into());
        q.speak_done(8);
        q.play(8);
        settle(&mut q).await;
        q.pump(&mut p);
        drain(&mut p);
        assert!(q.flush(&mut p).is_empty());
        assert_eq!(q.take_finished(&mut p), vec![8]);
    }

    #[tokio::test]
    async fn a_reply_with_nothing_to_say_finishes_once_played() {
        let mut q = queue();
        let mut p = Playout::default();
        q.play(9);
        q.speak_done(9);
        q.pump(&mut p);
        assert_eq!(q.take_finished(&mut p), vec![9]);
        assert!(q.is_idle());
    }

    #[tokio::test]
    async fn one_piece_renders_at_a_time() {
        let fake = Arc::new(FakeBackend::default());
        let backend: Arc<dyn SpeechBackend> = Arc::new(fake.clone());
        let mut q = SpeechQueue::new(Speaker::new(backend.clone(), ""), backend);
        let mut p = Playout::default();
        for idx in 0..4 {
            q.speak(1, idx, "x".into());
            q.speak(2, idx, "y".into());
        }
        q.speak_done(1);
        q.speak_done(2);
        q.play(1);
        q.play(2);
        settle(&mut q).await;
        q.pump(&mut p);
        assert_eq!(drain(&mut p).len(), 8);
        assert!(!fake.overlapped.load(Ordering::SeqCst), "two renders overlapped");
    }

    /// Records each chunk and speaks it as one frame per byte.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl Renderer for Recorder {
        fn render(&self, text: &str, _voice: &str) -> anyhow::Result<Vec<i16>> {
            self.0.lock().unwrap().push(text.into());
            Ok(vec![7; text.len() * FRAME])
        }
    }

    #[tokio::test]
    async fn a_chunked_reply_plays_whole_and_its_heard_chars_add_up() {
        let recorder = Arc::new(Recorder::default());
        let backend: Arc<dyn SpeechBackend> = Arc::new(ChunkedBackend::new("kokoro", "Kokoro", recorder.clone(), Vec::new()));
        let mut q = SpeechQueue::new(Speaker::new(backend.clone(), ""), backend);
        let mut p = Playout::default();
        let clauses = ["Sure, I can do that.", "It's on Friday.", "Anything else?"];
        for (idx, clause) in (0..).zip(clauses) {
            q.speak(1, idx, clause.into());
        }
        q.speak_done(1);
        q.play(1);
        settle(&mut q).await;
        q.pump(&mut p);
        let said = recorder.0.lock().unwrap().clone();
        assert!((2..=3).contains(&said.len()), "{said:?}");
        assert_eq!(said[0], clauses[0]);
        assert_eq!(said.join(" "), clauses.join(" "));
        let total: u32 = clauses.iter().map(|c| c.len() as u32).sum();
        for _ in 0..25 {
            p.next_frame().unwrap();
        }
        let heard = q.flush(&mut p);
        assert_eq!(heard.len(), 1);
        assert!(heard[0].1 > 0 && heard[0].1 < total, "{heard:?}");
    }

    #[tokio::test]
    async fn text_after_a_stream_ended_early_still_plays_before_the_reply_counts_played() {
        let mut q = queue();
        let mut p = Playout::default();
        q.speak(1, 0, "ab".into());
        q.speak(1, 1, "stop".into());
        q.play(1);
        settle(&mut q).await;
        q.pump(&mut p);
        assert_eq!(drain(&mut p), vec![2, 2, 4, 4, 4, 4]);
        assert!(q.take_finished(&mut p).is_empty());
        q.speak(1, 2, "cde".into());
        q.speak_done(1);
        assert!(q.take_finished(&mut p).is_empty(), "the new text is not played yet");
        settle(&mut q).await;
        q.pump(&mut p);
        assert_eq!(drain(&mut p), vec![3, 3, 3]);
        assert_eq!(q.take_finished(&mut p), vec![1]);
    }

    #[tokio::test]
    async fn playing_replies_are_rendered_before_held_ones() {
        let fake = Arc::new(FakeBackend::default());
        let backend: Arc<dyn SpeechBackend> = Arc::new(fake.clone());
        let mut q = SpeechQueue::new(Speaker::new(backend.clone(), ""), backend);
        let mut p = Playout::default();
        q.play(2);
        q.pump(&mut p);
        q.speak(3, 0, "slow".into());
        tokio::time::sleep(Duration::from_millis(10)).await;
        for idx in 0..3 {
            q.speak(1, idx, format!("held {idx}"));
            q.speak(2, idx, format!("live {idx}"));
        }
        settle(&mut q).await;
        let said = fake.said.lock().unwrap().clone();
        assert_eq!(said, ["slow", "live 0", "live 1", "live 2", "held 0", "held 1", "held 2"]);
    }

    /// Two seconds of audio per chunk, recording each.
    #[derive(Default)]
    struct Long(Mutex<Vec<String>>);

    impl Renderer for Long {
        fn render(&self, text: &str, _voice: &str) -> anyhow::Result<Vec<i16>> {
            self.0.lock().unwrap().push(text.into());
            Ok(vec![1; RATE as usize * 2])
        }
    }

    #[tokio::test]
    async fn a_held_reply_with_its_own_audio_ahead_waits_for_whole_sentences() {
        let long = Arc::new(Long::default());
        let backend: Arc<dyn SpeechBackend> = Arc::new(ChunkedBackend::new("kokoro", "Kokoro", long.clone(), Vec::new()));
        let mut q = SpeechQueue::new(Speaker::new(backend.clone(), ""), backend);
        q.speak(1, 0, "Sure, I can do that.".into());
        settle(&mut q).await;
        q.speak(1, 1, "The run moves to Friday,".into());
        settle(&mut q).await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(long.0.lock().unwrap().len(), 1, "a clause waits while two seconds are ahead");
        q.speak(1, 2, "early in the morning.".into());
        settle(&mut q).await;
        assert_eq!(*long.0.lock().unwrap(), ["Sure, I can do that.", "The run moves to Friday, early in the morning."]);
    }

    struct Broken;

    impl SpeechBackend for Broken {
        fn id(&self) -> &'static str {
            "broken"
        }

        fn label(&self) -> &'static str {
            "Broken"
        }

        fn input(&self) -> TextInput {
            TextInput::Incremental
        }

        fn voices(&self) -> Vec<VoiceInfo> {
            Vec::new()
        }

        fn open(&self, _voice: &str) -> anyhow::Result<Box<dyn SpeechStream>> {
            anyhow::bail!("down")
        }
    }

    #[tokio::test]
    async fn a_backend_that_will_not_open_leaves_the_reply_to_the_fallback() {
        let fake: Arc<dyn SpeechBackend> = Arc::new(Arc::new(FakeBackend::default()));
        let mut q = SpeechQueue::new(Speaker::new(Arc::new(Broken), "v"), fake);
        let mut p = Playout::default();
        q.speak(1, 0, "hi".into());
        q.speak_done(1);
        q.play(1);
        settle(&mut q).await;
        q.pump(&mut p);
        assert_eq!(drain(&mut p), vec![2, 2]);
        assert_eq!(q.take_finished(&mut p), vec![1]);
    }
}
