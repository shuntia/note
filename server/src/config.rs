use anyhow::Context;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub bind_addr: String,
    pub public_base_url: String,
    pub data_dir: PathBuf,
}

impl ServerConfig {
    pub fn load(config_dir: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(config_dir.join("server.toml"))
            .context("reading server.toml")?;
        Ok(toml::from_str(&raw)?)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserConfig {
    pub display_name: String,
    pub timezone: String,
    pub template: String,
}

impl UserConfig {
    pub fn load(config_dir: &Path, user: &str) -> anyhow::Result<Self> {
        let defaults: toml::Value = std::fs::read_to_string(config_dir.join("defaults/user.toml"))
            .context("reading defaults/user.toml")?
            .parse()?;
        let merged = match std::fs::read_to_string(
            config_dir.join("users").join(user).join("user.toml"),
        ) {
            Ok(raw) => overlay(defaults, raw.parse()?),
            Err(_) => defaults,
        };
        Ok(merged.try_into()?)
    }
}

fn overlay(base: toml::Value, over: toml::Value) -> toml::Value {
    match (base, over) {
        (toml::Value::Table(mut b), toml::Value::Table(o)) => {
            for (k, v) in o {
                let merged = match b.remove(&k) {
                    Some(bv) => overlay(bv, v),
                    None => v,
                };
                b.insert(k, merged);
            }
            toml::Value::Table(b)
        }
        (_, over) => over,
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
    fn user_config_overlays_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/user.toml",
            "display_name = \"Someone\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");
        write(tmp.path(), "users/aki/user.toml", "timezone = \"Asia/Tokyo\"\n");
        let cfg = UserConfig::load(tmp.path(), "aki").unwrap();
        assert_eq!(cfg.timezone, "Asia/Tokyo");
        assert_eq!(cfg.display_name, "Someone");
    }

    #[test]
    fn missing_user_dir_uses_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/user.toml",
            "display_name = \"Someone\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");
        let cfg = UserConfig::load(tmp.path(), "nobody").unwrap();
        assert_eq!(cfg.timezone, "UTC");
    }
}
