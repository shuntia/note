use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::engines::TextToSpeech;

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

#[derive(Default)]
pub struct Lines {
    rendered: Mutex<HashMap<(String, String, Line), Arc<Vec<i16>>>>,
}

impl Lines {
    /// The line's audio, synthesized with `tts` on first use and cached by language and voice.
    pub fn get(&self, tts: &dyn TextToSpeech, language: &str, voice: &str, line: Line) -> anyhow::Result<Arc<Vec<i16>>> {
        let key = (language.to_owned(), voice.to_owned(), line);
        if let Some(pcm) = self.rendered.lock().expect("lines lock").get(&key) {
            return Ok(pcm.clone());
        }
        let pcm = Arc::new(tts.synthesize(text(line, language), voice)?);
        Ok(self.rendered.lock().expect("lines lock").entry(key).or_insert(pcm).clone())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::super::engines::VoiceInfo;
    use super::*;

    #[derive(Default)]
    struct CountingTts(AtomicUsize);

    impl TextToSpeech for CountingTts {
        fn synthesize(&self, text: &str, voice: &str) -> anyhow::Result<Vec<i16>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(vec![(text.len() + voice.len()) as i16])
        }

        fn voices(&self) -> Vec<VoiceInfo> {
            Vec::new()
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
        let tts = CountingTts::default();
        let lines = Lines::default();
        let a = lines.get(&tts, "en", "", Line::Hi).unwrap();
        let b = lines.get(&tts, "en", "", Line::Hi).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(*a, vec![3]);
        let c = lines.get(&tts, "en", "af", Line::Hi).unwrap();
        assert_eq!(*c, vec![5]);
        lines.get(&tts, "en", "", Line::OneMoment).unwrap();
        assert_eq!(tts.0.load(Ordering::SeqCst), 3);
    }
}
