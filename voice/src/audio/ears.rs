use std::sync::Arc;

use tokio::sync::mpsc;

use super::engines::{SpeechEngines, SpeechToText};
use super::language::{confident, Choice, MIN_SECONDS};
use crate::session::{Event, RATE};

/// The voiced audio the caller's language is first judged from, unless their first turn ends sooner.
pub(crate) const ID_SPAN: usize = RATE * 5 / 2;
/// A turn with less voiced audio than this does not count toward `ID_TURNS`.
pub(crate) const ID_MIN: usize = RATE * 3 / 10;
/// The voiced audio that has the language judged at a pause, ahead of the turn's end.
pub(crate) const ID_AT_PAUSE: usize = RATE;
/// Turns, and voiced audio, heard without a confident language before the call keeps the user's.
pub(crate) const ID_TURNS: u32 = 3;
pub(crate) const ID_CAP: usize = RATE * 8;

pub(crate) enum SttCmd {
    /// `voiced`: the VAD heard speech in the chunk.
    Accept { samples: Vec<f32>, voiced: bool },
    Finish { turn: u64 },
    /// The caller paused.
    Pause,
    Reset,
}

enum SttOut {
    Partial(Option<Choice>, String),
    Finished(Option<Choice>, Option<(u64, String)>),
}

/// The call's recognizers. Until the caller's language is settled, every language the call can
/// speak listens at once with no partials shown, and the voiced audio of each turn is kept. It is
/// judged at `ID_SPAN`, at a pause with `ID_AT_PAUSE` heard, and at the end of each turn, over all
/// the voiced audio so far; the recognizer of a confident language, which heard the whole turn,
/// carries on alone. A turn that ends unsettled is heard in the fallback language and the language
/// stays open, until `ID_TURNS` such turns or `ID_CAP` of voiced audio settle it on the fallback.
pub(crate) struct Ears {
    engines: Arc<dyn SpeechEngines>,
    speakable: Vec<String>,
    fallback: String,
    listening: Vec<(String, Box<dyn SpeechToText>)>,
    voiced: Vec<f32>,
    /// Where the current turn's voiced audio starts in `voiced`.
    turn_from: usize,
    /// The voiced audio the last judgement saw.
    judged: usize,
    /// Turns that ended unsettled.
    turns: u32,
    pub(crate) chosen: Option<Box<dyn SpeechToText>>,
}

impl Ears {
    /// Settled on `fallback` from the start when there is nothing to choose between.
    pub(crate) fn new(engines: Arc<dyn SpeechEngines>, speakable: &[String], fallback: &str) -> anyhow::Result<Ears> {
        let choosing = engines.identifies() && speakable.len() > 1 && speakable.iter().any(|l| l == fallback);
        let mut ears = Ears {
            speakable: speakable.to_vec(),
            fallback: fallback.to_owned(),
            listening: Vec::new(),
            voiced: Vec::new(),
            turn_from: 0,
            judged: 0,
            turns: 0,
            chosen: None,
            engines,
        };
        if choosing {
            for language in speakable {
                let stt = if language == fallback { ears.engines.stt(language)? } else { ears.engines.stt_standby(language)? };
                ears.listening.push((language.clone(), stt));
            }
        } else {
            ears.chosen = Some(ears.engines.stt(fallback)?);
        }
        Ok(ears)
    }

    fn accept(&mut self, samples: &[f32], voiced: bool) -> Option<Choice> {
        if let Some(stt) = &mut self.chosen {
            stt.accept(samples);
            return None;
        }
        for (_, stt) in &mut self.listening {
            stt.accept(samples);
        }
        if voiced {
            let room = ID_CAP.saturating_sub(self.voiced.len());
            self.voiced.extend_from_slice(&samples[..samples.len().min(room)]);
        }
        if self.voiced.len() >= ID_CAP {
            return self.judge().or_else(|| Some(self.give_up()));
        }
        (self.voiced.len() >= ID_SPAN && self.judged < ID_SPAN).then(|| self.judge()).flatten()
    }

    fn partial(&mut self) -> String {
        self.chosen.as_mut().map(|stt| stt.partial()).unwrap_or_default()
    }

    fn pause(&mut self) -> Option<Choice> {
        let fresh = self.voiced.len() >= ID_AT_PAUSE && self.voiced.len() >= self.judged + ID_AT_PAUSE / 2;
        (self.chosen.is_none() && fresh).then(|| self.judge()).flatten()
    }

    fn finish(&mut self) -> (Option<Choice>, String) {
        let mut settled = None;
        if self.chosen.is_none() {
            settled = self.judge();
            if settled.is_none() && self.voiced.len() - self.turn_from >= ID_MIN {
                self.turns += 1;
                if self.turns >= ID_TURNS {
                    settled = Some(self.give_up());
                }
            }
        }
        if let Some(stt) = &mut self.chosen {
            return (settled, stt.finish());
        }
        let mut text = String::new();
        for (language, stt) in &mut self.listening {
            let heard = stt.finish();
            if *language == self.fallback {
                text = heard;
            }
        }
        self.turn_from = self.voiced.len();
        (None, text)
    }

    /// Forgets the current turn, its voiced audio with it.
    fn reset(&mut self) {
        if let Some(stt) = &mut self.chosen {
            stt.reset();
            return;
        }
        for (_, stt) in &mut self.listening {
            stt.reset();
        }
        self.voiced.truncate(self.turn_from);
        self.judged = self.judged.min(self.turn_from);
    }

    /// Settles on the language of the voiced audio so far, if it is confident.
    fn judge(&mut self) -> Option<Choice> {
        let seconds = self.voiced.len() as f32 / RATE as f32;
        if self.voiced.len() == self.judged || seconds < MIN_SECONDS {
            return None;
        }
        self.judged = self.voiced.len();
        let started = std::time::Instant::now();
        let scores = match self.engines.identify(&self.voiced)? {
            Ok(scores) => scores,
            Err(e) => {
                eprintln!("voice: identifying the caller's language failed: {e:#}");
                return None;
            }
        };
        let took = started.elapsed();
        let language = confident(&scores, seconds, &self.speakable);
        let top: Vec<String> = scores.iter().take(3).map(|(l, p)| format!("{l} {p:.2}")).collect();
        eprintln!("voice: over {seconds:.1} s voiced the caller sounds like {top:?} (judged in {took:.0?}); {language:?}");
        Some(self.settle(Choice { language: language?, identified: true }))
    }

    fn give_up(&mut self) -> Choice {
        eprintln!("voice: the caller's language stays unclear; the call speaks {}", self.fallback);
        self.settle(Choice { language: self.fallback.clone(), identified: false })
    }

    fn settle(&mut self, choice: Choice) -> Choice {
        let at = self.listening.iter().position(|(l, _)| *l == choice.language).expect("the choice is speakable");
        let mut chosen = self.listening.swap_remove(at).1;
        chosen.wake();
        self.chosen = Some(chosen);
        self.listening.clear();
        self.voiced = Vec::new();
        choice
    }
}

/// Owns the call's recognizers; each step runs on the blocking pool, in order. Reports the language
/// once settled, then partial changes, and an empty partial once a finish or reset clears the stream.
pub(crate) async fn stt_worker(
    mut ears: Ears,
    mut cmds: mpsc::UnboundedReceiver<SttCmd>,
    events: mpsc::UnboundedSender<Event>,
) {
    let mut last = String::new();
    while let Some(cmd) = cmds.recv().await {
        let step = tokio::task::spawn_blocking(move || {
            let out = match cmd {
                SttCmd::Accept { samples, voiced } => {
                    let settled = ears.accept(&samples, voiced);
                    SttOut::Partial(settled, ears.partial())
                }
                SttCmd::Finish { turn } => {
                    let (settled, text) = ears.finish();
                    SttOut::Finished(settled, Some((turn, text)))
                }
                SttCmd::Pause => {
                    let settled = ears.pause();
                    SttOut::Partial(settled, ears.partial())
                }
                SttCmd::Reset => {
                    ears.reset();
                    SttOut::Finished(None, None)
                }
            };
            (ears, out)
        });
        let (back, out) = match step.await {
            Ok(done) => done,
            Err(e) => {
                eprintln!("voice: an STT step failed: {e}");
                return;
            }
        };
        ears = back;
        match out {
            SttOut::Partial(settled, partial) => {
                if let Some(choice) = settled {
                    let _ = events.send(Event::Language(choice));
                }
                if partial != last {
                    last.clone_from(&partial);
                    let _ = events.send(Event::Partial(partial));
                }
            }
            SttOut::Finished(settled, finished) => {
                if let Some(choice) = settled {
                    let _ = events.send(Event::Language(choice));
                }
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
