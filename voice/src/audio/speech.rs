use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
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
}

enum Out {
    Piece { reply: u64, clip: Clip },
    Ended { reply: u64 },
}

/// What the worker reads of the queue's state.
#[derive(Default)]
struct Shared {
    /// Samples queued to play, as of the last pump.
    queued: AtomicU64,
    sent: AtomicU64,
    received: AtomicU64,
    cmds_sent: AtomicU64,
    /// Commands applied when the worker last found nothing to do.
    quiet_at: AtomicU64,
}

impl Shared {
    fn ahead(&self) -> Duration {
        let in_flight = self.sent.load(Ordering::SeqCst).saturating_sub(self.received.load(Ordering::SeqCst));
        let samples = self.queued.load(Ordering::SeqCst) + in_flight;
        Duration::from_micros(samples * 1_000_000 / u64::from(RATE))
    }
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
    ended: bool,
    ready: VecDeque<Clip>,
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
        }
    }

    fn send(&self, cmd: Cmd) {
        self.shared.cmds_sent.fetch_add(1, Ordering::SeqCst);
        self.cmds.send(cmd).expect("the TTS thread outlives the queue");
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
        state.opened |= !texts.is_empty();
        for text in texts {
            self.send(Cmd::Push { reply, text });
        }
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
        state.opened |= !texts.is_empty();
        let opened = state.opened;
        for text in texts {
            self.send(Cmd::Push { reply, text });
        }
        if opened {
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
        let ready: usize = self
            .playing
            .iter()
            .filter_map(|r| self.replies.get(r))
            .flat_map(|s| s.ready.iter().map(|c| c.pcm.len()))
            .sum();
        self.shared.queued.store((playout.queued_samples() + ready) as u64, Ordering::SeqCst);
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
                    self.shared.received.fetch_add(clip.pcm.len() as u64, Ordering::SeqCst);
                    if let Some(state) = self.replies.get_mut(&reply).filter(|s| s.gate != Gate::Dropped) {
                        state.ready.push_back(clip);
                    }
                }
                Out::Ended { reply } => {
                    if let Some(state) = self.replies.get_mut(&reply) {
                        state.ended = true;
                    }
                }
            }
        }
    }
}

const POLL: Duration = Duration::from_millis(10);

struct Active {
    reply: u64,
    stream: Box<dyn SpeechStream>,
    on_fallback: bool,
    /// The text pushed, kept until the reply is on the fallback.
    pushed: Vec<String>,
    /// Characters of `pushed` already covered by audio.
    covered: u32,
    finished: bool,
}

struct Worker {
    speaker: Speaker,
    fallback: Arc<dyn SpeechBackend>,
    streams: Vec<Active>,
    dropped: Arc<Mutex<HashSet<u64>>>,
    shared: Arc<Shared>,
    applied: u64,
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
                if !active.on_fallback {
                    active.pushed.push(text.clone());
                }
                if let Err(e) = active.stream.push(&text) {
                    self.fail(at, &e);
                }
            }
            Cmd::Finish { reply } => {
                let Some(at) = self.streams.iter().position(|a| a.reply == reply) else {
                    let _ = self.tx.send(Out::Ended { reply });
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
        }
    }

    fn open(&self, reply: u64) -> Option<Active> {
        let on_fallback = self.speaker.backend.id() == self.fallback.id();
        let opened = self.speaker.backend.open(&self.speaker.voice).map(|s| (s, on_fallback)).or_else(|e| {
            eprintln!("voice: opening {} for reply {reply} failed: {e:#}", self.speaker.backend.id());
            self.fallback.open("").map(|s| (s, true))
        });
        match opened {
            Ok((stream, on_fallback)) => {
                Some(Active { reply, stream, on_fallback, pushed: Vec::new(), covered: 0, finished: false })
            }
            Err(e) => {
                eprintln!("voice: reply {reply} cannot be spoken: {e:#}");
                None
            }
        }
    }

    /// Hands the text not yet covered by audio to the fallback, which speaks the rest of the reply.
    fn fail(&mut self, at: usize, err: &anyhow::Error) {
        let active = &mut self.streams[at];
        eprintln!("voice: speaking reply {} failed: {err:#}", active.reply);
        if active.on_fallback {
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
                active.on_fallback = true;
                active.pushed.clear();
                active.covered = 0;
            }
            Err(e) => {
                eprintln!("voice: reply {} cannot go on: {e:#}", active.reply);
                let reply = self.streams.remove(at).reply;
                let _ = self.tx.send(Out::Ended { reply });
            }
        }
    }

    /// Takes one piece from the first stream in reply order that has one; false when none moved.
    fn step(&mut self) -> bool {
        let ahead = self.shared.ahead();
        for at in 0..self.streams.len() {
            let reply = self.streams[at].reply;
            if self.is_dropped(reply) {
                self.streams.remove(at).stream.cancel();
                return true;
            }
            match self.streams[at].stream.next(ahead) {
                Ok(Next::Pending) => {}
                Ok(Next::Audio(audio)) => {
                    let active = &mut self.streams[at];
                    if !active.on_fallback {
                        active.covered += audio.chars;
                    }
                    self.shared.sent.fetch_add(audio.pcm.len() as u64, Ordering::SeqCst);
                    let clip = Clip { reply: Some(reply), chars: audio.chars, pcm: audio.pcm };
                    let _ = self.tx.send(Out::Piece { reply, clip });
                    return true;
                }
                Ok(Next::Done) => {
                    self.streams.remove(at);
                    let _ = self.tx.send(Out::Ended { reply });
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
    /// starting "slow" takes 50 ms and "fail" errors. Tracks whether two renders ever overlap.
    #[derive(Default)]
    struct FakeBackend {
        busy: AtomicBool,
        overlapped: AtomicBool,
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
