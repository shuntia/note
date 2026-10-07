use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::tts::{render_within, Speaker, SpeechBackend};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Line {
    OneMoment,
    LostNotes,
    Goodbye,
    CantReach,
    Hi,
    /// The call's voice is down: said from the kept rendering, then the call ends.
    NoVoice,
}

pub const ALL: [Line; 6] = [Line::Hi, Line::OneMoment, Line::LostNotes, Line::Goodbye, Line::CantReach, Line::NoVoice];

/// English stands in for any language without its own wording.
pub fn text(line: Line, language: &str) -> &'static str {
    match (language, line) {
        ("ja", Line::OneMoment) => "ちょっと待ってね。",
        ("ja", Line::LostNotes) => "ごめん、メモを一瞬見失っちゃった。",
        ("ja", Line::Goodbye) => "あとでメッセージするね。またね。",
        ("ja", Line::CantReach) => "いまメモにつながらないから、メッセージで送るね。",
        ("ja", Line::Hi) => "もしもし！",
        ("ja", Line::NoVoice) => "ごめん、いま声が出せないから、メッセージで送るね。",
        (_, Line::OneMoment) => "One moment.",
        (_, Line::LostNotes) => "I lost my notes for a moment.",
        (_, Line::Goodbye) => "I'll message you instead. Bye for now.",
        (_, Line::CantReach) => "I can't reach your notes right now. I'll message you.",
        (_, Line::Hi) => "Hi!",
        (_, Line::NoVoice) => "I can't speak right now. I'll message you instead.",
    }
}

type Key = (String, String, String, Line);

/// The call lines rendered so far, by language, backend and voice; kept under `dir` across restarts,
/// so a voice that is down can still say them.
#[derive(Default)]
pub struct Lines {
    rendered: Mutex<HashMap<Key, Arc<Vec<i16>>>>,
    dir: Option<PathBuf>,
}

impl Lines {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Lines { rendered: Mutex::default(), dir }
    }

    /// The line's audio in `speaker`, rendered on first use; when `speaker` fails, in `fallback`'s
    /// default voice.
    pub fn get(
        &self,
        speaker: &Speaker,
        fallback: &dyn SpeechBackend,
        language: &str,
        line: Line,
    ) -> anyhow::Result<Arc<Vec<i16>>> {
        match self.render(&*speaker.backend, &speaker.voice, language, line) {
            Err(e) if speaker.backend.id() != fallback.id() => {
                eprintln!("voice: rendering {line:?} in {} failed: {e:#}", speaker.backend.id());
                self.render(fallback, "", language, line)
            }
            rendered => rendered,
        }
    }

    pub fn cached(&self, speaker: &Speaker, language: &str, line: Line) -> Option<Arc<Vec<i16>>> {
        self.kept(speaker.backend.id(), &speaker.voice, language, line)
    }

    /// The line as last rendered by the backend `id` in `voice`, whether or not it is up now.
    pub fn kept(&self, id: &str, voice: &str, language: &str, line: Line) -> Option<Arc<Vec<i16>>> {
        let key = (language.to_owned(), id.to_owned(), voice.to_owned(), line);
        if let Some(pcm) = self.rendered.lock().expect("lines lock").get(&key) {
            return Some(pcm.clone());
        }
        let bytes = std::fs::read(self.file(&key)?).ok()?;
        let pcm = bytes.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
        Some(self.rendered.lock().expect("lines lock").entry(key).or_insert_with(|| Arc::new(pcm)).clone())
    }

    /// The line rendered in `speaker` within `UPGRADE_LIMIT`, stopping with `None` once `abort` turns true.
    pub fn upgrade(
        &self,
        speaker: &Speaker,
        language: &str,
        line: Line,
        abort: &dyn Fn() -> bool,
    ) -> anyhow::Result<Option<Arc<Vec<i16>>>> {
        if let Some(pcm) = self.cached(speaker, language, line) {
            return Ok(Some(pcm));
        }
        let Some(pcm) = render_within(&*speaker.backend, text(line, language), &speaker.voice, UPGRADE_LIMIT, abort)? else {
            return Ok(None);
        };
        Ok(Some(self.keep(key(&*speaker.backend, &speaker.voice, language, line), pcm)))
    }

    fn render(&self, backend: &dyn SpeechBackend, voice: &str, language: &str, line: Line) -> anyhow::Result<Arc<Vec<i16>>> {
        if let Some(pcm) = self.kept(backend.id(), voice, language, line) {
            return Ok(pcm);
        }
        Ok(self.keep(key(backend, voice, language, line), backend.render(text(line, language), voice)?))
    }

    fn keep(&self, key: Key, pcm: Vec<i16>) -> Arc<Vec<i16>> {
        if let Some(file) = self.file(&key) {
            if let Err(e) = write_pcm(&file, &pcm) {
                eprintln!("voice: keeping {:?} at {} failed: {e:#}", key.3, file.display());
            }
        }
        self.rendered.lock().expect("lines lock").entry(key).or_insert_with(|| Arc::new(pcm)).clone()
    }

    /// A name stable across builds: FNV-1a over the key and the line's wording.
    fn file(&self, (language, backend, voice, line): &Key) -> Option<PathBuf> {
        let parts = [language.as_str(), backend, voice, &format!("{line:?}"), text(*line, language)];
        let hash = parts.iter().flat_map(|part| part.bytes().chain([0])).fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
        Some(self.dir.as_ref()?.join(format!("{hash:016x}.pcm")))
    }
}

fn write_pcm(file: &Path, pcm: &[i16]) -> anyhow::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = file.with_extension("partial");
    std::fs::write(&partial, pcm.iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<u8>>())?;
    std::fs::rename(&partial, file)?;
    Ok(())
}

const UPGRADE_LIMIT: Duration = Duration::from_secs(5);

fn key(backend: &dyn SpeechBackend, voice: &str, language: &str, line: Line) -> Key {
    (language.to_owned(), backend.id().to_owned(), voice.to_owned(), line)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::super::tts::{ChunkedBackend, Mute, Renderer};
    use super::*;

    #[derive(Default)]
    struct CountingTts(AtomicUsize);

    impl Renderer for CountingTts {
        fn render(&self, text: &str, voice: &str) -> anyhow::Result<Vec<i16>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            if voice == "down" {
                anyhow::bail!("down");
            }
            Ok(vec![(text.len() + voice.len()) as i16])
        }
    }

    #[test]
    fn english_lines_read_as_written() {
        assert_eq!(text(Line::OneMoment, "en"), "One moment.");
        assert_eq!(text(Line::LostNotes, "en"), "I lost my notes for a moment.");
        assert_eq!(text(Line::Goodbye, "en"), "I'll message you instead. Bye for now.");
        assert_eq!(text(Line::CantReach, "en"), "I can't reach your notes right now. I'll message you.");
        assert_eq!(text(Line::Hi, "en"), "Hi!");
        assert_eq!(text(Line::NoVoice, "en"), "I can't speak right now. I'll message you instead.");
    }

    #[test]
    fn japanese_lines_are_short_and_end_in_their_own_marks() {
        for line in ALL {
            let ja = text(line, "ja");
            assert_ne!(ja, text(line, "en"), "{line:?}");
            assert!(ja.ends_with(['。', '！']), "{ja}");
            assert!(ja.chars().count() <= 28, "{ja}");
            assert!(!ja.chars().any(|c| c.is_ascii_alphabetic()), "{ja}");
        }
        assert_eq!(text(Line::Hi, "ja"), "もしもし！");
        assert_eq!(text(Line::OneMoment, "ja"), "ちょっと待ってね。");
        assert_eq!(text(Line::Hi, "fr"), text(Line::Hi, "en"), "English stands in");
    }

    #[test]
    fn a_line_renders_once_per_language_and_voice() {
        let tts = Arc::new(CountingTts::default());
        let kokoro: Arc<dyn SpeechBackend> = Arc::new(ChunkedBackend::new("kokoro", "Kokoro", tts.clone(), Vec::new()));
        let lines = Lines::default();
        let default = Speaker::new(kokoro.clone(), "");
        let a = lines.get(&default, &*kokoro, "en", Line::Hi).unwrap();
        let b = lines.get(&default, &*kokoro, "en", Line::Hi).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(*a, vec![3]);
        let c = lines.get(&Speaker::new(kokoro.clone(), "af"), &*kokoro, "en", Line::Hi).unwrap();
        assert_eq!(*c, vec![5]);
        lines.get(&default, &*kokoro, "en", Line::OneMoment).unwrap();
        assert_eq!(tts.0.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_line_its_backend_cannot_render_falls_back_to_the_default_voice() {
        let tts = Arc::new(CountingTts::default());
        let kokoro: Arc<dyn SpeechBackend> = Arc::new(ChunkedBackend::new("kokoro", "Kokoro", tts.clone(), Vec::new()));
        let side: Arc<dyn SpeechBackend> = Arc::new(ChunkedBackend::new("side", "Side", tts.clone(), Vec::new()));
        let lines = Lines::default();
        let pcm = lines.get(&Speaker::new(side, "down"), &*kokoro, "en", Line::Hi).unwrap();
        assert_eq!(*pcm, vec![3]);
    }

    #[test]
    fn an_upgrade_is_cached_and_stops_when_asked() {
        let tts = Arc::new(CountingTts::default());
        let side = Speaker::new(Arc::new(ChunkedBackend::new("side", "Side", tts.clone(), Vec::new())), "v");
        let lines = Lines::default();
        assert!(lines.upgrade(&side, "en", Line::Hi, &|| true).unwrap().is_none());
        assert!(lines.cached(&side, "en", Line::Hi).is_none());
        let pcm = lines.upgrade(&side, "en", Line::Hi, &|| false).unwrap().unwrap();
        assert_eq!(*pcm, vec![4]);
        assert!(Arc::ptr_eq(&lines.cached(&side, "en", Line::Hi).unwrap(), &pcm));
    }

    #[test]
    fn a_kept_line_outlives_its_backend_and_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let tts = Arc::new(CountingTts::default());
        let ja: Arc<dyn SpeechBackend> = Arc::new(ChunkedBackend::new("ja", "Japanese", tts.clone(), Vec::new()));
        let lines = Lines::new(Some(dir.path().join("lines")));
        assert!(lines.kept("ja", "", "ja", Line::NoVoice).is_none());
        let pcm = lines.get(&Speaker::new(ja, ""), &Mute::new("ja"), "ja", Line::NoVoice).unwrap();
        assert_eq!(pcm.len(), 1);
        drop(lines);
        let lines = Lines::new(Some(dir.path().join("lines")));
        assert_eq!(lines.kept("ja", "", "ja", Line::NoVoice).as_deref(), Some(&*pcm));
        assert!(lines.kept("ja", "", "ja", Line::Hi).is_none());
        assert!(lines.kept("ja", "other", "ja", Line::NoVoice).is_none());
        assert_eq!(tts.0.load(Ordering::SeqCst), 1);
    }
}
