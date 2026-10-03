use std::time::Duration;

use note_voice_proto::Floor;

pub struct TurnConfig {
    pub draft_pause: Duration,
    pub incomplete_cap: Duration,
    pub complete_threshold: f32,
    pub barge_window: Duration,
    pub barge_limit: Duration,
}

impl Default for TurnConfig {
    fn default() -> Self {
        Self {
            draft_pause: Duration::from_millis(200),
            incomplete_cap: Duration::from_millis(1200),
            complete_threshold: 0.5,
            barge_window: Duration::from_millis(600),
            barge_limit: Duration::from_secs(2),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// One VAD window: speech or not, at time `at`.
    Vad { at: Duration, speech: bool },
    /// The STT partial for the current turn changed.
    Partial { text: String },
    /// Smart Turn's answer for the request the machine made.
    TurnScore { at: Duration, p: f32 },
    /// Whether Note's audio is playing right now.
    Playing { playing: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Ask Smart Turn to score the audio so far.
    ScoreTurn,
    Draft { turn: u64, text: String },
    /// The session calls `stt.finish()` for the final text.
    Commit { turn: u64, text_hint: String },
    Retract { turn: u64 },
    PausePlayout,
    /// The session resets STT on this: the barge turn is discarded.
    ResumePlayout,
    FlushPlayout,
    Floor(Floor),
}

pub const BACKCHANNELS_EN: &[&str] =
    &["uh-huh", "uh huh", "mhm", "mm-hmm", "right", "okay", "ok", "yeah", "yep", "sure", "got it", "i see"];

/// Only English has a list; every other language falls back to it.
pub fn backchannels(_language: &str) -> &'static [&'static str] {
    BACKCHANNELS_EN
}

/// `quiet_since` is when the current run of non-speech VAD windows began; `scored_at` is when `ScoreTurn` was asked.
#[derive(Debug, Clone, Copy)]
enum State {
    Idle,
    Speaking { quiet_since: Option<Duration> },
    Paused { since: Duration, drafted: bool, scored_at: Duration },
    Barging { since: Duration, quiet_since: Option<Duration> },
}

pub struct TurnMachine {
    cfg: TurnConfig,
    backchannels: Vec<Vec<String>>,
    turn: u64,
    state: State,
    partial: String,
    playing: bool,
}

impl TurnMachine {
    pub fn new(cfg: TurnConfig, backchannels: &'static [&'static str]) -> Self {
        Self {
            cfg,
            backchannels: backchannels.iter().map(|w| words(w)).collect(),
            turn: 1,
            state: State::Idle,
            partial: String::new(),
            playing: false,
        }
    }

    pub fn input(&mut self, i: Input) -> Vec<Action> {
        match i {
            Input::Vad { at, speech } => self.vad(at, speech),
            Input::Partial { text } => {
                self.partial = text;
                match self.state {
                    State::Barging { quiet_since, .. } if self.has_real_words() => {
                        self.state = State::Speaking { quiet_since };
                        vec![Action::FlushPlayout]
                    }
                    _ => Vec::new(),
                }
            }
            Input::TurnScore { at, p } => match self.state {
                State::Paused { scored_at, .. } if at >= scored_at && p >= self.cfg.complete_threshold => {
                    vec![self.commit()]
                }
                _ => Vec::new(),
            },
            Input::Playing { playing } => {
                self.playing = playing;
                Vec::new()
            }
        }
    }

    /// Advances timers; call every 10 ms with the current time.
    pub fn tick(&mut self, now: Duration) -> Vec<Action> {
        match self.state {
            State::Speaking { quiet_since: Some(q) } if now.saturating_sub(q) >= self.cfg.draft_pause => {
                let drafted = !self.partial.is_empty();
                self.state = State::Paused { since: q, drafted, scored_at: now };
                let mut out = vec![Action::Floor(Floor::UserQuiet)];
                if drafted {
                    out.push(Action::Draft { turn: self.turn, text: self.partial.clone() });
                }
                out.push(Action::ScoreTurn);
                out
            }
            State::Paused { since, .. } if now.saturating_sub(since) >= self.cfg.incomplete_cap => {
                if self.partial.is_empty() {
                    self.state = State::Idle;
                    Vec::new()
                } else {
                    vec![self.commit()]
                }
            }
            State::Barging { since, quiet_since } => {
                let elapsed = now.saturating_sub(since);
                if elapsed >= self.cfg.barge_window && quiet_since.is_some() && !self.has_real_words() {
                    self.state = State::Idle;
                    self.partial.clear();
                    vec![Action::Floor(Floor::UserQuiet), Action::ResumePlayout]
                } else if elapsed >= self.cfg.barge_limit {
                    self.state = State::Speaking { quiet_since };
                    vec![Action::FlushPlayout]
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn vad(&mut self, at: Duration, speech: bool) -> Vec<Action> {
        match (self.state, speech) {
            (State::Idle, true) => {
                if self.playing {
                    self.state = State::Barging { since: at, quiet_since: None };
                    vec![Action::Floor(Floor::UserSpeaking), Action::PausePlayout]
                } else {
                    self.state = State::Speaking { quiet_since: None };
                    vec![Action::Floor(Floor::UserSpeaking)]
                }
            }
            (State::Speaking { .. }, true) => {
                self.state = State::Speaking { quiet_since: None };
                Vec::new()
            }
            (State::Speaking { quiet_since: None }, false) => {
                self.state = State::Speaking { quiet_since: Some(at) };
                Vec::new()
            }
            (State::Paused { drafted, .. }, true) => {
                self.state = State::Speaking { quiet_since: None };
                let mut out = Vec::new();
                if drafted {
                    out.push(Action::Retract { turn: self.turn });
                }
                out.push(Action::Floor(Floor::UserSpeaking));
                out
            }
            (State::Barging { since, .. }, true) => {
                self.state = State::Barging { since, quiet_since: None };
                Vec::new()
            }
            (State::Barging { since, quiet_since: None }, false) => {
                self.state = State::Barging { since, quiet_since: Some(at) };
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn commit(&mut self) -> Action {
        let action = Action::Commit { turn: self.turn, text_hint: std::mem::take(&mut self.partial) };
        self.turn += 1;
        self.state = State::Idle;
        action
    }

    /// False for an empty partial, a backchannel, or the first whole words of one still streaming in ("uh" of "uh-huh").
    fn has_real_words(&self) -> bool {
        let said = words(&self.partial);
        !said.is_empty() && !self.backchannels.iter().any(|b| b.starts_with(&said))
    }
}

/// Lowercased words with punctuation as separators, so "Uh-huh." and "uh-huh" both read as `["uh", "huh"]`.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_finished_sentence_drafts_then_commits() {
        let mut m = TurnMachine::new(TurnConfig::default(), BACKCHANNELS_EN);
        assert_eq!(m.input(Input::Vad { at: ms(0), speech: true }), vec![Action::Floor(Floor::UserSpeaking)]);
        m.input(Input::Partial { text: "move my run".into() });
        m.input(Input::Vad { at: ms(1000), speech: false });
        assert!(m.tick(ms(1150)).is_empty());
        assert_eq!(
            m.tick(ms(1200)),
            vec![
                Action::Floor(Floor::UserQuiet),
                Action::Draft { turn: 1, text: "move my run".into() },
                Action::ScoreTurn,
            ]
        );
        assert_eq!(
            m.input(Input::TurnScore { at: ms(1215), p: 0.9 }),
            vec![Action::Commit { turn: 1, text_hint: "move my run".into() }]
        );
    }

    #[test]
    fn speech_after_a_draft_retracts_it() {
        let mut m = TurnMachine::new(TurnConfig::default(), BACKCHANNELS_EN);
        m.input(Input::Vad { at: ms(0), speech: true });
        m.input(Input::Partial { text: "I need to".into() });
        m.input(Input::Vad { at: ms(800), speech: false });
        m.tick(ms(1000));
        assert!(m.input(Input::TurnScore { at: ms(1010), p: 0.1 }).is_empty());
        assert_eq!(
            m.input(Input::Vad { at: ms(1300), speech: true }),
            vec![Action::Retract { turn: 1 }, Action::Floor(Floor::UserSpeaking)]
        );
        m.input(Input::Partial { text: "I need to move my run".into() });
        m.input(Input::Vad { at: ms(2000), speech: false });
        let a = m.tick(ms(2200));
        assert!(a.contains(&Action::Draft { turn: 1, text: "I need to move my run".into() }), "same turn number");
    }

    #[test]
    fn an_incomplete_turn_commits_at_the_silence_cap() {
        let mut m = TurnMachine::new(TurnConfig::default(), BACKCHANNELS_EN);
        m.input(Input::Vad { at: ms(0), speech: true });
        m.input(Input::Partial { text: "so".into() });
        m.input(Input::Vad { at: ms(500), speech: false });
        m.tick(ms(700));
        m.input(Input::TurnScore { at: ms(710), p: 0.2 });
        assert!(m.tick(ms(1690)).is_empty());
        assert_eq!(m.tick(ms(1700)), vec![Action::Commit { turn: 1, text_hint: "so".into() }]);
    }

    #[test]
    fn a_backchannel_resumes_where_it_paused() {
        let mut m = TurnMachine::new(TurnConfig::default(), BACKCHANNELS_EN);
        m.input(Input::Playing { playing: true });
        assert_eq!(
            m.input(Input::Vad { at: ms(0), speech: true }),
            vec![Action::Floor(Floor::UserSpeaking), Action::PausePlayout]
        );
        m.input(Input::Partial { text: "Uh-huh.".into() });
        m.input(Input::Vad { at: ms(300), speech: false });
        let mut all = Vec::new();
        for t in (310..=700).step_by(10) {
            all.extend(m.tick(ms(t)));
        }
        assert!(all.contains(&Action::ResumePlayout));
        assert!(!all.iter().any(|a| matches!(a, Action::Draft { .. } | Action::Commit { .. } | Action::FlushPlayout)));
    }

    #[test]
    fn real_words_over_playout_flush_it() {
        let mut m = TurnMachine::new(TurnConfig::default(), BACKCHANNELS_EN);
        m.input(Input::Playing { playing: true });
        m.input(Input::Vad { at: ms(0), speech: true });
        assert_eq!(m.input(Input::Partial { text: "wait, actually".into() }), vec![Action::FlushPlayout]);
    }

    #[test]
    fn a_streaming_backchannel_does_not_flush() {
        let mut m = TurnMachine::new(TurnConfig::default(), BACKCHANNELS_EN);
        m.input(Input::Playing { playing: true });
        m.input(Input::Vad { at: ms(0), speech: true });
        assert!(m.input(Input::Partial { text: "Uh".into() }).is_empty());
        assert!(m.input(Input::Partial { text: "Uh-huh.".into() }).is_empty());
        m.input(Input::Vad { at: ms(300), speech: false });
        let mut all = Vec::new();
        for t in (310..=700).step_by(10) {
            all.extend(m.tick(ms(t)));
        }
        assert!(!all.contains(&Action::FlushPlayout));
        assert_eq!(all.iter().filter(|a| **a == Action::ResumePlayout).count(), 1);
    }

    #[test]
    fn a_backchannel_still_spoken_at_the_window_waits_for_the_pause() {
        let mut m = TurnMachine::new(TurnConfig::default(), BACKCHANNELS_EN);
        m.input(Input::Playing { playing: true });
        m.input(Input::Vad { at: ms(0), speech: true });
        m.input(Input::Partial { text: "uh-huh".into() });
        for t in (10..=900).step_by(10) {
            m.input(Input::Vad { at: ms(t), speech: true });
            assert!(m.tick(ms(t)).is_empty(), "still talking at {t} ms");
        }
        m.input(Input::Vad { at: ms(910), speech: false });
        assert_eq!(m.tick(ms(910)), vec![Action::Floor(Floor::UserQuiet), Action::ResumePlayout]);
    }

    #[test]
    fn talking_over_playout_for_two_seconds_flushes_it() {
        let mut m = TurnMachine::new(TurnConfig::default(), BACKCHANNELS_EN);
        m.input(Input::Playing { playing: true });
        m.input(Input::Vad { at: ms(0), speech: true });
        m.input(Input::Partial { text: "yeah".into() });
        for t in (10..2000).step_by(10) {
            m.input(Input::Vad { at: ms(t), speech: true });
            assert!(m.tick(ms(t)).is_empty(), "still deciding at {t} ms");
        }
        assert_eq!(m.tick(ms(2000)), vec![Action::FlushPlayout]);
        m.input(Input::Vad { at: ms(2100), speech: false });
        assert!(m.tick(ms(2300)).contains(&Action::Draft { turn: 1, text: "yeah".into() }));
    }

    #[test]
    fn a_score_older_than_the_latest_request_is_ignored() {
        let mut m = TurnMachine::new(TurnConfig::default(), BACKCHANNELS_EN);
        m.input(Input::Vad { at: ms(0), speech: true });
        m.input(Input::Partial { text: "I need to".into() });
        m.input(Input::Vad { at: ms(100), speech: false });
        m.tick(ms(300));
        m.input(Input::Vad { at: ms(400), speech: true });
        m.input(Input::Vad { at: ms(500), speech: false });
        assert!(m.tick(ms(700)).contains(&Action::ScoreTurn));
        assert!(m.input(Input::TurnScore { at: ms(450), p: 0.9 }).is_empty());
        assert_eq!(
            m.input(Input::TurnScore { at: ms(710), p: 0.9 }),
            vec![Action::Commit { turn: 1, text_hint: "I need to".into() }]
        );
    }

    #[test]
    fn a_wordless_pause_at_the_cap_goes_idle_without_a_commit() {
        let mut m = TurnMachine::new(TurnConfig::default(), BACKCHANNELS_EN);
        m.input(Input::Vad { at: ms(0), speech: true });
        m.input(Input::Vad { at: ms(100), speech: false });
        assert_eq!(m.tick(ms(300)), vec![Action::Floor(Floor::UserQuiet), Action::ScoreTurn]);
        assert!(m.tick(ms(1300)).is_empty());
        assert_eq!(m.input(Input::Vad { at: ms(2000), speech: true }), vec![Action::Floor(Floor::UserSpeaking)]);
        m.input(Input::Partial { text: "hello".into() });
        m.input(Input::Vad { at: ms(2500), speech: false });
        assert!(m.tick(ms(2700)).contains(&Action::Draft { turn: 1, text: "hello".into() }));
    }

    #[test]
    fn a_discarded_barge_frees_the_floor_and_keeps_the_turn_number() {
        let mut m = TurnMachine::new(TurnConfig::default(), BACKCHANNELS_EN);
        m.input(Input::Playing { playing: true });
        m.input(Input::Vad { at: ms(0), speech: true });
        m.input(Input::Partial { text: "mhm".into() });
        m.input(Input::Vad { at: ms(200), speech: false });
        assert!(m.tick(ms(590)).is_empty());
        assert_eq!(m.tick(ms(600)), vec![Action::Floor(Floor::UserQuiet), Action::ResumePlayout]);
        m.input(Input::Playing { playing: false });
        m.input(Input::Vad { at: ms(1000), speech: true });
        m.input(Input::Partial { text: "move my run".into() });
        m.input(Input::Vad { at: ms(1500), speech: false });
        assert!(m.tick(ms(1700)).contains(&Action::Draft { turn: 1, text: "move my run".into() }));
    }

    #[test]
    fn noise_with_no_words_resumes_playout() {
        let mut m = TurnMachine::new(TurnConfig::default(), BACKCHANNELS_EN);
        m.input(Input::Playing { playing: true });
        m.input(Input::Vad { at: ms(0), speech: true });
        m.input(Input::Vad { at: ms(150), speech: false });
        let mut all = Vec::new();
        for t in (160..=700).step_by(10) {
            all.extend(m.tick(ms(t)));
        }
        assert_eq!(all.iter().filter(|a| **a == Action::ResumePlayout).count(), 1);
    }
}
