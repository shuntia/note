use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Template {
    pub events: Vec<TemplateEvent>,
}

#[derive(Debug, Deserialize)]
pub struct TemplateEvent {
    pub kind: String,
    pub time: String,
    pub days: Vec<String>,
    #[serde(default = "default_flexibility")]
    pub flexibility: String,
    #[serde(default)]
    pub slide_window_min: i64,
    #[serde(default = "default_channel")]
    pub channel: String,
}

fn default_flexibility() -> String { "fixed".into() }
fn default_channel() -> String { "push".into() }

const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
const FLEXIBILITIES: [&str; 3] = ["fixed", "slide", "drop"];

/// Zero-padded 24-hour `HH:MM`; the padding matters because wall times are
/// compared and sorted as strings once stored.
fn valid_time(s: &str) -> bool {
    let Some((h, m)) = s.split_once(':') else {
        return false;
    };
    let padded = |p: &str| p.len() == 2 && p.bytes().all(|b| b.is_ascii_digit());
    padded(h)
        && padded(m)
        && h.parse::<u32>().is_ok_and(|h| h < 24)
        && m.parse::<u32>().is_ok_and(|m| m < 60)
}

impl Template {
    /// Loads a named template for `user`, preferring a per-user override
    /// under `config_dir/users/<user>/templates/` and falling back to
    /// `config_dir/defaults/templates/` when no override exists.
    pub fn load(config_dir: &Path, user: &str, name: &str) -> Result<Self> {
        let user_path = config_dir.join("users").join(user).join("templates").join(format!("{name}.toml"));
        let default_path = config_dir.join("defaults/templates").join(format!("{name}.toml"));
        let path = if user_path.exists() { user_path } else { default_path };
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
            if !FLEXIBILITIES.contains(&ev.flexibility.as_str()) {
                return Err(bad("flexibility", &ev.flexibility));
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
    fn out_of_range_time_fails_to_load() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='24:00'\ndays=['mon']\n");
        assert!(Template::load(tmp.path(), "aki", "default").is_err());
    }
}
