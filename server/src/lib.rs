pub mod admin;
pub mod agent;
pub mod allocate;
pub mod api;
pub mod auth;
pub mod calendar;
pub mod channels;
pub mod config;
pub mod context;
pub mod day;
pub mod db;
pub mod goals;
pub mod harvest;
pub mod idle;
pub mod inbox;
pub mod learn;
pub mod log;
pub mod matrix;
pub mod memory;
pub mod net;
pub mod nightly;
pub mod notes;
pub mod plan;
pub mod presence;
pub mod prompts;
pub mod providers;
pub mod push_subs;
pub mod review;
pub mod runner;
pub mod search;
pub mod security;
pub mod shares;
pub mod summaries;
pub mod talk;
pub mod tasks;
pub mod text;
pub mod tokens;
pub mod templates;
#[cfg(test)]
pub(crate) mod testhttp;
pub mod tools;
pub mod totp;
pub mod trace;
pub mod triggers;
pub mod voice;
pub mod work;

use crate::providers::{EmbeddingsProvider, LLMProvider};
use crate::search::SearchProvider;
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

pub const MAX_CONCURRENT_TALKS: usize = 4;

#[derive(Debug, Clone, Copy)]
pub enum TalkBusy {
    UserBusy,
    Full,
}

/// Caps concurrent talk sessions globally (each pins a blocking thread for up
/// to `MAX_TURNS` provider calls) and to one per user (interleaved tool calls
/// from two sessions of the same user would race).
pub struct TalkGate {
    semaphore: Arc<tokio::sync::Semaphore>,
    active: Mutex<HashSet<i64>>,
}

pub struct TalkPermit {
    gate: Arc<TalkGate>,
    user_id: i64,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl Drop for TalkPermit {
    fn drop(&mut self) {
        self.gate.active.lock().unwrap().remove(&self.user_id);
    }
}

impl Default for TalkGate {
    fn default() -> Self {
        Self::new()
    }
}

impl TalkGate {
    pub fn new() -> Self {
        Self {
            semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_TALKS)),
            active: Mutex::new(HashSet::new()),
        }
    }

    /// The server-wide slot alone, for a caller that is not one of the users
    /// the per-user rule protects.
    pub fn try_enter_global(&self) -> Result<tokio::sync::OwnedSemaphorePermit, TalkBusy> {
        self.semaphore.clone().try_acquire_owned().map_err(|_| TalkBusy::Full)
    }

    pub fn try_enter(self: &Arc<Self>, user_id: i64) -> Result<TalkPermit, TalkBusy> {
        if !self.active.lock().unwrap().insert(user_id) {
            return Err(TalkBusy::UserBusy);
        }
        if let Ok(permit) = self.semaphore.clone().try_acquire_owned() { Ok(TalkPermit { gate: self.clone(), user_id, _permit: permit }) } else {
            self.active.lock().unwrap().remove(&user_id);
            Err(TalkBusy::Full)
        }
    }
}

/// A lock poisoned by a panic elsewhere is not a reason to fail every later
/// request: the panicking scope already reported itself, the connection behind
/// the lock is intact, and refusing it would turn one bug into a dead server.
pub fn db_guard(db: &Mutex<Connection>) -> MutexGuard<'_, Connection> {
    db.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub llm: Arc<dyn LLMProvider>,
    pub embeddings: Option<Arc<dyn EmbeddingsProvider>>,
    /// Set when a search instance is configured; without one no session is
    /// offered the `web_search` tool.
    pub search: Option<Arc<dyn SearchProvider>>,
    pub vapid_public_key: Option<String>,
    pub matrix: Option<Arc<crate::channels::matrix::MatrixChannel>>,
    pub voice: Option<Arc<crate::voice::Voice>>,
    /// The model a call speaks on; `None` speaks on `llm`.
    pub voice_llm: Option<Arc<dyn LLMProvider>>,
    pub call_settings: crate::voice::call::CallSettings,
    pub hub: Arc<crate::channels::ws::ClientHub>,
    pub channels: Vec<Arc<dyn crate::channels::Channel>>,
    pub secure_cookies: bool,
    pub login_limiter: Arc<crate::auth::LoginLimiter>,
    /// Bounds concurrent password hashes; see `auth::MAX_CONCURRENT_LOGINS`.
    pub login_slots: Arc<tokio::sync::Semaphore>,
    pub talk_gate: Arc<TalkGate>,
    pub admin_limiter: Arc<crate::auth::LoginLimiter>,
    /// Caps password re-checks on the enrolment routes.
    pub security_limiter: Arc<crate::auth::LoginLimiter>,
    pub admin_secrets: Arc<crate::admin::AdminSecrets>,
    pub passkeys: Arc<crate::security::PasskeyService>,
    pub providers_info: crate::admin::ProvidersInfo,
    /// Per-user agent sessions allowed in any 24 h; 0 lifts the ceiling.
    pub agent_sessions_per_day: u32,
    /// Minutes of quiet after which a conversation is summarised; 0 turns the
    /// pass off.
    pub idle_summary_min: u32,
    pub share_max_days: u32,
    pub share_messages_per_day: u32,
    pub shares_per_user: u32,
    pub share_distant_km: u32,
    /// Counts unknown-token lookups and message posts per client address.
    pub share_limiter: Arc<crate::auth::LoginLimiter>,
    /// The origin a share URL is built on; `server.toml`'s `public_base_url`.
    pub public_base_url: String,
    /// Written by `POST /api/inbox/refresh`; `[inbox] refresh_signal`.
    pub inbox_refresh: Option<PathBuf>,
    pub seen_langs: Arc<crate::text::SeenLangs>,
    pub started_at: jiff::Timestamp,
}

impl AppState {
    /// Providers default to an unscripted mock and no embeddings, so a state
    /// built without `with_providers` is still fully runnable.
    pub fn new(conn: Connection, config_dir: PathBuf, data_dir: PathBuf) -> Self {
        let hub = Arc::new(crate::channels::ws::ClientHub::new());
        let limits = crate::config::LimitsConfig::default();
        let ws: Arc<dyn crate::channels::Channel> =
            Arc::new(crate::channels::ws::WsChannel::new(hub.clone()));
        Self {
            db: Arc::new(Mutex::new(conn)),
            config_dir,
            data_dir,
            llm: Arc::new(crate::providers::mock::MockLLM::empty()),
            embeddings: None,
            search: None,
            vapid_public_key: None,
            matrix: None,
            voice: None,
            voice_llm: None,
            call_settings: crate::voice::call::CallSettings::default(),
            hub,
            channels: vec![ws],
            secure_cookies: false,
            login_limiter: Arc::new(crate::auth::LoginLimiter::new()),
            login_slots: Arc::new(tokio::sync::Semaphore::new(
                crate::auth::MAX_CONCURRENT_LOGINS,
            )),
            talk_gate: Arc::new(TalkGate::new()),
            admin_limiter: Arc::new(crate::auth::LoginLimiter::new()),
            security_limiter: Arc::new(crate::auth::LoginLimiter::new()),
            admin_secrets: Arc::new(crate::admin::AdminSecrets::default()),
            passkeys: Arc::new(crate::security::PasskeyService::default()),
            providers_info: crate::admin::ProvidersInfo::default(),
            agent_sessions_per_day: limits.agent_sessions_per_day,
            idle_summary_min: crate::config::AgentConfig::default().idle_summary_min,
            share_max_days: limits.share_max_days,
            share_messages_per_day: limits.share_messages_per_day,
            shares_per_user: limits.shares_per_user,
            share_distant_km: limits.share_distant_km,
            share_limiter: Arc::new(crate::auth::LoginLimiter::with_limit(
                crate::shares::ADDRESS_ATTEMPTS,
            )),
            public_base_url: "http://localhost:3271".into(),
            inbox_refresh: None,
            seen_langs: Arc::default(),
            started_at: jiff::Timestamp::now(),
        }
    }

    pub fn db(&self) -> MutexGuard<'_, Connection> {
        db_guard(&self.db)
    }

    #[must_use]
    pub fn with_admin_secrets(mut self, secrets: crate::admin::AdminSecrets) -> Self {
        self.admin_secrets = Arc::new(secrets);
        self
    }

    #[must_use]
    pub fn with_passkeys(mut self, passkeys: crate::security::PasskeyService) -> Self {
        self.passkeys = Arc::new(passkeys);
        self
    }

    #[must_use]
    pub fn with_limits(mut self, limits: &crate::config::LimitsConfig) -> Self {
        self.agent_sessions_per_day = limits.agent_sessions_per_day;
        self.share_max_days = limits.share_max_days;
        self.share_messages_per_day = limits.share_messages_per_day;
        self.shares_per_user = limits.shares_per_user;
        self.share_distant_km = limits.share_distant_km;
        self
    }

    #[must_use]
    pub fn with_public_base_url(mut self, url: &str) -> Self {
        self.public_base_url = url.trim_end_matches('/').to_string();
        self
    }

    #[must_use]
    pub fn with_idle_summary_min(mut self, minutes: u32) -> Self {
        self.idle_summary_min = minutes;
        self
    }

    #[must_use]
    pub fn with_inbox_refresh(mut self, path: Option<PathBuf>) -> Self {
        self.inbox_refresh = path;
        self
    }

    #[must_use]
    pub fn with_providers_info(mut self, info: crate::admin::ProvidersInfo) -> Self {
        self.providers_info = info;
        self
    }

    #[must_use]
    pub fn with_providers(
        mut self,
        llm: Arc<dyn LLMProvider>,
        embeddings: Option<Arc<dyn EmbeddingsProvider>>,
    ) -> Self {
        self.llm = llm;
        self.embeddings = embeddings;
        self
    }

    #[must_use]
    pub fn with_search(mut self, search: Arc<dyn SearchProvider>) -> Self {
        self.search = Some(search);
        self
    }

    #[must_use]
    pub fn with_webpush(
        mut self,
        ch: crate::channels::webpush::WebPushChannel,
        public_key: String,
    ) -> Self {
        self.channels.push(Arc::new(ch));
        self.vapid_public_key = Some(public_key);
        self
    }

    /// Right after the voice channel in the ladder, which `with_voice` makes
    /// true by inserting itself at the front afterwards.
    #[must_use]
    pub fn with_matrix(mut self, ch: crate::channels::matrix::MatrixChannel) -> Self {
        let ch = Arc::new(ch);
        self.channels.insert(0, ch.clone());
        self.matrix = Some(ch);
        self
    }

    #[must_use]
    pub fn with_voice_llm(mut self, llm: Arc<dyn LLMProvider>, settings: crate::voice::call::CallSettings) -> Self {
        self.voice_llm = Some(llm);
        self.call_settings = settings;
        self
    }

    /// Must come after every other channel, the providers and search: the
    /// channels present now are what a rung message falls through to, and a
    /// call talks with the providers and search present now.
    #[must_use]
    pub fn with_voice(mut self, voice: Arc<crate::voice::Voice>) -> Self {
        voice.set_fallback(self.channels.clone());
        voice.set_calls(crate::voice::call::CallDeps {
            db: self.db.clone(),
            config_dir: self.config_dir.clone(),
            data_dir: self.data_dir.clone(),
            llm: self.llm.clone(),
            voice_llm: self.voice_llm.clone().unwrap_or_else(|| self.llm.clone()),
            embeddings: self.embeddings.clone(),
            search: self.search.clone(),
            settings: self.call_settings.clone(),
        });
        let ch = crate::channels::voice::VoiceChannel::new(voice.clone(), self.db.clone(), self.config_dir.clone());
        self.channels.insert(0, Arc::new(ch));
        self.voice = Some(voice);
        self
    }

    #[must_use]
    pub fn with_channels(mut self, channels: Vec<Arc<dyn crate::channels::Channel>>) -> Self {
        self.channels = channels;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One panicked request must not take the database down with it for the
    /// life of the process.
    #[test]
    fn a_poisoned_lock_still_hands_out_the_connection() {
        let tmp = tempfile::tempdir().unwrap();
        let state = AppState::new(
            crate::db::open_memory().unwrap(),
            tmp.path().to_path_buf(),
            tmp.path().to_path_buf(),
        );
        let db = state.db.clone();
        let _ = std::thread::spawn(move || {
            let _guard = db.lock().unwrap();
            panic!("a handler died holding the lock");
        })
        .join();
        assert!(state.db.lock().is_err(), "the lock must really be poisoned");
        assert_eq!(
            state.db().query_row("SELECT 1", [], |r| r.get::<_, i64>(0)).unwrap(),
            1
        );
    }

    #[test]
    fn talk_gate_blocks_same_user_and_caps_total() {
        let gate = Arc::new(TalkGate::new());
        let p1 = gate.try_enter(1).unwrap();
        assert!(matches!(gate.try_enter(1), Err(TalkBusy::UserBusy)));
        let _p2 = gate.try_enter(2).unwrap();
        let _p3 = gate.try_enter(3).unwrap();
        let _p4 = gate.try_enter(4).unwrap();
        assert!(matches!(gate.try_enter(5), Err(TalkBusy::Full)));
        drop(p1);
        let _p5 = gate.try_enter(1).unwrap();
    }
}
