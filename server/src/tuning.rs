//! The per-user knobs behind the day's rhythm, Note's patience and its
//! working limits. Every value left unset reads as the shipped default.

use serde::{Deserialize, Serialize};

pub const PATIENCE: &[&str] = &["quick", "normal", "patient"];
pub const REPLY_LENGTHS: &[&str] = &["short", "normal", "detailed"];
pub const REPLY_TONES: &[&str] = &["default", "warm", "direct", "playful"];
pub const REVIEW_DAYS: &[&str] = &["mon", "tue", "wed", "thu", "fri", "sat", "sun", "off"];
/// `push` walks the notification ladder; `chat` only lands in the thread, or
/// for the brief only in the app.
pub const CHANNELS: &[&str] = &["push", "chat"];
pub const LATEST_DAY_END: &str = "06:00";
pub const WAKE_EARLIEST: std::ops::RangeInclusive<&str> = "03:00"..="10:00";

macro_rules! tuning {
    (
        numbers { $( $num:ident: $default:expr, $lo:literal ..= $hi:literal; )* }
        flags { $( $flag:ident: $fdefault:expr; )* }
        choices { $( $choice:ident: $cdefault:expr; )* }
    ) => {
        #[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
        pub struct Tuning {
            $( #[serde(default, skip_serializing_if = "Option::is_none")] pub $num: Option<u32>, )*
            $( #[serde(default, skip_serializing_if = "Option::is_none")] pub $flag: Option<bool>, )*
            $( #[serde(default, skip_serializing_if = "Option::is_none")] pub $choice: Option<String>, )*
        }

        #[derive(Debug, Default, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct TuningPatch {
            $( $num: Option<u32>, )*
            $( $flag: Option<bool>, )*
            $( $choice: Option<String>, )*
        }

        impl Tuning {
            $( pub fn $num(&self) -> u32 { self.$num.unwrap_or($default).clamp($lo, $hi) } )*
            $( pub fn $flag(&self) -> bool { self.$flag.unwrap_or($fdefault) } )*
            $(
                pub fn $choice(&self) -> &str {
                    self.$choice.as_deref().filter(|v| valid_choice(stringify!($choice), v)).unwrap_or($cdefault)
                }
            )*

            pub fn is_empty(&self) -> bool {
                *self == Self::default()
            }

            pub fn view(&self) -> serde_json::Value {
                let mut out = serde_json::Map::new();
                $( out.insert(stringify!($num).into(), self.$num().into()); )*
                $( out.insert(stringify!($flag).into(), self.$flag().into()); )*
                $( out.insert(stringify!($choice).into(), self.$choice().into()); )*
                out.into()
            }

            /// Merges `patch` in whole or not at all, refusing the first value
            /// out of range with the field's name and what it must be.
            pub fn apply(&mut self, patch: TuningPatch) -> Result<(), (&'static str, String)> {
                let mut next = self.clone();
                next.merge(patch)?;
                *self = next;
                Ok(())
            }

            fn merge(&mut self, patch: TuningPatch) -> Result<(), (&'static str, String)> {
                $(
                    if let Some(n) = patch.$num {
                        if !($lo..=$hi).contains(&n) {
                            return Err((stringify!($num), format!("must be {} to {}", $lo, $hi)));
                        }
                        self.$num = Some(n);
                    }
                )*
                $( if let Some(on) = patch.$flag { self.$flag = Some(on); } )*
                $(
                    if let Some(v) = patch.$choice {
                        let v = v.trim().to_string();
                        if !valid_choice(stringify!($choice), &v) {
                            return Err((stringify!($choice), choice_requirement(stringify!($choice))));
                        }
                        self.$choice = Some(v);
                    }
                )*
                if self.overrun_end_pct() <= self.overrun_ask_pct() {
                    return Err(("overrun_end_pct", "must be above overrun_ask_pct".into()));
                }
                if self.tidy_small_min() >= self.tidy_big_min() {
                    return Err(("tidy_small_min", "must be below tidy_big_min".into()));
                }
                Ok(())
            }
        }
    };
}

tuning! {
    numbers {
        overrun_ask_pct: 150, 110..=500;
        overrun_end_pct: 300, 120..=1000;
        gone_grace_min: 5, 1..=60;
        max_pause_min: 120, 10..=480;
        first_check_min: 25, 5..=120;
        block_min: 25, 5..=240;
        spoke_hold_min: 60, 10..=480;
        idle_backoff_steps: 2, 0..=6;
        no_show_grace_min: 30, 0..=240;
        close_day_calls_min: 30, 0..=180;
        ring_secs: 30, 10..=120;
        due_soon_days: 3, 1..=14;
        soon_days: 7, 1..=30;
        lead_min: 10, 0..=60;
        near_min: 20, 5..=120;
        tidy_stale_days: 14, 3..=90;
        tidy_overdue_days: 3, 1..=30;
        tidy_big_min: 90, 30..=480;
        tidy_small_min: 30, 5..=120;
        note_stale_days: 3, 1..=30;
        learn_window_days: 14, 3..=60;
        history_limit: 32, 8..=128;
        lookup_rounds: 4, 1..=10;
        completion_cap: 150_000, 20_000..=400_000;
        working_memory_cap: 40, 10..=200;
    }
    flags {
        overrun_auto_end: true;
        block_follows_pomodoro: false;
        learning: true;
        auto_breakdown: true;
        calendar_quiet: true;
    }
    choices {
        day_end: "00:00";
        wake_earliest: "05:00";
        review_day: "mon";
        patience: "normal";
        reply_length: "normal";
        reply_tone: "default";
        nudge_channel: "push";
        checkin_channel: "push";
        review_channel: "push";
        brief_channel: "push";
    }
}

fn valid_choice(field: &str, v: &str) -> bool {
    match field {
        "day_end" => crate::templates::valid_time(v) && v <= LATEST_DAY_END,
        "wake_earliest" => crate::templates::valid_time(v) && WAKE_EARLIEST.contains(&v),
        "review_day" => REVIEW_DAYS.contains(&v),
        "patience" => PATIENCE.contains(&v),
        "reply_length" => REPLY_LENGTHS.contains(&v),
        "reply_tone" => REPLY_TONES.contains(&v),
        _ => CHANNELS.contains(&v),
    }
}

fn choice_requirement(field: &str) -> String {
    match field {
        "day_end" => format!("must be a zero-padded HH:MM no later than {LATEST_DAY_END}"),
        "wake_earliest" => format!("must be a zero-padded HH:MM from {} to {}", WAKE_EARLIEST.start(), WAKE_EARLIEST.end()),
        "review_day" => format!("must be one of {}", REVIEW_DAYS.join(", ")),
        "patience" => format!("must be one of {}", PATIENCE.join(", ")),
        "reply_length" => format!("must be one of {}", REPLY_LENGTHS.join(", ")),
        "reply_tone" => format!("must be one of {}", REPLY_TONES.join(", ")),
        _ => format!("must be one of {}", CHANNELS.join(", ")),
    }
}

/// The user's tuning; the shipped values when their config cannot be read.
pub fn of(config_dir: &std::path::Path, username: &str) -> Tuning {
    crate::config::UserConfig::load(config_dir, username).map(|c| c.tuning).unwrap_or_default()
}

/// Which delivery setting a wake-up of this origin follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Nudge,
    Checkin,
    Review,
    Brief,
}

impl Tuning {
    pub fn channel(&self, category: Category) -> &str {
        match category {
            Category::Nudge => self.nudge_channel(),
            Category::Checkin => self.checkin_channel(),
            Category::Review => self.review_channel(),
            Category::Brief => self.brief_channel(),
        }
    }

    /// The weekday a review is written on; `None` with reviews off.
    pub fn review_weekday(&self) -> Option<jiff::civil::Weekday> {
        use jiff::civil::Weekday as W;
        Some(match self.review_day() {
            "mon" => W::Monday,
            "tue" => W::Tuesday,
            "wed" => W::Wednesday,
            "thu" => W::Thursday,
            "fri" => W::Friday,
            "sat" => W::Saturday,
            "sun" => W::Sunday,
            _ => return None,
        })
    }

    /// A shipped wait in minutes, stretched or shortened by the user's patience.
    pub fn patient<T: Into<i64>>(&self, minutes: T) -> i64 {
        let m = minutes.into();
        match self.patience() {
            "quick" => (m / 2).max(1),
            "patient" => m * 2,
            _ => m,
        }
    }

    /// A planned length's first check: halfway, capped at `first_check_min`.
    pub fn first_check(&self, planned_min: Option<i64>) -> i64 {
        let cap = i64::from(self.first_check_min());
        planned_min.map_or(cap, |min| (min / 2).min(cap)).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(json: &str) -> TuningPatch {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn unset_values_read_as_the_shipped_ones() {
        let t = Tuning::default();
        assert_eq!(t.overrun_ask_pct(), 150);
        assert_eq!(t.completion_cap(), 150_000);
        assert_eq!(t.day_end(), "00:00");
        assert_eq!(t.review_weekday(), Some(jiff::civil::Weekday::Monday));
        assert_eq!(t.patient(20u32), 20);
        assert!(t.overrun_auto_end() && t.calendar_quiet() && !t.block_follows_pomodoro());
        assert!(t.is_empty());
    }

    #[test]
    fn a_patch_out_of_range_is_refused_and_changes_nothing_it_names() {
        let mut t = Tuning::default();
        assert_eq!(t.apply(patch(r#"{"ring_secs": 5}"#)).unwrap_err().0, "ring_secs");
        assert_eq!(t.apply(patch(r#"{"day_end": "07:00"}"#)).unwrap_err().0, "day_end");
        assert_eq!(t.apply(patch(r#"{"patience": "eager"}"#)).unwrap_err().0, "patience");
        assert_eq!(t.apply(patch(r#"{"overrun_end_pct": 140}"#)).unwrap_err().0, "overrun_end_pct");
        assert!(serde_json::from_str::<TuningPatch>(r#"{"nope": 1}"#).is_err());
        t.apply(patch(r#"{"ring_secs": 45, "day_end": "03:00", "review_day": "off", "patience": "patient"}"#)).unwrap();
        assert_eq!((t.ring_secs(), t.day_end(), t.review_weekday(), t.patient(15u32)), (45, "03:00", None, 30));
    }

    #[test]
    fn a_hand_edited_value_out_of_range_reads_clamped() {
        let t: Tuning = toml::from_str("ring_secs = 9000\nday_end = \"12:00\"\n").unwrap();
        assert_eq!((t.ring_secs(), t.day_end()), (120, "00:00"));
    }
}
