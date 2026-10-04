use std::collections::HashMap;
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
}

/// English stands in for any language without its own wording.
pub fn text(line: Line, _language: &str) -> &'static str {
    match line {
        Line::OneMoment => "One moment.",
        Line::LostNotes => "I lost my notes for a moment.",
        Line::Goodbye => "I'll message you instead. Bye for now.",
        Line::CantReach => "I can't reach your notes right now. I'll message you.",
        Line::Hi => "Hi!",
    }
}

type Key = (String, String, String, Line);

#[derive(Default)]
pub struct Lines {
    rendered: Mutex<HashMap<Key, Arc<Vec<i16>>>>,
}

impl Lines {
    /// The line's audio in `speaker`, rendered on first use and cached by language and voice; when
    /// `speaker` fails, in `fallback`'s default voice.
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
        self.rendered.lock().expect("lines lock").get(&key(&*speaker.backend, &speaker.voice, language, line)).cloned()
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
        let key = key(backend, voice, language, line);
        if let Some(pcm) = self.rendered.lock().expect("lines lock").get(&key) {
            return Ok(pcm.clone());
        }
        Ok(self.keep(key, backend.render(text(line, language), voice)?))
    }

    fn keep(&self, key: Key, pcm: Vec<i16>) -> Arc<Vec<i16>> {
        self.rendered.lock().expect("lines lock").entry(key).or_insert_with(|| Arc::new(pcm)).clone()
    }
}

const UPGRADE_LIMIT: Duration = Duration::from_secs(5);

fn key(backend: &dyn SpeechBackend, voice: &str, language: &str, line: Line) -> Key {
    (language.to_owned(), backend.id().to_owned(), voice.to_owned(), line)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::super::tts::{ChunkedBackend, Renderer};
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
}
