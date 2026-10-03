use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use super::engines::TextToSpeech;
use super::playout::{Clip, Playout};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Held,
    Playing,
    Dropped,
}

struct Job {
    reply: u64,
    idx: u32,
    text: String,
}

struct Synthesized {
    reply: u64,
    idx: u32,
    clip: Option<Clip>,
}

struct Reply {
    gate: Gate,
    /// Any clause arrived while not dropped.
    spoken: bool,
    done: bool,
    unplayed: BTreeSet<u32>,
    ready: BTreeMap<u32, Option<Clip>>,
}

impl Default for Reply {
    fn default() -> Self {
        Self { gate: Gate::Held, spoken: false, done: false, unplayed: BTreeSet::new(), ready: BTreeMap::new() }
    }
}

pub struct SpeechQueue {
    jobs: std::sync::mpsc::Sender<Job>,
    dropped: Arc<Mutex<HashSet<u64>>>,
    to_remove: Vec<u64>,
    replies: HashMap<u64, Reply>,
    play_order: VecDeque<u64>,
    playing: Vec<u64>,
    rx: mpsc::UnboundedReceiver<Synthesized>,
    outstanding: usize,
}

impl SpeechQueue {
    /// Starts one synthesis thread that works through clauses in the order they arrive.
    pub fn new(tts: Arc<dyn TextToSpeech>, voice: String) -> Self {
        let (jobs, queued) = std::sync::mpsc::channel::<Job>();
        let (tx, rx) = mpsc::unbounded_channel();
        let dropped = Arc::new(Mutex::new(HashSet::new()));
        let skip = dropped.clone();
        std::thread::Builder::new()
            .name("note-voice-tts".into())
            .spawn(move || {
                for Job { reply, idx, text } in queued {
                    let clip = if skip.lock().expect("dropped lock").contains(&reply) {
                        None
                    } else {
                        match tts.synthesize(&text, &voice) {
                            Ok(pcm) => Some(Clip { reply: Some(reply), chars: text.chars().count() as u32, pcm }),
                            Err(e) => {
                                eprintln!("voice: synthesizing clause {idx} of reply {reply} failed: {e:#}");
                                None
                            }
                        }
                    };
                    if tx.send(Synthesized { reply, idx, clip }).is_err() {
                        break;
                    }
                }
            })
            .expect("spawning the TTS thread");
        Self {
            jobs,
            dropped,
            to_remove: Vec::new(),
            replies: HashMap::new(),
            play_order: VecDeque::new(),
            playing: Vec::new(),
            rx,
            outstanding: 0,
        }
    }

    pub fn speak(&mut self, reply: u64, idx: u32, text: String) {
        let state = self.replies.entry(reply).or_default();
        if state.gate == Gate::Dropped {
            return;
        }
        state.spoken = true;
        state.unplayed.insert(idx);
        self.outstanding += 1;
        self.jobs.send(Job { reply, idx, text }).expect("the TTS thread outlives the queue");
    }

    pub fn speak_done(&mut self, reply: u64) {
        self.replies.entry(reply).or_default().done = true;
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
        state.gate = Gate::Dropped;
        state.unplayed.clear();
        state.ready.clear();
        self.play_order.retain(|&r| r != reply);
        self.playing.retain(|&r| r != reply);
    }

    /// Moves synthesized clauses of playing replies, in idx order, into the playout.
    pub fn pump(&mut self, playout: &mut Playout) {
        self.receive();
        for reply in self.to_remove.drain(..) {
            playout.remove(reply);
        }
        while let Some(&reply) = self.play_order.front() {
            let state = self.replies.get_mut(&reply).expect("a playing reply has state");
            while let Some(&idx) = state.unplayed.first() {
                let Some(clip) = state.ready.remove(&idx) else { break };
                state.unplayed.remove(&idx);
                if let Some(clip) = clip {
                    playout.push(clip);
                }
            }
            if !(state.done && state.unplayed.is_empty()) {
                break;
            }
            self.play_order.pop_front();
        }
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
        self.replies.get(&reply).is_some_and(|s| {
            s.gate == Gate::Playing && s.done && s.unplayed.is_empty() && !playout.holds(reply)
        })
    }

    /// No reply told to play is still waiting to finish.
    pub fn is_idle(&self) -> bool {
        self.playing.is_empty()
    }

    /// Replies told to play whose every clause has been synthesized and played out, each reported once.
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
        while let Ok(Synthesized { reply, idx, clip }) = self.rx.try_recv() {
            self.outstanding -= 1;
            if let Some(state) = self.replies.get_mut(&reply).filter(|s| s.gate != Gate::Dropped) {
                state.ready.insert(idx, clip);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::engines::VoiceInfo;
    use super::super::playout::FRAME;
    use super::*;

    /// `text.len()` frames, each sample equal to `text.len()`; text starting "slow" takes 50 ms and "fail" errors.
    struct FakeTts;

    impl TextToSpeech for FakeTts {
        fn synthesize(&self, text: &str, _voice: &str) -> anyhow::Result<Vec<i16>> {
            if text.starts_with("slow") {
                std::thread::sleep(Duration::from_millis(50));
            }
            if text == "fail" {
                anyhow::bail!("cannot say {text}");
            }
            Ok(vec![text.len() as i16; text.len() * FRAME])
        }

        fn synthesize_native(&self, text: &str, voice: &str) -> anyhow::Result<Vec<i16>> {
            self.synthesize(text, voice)
        }

        fn voices(&self) -> Vec<VoiceInfo> {
            Vec::new()
        }
    }

    async fn settle(q: &mut SpeechQueue) {
        while q.outstanding > 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
            q.receive();
        }
    }

    fn drain(p: &mut Playout) -> Vec<i16> {
        std::iter::from_fn(|| p.next_frame()).map(|f| f[0]).collect()
    }

    #[tokio::test]
    async fn a_held_reply_plays_only_after_play_and_a_dropped_one_never() {
        let mut q = SpeechQueue::new(Arc::new(FakeTts), String::new());
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
    async fn clauses_play_in_idx_order_even_when_synthesized_out_of_order() {
        let mut q = SpeechQueue::new(Arc::new(FakeTts), String::new());
        let mut p = Playout::default();
        q.speak(5, 1, "ok".into());
        q.speak(5, 0, "slow start".into());
        q.speak_done(5);
        q.play(5);
        while q.outstanding > 1 {
            tokio::time::sleep(Duration::from_millis(1)).await;
            q.receive();
        }
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
    async fn replies_play_in_play_order_and_a_failed_clause_is_skipped() {
        let mut q = SpeechQueue::new(Arc::new(FakeTts), String::new());
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

    async fn until_outstanding(q: &mut SpeechQueue, n: usize) {
        while q.outstanding > n {
            tokio::time::sleep(Duration::from_millis(1)).await;
            q.receive();
        }
    }

    #[tokio::test]
    async fn a_synthesis_gap_mid_reply_is_not_reported_played() {
        let mut q = SpeechQueue::new(Arc::new(FakeTts), String::new());
        let mut p = Playout::default();
        q.speak(3, 0, "abc".into());
        q.speak(3, 1, "slow tail".into());
        q.speak_done(3);
        q.play(3);
        until_outstanding(&mut q, 1).await;
        q.pump(&mut p);
        assert_eq!(drain(&mut p), vec![3, 3, 3]);
        assert!(q.take_finished(&mut p).is_empty(), "clause 1 is still synthesizing");
        settle(&mut q).await;
        q.pump(&mut p);
        assert_eq!(drain(&mut p), vec![9; 9]);
        assert_eq!(q.take_finished(&mut p), vec![3]);
        assert!(q.take_finished(&mut p).is_empty());
    }

    #[tokio::test]
    async fn a_flush_in_a_synthesis_gap_cuts_the_reply_with_what_was_heard() {
        let mut q = SpeechQueue::new(Arc::new(FakeTts), String::new());
        let mut p = Playout::default();
        q.speak(3, 0, "abc".into());
        q.speak(3, 1, "slow tail".into());
        q.speak_done(3);
        q.play(3);
        until_outstanding(&mut q, 1).await;
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
        let mut q = SpeechQueue::new(Arc::new(FakeTts), String::new());
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
        let mut q = SpeechQueue::new(Arc::new(FakeTts), String::new());
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
        let mut q = SpeechQueue::new(Arc::new(FakeTts), String::new());
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
    async fn one_clause_synthesizes_at_a_time() {
        use std::sync::atomic::{AtomicBool, Ordering};
        #[derive(Default)]
        struct Exclusive {
            busy: AtomicBool,
            overlapped: AtomicBool,
        }
        impl TextToSpeech for Exclusive {
            fn synthesize(&self, _text: &str, _voice: &str) -> anyhow::Result<Vec<i16>> {
                if self.busy.swap(true, Ordering::SeqCst) {
                    self.overlapped.store(true, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_millis(5));
                self.busy.store(false, Ordering::SeqCst);
                Ok(vec![1; FRAME])
            }
            fn synthesize_native(&self, text: &str, voice: &str) -> anyhow::Result<Vec<i16>> {
                self.synthesize(text, voice)
            }
            fn voices(&self) -> Vec<VoiceInfo> {
                Vec::new()
            }
        }
        let tts = Arc::new(Exclusive::default());
        let mut q = SpeechQueue::new(tts.clone(), String::new());
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
        assert!(!tts.overlapped.load(Ordering::SeqCst), "two syntheses overlapped");
    }
}
