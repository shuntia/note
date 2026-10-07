use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::anyhow;

use super::engines::VoiceInfo;

pub const RATE: u32 = 48_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextInput {
    Incremental,
    Chunks,
}

pub trait SpeechBackend: Send + Sync {
    fn id(&self) -> &str;
    /// The name its voices are grouped under.
    fn label(&self) -> &str;
    /// Slow to start speaking.
    fn slow(&self) -> bool {
        false
    }
    fn input(&self) -> TextInput;
    fn voices(&self) -> Vec<VoiceInfo>;
    /// A stream for one reply in `voice` (empty = the backend's default).
    fn open(&self, voice: &str) -> anyhow::Result<Box<dyn SpeechStream>>;
    /// All of `text` as one piece of audio.
    fn render(&self, text: &str, voice: &str) -> anyhow::Result<Vec<i16>> {
        render_through(self.open(voice)?, text, RENDER_LIMIT, &|| false)?.ok_or_else(|| anyhow!("rendering {text:?} stopped"))
    }
}

pub enum Next {
    Audio(Audio),
    /// Waiting for more text, or for audio still on its way.
    Pending,
    Done,
}

pub trait SpeechStream: Send {
    fn push(&mut self, text: &str) -> anyhow::Result<()>;
    fn finish(&mut self) -> anyhow::Result<()>;
    /// The next piece of audio, rendered now if one is due; never waits for text. `ahead` is how much
    /// audio is queued to play before it. An error loses only the piece it was rendering.
    fn next(&mut self, ahead: Duration) -> anyhow::Result<Next>;
    fn cancel(&mut self);
    /// After an error: whether the stream can still produce the pieces that follow.
    fn alive(&self) -> bool {
        true
    }
}

/// 48 kHz mono s16 audio and how many characters of the pushed text it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Audio {
    pub pcm: Vec<i16>,
    pub chars: u32,
}

const RENDER_LIMIT: Duration = Duration::from_secs(30);

/// All of `text` through a stream of `backend`, within `limit`; `None` once `abort` turns true.
pub fn render_within(
    backend: &dyn SpeechBackend,
    text: &str,
    voice: &str,
    limit: Duration,
    abort: &dyn Fn() -> bool,
) -> anyhow::Result<Option<Vec<i16>>> {
    render_through(backend.open(voice)?, text, limit, abort)
}

fn render_through(
    mut stream: Box<dyn SpeechStream>,
    text: &str,
    limit: Duration,
    abort: &dyn Fn() -> bool,
) -> anyhow::Result<Option<Vec<i16>>> {
    stream.push(text)?;
    stream.finish()?;
    let deadline = Instant::now() + limit;
    let mut pcm = Vec::new();
    loop {
        if abort() {
            stream.cancel();
            return Ok(None);
        }
        match stream.next(Duration::MAX)? {
            Next::Audio(audio) => pcm.extend(audio.pcm),
            Next::Done => return Ok(Some(pcm)),
            Next::Pending if Instant::now() >= deadline => {
                stream.cancel();
                return Err(anyhow!("rendering {text:?} took over {limit:?}"));
            }
            Next::Pending => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

/// A voice on the backend that speaks it.
#[derive(Clone)]
pub struct Speaker {
    pub backend: Arc<dyn SpeechBackend>,
    pub voice: String,
}

impl Speaker {
    pub fn new(backend: Arc<dyn SpeechBackend>, voice: &str) -> Self {
        Speaker { backend, voice: voice.into() }
    }
}

/// `<backend>:<voice>` on a live backend that offers it for `language`, or a bare id the language's
/// base voice offers.
pub fn find_speaker(
    id: &str,
    language: &str,
    base: &Arc<dyn SpeechBackend>,
    sidecars: &[Arc<dyn SpeechBackend>],
) -> Option<Speaker> {
    let (backend, voice) = match id.split_once(':') {
        Some((backend, voice)) => (sidecars.iter().find(|b| b.id() == backend)?, voice),
        None => (base, id),
    };
    backend
        .voices()
        .iter()
        .any(|v| v.id == voice && (v.languages.is_empty() || v.languages.iter().any(|l| l == language)))
        .then(|| Speaker::new(backend.clone(), voice))
}

/// `find_speaker`, else the base voice's default.
pub fn speaker(id: &str, language: &str, base: &Arc<dyn SpeechBackend>, sidecars: &[Arc<dyn SpeechBackend>]) -> Speaker {
    find_speaker(id, language, base, sidecars).unwrap_or_else(|| Speaker::new(base.clone(), ""))
}

/// Stands in for a base voice whose sidecar is down: every stream fails to open.
pub struct Mute {
    id: String,
}

impl Mute {
    pub fn new(id: &str) -> Self {
        Mute { id: id.into() }
    }
}

impl SpeechBackend for Mute {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> &str {
        &self.id
    }

    fn input(&self) -> TextInput {
        TextInput::Chunks
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        Vec::new()
    }

    fn open(&self, _voice: &str) -> anyhow::Result<Box<dyn SpeechStream>> {
        Err(anyhow!("the {} voice is down", self.id))
    }
}

/// Linear interpolation from `rate` to 48 kHz.
pub fn resample(samples: &[i16], rate: u32) -> Vec<i16> {
    if rate == RATE || samples.is_empty() {
        return samples.to_vec();
    }
    if rate == 0 {
        return Vec::new();
    }
    let step = f64::from(rate) / f64::from(RATE);
    let len = (samples.len() as f64 / step).round() as usize;
    (0..len)
        .map(|i| {
            let pos = i as f64 * step;
            let at = (pos.floor() as usize).min(samples.len() - 1);
            let a = f64::from(samples[at]);
            let b = samples.get(at + 1).map_or(a, |&s| f64::from(s));
            (a + (b - a) * pos.fract()).round() as i16
        })
        .collect()
}

pub trait Renderer: Send + Sync {
    /// 48 kHz mono s16 for `text` in `voice` (empty = the default).
    fn render(&self, text: &str, voice: &str) -> anyhow::Result<Vec<i16>>;
    /// `render`, given the chunk spoken before it for continuity where the model can use it.
    fn render_with_context(&self, text: &str, voice: &str, _previous: Option<&str>) -> anyhow::Result<Vec<i16>> {
        self.render(text, voice)
    }
}

/// A backend that renders whole chunks, fed by `ChunkedStream`.
pub struct ChunkedBackend {
    id: String,
    label: String,
    renderer: Arc<dyn Renderer>,
    voices: Vec<VoiceInfo>,
}

impl ChunkedBackend {
    pub fn new(id: &str, label: &str, renderer: Arc<dyn Renderer>, voices: Vec<VoiceInfo>) -> Self {
        ChunkedBackend { id: id.into(), label: label.into(), renderer, voices }
    }
}

impl SpeechBackend for ChunkedBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn input(&self) -> TextInput {
        TextInput::Chunks
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        self.voices.clone()
    }

    fn open(&self, voice: &str) -> anyhow::Result<Box<dyn SpeechStream>> {
        Ok(Box::new(ChunkedStream::new(self.renderer.clone(), voice)))
    }

    fn render(&self, text: &str, voice: &str) -> anyhow::Result<Vec<i16>> {
        self.renderer.render(text, voice)
    }
}

pub struct ChunkedStream {
    renderer: Arc<dyn Renderer>,
    voice: String,
    chunker: Chunker,
    last_push: Instant,
    previous: Option<String>,
}

impl ChunkedStream {
    pub fn new(renderer: Arc<dyn Renderer>, voice: &str) -> Self {
        ChunkedStream { renderer, voice: voice.into(), chunker: Chunker::default(), last_push: Instant::now(), previous: None }
    }
}

impl SpeechStream for ChunkedStream {
    fn push(&mut self, text: &str) -> anyhow::Result<()> {
        self.chunker.push(text);
        self.last_push = Instant::now();
        Ok(())
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        self.chunker.finish();
        Ok(())
    }

    fn next(&mut self, ahead: Duration) -> anyhow::Result<Next> {
        let Some(chunk) = self.chunker.next(ahead, self.last_push.elapsed()) else {
            return Ok(if self.chunker.done() { Next::Done } else { Next::Pending });
        };
        let text = chunk.text.trim();
        if text.is_empty() {
            return Ok(Next::Audio(Audio { pcm: Vec::new(), chars: chunk.chars }));
        }
        let pcm = self.renderer.render_with_context(text, &self.voice, self.previous.as_deref())?;
        self.previous = Some(text.to_owned());
        Ok(Next::Audio(Audio { pcm, chars: chunk.chars }))
    }

    fn cancel(&mut self) {
        self.chunker.cancel();
    }
}

/// Below this much queued audio a waiting clause renders rather than leave a gap.
pub const STARVING: Duration = Duration::from_secs(1);
/// A first chunk too short for its own rule renders once no text has come for this long.
pub const FIRST_STALL: Duration = Duration::from_millis(250);
const FIRST_WORDS: usize = 6;
/// The weight of a chunk: a CJK character weighs two, anything else one.
const MAX_WEIGHT: usize = 300;
const MAX_SENTENCES: usize = 2;
const ABBREVIATIONS: [&str; 11] = ["dr", "mr", "mrs", "ms", "st", "vs", "etc", "a.m", "p.m", "e.g", "i.e"];

/// Japanese and Chinese script, their punctuation and full-width forms: text written without spaces.
pub(crate) fn is_cjk(c: char) -> bool {
    matches!(c, '\u{3000}'..='\u{30FF}' | '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' | '\u{FF00}'..='\u{FFEF}')
}

fn ends_sentence(c: char) -> bool {
    matches!(c, '.' | '?' | '!' | '…' | '。' | '！' | '？' | '‼' | '⁉' | '\n')
}

fn ends_clause(c: char) -> bool {
    matches!(c, ',' | ';' | ':' | '—' | '–' | '、' | '，' | '；' | '：')
}

/// Spoken units: whitespace-separated words, and each CJK character on its own.
fn units(text: &str) -> usize {
    let cjk = text.chars().filter(|&c| is_cjk(c) && c.is_alphanumeric()).count();
    let words = text.split(|c: char| c.is_whitespace() || is_cjk(c)).filter(|w| !w.is_empty()).count();
    cjk + words
}

fn weight(c: char) -> usize {
    if is_cjk(c) {
        2
    } else {
        1
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    /// The pushed characters it covers; the spaces put between pushes are not counted.
    pub chars: u32,
}

#[derive(Debug, Clone, Copy)]
struct Boundary {
    end: usize,
    sentence: bool,
}

/// Decides when gathered text is rendered: a quick first chunk, then whole sentences, with clauses
/// let through when the audio ahead runs low.
#[derive(Debug, Default)]
pub struct Chunker {
    buf: String,
    /// Byte offsets of the spaces put between pushes.
    seps: Vec<usize>,
    /// Byte offsets where each push ended; Note pushes whole clauses.
    ends: Vec<usize>,
    started: bool,
    finished: bool,
    cancelled: bool,
}

impl Chunker {
    pub fn push(&mut self, text: &str) {
        if self.finished || self.cancelled || text.is_empty() {
            return;
        }
        let joins = self.buf.chars().next_back().is_some_and(|c| !c.is_whitespace() && !is_cjk(c))
            && text.chars().next().is_some_and(|c| !c.is_whitespace() && !is_cjk(c));
        if joins {
            self.seps.push(self.buf.len());
            self.buf.push(' ');
        }
        self.buf.push_str(text);
        self.ends.push(self.buf.len());
    }

    pub fn finish(&mut self) {
        self.finished = true;
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
        self.buf.clear();
        self.seps.clear();
        self.ends.clear();
    }

    /// Nothing is left to render, nor will be.
    pub fn done(&self) -> bool {
        self.cancelled || (self.finished && self.buf.is_empty())
    }

    /// The chunk to render now, given `ahead` of audio queued and `idle` since the last push.
    pub fn next(&mut self, ahead: Duration, idle: Duration) -> Option<Chunk> {
        if self.cancelled || self.buf.is_empty() {
            return None;
        }
        if self.buf.trim().is_empty() {
            return self.finished.then(|| self.take(self.buf.len()));
        }
        let bounds = self.boundaries();
        let cut = if self.started { self.later_cut(&bounds, ahead) } else { self.first_cut(&bounds, idle) }?;
        let cut = self.limit(cut, &bounds);
        self.started = true;
        Some(self.take(cut))
    }

    fn first_cut(&self, bounds: &[Boundary], idle: Duration) -> Option<usize> {
        bounds
            .iter()
            .find(|b| units(&self.buf[..b.end]) >= 2)
            .map(|b| b.end)
            .or_else(|| self.word_end(FIRST_WORDS))
            .or_else(|| self.finished.then_some(self.buf.len()))
            .or_else(|| bounds.last().filter(|_| idle >= FIRST_STALL).map(|b| b.end))
    }

    fn later_cut(&self, bounds: &[Boundary], ahead: Duration) -> Option<usize> {
        if self.finished {
            return Some(self.buf.len());
        }
        let sentences: Vec<usize> = bounds.iter().filter(|b| b.sentence).map(|b| b.end).collect();
        sentences
            .get(MAX_SENTENCES - 1)
            .or(sentences.last())
            .copied()
            .or_else(|| bounds.last().filter(|_| ahead < STARVING).map(|b| b.end))
    }

    /// Pulls `cut` back to at most two sentences and `MAX_WEIGHT`, at the latest boundary that fits.
    fn limit(&self, cut: usize, bounds: &[Boundary]) -> usize {
        let within = |b: &&Boundary| b.end < cut;
        let cut = bounds.iter().filter(within).filter(|b| b.sentence).nth(MAX_SENTENCES - 1).map_or(cut, |b| b.end);
        let mut weighed = 0;
        let max = self
            .buf
            .char_indices()
            .find(|&(_, c)| {
                weighed += weight(c);
                weighed > MAX_WEIGHT
            })
            .map_or(self.buf.len(), |(i, _)| i);
        if cut <= max {
            return cut;
        }
        let fits = |b: &&Boundary| b.end <= max;
        bounds
            .iter()
            .rev()
            .filter(fits)
            .find(|b| b.sentence)
            .or_else(|| bounds.iter().rev().find(fits))
            .map(|b| b.end)
            .or_else(|| self.buf[..max].rfind(char::is_whitespace).filter(|&i| i > 0))
            .unwrap_or(max)
    }

    /// The byte end of the `n`th word, once whitespace follows it.
    fn word_end(&self, n: usize) -> Option<usize> {
        let mut words = 0;
        let mut in_word = false;
        for (i, c) in self.buf.char_indices() {
            if c.is_whitespace() {
                if in_word {
                    words += 1;
                    if words == n {
                        return Some(i);
                    }
                }
                in_word = false;
            } else {
                in_word = true;
            }
        }
        None
    }

    /// The '.' at byte `at` closes an abbreviation or an initial, not a sentence.
    fn abbreviates(&self, at: usize) -> bool {
        let word = self.buf[..at]
            .rsplit(char::is_whitespace)
            .next()
            .unwrap_or("")
            .trim_start_matches(['"', '\'', '(', '[', '“', '‘']);
        let mut letters = word.chars();
        match (letters.next(), letters.next()) {
            (Some(c), None) => c.is_ascii_uppercase() && c != 'I',
            _ => ABBREVIATIONS.contains(&word.to_lowercase().as_str()),
        }
    }

    fn boundaries(&self) -> Vec<Boundary> {
        let chars: Vec<(usize, char)> = self.buf.char_indices().collect();
        let mut out: Vec<Boundary> = Vec::new();
        for (k, &(i, c)) in chars.iter().enumerate() {
            let sentence = ends_sentence(c);
            if !(sentence || ends_clause(c)) || (c == '.' && self.abbreviates(i)) {
                continue;
            }
            let mut j = k + 1;
            while chars.get(j).is_some_and(|&(_, c)| matches!(c, '"' | '\'' | '”' | '’' | ')' | ']' | '」' | '』')) {
                j += 1;
            }
            let end = chars.get(j).map_or(self.buf.len(), |&(i, _)| i);
            let closes = is_cjk(c) || c == '\n' || chars.get(j).is_none_or(|&(_, n)| n.is_whitespace() || is_cjk(n));
            if closes || self.ends.contains(&end) {
                out.push(Boundary { end, sentence });
            }
        }
        for &end in &self.ends {
            if !out.iter().any(|b| b.end == end) {
                out.push(Boundary { end, sentence: false });
            }
        }
        out.retain(|b| !self.buf[..b.end].trim().is_empty());
        out.sort_by_key(|b| b.end);
        out
    }

    fn take(&mut self, cut: usize) -> Chunk {
        let seps = self.seps.iter().filter(|&&s| s < cut).count();
        let text: String = self.buf.drain(..cut).collect();
        let chars = (text.chars().count() - seps) as u32;
        self.seps.retain(|&s| s >= cut);
        self.seps.iter_mut().for_each(|s| *s -= cut);
        self.ends.retain(|&e| e > cut);
        self.ends.iter_mut().for_each(|e| *e -= cut);
        Chunk { text, chars }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLENTY: Duration = Duration::from_secs(5);
    const NOW: Duration = Duration::ZERO;

    fn texts(chunks: &[Chunk]) -> Vec<&str> {
        chunks.iter().map(|c| c.text.trim()).collect()
    }

    fn drain(c: &mut Chunker, ahead: Duration) -> Vec<Chunk> {
        std::iter::from_fn(|| c.next(ahead, NOW)).collect()
    }

    #[test]
    fn the_first_chunk_ends_at_the_first_clause_of_two_words() {
        let mut c = Chunker::default();
        c.push("Sure, I moved it to 7.30 tomorrow, as asked");
        let first = c.next(PLENTY, NOW).unwrap();
        assert_eq!(first.text, "Sure, I moved it to 7.30 tomorrow,");
        assert_eq!(c.next(PLENTY, NOW), None, "the rest waits for its sentence to end");
    }

    #[test]
    fn a_first_chunk_with_no_boundary_takes_six_words() {
        let mut c = Chunker::default();
        c.push("Okay");
        assert_eq!(c.next(PLENTY, NOW), None, "one word waits");
        let mut c = Chunker { buf: "so here is what I found in".into(), ..Chunker::default() };
        assert_eq!(c.next(PLENTY, NOW).unwrap().text, "so here is what I found");
    }

    #[test]
    fn a_short_first_clause_renders_once_the_text_stalls() {
        let mut c = Chunker::default();
        c.push("Sure.");
        assert_eq!(c.next(PLENTY, FIRST_STALL / 2), None);
        assert_eq!(c.next(PLENTY, FIRST_STALL).unwrap().text, "Sure.");
    }

    #[test]
    fn later_chunks_are_whole_sentences() {
        let mut c = Chunker::default();
        for clause in ["Sure, I can do that.", "The run moves to Friday,", "early in the morning."] {
            c.push(clause);
        }
        c.push("Anything else?");
        c.push("Or is that all?");
        c.push("Then,");
        c.finish();
        let chunks = drain(&mut c, PLENTY);
        assert_eq!(
            texts(&chunks),
            ["Sure, I can do that.", "The run moves to Friday, early in the morning. Anything else?", "Or is that all? Then,"]
        );
        assert!(c.done());
    }

    #[test]
    fn a_sentence_ending_at_a_push_renders_without_waiting_for_a_space() {
        let mut c = Chunker::default();
        c.push("Sure, I can.");
        c.next(PLENTY, NOW).unwrap();
        c.push("It is done.");
        assert_eq!(c.next(PLENTY, NOW).unwrap().text, "It is done.");
    }

    #[test]
    fn a_starving_playout_takes_a_waiting_clause() {
        let mut c = Chunker::default();
        c.push("Sure, I can.");
        c.next(PLENTY, NOW).unwrap();
        c.push("Then the run moves to Friday,");
        c.push("and the swim");
        let mut ahead = Duration::from_millis(2500);
        let mut got = None;
        while got.is_none() && !ahead.is_zero() {
            got = c.next(ahead, NOW);
            if got.is_none() {
                ahead = ahead.saturating_sub(Duration::from_millis(100));
            }
        }
        assert_eq!(ahead, Duration::from_millis(900));
        assert_eq!(got.unwrap().text.trim(), "Then the run moves to Friday, and the swim");
    }

    #[test]
    fn abbreviations_and_initials_do_not_end_sentences() {
        let mut c = Chunker::default();
        c.push("Sure, I can.");
        c.next(PLENTY, NOW).unwrap();
        c.push("Your run is at 7 a.m. with Dr. Smith, Mrs. Doe, J. R. Ray, e.g. Bo etc. and St. Ives vs. Al");
        assert_eq!(c.next(PLENTY, NOW), None, "no sentence has ended");
        c.push("Neither did I. Fine.");
        assert!(c.next(PLENTY, NOW).unwrap().text.ends_with("Neither did I. Fine."));
    }

    #[test]
    fn a_chunk_never_exceeds_two_sentences_or_300_chars() {
        let mut c = Chunker::default();
        c.push("One. Two. Three.");
        c.finish();
        assert_eq!(texts(&drain(&mut c, PLENTY)), ["One. Two.", "Three."]);

        let clause = "the run moves to Friday morning, ".repeat(12);
        let mut c = Chunker::default();
        c.push(&clause);
        c.finish();
        let chunks = drain(&mut c, PLENTY);
        assert!(chunks.iter().all(|ch| ch.text.chars().count() <= MAX_WEIGHT), "{chunks:?}");
        assert!(chunks[1].text.ends_with(','), "split at a clause: {:?}", chunks[1].text);
        assert_eq!(chunks.iter().map(|ch| ch.chars).sum::<u32>(), clause.chars().count() as u32);

        let mut c = Chunker::default();
        let word = "x".repeat(400);
        c.push(&word);
        c.finish();
        let chunks = drain(&mut c, PLENTY);
        assert_eq!(chunks.iter().map(|ch| ch.chars).collect::<Vec<_>>(), [300, 100]);
    }

    #[test]
    fn japanese_sentences_end_at_their_own_marks_without_a_space() {
        let mut c = Chunker::default();
        c.push("はい。");
        assert_eq!(c.next(PLENTY, NOW).unwrap().text, "はい。", "two characters make a first chunk");
        c.push("明日です。今日は");
        assert_eq!(c.next(PLENTY, NOW).unwrap().text, "明日です。");
        assert_eq!(c.next(PLENTY, NOW), None, "the rest waits for its sentence to end");
        c.push("どう？それでいい？じゃあね！");
        assert_eq!(c.next(PLENTY, NOW).unwrap().text, "今日はどう？それでいい？", "two sentences at most, no space between pushes");
        c.finish();
        assert_eq!(texts(&drain(&mut c, PLENTY)), ["じゃあね！"]);
    }

    #[test]
    fn a_japanese_first_chunk_cuts_at_a_comma_or_an_ellipsis() {
        let mut c = Chunker::default();
        c.push("うん、明日の朝七時に移動しておいたよ。");
        assert_eq!(c.next(PLENTY, NOW).unwrap().text, "うん、");
        assert_eq!(c.next(PLENTY, NOW).unwrap().text, "明日の朝七時に移動しておいたよ。");
        let mut c = Chunker::default();
        c.push("そうだね…じゃあ、金曜にしよう！");
        assert_eq!(c.next(PLENTY, NOW).unwrap().text, "そうだね…");
        let mut c = Chunker::default();
        c.push("「はい」と言った。");
        assert_eq!(c.next(PLENTY, NOW).unwrap().text, "「はい」と言った。");
    }

    #[test]
    fn a_line_break_ends_a_sentence_and_english_abbreviations_do_not_apply_to_japanese() {
        let mut c = Chunker::default();
        c.push("一つ目\n二つ目です。三つ目");
        assert_eq!(c.next(PLENTY, NOW).unwrap().text, "一つ目\n");
        assert_eq!(c.next(PLENTY, NOW).unwrap().text, "二つ目です。");
        let mut c = Chunker::default();
        c.push("Sure.");
        c.next(PLENTY, FIRST_STALL).unwrap();
        c.push("Dr Smith は来ます。次は");
        assert_eq!(c.next(PLENTY, NOW).unwrap().text, "Dr Smith は来ます。");
    }

    #[test]
    fn a_japanese_chunk_weighs_two_per_character() {
        let mut c = Chunker::default();
        c.push(&"あ".repeat(400));
        c.finish();
        let chunks = drain(&mut c, PLENTY);
        assert_eq!(chunks.iter().map(|ch| ch.chars).collect::<Vec<_>>(), [150, 150, 100]);
        let clause = "明日の朝、".repeat(60);
        let mut c = Chunker::default();
        c.push(&clause);
        c.finish();
        let chunks = drain(&mut c, PLENTY);
        assert!(chunks.iter().all(|ch| ch.text.chars().count() <= MAX_WEIGHT / 2), "{chunks:?}");
        assert!(chunks[1].text.ends_with('、'), "split at a clause: {:?}", chunks[1].text);
        assert_eq!(chunks.iter().map(|ch| ch.chars).sum::<u32>(), clause.chars().count() as u32);
    }

    #[test]
    fn chars_count_the_pushed_text_without_the_joining_spaces() {
        let mut c = Chunker::default();
        let pushed = ["Sure, I can.", "It is done.", "Bye."];
        for p in pushed {
            c.push(p);
        }
        c.finish();
        let chunks = drain(&mut c, PLENTY);
        assert_eq!(chunks.iter().map(|ch| ch.chars).sum::<u32>(), pushed.iter().map(|p| p.len() as u32).sum::<u32>());
        assert_eq!(chunks[0].chars, 12);
    }

    #[test]
    fn a_cancel_mid_chunk_ends_the_stream() {
        let mut c = Chunker::default();
        c.push("Sure, I can do that.");
        c.next(PLENTY, NOW).unwrap();
        c.push("The run moves to");
        c.cancel();
        assert_eq!(c.next(Duration::ZERO, FIRST_STALL), None);
        c.push("Friday.");
        c.finish();
        assert_eq!(c.next(Duration::ZERO, FIRST_STALL), None);
        assert!(c.done());
    }

    #[test]
    fn finishing_with_only_space_left_reports_nothing_more() {
        let mut c = Chunker::default();
        c.push("Done.");
        c.finish();
        assert_eq!(drain(&mut c, PLENTY).len(), 1);
        assert!(c.done());
    }

    #[test]
    fn resampling_to_48k_interpolates_and_keeps_48k_as_is() {
        assert_eq!(resample(&[0, 100, 200], 24_000), vec![0, 50, 100, 150, 200, 200]);
        assert_eq!(resample(&[5, 6], RATE), vec![5, 6]);
    }

    struct Echo;

    impl Renderer for Echo {
        fn render(&self, text: &str, _voice: &str) -> anyhow::Result<Vec<i16>> {
            if text == "fail." {
                anyhow::bail!("cannot");
            }
            Ok(vec![text.len() as i16; text.len()])
        }
    }

    fn voices(ids: &[&str]) -> Vec<VoiceInfo> {
        ids.iter().map(|id| VoiceInfo { id: (*id).into(), label: (*id).into(), languages: vec!["en".into()], credit: None }).collect()
    }

    #[test]
    fn a_chunked_stream_renders_its_chunks_and_skips_a_failed_one() {
        let mut s = ChunkedStream::new(Arc::new(Echo), "");
        s.push("Sure, I can.").unwrap();
        s.push("fail.").unwrap();
        let Next::Audio(first) = s.next(PLENTY).unwrap() else { panic!() };
        assert_eq!(first, Audio { pcm: vec![12; 12], chars: 12 });
        assert!(s.next(PLENTY).is_err());
        s.push("Bye.").unwrap();
        assert!(matches!(s.next(PLENTY).unwrap(), Next::Audio(Audio { chars: 4, .. })));
        assert!(matches!(s.next(PLENTY).unwrap(), Next::Pending));
        s.finish().unwrap();
        assert!(matches!(s.next(PLENTY).unwrap(), Next::Done));
    }

    #[test]
    fn a_voice_id_names_its_backend_and_a_bare_one_kokoro() {
        let kokoro: Arc<dyn SpeechBackend> = Arc::new(ChunkedBackend::new("kokoro", "Kokoro", Arc::new(Echo), voices(&["af_heart", "bm_george"])));
        let mut ja = voices(&["yui"]);
        ja[0].languages = vec!["ja".into()];
        let side: Arc<dyn SpeechBackend> = Arc::new(ChunkedBackend::new("kyutai", "Natural", Arc::new(Echo), {
            let mut v = voices(&["alba"]);
            v[0].languages.clear();
            v
        }));
        let sidecars = [side, Arc::new(ChunkedBackend::new("ja", "Japanese", Arc::new(Echo), ja))];
        let pick = |id: &str, language: &str| {
            let s = speaker(id, language, &kokoro, &sidecars);
            (s.backend.id().to_owned(), s.voice)
        };
        assert_eq!(pick("bm_george", "en"), ("kokoro".into(), "bm_george".into()));
        assert_eq!(pick("kyutai:alba", "en"), ("kyutai".into(), "alba".into()));
        assert_eq!(pick("kyutai:alba", "ja"), ("kyutai".into(), "alba".into()), "a voice of no language speaks any");
        assert_eq!(pick("ja:yui", "ja"), ("ja".into(), "yui".into()));
        assert_eq!(pick("ja:yui", "en"), ("kokoro".into(), String::new()), "a Japanese voice does not speak English");
        assert_eq!(pick("bm_george", "ja"), ("kokoro".into(), String::new()), "nor an English one Japanese");
        assert_eq!(pick("kyutai:nope", "en"), ("kokoro".into(), String::new()));
        assert_eq!(pick("gone:alba", "en"), ("kokoro".into(), String::new()));
        assert!(find_speaker("nope", "en", &kokoro, &sidecars).is_none());
    }
}
