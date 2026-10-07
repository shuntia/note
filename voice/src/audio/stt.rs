use std::sync::Arc;
use std::time::{Duration, Instant};

use super::engines::SpeechToText;

const RATE: usize = 16_000;
/// A partial re-decodes once this much new audio is in.
const MIN_NEW: usize = RATE * 32 / 100;
/// The least a partial waits after the last decode, and the multiple of that decode's cost it waits.
const MIN_GAP: Duration = Duration::from_millis(200);
const COST_FACTOR: u32 = 2;
/// A partial decodes at most this much of the turn's end.
const PARTIAL_WINDOW: usize = RATE * 12;
/// The most of a turn kept; older audio is dropped.
const MAX_TURN: usize = RATE * 40;
/// Quiet audio kept ahead of the first speech, so a soft onset is not cut.
const PRE_ROLL: usize = RATE / 2;
/// RMS below which a chunk counts as quiet while no speech has come yet.
const QUIET_RMS: f32 = 0.004;

/// Decodes one whole utterance.
pub trait Decoder: Send + Sync {
    fn decode(&self, samples_16k: &[f32]) -> String;
}

/// Turn-at-a-time recognition over a whole-utterance model: the audio of the turn accumulates and
/// is re-decoded for partials at a cadence that tracks the decoder's own cost.
pub struct OfflineStt<D: ?Sized> {
    decoder: Arc<D>,
    samples: Vec<f32>,
    heard_speech: bool,
    text: String,
    /// Samples the text covers, and whether the decode saw the whole turn.
    decoded: Option<(usize, bool)>,
    last: Option<(Instant, Duration)>,
}

impl<D: Decoder + ?Sized> OfflineStt<D> {
    pub fn new(decoder: Arc<D>) -> Self {
        OfflineStt { decoder, samples: Vec::new(), heard_speech: false, text: String::new(), decoded: None, last: None }
    }

    fn clear(&mut self) {
        self.samples.clear();
        self.heard_speech = false;
        self.text.clear();
        self.decoded = None;
        self.last = None;
    }

    fn due(&self, now: Instant) -> bool {
        let fresh = self.samples.len() - self.decoded.map_or(0, |(n, _)| n);
        if fresh < MIN_NEW {
            return false;
        }
        self.last.is_none_or(|(at, cost)| now.duration_since(at) >= MIN_GAP.max(cost * COST_FACTOR))
    }

    fn decode(&mut self, whole: bool) {
        let from = if whole { 0 } else { self.samples.len().saturating_sub(PARTIAL_WINDOW) };
        let start = Instant::now();
        self.decoder.decode(&self.samples[from..]).trim().clone_into(&mut self.text);
        self.last = Some((start, start.elapsed()));
        self.decoded = Some((self.samples.len(), from == 0));
    }
}

impl<D: Decoder + ?Sized> SpeechToText for OfflineStt<D> {
    fn accept(&mut self, samples_16k: &[f32]) {
        self.samples.extend_from_slice(samples_16k);
        if !self.heard_speech {
            let rms = (samples_16k.iter().map(|s| s * s).sum::<f32>() / samples_16k.len().max(1) as f32).sqrt();
            if rms >= QUIET_RMS {
                self.heard_speech = true;
            } else {
                let excess = self.samples.len().saturating_sub(PRE_ROLL);
                self.samples.drain(..excess);
            }
        }
        let excess = self.samples.len().saturating_sub(MAX_TURN);
        if excess > 0 {
            self.samples.drain(..excess);
            self.decoded = None;
        }
    }

    fn partial(&mut self) -> String {
        if self.heard_speech && self.due(Instant::now()) {
            self.decode(false);
        }
        self.text.clone()
    }

    fn finish(&mut self) -> String {
        if self.heard_speech && !matches!(self.decoded, Some((n, true)) if n == self.samples.len()) {
            self.decode(true);
        }
        let text = std::mem::take(&mut self.text);
        self.clear();
        text
    }

    fn reset(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::*;

    /// Transcribes audio as its length in tenths of a second; a `slow` decode sleeps `slow` per call.
    #[derive(Default)]
    struct Counting {
        calls: AtomicUsize,
        lengths: Mutex<Vec<usize>>,
        slow: Duration,
    }

    impl Decoder for Counting {
        fn decode(&self, samples_16k: &[f32]) -> String {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.lengths.lock().unwrap().push(samples_16k.len());
            std::thread::sleep(self.slow);
            format!("{} tenths", samples_16k.len() / (RATE / 10))
        }
    }

    const CHUNK: usize = RATE * 160 / 1000;

    fn speech(n: usize) -> Vec<f32> {
        (0..n).map(|i| if i % 2 == 0 { 0.1 } else { -0.1 }).collect()
    }

    #[test]
    fn partials_wait_for_new_audio_and_the_finish_decodes_the_whole_turn() {
        let decoder = Arc::new(Counting::default());
        let mut stt = OfflineStt::new(decoder.clone());
        stt.accept(&speech(CHUNK));
        assert_eq!(stt.partial(), "", "one chunk is too little to decode");
        stt.accept(&speech(CHUNK));
        assert_eq!(stt.partial(), "3 tenths");
        assert_eq!(stt.partial(), "3 tenths", "nothing new, nothing decoded");
        assert_eq!(decoder.calls.load(Ordering::SeqCst), 1);
        stt.accept(&speech(CHUNK));
        std::thread::sleep(MIN_GAP);
        assert_eq!(stt.partial(), "3 tenths", "one chunk of new audio waits");
        stt.accept(&speech(CHUNK));
        assert_eq!(stt.partial(), "6 tenths");
        assert_eq!(stt.finish(), "6 tenths");
        assert_eq!(decoder.calls.load(Ordering::SeqCst), 2, "a finish right after a whole decode reuses it");
        stt.accept(&speech(CHUNK));
        assert_eq!(stt.finish(), "1 tenths");
        assert_eq!(stt.partial(), "", "the turn is fresh");
    }

    #[test]
    fn a_slow_decode_holds_the_next_partial_back() {
        let decoder = Arc::new(Counting { slow: Duration::from_millis(150), ..Counting::default() });
        let mut stt = OfflineStt::new(decoder.clone());
        for _ in 0..2 {
            stt.accept(&speech(CHUNK));
        }
        stt.partial();
        for _ in 0..2 {
            stt.accept(&speech(CHUNK));
        }
        stt.partial();
        assert_eq!(decoder.calls.load(Ordering::SeqCst), 1, "within twice the last decode's cost");
        std::thread::sleep(Duration::from_millis(320));
        stt.partial();
        assert_eq!(decoder.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn quiet_before_speech_is_trimmed_and_a_silent_turn_is_not_decoded() {
        let decoder = Arc::new(Counting::default());
        let mut stt = OfflineStt::new(decoder.clone());
        for _ in 0..20 {
            stt.accept(&vec![0.0; CHUNK]);
        }
        assert_eq!(stt.partial(), "");
        assert_eq!(stt.finish(), "");
        assert_eq!(decoder.calls.load(Ordering::SeqCst), 0);
        for _ in 0..20 {
            stt.accept(&vec![0.0; CHUNK]);
        }
        stt.accept(&speech(CHUNK));
        stt.accept(&speech(CHUNK));
        stt.finish();
        assert_eq!(decoder.lengths.lock().unwrap()[0], PRE_ROLL + 2 * CHUNK);
    }

    #[test]
    fn partials_decode_a_window_and_the_turn_is_capped() {
        let decoder = Arc::new(Counting::default());
        let mut stt = OfflineStt::new(decoder.clone());
        stt.accept(&speech(PARTIAL_WINDOW + RATE));
        stt.partial();
        assert_eq!(decoder.lengths.lock().unwrap()[0], PARTIAL_WINDOW);
        stt.accept(&speech(MAX_TURN));
        stt.finish();
        assert_eq!(decoder.lengths.lock().unwrap()[1], MAX_TURN);
    }

    #[test]
    fn a_reset_forgets_the_turn_without_decoding() {
        let decoder = Arc::new(Counting::default());
        let mut stt = OfflineStt::new(decoder.clone());
        stt.accept(&speech(CHUNK * 3));
        stt.reset();
        assert_eq!(stt.finish(), "");
        assert_eq!(decoder.calls.load(Ordering::SeqCst), 0);
    }
}
