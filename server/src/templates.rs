use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Template {
    pub events: Vec<TemplateEvent>,
}

/// The two shapes a template entry can take: a routine happens at a moment, a
/// block occupies a range.
#[derive(Debug, Default, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Entry {
    #[default]
    Routine,
    Block,
}

#[derive(Debug, Default, Deserialize)]
pub struct TemplateEvent {
    pub kind: String,
    pub time: String,
    pub days: Vec<String>,
    #[serde(default)]
    pub entry: Entry,
    #[serde(default)]
    pub end_time: Option<String>,
    #[serde(default)]
    pub flexibility: Option<String>,
    #[serde(default)]
    pub slide_window_min: Option<i64>,
    #[serde(default = "default_channel")]
    pub channel: String,
    #[serde(default)]
    pub alert: Option<bool>,
}

impl TemplateEvent {
    pub fn is_block(&self) -> bool {
        self.entry == Entry::Block
    }

    /// A block is always the agent's to reshape, so it declares no flexibility
    /// of its own.
    pub fn flexibility(&self) -> &str {
        if self.is_block() {
            "slide"
        } else {
            self.flexibility.as_deref().unwrap_or("fixed")
        }
    }

    pub fn slide_window_min(&self) -> i64 {
        if self.is_block() { 0 } else { self.slide_window_min.unwrap_or(0) }
    }

    pub fn alert(&self) -> bool {
        !self.is_block() && self.alert.unwrap_or(true)
    }

    /// How long a routine occupies, from its `end_time` or the fifteen-minute
    /// default; a block's range is stored on the event instead.
    pub fn span_min(&self) -> i64 {
        if self.is_block() {
            return 0;
        }
        match self.end_time.as_deref() {
            Some(end) => wall_minutes(end) - wall_minutes(&self.time),
            None => 15,
        }
    }

    /// The effective end of the entry, for both shapes.
    pub fn end(&self) -> String {
        match (self.is_block(), self.end_time.as_deref()) {
            (true, Some(end)) => end.to_string(),
            (true, None) => self.time.clone(),
            (false, _) => wall_add(&self.time, self.span_min()),
        }
    }
}

fn default_channel() -> String { "push".into() }

/// One template entry as the settings pane lists it, with every effective value
/// resolved so the client never has to know the defaults.
#[derive(Debug, Serialize)]
pub struct ScheduleRow {
    pub index: usize,
    pub kind: String,
    pub entry: Entry,
    pub time: String,
    pub end_time: Option<String>,
    pub days: Vec<String>,
    pub flexibility: String,
    pub slide_window_min: i64,
    pub channel: String,
    pub alert: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AlertError {
    #[error("no entry at index {0}")]
    OutOfRange(usize),
    #[error("entry {0} is a block, and blocks never ping")]
    Block(usize),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
const FLEXIBILITIES: [&str; 3] = ["fixed", "slide", "drop"];
const CHANNELS: [&str; 1] = ["push"];

/// Zero-padded 24-hour `HH:MM`; the padding matters because wall times are
/// compared and sorted as strings once stored.
pub(crate) fn valid_time(s: &str) -> bool {
    let Some((h, m)) = s.split_once(':') else {
        return false;
    };
    let padded = |p: &str| p.len() == 2 && p.bytes().all(|b| b.is_ascii_digit());
    padded(h)
        && padded(m)
        && h.parse::<u32>().is_ok_and(|h| h < 24)
        && m.parse::<u32>().is_ok_and(|m| m < 60)
}

/// Adds `minutes` to a zero-padded wall time, never crossing midnight.
pub(crate) fn wall_add(wall: &str, minutes: i64) -> String {
    let total = wall_minutes(wall).saturating_add(minutes).clamp(0, 23 * 60 + 59);
    format!("{:02}:{:02}", total / 60, total % 60)
}

pub(crate) fn wall_minutes(wall: &str) -> i64 {
    let (h, m) = wall.split_once(':').unwrap_or(("0", "0"));
    h.parse::<i64>().unwrap_or(0) * 60 + m.parse::<i64>().unwrap_or(0)
}

/// Every template name `user` can select: the shared defaults plus their own
/// overrides, deduped (an override shadows the default of the same name) and
/// sorted. An unreadable directory contributes nothing rather than failing —
/// a user with no `templates/` of their own is the common case.
pub fn available(config_dir: &Path, user: &str) -> Vec<String> {
    let dirs = [
        config_dir.join("defaults/templates"),
        config_dir.join("users").join(user).join("templates"),
    ];
    let mut names = std::collections::BTreeSet::new();
    for entry in dirs.iter().filter_map(|d| std::fs::read_dir(d).ok()).flatten().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "toml") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                names.insert(stem.to_string());
            }
        }
    }
    names.into_iter().collect()
}

/// The file a load of `name` for `user` actually reads: their own override when
/// it exists, the shared default otherwise.
fn effective_path(config_dir: &Path, user: &str, name: &str) -> std::path::PathBuf {
    let user_path = override_path(config_dir, user, name);
    if user_path.exists() {
        user_path
    } else {
        config_dir.join("defaults/templates").join(format!("{name}.toml"))
    }
}

fn override_path(config_dir: &Path, user: &str, name: &str) -> std::path::PathBuf {
    config_dir.join("users").join(user).join("templates").join(format!("{name}.toml"))
}

/// Sets the bell on the named entries and writes the result to `user`'s own copy
/// of the template, leaving the shared default untouched. Every change is
/// checked before anything is written, so a rejected request leaves no file
/// behind. Because the nightly job rebuilds each day from this same file, a bell
/// set here survives every rebuild.
pub fn set_alerts(
    config_dir: &Path,
    user: &str,
    name: &str,
    changes: &[(usize, bool)],
) -> Result<(), AlertError> {
    let template = Template::load(config_dir, user, name)?;
    for (index, _) in changes {
        let ev = template.events.get(*index).ok_or(AlertError::OutOfRange(*index))?;
        if ev.is_block() {
            return Err(AlertError::Block(*index));
        }
    }
    let path = effective_path(config_dir, user, name);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading template {}", path.display()))?;
    let mut doc: toml::Value = raw.parse().map_err(anyhow::Error::from)?;
    let events = doc
        .get_mut("events")
        .and_then(toml::Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("template {} has no events", path.display()))?;
    for (index, alert) in changes {
        let entry = events[*index]
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("template entry {index} is not a table"))?;
        entry.insert("alert".into(), toml::Value::Boolean(*alert));
    }
    let out = override_path(config_dir, user, name);
    std::fs::create_dir_all(out.parent().expect("a template file always has a parent"))
        .map_err(anyhow::Error::from)?;
    crate::context::write_atomic(&out, &toml::to_string(&doc).map_err(anyhow::Error::from)?)
        .map_err(anyhow::Error::from)?;
    Ok(())
}

impl Template {
    pub fn rows(&self) -> Vec<ScheduleRow> {
        self.events
            .iter()
            .enumerate()
            .map(|(index, ev)| ScheduleRow {
                index,
                kind: ev.kind.clone(),
                entry: ev.entry,
                time: ev.time.clone(),
                end_time: Some(ev.end()),
                days: ev.days.clone(),
                flexibility: ev.flexibility().to_string(),
                slide_window_min: ev.slide_window_min(),
                channel: ev.channel.clone(),
                alert: ev.alert(),
            })
            .collect()
    }

    /// Loads a named template for `user`, preferring a per-user override
    /// under `config_dir/users/<user>/templates/` and falling back to
    /// `config_dir/defaults/templates/` when no override exists.
    pub fn load(config_dir: &Path, user: &str, name: &str) -> Result<Self> {
        let path = effective_path(config_dir, user, name);
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading template {}", path.display()))?;
        let template: Template = toml::from_str(&raw)?;
        template.validate(&path)?;
        Ok(template)
    }

    /// Rejects event fields the rest of the pipeline cannot act on: an
    /// unrecognized day silently drops the event from every plan, and a bad time
    /// or flexibility only surfaces later as a runner error or a rejected insert.
    fn validate(&self, path: &Path) -> Result<()> {
        for ev in &self.events {
            let bad = |field: &str, value: &str| {
                anyhow::anyhow!("template {}: invalid {field} {value:?}", path.display())
            };
            if !valid_time(&ev.time) {
                return Err(bad("time", &ev.time));
            }
            if let Some(d) = ev.days.iter().find(|d| !DAYS.contains(&d.as_str())) {
                return Err(bad("day", d));
            }
            if !FLEXIBILITIES.contains(&ev.flexibility()) {
                return Err(bad("flexibility", ev.flexibility()));
            }
            if !CHANNELS.contains(&ev.channel.as_str()) {
                return Err(bad("channel", &ev.channel));
            }
            if ev.slide_window_min() < 0 {
                return Err(bad("slide_window_min", &ev.slide_window_min().to_string()));
            }
            if ev.kind.trim().is_empty() {
                return Err(bad("kind", &ev.kind));
            }
            if let Some(end) = ev.end_time.as_deref() {
                if !valid_time(end) || end <= ev.time.as_str() {
                    return Err(bad("end_time", end));
                }
            }
            if ev.is_block() {
                if ev.end_time.is_none() {
                    return Err(bad("end_time", "(a block needs a start and an end)"));
                }
                if ev.flexibility.is_some() {
                    return Err(bad("flexibility", "(a block is always reshapable)"));
                }
                if ev.slide_window_min.is_some() {
                    return Err(bad("slide_window_min", "(a block is always reshapable)"));
                }
                if ev.alert.is_some() {
                    return Err(bad("alert", "(a block never pings)"));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn loads_user_template_over_default() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='08:00'\ndays=['mon']\n");
        write(tmp.path(), "users/aki/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='10:00'\ndays=['mon']\nflexibility='slide'\n");
        let t = Template::load(tmp.path(), "aki", "default").unwrap();
        assert_eq!(t.events[0].time, "10:00");
    }

    #[test]
    fn unpadded_time_fails_to_load() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='9:00'\ndays=['mon']\n");
        let err = Template::load(tmp.path(), "aki", "default").unwrap_err().to_string();
        assert!(err.contains("\"9:00\""), "unexpected error: {err}");
        assert!(err.contains("default.toml"), "unexpected error: {err}");
    }

    #[test]
    fn miscased_day_fails_to_load() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='09:00'\ndays=['Mon']\n");
        let err = Template::load(tmp.path(), "aki", "default").unwrap_err().to_string();
        assert!(err.contains("\"Mon\""), "unexpected error: {err}");
    }

    #[test]
    fn unknown_flexibility_fails_to_load() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='09:00'\ndays=['mon']\nflexibility='soft'\n");
        let err = Template::load(tmp.path(), "aki", "default").unwrap_err().to_string();
        assert!(err.contains("\"soft\""), "unexpected error: {err}");
    }

    #[test]
    fn unknown_channel_negative_window_and_empty_kind_fail_to_load() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='09:00'\ndays=['mon']\nchannel='sms'\n");
        let err = Template::load(tmp.path(), "aki", "default").unwrap_err().to_string();
        assert!(err.contains("\"sms\""), "unexpected error: {err}");

        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='09:00'\ndays=['mon']\nslide_window_min=-5\n");
        let err = Template::load(tmp.path(), "aki", "default").unwrap_err().to_string();
        assert!(err.contains("slide_window_min"), "unexpected error: {err}");

        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nkind=''\ntime='09:00'\ndays=['mon']\n");
        assert!(Template::load(tmp.path(), "aki", "default").is_err());
    }

    #[test]
    fn a_block_carries_a_range_never_pings_and_is_always_reshapable() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nentry='block'\nkind='Work time'\ntime='09:30'\nend_time='12:30'\ndays=['mon']\n");
        let t = Template::load(tmp.path(), "aki", "default").unwrap();
        let ev = &t.events[0];
        assert!(ev.is_block());
        assert_eq!(ev.end_time.as_deref(), Some("12:30"));
        assert_eq!(ev.flexibility(), "slide");
        assert_eq!(ev.slide_window_min(), 0);
        assert!(!ev.alert());
    }

    #[test]
    fn a_routine_defaults_to_pinging_and_keeps_its_flexibility() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='09:00'\ndays=['mon']\nflexibility='slide'\nslide_window_min=30\n");
        let ev = &Template::load(tmp.path(), "aki", "default").unwrap().events[0];
        assert!(!ev.is_block());
        assert!(ev.alert());
        assert_eq!(ev.flexibility(), "slide");
        assert_eq!(ev.slide_window_min(), 30);

        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='09:00'\ndays=['mon']\nalert=false\n");
        let ev = &Template::load(tmp.path(), "aki", "default").unwrap().events[0];
        assert!(!ev.alert());
        assert_eq!(ev.flexibility(), "fixed");
    }

    #[test]
    fn malformed_entries_fail_to_load() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = |toml: &str| -> String {
            write(tmp.path(), "defaults/templates/default.toml", toml);
            Template::load(tmp.path(), "aki", "default").unwrap_err().to_string()
        };
        assert!(bad("[[events]]\nentry='block'\nkind='Work'\ntime='09:30'\ndays=['mon']\n")
            .contains("end_time"));
        assert!(bad("[[events]]\nentry='block'\nkind='Work'\ntime='09:30'\nend_time='9:30'\ndays=['mon']\n")
            .contains("end_time"));
        assert!(bad("[[events]]\nentry='block'\nkind='Work'\ntime='12:30'\nend_time='09:30'\ndays=['mon']\n")
            .contains("end_time"));
        assert!(bad("[[events]]\nentry='block'\nkind='Work'\ntime='09:30'\nend_time='12:30'\ndays=['mon']\nalert=true\n")
            .contains("alert"));
        assert!(bad("[[events]]\nentry='block'\nkind='Work'\ntime='09:30'\nend_time='12:30'\ndays=['mon']\nflexibility='fixed'\n")
            .contains("flexibility"));
        assert!(bad("[[events]]\nentry='band'\nkind='Work'\ntime='09:30'\ndays=['mon']\n")
            .contains("entry"));
    }

    #[test]
    fn toggling_a_bell_writes_a_user_override_and_leaves_the_default_alone() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/templates/default.toml", concat!(
            "[[events]]\nkind='meds'\ntime='08:00'\ndays=['mon']\n",
            "[[events]]\nentry='block'\nkind='Work time'\ntime='09:30'\nend_time='12:30'\ndays=['mon']\n"));
        set_alerts(tmp.path(), "aki", "default", &[(0, false)]).unwrap();
        let t = Template::load(tmp.path(), "aki", "default").unwrap();
        assert!(!t.events[0].alert());
        assert!(t.events[1].is_block());
        let default =
            std::fs::read_to_string(tmp.path().join("defaults/templates/default.toml")).unwrap();
        assert!(!default.contains("alert"), "the shared default was rewritten: {default}");

        set_alerts(tmp.path(), "aki", "default", &[(0, true)]).unwrap();
        assert!(Template::load(tmp.path(), "aki", "default").unwrap().events[0].alert());

        assert!(matches!(
            set_alerts(tmp.path(), "aki", "default", &[(1, false)]),
            Err(AlertError::Block(1))
        ));
        assert!(matches!(
            set_alerts(tmp.path(), "aki", "default", &[(9, false)]),
            Err(AlertError::OutOfRange(9))
        ));
    }

    #[test]
    fn rows_describe_every_entry_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/templates/default.toml", concat!(
            "[[events]]\nkind='meds'\ntime='08:00'\ndays=['mon']\n",
            "[[events]]\nentry='block'\nkind='Work time'\ntime='09:30'\nend_time='12:30'\ndays=['mon']\n"));
        let rows = Template::load(tmp.path(), "aki", "default").unwrap().rows();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].index, rows[0].entry, rows[0].alert), (0, Entry::Routine, true));
        assert_eq!(rows[0].flexibility, "fixed");
        assert_eq!(rows[0].end_time.as_deref(), Some("08:15"));
        assert_eq!((rows[1].index, rows[1].entry, rows[1].alert), (1, Entry::Block, false));
        assert_eq!(rows[1].end_time.as_deref(), Some("12:30"));
    }

    #[test]
    fn a_routine_may_carry_an_end_time_and_defaults_to_fifteen_minutes() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nkind='meds'\ntime='08:00'\ndays=['mon']\n\
             [[events]]\nkind='walk'\ntime='18:00'\nend_time='18:45'\ndays=['mon']\n");
        let t = Template::load(tmp.path(), "aki", "default").unwrap();
        assert_eq!(t.events[0].span_min(), 15);
        assert_eq!(t.events[1].span_min(), 45);
        let rows = t.rows();
        assert_eq!(rows[0].end_time.as_deref(), Some("08:15"));
        assert_eq!(rows[1].end_time.as_deref(), Some("18:45"));
        assert_eq!(rows[0].entry, Entry::Routine);
    }

    #[test]
    fn a_routine_end_before_its_start_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = |toml: &str| -> String {
            write(tmp.path(), "defaults/templates/default.toml", toml);
            Template::load(tmp.path(), "aki", "default").unwrap_err().to_string()
        };
        assert!(bad("[[events]]\nkind='nudge'\ntime='09:00'\nend_time='08:30'\ndays=['mon']\n")
            .contains("end_time"));
        assert!(bad("[[events]]\nkind='nudge'\ntime='09:00'\nend_time='9:30'\ndays=['mon']\n")
            .contains("end_time"));
    }

    #[test]
    fn wall_add_pads_and_clamps() {
        assert_eq!(wall_add("09:00", 15), "09:15");
        assert_eq!(wall_add("23:50", 30), "23:59");
        assert_eq!(wall_add("08:05", 55), "09:00");
    }

    #[test]
    fn out_of_range_time_fails_to_load() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='24:00'\ndays=['mon']\n");
        assert!(Template::load(tmp.path(), "aki", "default").is_err());
    }
}
