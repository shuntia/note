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
    #[serde(default)]
    pub agent: AgentConfig,
    /// Absent turns the `web_search` tool off: no session is offered it.
    #[serde(default)]
    pub search: Option<SearchConfig>,
    #[serde(default)]
    pub inbox: InboxConfig,
    #[serde(default)]
    pub voice: Option<VoiceConfig>,
}

/// Present turns the voice link on: Note listens on `socket` for the voice
/// service.
#[derive(Debug, Clone, Deserialize)]
pub struct VoiceConfig {
    pub socket: PathBuf,
}

/// `NOTE_DEFAULT_WEB_DIR` at build time bakes in an install-specific location
/// (the Nix package points it at its own `share/note/web`).
fn default_web_dir() -> PathBuf {
    PathBuf::from(option_env!("NOTE_DEFAULT_WEB_DIR").unwrap_or("web/dist"))
}

/// Shipped defaults (`user.toml`, `prompts/`, `templates/`): `NOTE_DEFAULTS_DIR`
/// when set, so a packaged install keeps them apart from per-user state.
pub fn defaults_dir(config_dir: &Path) -> PathBuf {
    std::env::var_os("NOTE_DEFAULTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| config_dir.join("defaults"))
}

fn default_secrets_dir() -> PathBuf {
    PathBuf::from("persist/secrets")
}

impl ServerConfig {
    pub fn load(config_dir: &Path) -> anyhow::Result<Self> {
        Self::load_file(&config_dir.join("server.toml"))
    }

    pub fn load_file(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(toml::from_str(&raw)?)
    }

    /// How long a conversation must have been quiet before it is summarised.
    /// A summary replays the thread, so it is worth nothing once the
    /// provider's prompt cache has expired: whichever is shorter wins.
    pub fn idle_summary_min(&self) -> u32 {
        let idle = self.agent.idle_summary_min;
        match self.providers.llm.as_ref().and_then(|l| l.cache_ttl_min) {
            Some(ttl) => idle.min(ttl),
            None => idle,
        }
    }
}

/// The background passes that spend model tokens on their own schedule.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Minutes of quiet after which a conversation is summarised; 0 turns the
    /// pass off.
    pub idle_summary_min: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self { idle_summary_min: 60 }
    }
}

/// The SearXNG instance the `web_search` tool queries.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchConfig {
    pub searxng_url: String,
    #[serde(default = "default_search_max_results")]
    pub max_results: usize,
    #[serde(default = "default_search_timeout")]
    pub timeout_secs: u64,
}

pub const DEFAULT_SEARCH_MAX_RESULTS: usize = 8;
pub const DEFAULT_SEARCH_TIMEOUT_SECS: u64 = 15;

fn default_search_max_results() -> usize {
    DEFAULT_SEARCH_MAX_RESULTS
}

fn default_search_timeout() -> u64 {
    DEFAULT_SEARCH_TIMEOUT_SECS
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct InboxConfig {
    /// A file the host watches: writing it asks for a sync. Unset hides Refresh.
    pub refresh_signal: Option<PathBuf>,
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
    /// The same cap for a session nobody is waiting on: a nightly letter written
    /// over a full context takes longer than a chat turn may.
    #[serde(default = "default_background_timeout")]
    pub background_timeout_secs: u64,
    /// Reasoning effort asked of an OpenAI-compatible endpoint: "none" (the
    /// default), "low", "medium" or "high".
    #[serde(default)]
    pub reasoning: String,
    /// How long the endpoint keeps a prompt cached, when it caches at all.
    #[serde(default)]
    pub cache_ttl_min: Option<u32>,
}

pub const DEFAULT_PROVIDER_TIMEOUT_SECS: u64 = 45;

fn default_provider_timeout() -> u64 {
    DEFAULT_PROVIDER_TIMEOUT_SECS
}

pub const DEFAULT_BACKGROUND_TIMEOUT_SECS: u64 = 180;

fn default_background_timeout() -> u64 {
    DEFAULT_BACKGROUND_TIMEOUT_SECS
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

pub const DEFAULT_TELEGRAM_BASE_URL: &str = "https://api.telegram.org";

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramSettings {
    /// The bot token, alone on a line; the file is the only place it lives.
    pub token_file: PathBuf,
    #[serde(default = "default_telegram_base_url")]
    pub base_url: String,
}

fn default_telegram_base_url() -> String {
    DEFAULT_TELEGRAM_BASE_URL.into()
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ChannelsConfig {
    pub webpush: Option<WebPushSettings>,
    pub telegram: Option<TelegramSettings>,
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

/// Spend ceilings an operator can raise or lift.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct LimitsConfig {
    /// 0 means unlimited.
    pub agent_sessions_per_day: u32,
    /// The farthest a share link may be set to expire; the floor is one day.
    pub share_max_days: u32,
    /// The ceiling a link's own daily message cap may be raised to; the floor is one.
    pub share_messages_per_day: u32,
    /// Share links one user may hold; 0 turns share links off.
    pub shares_per_user: u32,
    /// How far from the owner's last seen place a share visit is marked distant.
    pub share_distant_km: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            agent_sessions_per_day: 200,
            share_max_days: 120,
            share_messages_per_day: 100,
            shares_per_user: 20,
            share_distant_km: 300,
        }
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
}

impl Features {
    /// A test account exercises the API and should cost nothing while idle, so
    /// it starts with every background feature off and a member with them on.
    pub fn for_category(category: &str) -> Self {
        let on = category != CATEGORY_TEST;
        Self { nightly: on, checkins: on }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserConfig {
    pub display_name: String,
    pub timezone: String,
    /// Whether the client moves `timezone` to the device's zone when they differ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone_auto: Option<bool>,
    pub template: String,
    #[serde(default = "default_nightly_time")]
    pub nightly_time: String,
    /// When the close-the-day trigger fires, zero-padded HH:MM; blank turns the
    /// ritual off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_day_time: Option<String>,
    #[serde(default = "default_true")]
    pub show_arc_between_sessions: bool,
    #[serde(default = "default_counter")]
    pub counter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nightly_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkins_enabled: Option<bool>,
    /// How many trigger points Note may lay for itself in a day before it has
    /// to ask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triggers_per_day: Option<u32>,
    /// Whether a session runs in rounds of work and break rather than straight
    /// through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pomodoro_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pomodoro_work_min: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pomodoro_break_min: Option<u32>,
    /// Whether the end of a session's planned time, and each pomodoro phase,
    /// is announced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_end_notify: Option<bool>,
    /// Which messages ring the linked phone: `urgent` or `never`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring_for: Option<String>,
}

pub const DEFAULT_TRIGGERS_PER_DAY: u32 = 4;
pub const DEFAULT_CLOSE_DAY_TIME: &str = "21:30";
pub const DEFAULT_POMODORO_WORK_MIN: u32 = 25;
pub const DEFAULT_POMODORO_BREAK_MIN: u32 = 5;
pub const RING_FOR_URGENT: &str = "urgent";
pub const RING_FOR_NEVER: &str = "never";
pub const RING_FOR: &[&str] = &[RING_FOR_URGENT, RING_FOR_NEVER];

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
        }
    }

    /// Blank where the user has turned the close of the day off.
    pub fn close_day_time(&self) -> &str {
        self.close_day_time.as_deref().unwrap_or(DEFAULT_CLOSE_DAY_TIME)
    }

    pub fn triggers_per_day(&self) -> u32 {
        match self.triggers_per_day {
            Some(n) => n,
            None => DEFAULT_TRIGGERS_PER_DAY,
        }
    }

    pub fn timezone_auto(&self) -> bool {
        self.timezone_auto.unwrap_or(true)
    }

    pub fn pomodoro_enabled(&self) -> bool {
        self.pomodoro_enabled.unwrap_or(false)
    }

    pub fn session_end_notify(&self) -> bool {
        self.session_end_notify.unwrap_or(true)
    }

    pub fn ring_for(&self) -> &str {
        self.ring_for.as_deref().unwrap_or(RING_FOR_URGENT)
    }

    pub fn pomodoro_work_min(&self) -> u32 {
        self.pomodoro_work_min.unwrap_or(DEFAULT_POMODORO_WORK_MIN)
    }

    pub fn pomodoro_break_min(&self) -> u32 {
        self.pomodoro_break_min.unwrap_or(DEFAULT_POMODORO_BREAK_MIN)
    }

    pub fn load(config_dir: &Path, user: &str) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(config_dir.join("users").join(user).join("user.toml")).ok();
        Self::from_overlay(config_dir, raw.as_deref())
    }

    /// The effective config for a user file with this content (`None` for no
    /// file), validated the same way `load` validates the file on disk.
    pub fn from_overlay(config_dir: &Path, raw: Option<&str>) -> anyhow::Result<Self> {
        let defaults: toml::Value = toml::from_str(
            &std::fs::read_to_string(defaults_dir(config_dir).join("user.toml"))
                .context("reading defaults/user.toml")?,
        )?;
        let merged = match raw {
            Some(raw) => overlay(defaults, toml::from_str(raw)?),
            None => defaults,
        };
        let cfg: UserConfig = merged.try_into()?;
        anyhow::ensure!(
            crate::templates::valid_time(&cfg.nightly_time),
            "invalid nightly_time {:?}",
            cfg.nightly_time
        );
        anyhow::ensure!(
            cfg.close_day_time().is_empty() || crate::templates::valid_time(cfg.close_day_time()),
            "invalid close_day_time {:?}",
            cfg.close_day_time()
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

    /// An operator's file outlives the server that read it, so a section this
    /// build knows nothing about is ignored rather than fatal.
    #[test]
    fn an_unknown_section_does_not_fail_the_load() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "server.toml", concat!(
            "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n",
            "[channels.smoke]\nbase_url = \"http://10.0.0.1:2586\"\n"));
        assert!(ServerConfig::load(tmp.path()).unwrap().channels.webpush.is_none());
    }

    #[test]
    fn the_idle_summary_threshold_never_outlives_the_prompt_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let base = "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n";
        write(tmp.path(), "server.toml", base);
        let cfg = ServerConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.agent.idle_summary_min, 60);
        assert_eq!(cfg.idle_summary_min(), 60);

        write(tmp.path(), "server.toml", &format!("{base}[agent]\nidle_summary_min = 120\n"));
        assert_eq!(ServerConfig::load(tmp.path()).unwrap().idle_summary_min(), 120);

        write(
            tmp.path(),
            "server.toml",
            &format!("{base}[agent]\nidle_summary_min = 120\n[providers.llm]\nkind = \"mock\"\ncache_ttl_min = 15\n"),
        );
        let cfg = ServerConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.providers.llm.as_ref().unwrap().cache_ttl_min, Some(15));
        assert_eq!(cfg.idle_summary_min(), 15);

        write(
            tmp.path(),
            "server.toml",
            &format!("{base}[agent]\nidle_summary_min = 0\n[providers.llm]\nkind = \"mock\"\ncache_ttl_min = 15\n"),
        );
        assert_eq!(ServerConfig::load(tmp.path()).unwrap().idle_summary_min(), 0);
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
    fn telegram_section_parses_with_a_default_api_base_and_is_absent_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let base = "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n";
        write(tmp.path(), "server.toml", base);
        assert!(ServerConfig::load(tmp.path()).unwrap().channels.telegram.is_none());

        write(tmp.path(), "server.toml", &format!(
            "{base}[channels.telegram]\ntoken_file = \"config/telegram.token\"\n"));
        let tg = ServerConfig::load(tmp.path()).unwrap().channels.telegram.unwrap();
        assert_eq!(tg.token_file, PathBuf::from("config/telegram.token"));
        assert_eq!(tg.base_url, DEFAULT_TELEGRAM_BASE_URL);

        write(tmp.path(), "server.toml", &format!(
            "{base}[channels.telegram]\ntoken_file = \"t\"\nbase_url = \"http://127.0.0.1:9\"\n"));
        assert_eq!(
            ServerConfig::load(tmp.path()).unwrap().channels.telegram.unwrap().base_url,
            "http://127.0.0.1:9"
        );
    }

    #[test]
    fn timezone_follows_the_device_unless_told_otherwise() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/user.toml",
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");
        let cfg = UserConfig::load(tmp.path(), "aki").unwrap();
        assert!(cfg.timezone_auto());
        cfg.save(tmp.path(), "aki").unwrap();
        let raw = std::fs::read_to_string(tmp.path().join("users/aki/user.toml")).unwrap();
        assert!(!raw.contains("timezone_auto"), "unexpected file: {raw}");

        write(tmp.path(), "users/aki/user.toml", "timezone_auto = false\n");
        assert!(!UserConfig::load(tmp.path(), "aki").unwrap().timezone_auto());
    }

    #[test]
    fn the_trigger_allowance_defaults_and_stays_out_of_an_untouched_file() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/user.toml",
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");
        let cfg = UserConfig::load(tmp.path(), "aki").unwrap();
        assert_eq!(cfg.triggers_per_day(), DEFAULT_TRIGGERS_PER_DAY);
        cfg.save(tmp.path(), "aki").unwrap();
        let raw = std::fs::read_to_string(tmp.path().join("users/aki/user.toml")).unwrap();
        assert!(!raw.contains("triggers_per_day"), "unexpected file: {raw}");

        write(tmp.path(), "users/aki/user.toml", "triggers_per_day = 7\n");
        assert_eq!(UserConfig::load(tmp.path(), "aki").unwrap().triggers_per_day(), 7);
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
        write(tmp.path(), "server.toml", &format!("{base}[limits]\nshare_max_days = 30\n"));
        let limits = ServerConfig::load(tmp.path()).unwrap().limits;
        assert_eq!(limits.share_max_days, 30);
        assert_eq!(limits.agent_sessions_per_day, 200);
        assert_eq!(limits.share_messages_per_day, 100);
        assert_eq!(limits.shares_per_user, 20);
    }

    #[test]
    fn search_section_is_absent_by_default_and_defaults_its_bounds() {
        let tmp = tempfile::tempdir().unwrap();
        let base = "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n";
        write(tmp.path(), "server.toml", base);
        assert!(ServerConfig::load(tmp.path()).unwrap().search.is_none());

        write(tmp.path(), "server.toml", &format!("{base}[search]\nsearxng_url = \"http://127.0.0.1:8888\"\n"));
        let search = ServerConfig::load(tmp.path()).unwrap().search.unwrap();
        assert_eq!(search.searxng_url, "http://127.0.0.1:8888");
        assert_eq!(search.max_results, DEFAULT_SEARCH_MAX_RESULTS);
        assert_eq!(search.timeout_secs, DEFAULT_SEARCH_TIMEOUT_SECS);

        write(tmp.path(), "server.toml", &format!(
            "{base}[search]\nsearxng_url = \"http://s:8888\"\nmax_results = 3\ntimeout_secs = 4\n"));
        let search = ServerConfig::load(tmp.path()).unwrap().search.unwrap();
        assert_eq!((search.max_results, search.timeout_secs), (3, 4));
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
    #[test]
    fn inbox_section_names_the_refresh_signal_and_defaults_to_none() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "server.toml", concat!(
            "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n",
            "[inbox]\nrefresh_signal = \"/run/note/inbox-refresh\"\n"));
        let cfg = ServerConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.inbox.refresh_signal, Some(PathBuf::from("/run/note/inbox-refresh")));
        write(tmp.path(), "server.toml",
            "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n");
        assert!(ServerConfig::load(tmp.path()).unwrap().inbox.refresh_signal.is_none());
    }
}
