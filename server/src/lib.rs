pub mod admin;
pub mod agent;
pub mod api;
pub mod auth;
pub mod channels;
pub mod config;
pub mod context;
pub mod db;
pub mod log;
pub mod memory;
pub mod nightly;
pub mod plan;
pub mod prompts;
pub mod providers;
pub mod push_subs;
pub mod runner;
pub mod talk;
pub mod tasks;
pub mod tokens;
pub mod templates;
pub mod tools;
pub mod totp;

use crate::providers::{EmbeddingsProvider, LLMProvider};
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const MAX_CONCURRENT_TALKS: usize = 4;
pub const EMPTY_REPLY_FALLBACK: &str = "(the assistant is not configured on this server)";

#[derive(Debug)]
pub enum TalkBusy {
    UserBusy,
    Full,
}

/// Caps concurrent talk sessions globally (each pins a blocking thread for up
/// to MAX_TURNS provider calls) and to one per user (interleaved tool calls
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

    pub fn try_enter(self: &Arc<Self>, user_id: i64) -> Result<TalkPermit, TalkBusy> {
        if !self.active.lock().unwrap().insert(user_id) {
            return Err(TalkBusy::UserBusy);
        }
        match self.semaphore.clone().try_acquire_owned() {
            Ok(permit) => Ok(TalkPermit { gate: self.clone(), user_id, _permit: permit }),
            Err(_) => {
                self.active.lock().unwrap().remove(&user_id);
                Err(TalkBusy::Full)
            }
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub llm: Arc<dyn LLMProvider>,
    pub embeddings: Option<Arc<dyn EmbeddingsProvider>>,
    pub vapid_public_key: Option<String>,
    /// Set when the ntfy channel is configured; also the prefix its default
    /// topics are built from.
    pub ntfy_topic_prefix: Option<String>,
    pub hub: Arc<crate::channels::ws::ClientHub>,
    pub channels: Vec<Arc<dyn crate::channels::Channel>>,
    pub secure_cookies: bool,
    pub login_limiter: Arc<crate::auth::LoginLimiter>,
    pub talk_gate: Arc<TalkGate>,
    pub admin_limiter: Arc<crate::auth::LoginLimiter>,
    pub admin_secrets: Arc<crate::admin::AdminSecrets>,
    pub providers_info: crate::admin::ProvidersInfo,
    pub started_at: jiff::Timestamp,
}

impl AppState {
    /// Providers default to an unscripted mock and no embeddings, so a state
    /// built without `with_providers` is still fully runnable.
    pub fn new(conn: Connection, config_dir: PathBuf, data_dir: PathBuf) -> Self {
        let hub = Arc::new(crate::channels::ws::ClientHub::new());
        let ws: Arc<dyn crate::channels::Channel> =
            Arc::new(crate::channels::ws::WsChannel::new(hub.clone()));
        Self {
            db: Arc::new(Mutex::new(conn)),
            config_dir,
            data_dir,
            llm: Arc::new(crate::providers::mock::MockLLM::empty()),
            embeddings: None,
            vapid_public_key: None,
            ntfy_topic_prefix: None,
            hub,
            channels: vec![ws],
            secure_cookies: false,
            login_limiter: Arc::new(crate::auth::LoginLimiter::new()),
            talk_gate: Arc::new(TalkGate::new()),
            admin_limiter: Arc::new(crate::auth::LoginLimiter::new()),
            admin_secrets: Arc::new(crate::admin::AdminSecrets::default()),
            providers_info: crate::admin::ProvidersInfo::default(),
            started_at: jiff::Timestamp::now(),
        }
    }

    pub fn with_admin_secrets(mut self, secrets: crate::admin::AdminSecrets) -> Self {
        self.admin_secrets = Arc::new(secrets);
        self
    }

    pub fn with_providers_info(mut self, info: crate::admin::ProvidersInfo) -> Self {
        self.providers_info = info;
        self
    }

    pub fn with_providers(
        mut self,
        llm: Arc<dyn LLMProvider>,
        embeddings: Option<Arc<dyn EmbeddingsProvider>>,
    ) -> Self {
        self.llm = llm;
        self.embeddings = embeddings;
        self
    }

    pub fn with_webpush(
        mut self,
        ch: crate::channels::webpush::WebPushChannel,
        public_key: String,
    ) -> Self {
        self.channels.push(Arc::new(ch));
        self.vapid_public_key = Some(public_key);
        self
    }

    pub fn with_ntfy(
        mut self,
        ch: crate::channels::ntfy::NtfyChannel,
        topic_prefix: String,
    ) -> Self {
        self.channels.push(Arc::new(ch));
        self.ntfy_topic_prefix = Some(topic_prefix);
        self
    }

    pub fn with_channels(mut self, channels: Vec<Arc<dyn crate::channels::Channel>>) -> Self {
        self.channels = channels;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
