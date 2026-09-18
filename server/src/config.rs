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
    #[serde(default = "default_secrets_dir")]
    pub secrets_dir: PathBuf,
    #[serde(default)]
    pub providers: ProvidersConfig,
    #[serde(default)]
    pub channels: ChannelsConfig,
    #[serde(default)]
    pub admin: AdminConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
}

/// `NOTE_DEFAULT_WEB_DIR` at build time bakes in an install-specific location
/// (the Nix package points it at its own `share/note/web`).
fn default_web_dir() -> PathBuf {
    PathBuf::from(option_env!("NOTE_DEFAULT_WEB_DIR").unwrap_or("web/dist"))
}

fn default_secrets_dir() -> PathBuf {
    PathBuf::from("persist/secrets")
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
    /// Read/write/overall cap on one chat call. The default sits under the 100 s
    /// a Cloudflare tunnel allows a response to take.
    #[serde(default = "default_provider_timeout")]
    pub timeout_secs: u64,
    /// Reasoning effort asked of an OpenAI-compatible endpoint: "none" (the
    /// default), "low", "medium" or "high".
    #[serde(default)]
    pub reasoning: String,
}

pub const DEFAULT_PROVIDER_TIMEOUT_SECS: u64 = 45;

fn default_provider_timeout() -> u64 {
    DEFAULT_PROVIDER_TIMEOUT_SECS
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

pub const DEFAULT_NTFY_TOPIC_PREFIX: &str = "note-";

#[derive(Debug, Clone, Deserialize)]
pub struct NtfySettings {
    pub base_url: String,
    /// Bearer token for an authenticated ntfy server; empty means none.
    #[serde(default)]
    pub token_file: PathBuf,
    #[serde(default = "default_topic_prefix")]
    pub topic_prefix: String,
}

fn default_topic_prefix() -> String {
    DEFAULT_NTFY_TOPIC_PREFIX.into()
}

pub const DEFAULT_TWILIO_BASE_URL: &str = "https://api.twilio.com";

#[derive(Debug, Clone, Deserialize)]
pub struct VoiceSettings {
    pub account_sid: String,
    pub auth_token_file: PathBuf,
    /// The caller id calls are placed from, in E.164.
    pub from_number: String,
    #[serde(default = "default_twilio_base_url")]
    pub base_url: String,
}

fn default_twilio_base_url() -> String {
    DEFAULT_TWILIO_BASE_URL.into()
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ChannelsConfig {
    pub webpush: Option<WebPushSettings>,
    pub ntfy: Option<NtfySettings>,
    pub voice: Option<VoiceSettings>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct AdminConfig {
    /// `require_totp` is the key's former name and still reads.
    #[serde(alias = "require_totp")]
    pub require_second_factor: bool,
    /// Relying party overrides for a deployment whose public origin is not the
    /// one browsers see; both default to `public_base_url`.
    pub rp_id: Option<String>,
    pub rp_origin: Option<String>,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self { require_second_factor: true, rp_id: None, rp_origin: None }
    }
}

/// Spend ceilings an operator can raise or lift; 0 means unlimited.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct LimitsConfig {
    pub agent_sessions_per_day: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self { agent_sessions_per_day: 200 }
    }
}

pub const CATEGORY_MEMBER: &str = "member";
pub const CATEGORY_TEST: &str = "test";
pub const CATEGORIES: [&str; 2] = [CATEGORY_MEMBER, CATEGORY_TEST];

/// The background work that spends model tokens without the user asking for it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Features {
    pub nightly: bool,
    pub checkins: bool,
    pub calls: bool,
}

impl Features {
    /// A test account exercises the API and should cost nothing while idle, so
    /// it starts with every background feature off and a member with them on.
    pub fn for_category(category: &str) -> Self {
        let on = category != CATEGORY_TEST;
        Self { nightly: on, checkins: on, calls: on }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserConfig {
    pub display_name: String,
    pub timezone: String,
    pub template: String,
    #[serde(default = "default_nightly_time")]
    pub nightly_time: String,
    #[serde(default = "default_true")]
    pub show_arc_between_sessions: bool,
    #[serde(default = "default_counter")]
    pub counter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nightly_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkins_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ntfy_topic: Option<String>,
    /// E.164; the number accountability calls ring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calls_enabled: Option<bool>,
}

fn default_nightly_time() -> String {
    "03:00".into()
}

fn default_true() -> bool {
    true
}

fn default_counter() -> String {
    "remaining".into()
}

impl UserConfig {
    /// The user's own toggles where they set one, the category's default where
    /// they did not.
    pub fn features(&self, category: &str) -> Features {
        let default = Features::for_category(category);
        Features {
            nightly: self.nightly_enabled.unwrap_or(default.nightly),
            checkins: self.checkins_enabled.unwrap_or(default.checkins),
            calls: self.calls_enabled.unwrap_or(default.calls),
        }
    }

    /// The number to ring, or `None` when the user has not given one.
    pub fn phone(&self) -> Option<&str> {
        self.phone_number.as_deref().map(str::trim).filter(|p| !p.is_empty())
    }

    /// The user's own topic where they set a non-blank one, else the server's
    /// prefix and their username.
    pub fn ntfy_topic_for(&self, prefix: &str, username: &str) -> String {
        match self.ntfy_topic.as_deref().map(str::trim) {
            Some(topic) if !topic.is_empty() => topic.to_string(),
            _ => format!("{prefix}{username}"),
        }
    }

    pub fn load(config_dir: &Path, user: &str) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(config_dir.join("users").join(user).join("user.toml")).ok();
        Self::from_overlay(config_dir, raw.as_deref())
    }

    /// The effective config for a user file with this content (`None` for no
    /// file), validated the same way `load` validates the file on disk.
    pub fn from_overlay(config_dir: &Path, raw: Option<&str>) -> anyhow::Result<Self> {
        let defaults: toml::Value = std::fs::read_to_string(config_dir.join("defaults/user.toml"))
            .context("reading defaults/user.toml")?
            .parse()?;
        let merged = match raw {
            Some(raw) => overlay(defaults, raw.parse()?),
            None => defaults,
        };
        let cfg: UserConfig = merged.try_into()?;
        anyhow::ensure!(
            crate::templates::valid_time(&cfg.nightly_time),
            "invalid nightly_time {:?}",
            cfg.nightly_time
        );
        Ok(cfg)
    }

    /// Writes every field to the user's own file, so a later edit of
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
        assert_eq!(cfg.secrets_dir, PathBuf::from("persist/secrets"));
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
    fn background_features_default_off_for_test_accounts() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/user.toml",
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");

        let cfg = UserConfig::load(tmp.path(), "aki").unwrap();
        let on = cfg.features(CATEGORY_MEMBER);
        assert!(on.nightly && on.checkins);
        let off = cfg.features(CATEGORY_TEST);
        assert!(!off.nightly && !off.checkins);

        write(tmp.path(), "users/aki/user.toml", "nightly_enabled = true\n");
        let cfg = UserConfig::load(tmp.path(), "aki").unwrap();
        let f = cfg.features(CATEGORY_TEST);
        assert!(f.nightly, "an explicit toggle beats the category default");
        assert!(!f.checkins);

        write(tmp.path(), "users/aki/user.toml", "checkins_enabled = false\n");
        assert!(!UserConfig::load(tmp.path(), "aki").unwrap().features(CATEGORY_MEMBER).checkins);
    }

    #[test]
    fn saving_keeps_untouched_toggles_on_their_category_default() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/user.toml",
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");
        let mut cfg = UserConfig::load(tmp.path(), "aki").unwrap();
        cfg.timezone = "Asia/Tokyo".into();
        cfg.save(tmp.path(), "aki").unwrap();
        let back = UserConfig::load(tmp.path(), "aki").unwrap();
        assert!(!back.features(CATEGORY_TEST).nightly);
        assert!(back.features(CATEGORY_MEMBER).nightly);
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
    fn admin_section_defaults_to_requiring_a_second_factor() {
        let tmp = tempfile::tempdir().unwrap();
        let base = "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n";
        write(tmp.path(), "server.toml", base);
        let admin = ServerConfig::load(tmp.path()).unwrap().admin;
        assert!(admin.require_second_factor);
        assert!(admin.rp_id.is_none() && admin.rp_origin.is_none());

        write(tmp.path(), "server.toml", &format!("{base}[admin]\nrequire_second_factor = false\n"));
        assert!(!ServerConfig::load(tmp.path()).unwrap().admin.require_second_factor);

        write(tmp.path(), "server.toml", &format!("{base}[admin]\nrequire_totp = false\n"));
        assert!(
            !ServerConfig::load(tmp.path()).unwrap().admin.require_second_factor,
            "the former key name still reads"
        );

        write(
            tmp.path(),
            "server.toml",
            &format!("{base}[admin]\nrp_id = \"note.example.net\"\nrp_origin = \"https://note.example.net\"\n"),
        );
        let admin = ServerConfig::load(tmp.path()).unwrap().admin;
        assert_eq!(admin.rp_id.unwrap(), "note.example.net");
        assert_eq!(admin.rp_origin.unwrap(), "https://note.example.net");
    }

    #[test]
    fn ntfy_section_parses_with_defaults_and_is_absent_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let base = "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n";
        write(tmp.path(), "server.toml", base);
        assert!(ServerConfig::load(tmp.path()).unwrap().channels.ntfy.is_none());

        write(tmp.path(), "server.toml",
            &format!("{base}[channels.ntfy]\nbase_url = \"http://10.0.0.1:2586\"\n"));
        let ntfy = ServerConfig::load(tmp.path()).unwrap().channels.ntfy.unwrap();
        assert_eq!(ntfy.base_url, "http://10.0.0.1:2586");
        assert_eq!(ntfy.topic_prefix, DEFAULT_NTFY_TOPIC_PREFIX);
        assert!(ntfy.token_file.as_os_str().is_empty());

        write(tmp.path(), "server.toml", &format!(
            "{base}[channels.ntfy]\nbase_url = \"http://x:2586\"\ntoken_file = \"/run/secrets/ntfy\"\ntopic_prefix = \"plan-\"\n"));
        let ntfy = ServerConfig::load(tmp.path()).unwrap().channels.ntfy.unwrap();
        assert_eq!(ntfy.topic_prefix, "plan-");
        assert_eq!(ntfy.token_file, PathBuf::from("/run/secrets/ntfy"));
    }

    #[test]
    fn voice_section_parses_with_a_default_api_base_and_is_absent_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let base = "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n";
        write(tmp.path(), "server.toml", base);
        assert!(ServerConfig::load(tmp.path()).unwrap().channels.voice.is_none());

        write(tmp.path(), "server.toml", &format!(
            "{base}[channels.voice]\naccount_sid = \"AC1\"\nauth_token_file = \"config/twilio.token\"\nfrom_number = \"+15005550006\"\n"));
        let voice = ServerConfig::load(tmp.path()).unwrap().channels.voice.unwrap();
        assert_eq!(voice.account_sid, "AC1");
        assert_eq!(voice.from_number, "+15005550006");
        assert_eq!(voice.auth_token_file, PathBuf::from("config/twilio.token"));
        assert_eq!(voice.base_url, DEFAULT_TWILIO_BASE_URL);

        write(tmp.path(), "server.toml", &format!(
            "{base}[channels.voice]\naccount_sid = \"AC1\"\nauth_token_file = \"t\"\nfrom_number = \"+1\"\nbase_url = \"http://127.0.0.1:9\"\n"));
        assert_eq!(
            ServerConfig::load(tmp.path()).unwrap().channels.voice.unwrap().base_url,
            "http://127.0.0.1:9"
        );
    }

    #[test]
    fn calls_follow_the_category_until_the_user_chooses() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/user.toml",
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");
        let cfg = UserConfig::load(tmp.path(), "aki").unwrap();
        assert!(cfg.phone().is_none());
        assert!(cfg.features(CATEGORY_MEMBER).calls);
        assert!(!cfg.features(CATEGORY_TEST).calls);

        write(tmp.path(), "users/aki/user.toml",
            "calls_enabled = false\nphone_number = \" +819012345678 \"\n");
        let cfg = UserConfig::load(tmp.path(), "aki").unwrap();
        assert!(!cfg.features(CATEGORY_MEMBER).calls);
        assert_eq!(cfg.phone(), Some("+819012345678"));

        write(tmp.path(), "users/aki/user.toml", "phone_number = \"\"\n");
        assert!(UserConfig::load(tmp.path(), "aki").unwrap().phone().is_none());
    }

    #[test]
    fn ntfy_topic_falls_back_to_the_prefixed_username() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/user.toml",
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");
        let cfg = UserConfig::load(tmp.path(), "aki").unwrap();
        assert!(cfg.ntfy_topic.is_none());
        assert_eq!(cfg.ntfy_topic_for("note-", "aki"), "note-aki");

        write(tmp.path(), "users/aki/user.toml", "ntfy_topic = \"my-desk\"\n");
        let cfg = UserConfig::load(tmp.path(), "aki").unwrap();
        assert_eq!(cfg.ntfy_topic_for("note-", "aki"), "my-desk");

        write(tmp.path(), "users/aki/user.toml", "ntfy_topic = \"\"\n");
        let cfg = UserConfig::load(tmp.path(), "aki").unwrap();
        assert_eq!(cfg.ntfy_topic_for("note-", "aki"), "note-aki");
    }

    #[test]
    fn an_unset_ntfy_topic_stays_out_of_the_saved_file() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/user.toml",
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");
        let cfg = UserConfig::load(tmp.path(), "aki").unwrap();
        cfg.save(tmp.path(), "aki").unwrap();
        let raw = std::fs::read_to_string(tmp.path().join("users/aki/user.toml")).unwrap();
        assert!(!raw.contains("ntfy_topic"), "unexpected file: {raw}");
    }

    #[test]
    fn a_providers_timeout_defaults_and_can_be_overridden() {
        let tmp = tempfile::tempdir().unwrap();
        let base = "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n";
        write(tmp.path(), "server.toml",
            &format!("{base}[providers.llm]\nkind = \"anthropic\"\nmodel = \"m\"\napi_key_env = \"K\"\n"));
        let llm = ServerConfig::load(tmp.path()).unwrap().providers.llm.unwrap();
        assert_eq!(llm.timeout_secs, DEFAULT_PROVIDER_TIMEOUT_SECS);
        assert_eq!(llm.timeout_secs, 45);

        write(tmp.path(), "server.toml", &format!(
            "{base}[providers.llm]\nkind = \"anthropic\"\nmodel = \"m\"\napi_key_env = \"K\"\ntimeout_secs = 90\n"));
        let llm = ServerConfig::load(tmp.path()).unwrap().providers.llm.unwrap();
        assert_eq!(llm.timeout_secs, 90);
    }

    #[test]
    fn limits_section_is_optional_and_overridable() {
        let tmp = tempfile::tempdir().unwrap();
        let base = "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n";
        write(tmp.path(), "server.toml", base);
        assert_eq!(ServerConfig::load(tmp.path()).unwrap().limits.agent_sessions_per_day, 200);
        write(tmp.path(), "server.toml", &format!("{base}[limits]\nagent_sessions_per_day = 0\n"));
        assert_eq!(ServerConfig::load(tmp.path()).unwrap().limits.agent_sessions_per_day, 0);
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
