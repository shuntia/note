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
        Ok(toml::from_str(&raw)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_user_template_over_default() {
        let tmp = tempfile::tempdir().unwrap();
        let write = |rel: &str, c: &str| {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        };
        write("defaults/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='08:00'\ndays=['mon']\n");
        write("users/aki/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='10:00'\ndays=['mon']\n");
        let t = Template::load(tmp.path(), "aki", "default").unwrap();
        assert_eq!(t.events[0].time, "10:00");
    }
}
