use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub bind_addr: String,
    pub public_base_url: String,
    pub data_dir: PathBuf,
    #[serde(default = "default_web_dir")]
    pub web_dir: PathBuf,
    #[serde(default)]
    pub providers: ProvidersConfig,
    #[serde(default)]
    pub channels: ChannelsConfig,
}

fn default_web_dir() -> PathBuf {
    PathBuf::from("web/dist")
}

impl ServerConfig {
    pub fn load(config_dir: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(config_dir.join("server.toml"))
            .context("reading server.toml")?;
        Ok(toml::from_str(&raw)?)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub kind: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default)]
    pub api_key_file: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ProvidersConfig {
    pub llm: Option<ProviderConfig>,
    pub embeddings: Option<ProviderConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebPushSettings {
    pub vapid_pem_file: PathBuf,
    pub subject: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ChannelsConfig {
    pub webpush: Option<WebPushSettings>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserConfig {
    pub display_name: String,
    pub timezone: String,
    pub template: String,
    #[serde(default = "default_nightly_time")]
    pub nightly_time: String,
}

fn default_nightly_time() -> String {
    "03:00".into()
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
        let cfg: UserConfig = merged.try_into()?;
        anyhow::ensure!(
            crate::templates::valid_time(&cfg.nightly_time),
            "invalid nightly_time {:?}",
            cfg.nightly_time
        );
        Ok(cfg)
    }

    /// Writes all four fields to the user's own file, so a later edit of
    /// `defaults/user.toml` cannot move settings the user has already chosen.
    pub fn save(&self, config_dir: &Path, user: &str) -> anyhow::Result<()> {
        let path = config_dir.join("users").join(user).join("user.toml");
        std::fs::create_dir_all(path.parent().expect("user.toml always has a parent"))?;
        crate::context::write_atomic(&path, &toml::to_string(self)?)?;
        Ok(())
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

    #[test]
    fn server_config_without_providers_section_still_loads() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "server.toml",
            "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n");
        let cfg = ServerConfig::load(tmp.path()).unwrap();
        assert!(cfg.providers.llm.is_none());
    }

    #[test]
    fn nightly_time_defaults_and_validates() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/user.toml",
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");
        assert_eq!(UserConfig::load(tmp.path(), "a").unwrap().nightly_time, "03:00");
        write(tmp.path(), "users/aki/user.toml", "nightly_time = \"4:00\"\n");
        assert!(UserConfig::load(tmp.path(), "aki").is_err());
    }

    #[test]
    fn channels_section_parses_and_defaults_empty() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "server.toml", concat!(
            "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n",
            "[channels.webpush]\nvapid_pem_file = \"config/vapid.pem\"\nsubject = \"mailto:admin@example.com\"\n"));
        let cfg = ServerConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.channels.webpush.unwrap().subject, "mailto:admin@example.com");
        write(tmp.path(), "server.toml",
            "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n");
        assert!(ServerConfig::load(tmp.path()).unwrap().channels.webpush.is_none());
    }

    #[test]
    fn providers_section_parses() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "server.toml", concat!(
            "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n",
            "[providers.llm]\nkind = \"anthropic\"\nmodel = \"claude-sonnet-5\"\napi_key_env = \"ANTHROPIC_API_KEY\"\napi_key_file = \"/run/secrets/llm\"\n",
            "[providers.embeddings]\nkind = \"openai\"\nbase_url = \"http://localhost:8080/v1\"\nmodel = \"embeddinggemma\"\n"));
        let cfg = ServerConfig::load(tmp.path()).unwrap();
        let llm = cfg.providers.llm.unwrap();
        assert_eq!(llm.kind, "anthropic");
        assert_eq!(llm.api_key_file, PathBuf::from("/run/secrets/llm"));
        assert_eq!(cfg.providers.embeddings.unwrap().base_url, "http://localhost:8080/v1");
    }
}
