//! Each user's own LLM key: sealed at rest under the server's key file, and
//! made into the providers that user's sessions and calls run on.

use crate::failure::Reason;
use crate::providers::{openai::OpenAILLM, ChatAgents, ChatRequest, ChatResponse, Effort, LLMProvider, ProviderError, StreamOpts, StreamSink};
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::Aes256Gcm;
use anyhow::{Context, Result};
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

const KEY_FILE: &str = "llm_keys";
pub const SEEDED_SETTING: &str = "llm_keys_seeded";
pub const OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";
pub const OPENAI_BASE: &str = "https://api.openai.com/v1";
pub const MAX_KEY_LEN: usize = 512;
pub const MAX_MODEL_LEN: usize = 200;
/// How many of an `OpenAI` key's models are offered besides the one it names.
const OPENAI_MODELS: usize = 11;
const VERIFY_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyKind {
    Openrouter,
    Openai,
}

impl KeyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyKind::Openrouter => "openrouter",
            KeyKind::Openai => "openai",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [KeyKind::Openrouter, KeyKind::Openai].into_iter().find(|k| k.as_str() == s)
    }
}

/// What the API shows of a saved key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyInfo {
    pub kind: KeyKind,
    pub model: String,
    pub key_hint: String,
}

/// Why a key was not saved.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    Key,
    Model,
    Unreachable(String),
}

/// The providers a user with a key of their own runs on.
pub struct Own {
    pub llm: Arc<dyn LLMProvider>,
    pub voice: Arc<dyn LLMProvider>,
}

/// kind, model, models, nonce, sealed key
type SealedRow = (String, String, String, Vec<u8>, Vec<u8>);

enum Secret {
    File(PathBuf),
    Fixed([u8; 32]),
}

pub struct OwnKeys {
    secret: Secret,
    loaded: Mutex<Option<[u8; 32]>>,
    openrouter_base: String,
    openai_base: String,
    timeouts: (u64, u64),
    voice: Option<crate::config::VoiceConfig>,
    /// A user with no key runs on the server's provider; only a state built
    /// without `with_own_keys`, as the tests build it, allows this.
    server_stands_in: bool,
    built: Mutex<HashMap<i64, Option<Arc<Own>>>>,
}

impl OwnKeys {
    /// Seals under `secrets_dir/llm_keys`, written with a fresh key on first use.
    pub fn new(secrets_dir: &Path, providers: &crate::config::ProvidersConfig, voice: Option<&crate::config::VoiceConfig>) -> Self {
        let timeouts = providers.llm.as_ref().map_or(
            (crate::config::DEFAULT_PROVIDER_TIMEOUT_SECS, crate::config::DEFAULT_BACKGROUND_TIMEOUT_SECS),
            |p| (p.timeout_secs, p.background_timeout_secs),
        );
        Self { timeouts, voice: voice.cloned(), ..Self::with_secret(Secret::File(secrets_dir.join(KEY_FILE))) }
    }

    /// Seals under a key that lives only as long as the process.
    pub fn ephemeral() -> Self {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).expect("os rng");
        Self::with_secret(Secret::Fixed(key))
    }

    /// `ephemeral`, with a user who has no key running on the server's provider.
    pub fn server_stands_in() -> Self {
        Self { server_stands_in: true, ..Self::ephemeral() }
    }

    fn with_secret(secret: Secret) -> Self {
        Self {
            secret,
            loaded: Mutex::default(),
            openrouter_base: OPENROUTER_BASE.into(),
            openai_base: OPENAI_BASE.into(),
            timeouts: (crate::config::DEFAULT_PROVIDER_TIMEOUT_SECS, crate::config::DEFAULT_BACKGROUND_TIMEOUT_SECS),
            voice: None,
            server_stands_in: false,
            built: Mutex::default(),
        }
    }

    #[must_use]
    pub fn with_bases(mut self, openrouter: &str, openai: &str) -> Self {
        self.openrouter_base = openrouter.trim_end_matches('/').into();
        self.openai_base = openai.trim_end_matches('/').into();
        self
    }

    fn base(&self, kind: KeyKind) -> &str {
        match kind {
            KeyKind::Openrouter => &self.openrouter_base,
            KeyKind::Openai => &self.openai_base,
        }
    }

    fn secret(&self) -> Result<[u8; 32]> {
        let mut loaded = self.loaded.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(key) = *loaded {
            return Ok(key);
        }
        let key = match &self.secret {
            Secret::Fixed(key) => *key,
            Secret::File(path) => load_or_create(path)?,
        };
        *loaded = Some(key);
        Ok(key)
    }

    pub fn info(conn: &Connection, user_id: i64) -> Result<Option<KeyInfo>> {
        let row: Option<(String, String, String)> = conn
            .query_row("SELECT kind, model, hint FROM llm_keys WHERE user_id = ?1", [user_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?;
        Ok(row.and_then(|(kind, model, key_hint)| Some(KeyInfo { kind: KeyKind::parse(&kind)?, model, key_hint })))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn store(&self, conn: &Connection, user_id: i64, kind: KeyKind, key: &str, model: &str, models: &[String], now: jiff::Timestamp) -> Result<()> {
        let (nonce, sealed) = seal(&self.secret()?, user_id, key)?;
        let hint: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        conn.execute(
            "INSERT INTO llm_keys (user_id, kind, model, models, nonce, sealed, hint, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (user_id) DO UPDATE SET kind = excluded.kind, model = excluded.model, models = excluded.models,
                 nonce = excluded.nonce, sealed = excluded.sealed, hint = excluded.hint, created_at = excluded.created_at",
            rusqlite::params![user_id, kind.as_str(), model, serde_json::to_string(models)?, nonce, sealed, hint, now.to_string()],
        )?;
        self.forget(user_id);
        Ok(())
    }

    pub fn remove(&self, conn: &Connection, user_id: i64) -> Result<bool> {
        let n = conn.execute("DELETE FROM llm_keys WHERE user_id = ?1", [user_id])?;
        self.forget(user_id);
        Ok(n > 0)
    }

    fn forget(&self, user_id: i64) {
        self.built.lock().unwrap_or_else(PoisonError::into_inner).remove(&user_id);
    }

    /// The user's providers, built once and kept until their key changes.
    /// `db` is locked only to build them, so the caller must not hold it.
    pub fn own(&self, db: &Mutex<Connection>, user_id: i64, server: &dyn LLMProvider) -> Option<Arc<Own>> {
        if let Some(own) = self.built.lock().unwrap_or_else(PoisonError::into_inner).get(&user_id) {
            return own.clone();
        }
        let own = self.build(&crate::db_guard(db), user_id, server).map(Arc::new);
        self.built.lock().unwrap_or_else(PoisonError::into_inner).insert(user_id, own.clone());
        own
    }

    /// What the user's sessions run on: their own provider, or one that fails
    /// every call asking for a key.
    pub fn llm(&self, db: &Mutex<Connection>, user_id: i64, server: &Arc<dyn LLMProvider>) -> Arc<dyn LLMProvider> {
        match self.own(db, user_id, server.as_ref()) {
            Some(own) => own.llm.clone(),
            None if self.server_stands_in => server.clone(),
            None => Arc::new(Unusable(Reason::NoKey, "no AI key of the user's own is saved")),
        }
    }

    /// `llm`'s pair for calls: the reply model and the tool model.
    pub fn voice(&self, db: &Mutex<Connection>, user_id: i64, server: &Arc<dyn LLMProvider>, server_voice: &Arc<dyn LLMProvider>) -> (Arc<dyn LLMProvider>, Arc<dyn LLMProvider>) {
        match self.own(db, user_id, server.as_ref()) {
            Some(own) => (own.voice.clone(), own.llm.clone()),
            None if self.server_stands_in => (server_voice.clone(), server.clone()),
            None => {
                let none: Arc<dyn LLMProvider> = Arc::new(Unusable(Reason::NoKey, "no AI key of the user's own is saved"));
                (none.clone(), none)
            }
        }
    }

    /// Whether the user's sessions have a model to run on.
    pub fn usable(&self, db: &Mutex<Connection>, user_id: i64, server: &dyn LLMProvider) -> bool {
        self.server_stands_in || self.own(db, user_id, server).is_some()
    }

    /// Whether a user without a key of their own has to save one.
    pub fn required(&self) -> bool {
        !self.server_stands_in
    }

    fn build(&self, conn: &Connection, user_id: i64, server: &dyn LLMProvider) -> Option<Own> {
        let row: Option<SealedRow> = conn
            .query_row("SELECT kind, model, models, nonce, sealed FROM llm_keys WHERE user_id = ?1", [user_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .optional()
            .ok()
            .flatten();
        let (kind, model, models, nonce, sealed) = row?;
        let opened = KeyKind::parse(&kind)
            .context("unknown key kind")
            .and_then(|kind| Ok((kind, open(&self.secret()?, user_id, &nonce, &sealed)?)));
        let (kind, key) = match opened {
            Ok(opened) => opened,
            Err(e) => {
                let _ = crate::log::record(conn, Some(user_id), "llm_key_unreadable", &format!("{e:#}"));
                let broken: Arc<dyn LLMProvider> = Arc::new(Unusable(Reason::AuthInvalid, "the saved AI key cannot be read; save it again"));
                return Some(Own { llm: broken.clone(), voice: broken });
            }
        };
        let models: Vec<String> = serde_json::from_str(&models).unwrap_or_default();
        Some(self.providers(kind, &key, &model, &models, server))
    }

    fn providers(&self, kind: KeyKind, key: &str, model: &str, models: &[String], server: &dyn LLMProvider) -> Own {
        let agents = || ChatAgents::new(self.timeouts.0, self.timeouts.1);
        let base = self.base(kind);
        let built = OpenAILLM::new(base, model, key, agents(), None);
        let llm = match kind {
            KeyKind::Openrouter => match server.effort() {
                Some(Effort::Off) => built.without_reasoning(),
                Some(e) => built.with_effort(e.as_str()),
                None => built,
            }
            .with_models(&server.catalogue()),
            KeyKind::Openai => {
                let offered: Vec<crate::config::ModelInfo> =
                    models.iter().map(|id| crate::config::ModelInfo { id: id.clone(), ..Default::default() }).collect();
                built.with_openai_shape().with_models(&offered)
            }
        };
        let llm = Arc::new(llm);
        if base.contains("openrouter.ai") {
            let learning = llm.clone();
            std::thread::spawn(move || learning.learn_caps());
        }
        let voice: Arc<dyn LLMProvider> = match (kind, &self.voice) {
            (KeyKind::Openrouter, Some(v)) => {
                let voice_model = v.model.clone().unwrap_or_else(|| model.to_string());
                match crate::providers::voice_llm(base, &voice_model, key, agents(), v) {
                    Ok(voice) => Arc::new(OnOwnKey(Arc::new(voice))),
                    Err(_) => Arc::new(OnOwnKey(llm.clone())),
                }
            }
            _ => Arc::new(OnOwnKey(llm.clone())),
        };
        Own { llm: Arc::new(OnOwnKey(llm)), voice }
    }

    /// Checks `key` with the provider, cheaply and without spending, and that
    /// it offers `model`; returns what an `OpenAI` key may pick from.
    pub fn verify(&self, kind: KeyKind, key: &str, model: &str) -> Result<Vec<String>, Refusal> {
        let agent = crate::providers::http_agent(VERIFY_TIMEOUT_SECS);
        let base = self.base(kind);
        let get = |path: &str| -> Result<(u16, serde_json::Value), Refusal> {
            let url = format!("{base}{path}");
            let mut resp = agent
                .get(&url)
                .header("Authorization", format!("Bearer {key}"))
                .call()
                .map_err(|e| Refusal::Unreachable(e.to_string()))?;
            let code = resp.status().as_u16();
            let body = resp.body_mut().with_config().limit(16 * 1024 * 1024).read_json().unwrap_or(serde_json::Value::Null);
            Ok((code, body))
        };
        let refused = |code: u16| match code {
            401 | 403 => Err(Refusal::Key),
            200..=299 => Ok(()),
            other => Err(Refusal::Unreachable(format!("status {other}"))),
        };
        match kind {
            KeyKind::Openrouter => {
                refused(get("/key")?.0)?;
                match get("/models") {
                    Ok((200..=299, catalogue)) if !ids(&catalogue).is_empty() && !ids(&catalogue).iter().any(|id| id == model) => {
                        Err(Refusal::Model)
                    }
                    _ => Ok(vec![model.to_string()]),
                }
            }
            KeyKind::Openai => {
                let (code, catalogue) = get("/models")?;
                refused(code)?;
                if !ids(&catalogue).iter().any(|id| id == model) {
                    return Err(Refusal::Model);
                }
                Ok(chat_models(&catalogue, model))
            }
        }
    }

    /// Gives every user without a key the server's own, once.
    pub fn seed_from_server(&self, conn: &Connection, cfg: &crate::config::ProvidersConfig, live_model: Option<String>, now: jiff::Timestamp) -> Result<usize> {
        if crate::db::server_setting(conn, SEEDED_SETTING)?.is_some() {
            return Ok(0);
        }
        let mut seeded = 0;
        if let Some(p) = cfg.llm.as_ref().filter(|p| p.kind == "openai") {
            let key = crate::providers::read_key(p, false)?;
            if !key.is_empty() {
                let kind = if p.base_url.contains("openrouter.ai") { KeyKind::Openrouter } else { KeyKind::Openai };
                let model = live_model.unwrap_or_else(|| p.model.clone());
                let users: Vec<i64> = conn
                    .prepare("SELECT id FROM users WHERE id NOT IN (SELECT user_id FROM llm_keys)")?
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                let tx = conn.unchecked_transaction()?;
                for user_id in users {
                    self.store(&tx, user_id, kind, &key, &model, std::slice::from_ref(&model), now)?;
                    seeded += 1;
                }
                tx.commit()?;
            }
        }
        crate::db::set_server_setting(conn, SEEDED_SETTING, "1")?;
        Ok(seeded)
    }
}

fn ids(catalogue: &serde_json::Value) -> Vec<String> {
    catalogue["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(String::from)).collect()
}

/// The newest of an `OpenAI` catalogue's undated chat models, `chosen` first.
fn chat_models(catalogue: &serde_json::Value, chosen: &str) -> Vec<String> {
    const NOT_CHAT: [&str; 11] =
        ["audio", "realtime", "transcribe", "tts", "image", "search", "embedding", "instruct", "moderation", "codex", "preview"];
    let chat = |id: &str| {
        let family = id.starts_with("gpt-")
            || id.starts_with("chatgpt-")
            || (id.starts_with('o') && id[1..].starts_with(|c: char| c.is_ascii_digit()));
        let dated = id.split('-').any(|part| part.len() >= 4 && part.chars().all(|c| c.is_ascii_digit()));
        family && !dated && !NOT_CHAT.iter().any(|w| id.contains(w))
    };
    let mut rows: Vec<(i64, String)> = catalogue["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| Some((m["created"].as_i64().unwrap_or(0), m["id"].as_str()?.to_string())))
        .filter(|(_, id)| chat(id) && id != chosen)
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    std::iter::once(chosen.to_string()).chain(rows.into_iter().take(OPENAI_MODELS).map(|(_, id)| id)).collect()
}

fn load_or_create(path: &Path) -> Result<[u8; 32]> {
    use std::io::Write;
    let engine = base64::engine::general_purpose::STANDARD;
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let bytes = engine.decode(raw.trim()).with_context(|| format!("{} is not base64", path.display()))?;
            return bytes.try_into().map_err(|_| anyhow::anyhow!("{} does not hold 32 bytes", path.display()));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(anyhow::Error::new(e).context(format!("reading {}", path.display()))),
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).expect("os rng");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(engine.encode(key).as_bytes())?;
            file.sync_all()?;
            Ok(key)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => load_or_create(path),
        Err(e) => Err(anyhow::Error::new(e).context(format!("writing {}", path.display()))),
    }
}

/// The key sealed to its user, so a row copied to another user opens nowhere.
fn seal(secret: &[u8; 32], user_id: i64, key: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let cipher = Aes256Gcm::new(&(*secret).into());
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).expect("os rng");
    let sealed = cipher
        .encrypt(&nonce.into(), Payload { msg: key.as_bytes(), aad: &user_id.to_be_bytes() })
        .map_err(|_| anyhow::anyhow!("sealing the key failed"))?;
    Ok((nonce.to_vec(), sealed))
}

fn open(secret: &[u8; 32], user_id: i64, nonce: &[u8], sealed: &[u8]) -> Result<String> {
    let cipher = Aes256Gcm::new(&(*secret).into());
    let nonce: [u8; 12] = nonce.try_into().map_err(|_| anyhow::anyhow!("the nonce is not 12 bytes"))?;
    let plain = cipher
        .decrypt(&nonce.into(), Payload { msg: sealed, aad: &user_id.to_be_bytes() })
        .map_err(|_| anyhow::anyhow!("the key does not open under the server's key file"))?;
    Ok(String::from_utf8(plain)?)
}

/// Fails every call with `reason`.
struct Unusable(Reason, &'static str);

impl LLMProvider for Unusable {
    fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse> {
        Err(ProviderError::new(self.0, self.1).into())
    }

    fn offered(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Meters its calls as spent on the user's own key.
struct OnOwnKey(Arc<dyn LLMProvider>);

impl LLMProvider for OnOwnKey {
    fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        crate::usage::on_own_key(|| self.0.chat(req))
    }

    fn chat_stream(&self, req: &ChatRequest, opts: &StreamOpts, sink: &mut dyn StreamSink) -> Result<ChatResponse> {
        crate::usage::on_own_key(|| self.0.chat_stream(req, opts, sink))
    }

    fn chat_with_reasoning(&self, req: &ChatRequest) -> Result<(ChatResponse, String)> {
        crate::usage::on_own_key(|| self.0.chat_with_reasoning(req))
    }

    fn model(&self) -> Option<String> {
        self.0.model()
    }

    fn streams(&self) -> bool {
        self.0.streams()
    }

    fn set_model(&self, model: &str) -> bool {
        self.0.set_model(model)
    }

    fn offered(&self) -> Vec<String> {
        self.0.offered()
    }

    fn catalogue(&self) -> Vec<crate::config::ModelInfo> {
        self.0.catalogue()
    }

    fn effort(&self) -> Option<Effort> {
        self.0.effort()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Mutex<Connection> {
        let conn = crate::db::open_memory().unwrap();
        conn.execute_batch("INSERT INTO users (username, pass_hash, role) VALUES ('aitest','h','member'), ('shuntia','h','member');")
            .unwrap();
        Mutex::new(conn)
    }

    fn now() -> jiff::Timestamp {
        "2026-11-24T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn a_key_opens_only_for_its_user_and_under_its_secret() {
        let secret = [7u8; 32];
        let (nonce, sealed) = seal(&secret, 1, "sk-or-v1-abcdef").unwrap();
        assert!(!sealed.windows(6).any(|w| w == b"abcdef"));
        assert_eq!(open(&secret, 1, &nonce, &sealed).unwrap(), "sk-or-v1-abcdef");
        assert!(open(&secret, 2, &nonce, &sealed).is_err());
        assert!(open(&[8u8; 32], 1, &nonce, &sealed).is_err());
        let (again, _) = seal(&secret, 1, "sk-or-v1-abcdef").unwrap();
        assert_ne!(nonce, again, "each record gets its own nonce");
    }

    #[test]
    fn the_key_file_is_made_once_owner_only_and_read_back() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("secrets");
        let first = load_or_create(&dir.join(KEY_FILE)).unwrap();
        assert_eq!(load_or_create(&dir.join(KEY_FILE)).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(KEY_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let keys = OwnKeys::new(&dir, &crate::config::ProvidersConfig::default(), None);
        let conn = db();
        keys.store(&crate::db_guard(&conn), 1, KeyKind::Openai, "sk-proj-1234", "gpt-5", &["gpt-5".into()], now()).unwrap();
        let reread = OwnKeys::new(&dir, &crate::config::ProvidersConfig::default(), None);
        let server: Arc<dyn LLMProvider> = Arc::new(crate::providers::mock::NullLLM);
        assert_eq!(reread.llm(&conn, 1, &server).model().as_deref(), Some("gpt-5"));
    }

    #[test]
    fn a_user_runs_on_their_own_key_and_without_one_is_asked_for_it() {
        let keys = OwnKeys::ephemeral();
        let conn = db();
        let server: Arc<dyn LLMProvider> = Arc::new(crate::providers::mock::MockLLM::empty().offering(&["server/model", "other/model"]));
        keys.store(&crate::db_guard(&conn), 1, KeyKind::Openrouter, "sk-or-1234", "mine/model", &[], now()).unwrap();
        let own = keys.llm(&conn, 1, &server);
        assert_eq!(own.model().as_deref(), Some("mine/model"));
        assert_eq!(own.offered(), vec!["mine/model", "server/model", "other/model"]);
        assert!(keys.usable(&conn, 1, server.as_ref()));

        let none = keys.llm(&conn, 2, &server);
        let req = ChatRequest { system: "", messages: &[], tools: &[], background: false, plain: false, must_call: None, model: None, effort: None };
        assert_eq!(Reason::of(&none.chat(&req).unwrap_err()), Reason::NoKey);
        assert!(!keys.usable(&conn, 2, server.as_ref()));

        assert!(keys.remove(&crate::db_guard(&conn), 1).unwrap());
        assert!(!keys.usable(&conn, 1, server.as_ref()), "a removed key is forgotten at once");
        assert!(Arc::ptr_eq(&OwnKeys::server_stands_in().llm(&conn, 2, &server), &server));
    }

    #[test]
    fn info_shows_the_kind_model_and_last_four_only() {
        let keys = OwnKeys::ephemeral();
        let conn = db();
        let conn = crate::db_guard(&conn);
        assert_eq!(OwnKeys::info(&conn, 1).unwrap(), None);
        keys.store(&conn, 1, KeyKind::Openai, "sk-proj-SECRET-9xyz", "gpt-5", &[], now()).unwrap();
        let info = OwnKeys::info(&conn, 1).unwrap().unwrap();
        assert_eq!(info, KeyInfo { kind: KeyKind::Openai, model: "gpt-5".into(), key_hint: "9xyz".into() });
        assert!(!serde_json::to_string(&info).unwrap().contains("SECRET"));
    }

    #[test]
    fn a_refused_key_is_not_taken() {
        let (base, rx) = crate::testhttp::serve(vec![("401 Unauthorized", r#"{"error":{"message":"No auth credentials found"}}"#)]);
        let keys = OwnKeys::ephemeral().with_bases(&base, &base);
        assert_eq!(keys.verify(KeyKind::Openrouter, "sk-or-bad", "a/model"), Err(Refusal::Key));
        let raw = rx.recv().unwrap();
        assert!(raw.starts_with("GET /key ") && raw.contains("Bearer sk-or-bad"), "{raw}");

        let (base, _rx) = crate::testhttp::serve(vec![("403 Forbidden", "{}")]);
        let keys = OwnKeys::ephemeral().with_bases(&base, &base);
        assert_eq!(keys.verify(KeyKind::Openai, "sk-bad", "gpt-5"), Err(Refusal::Key));
    }

    #[test]
    fn a_good_key_is_taken_with_the_models_it_may_pick() {
        let (base, _rx) = crate::testhttp::serve(vec![
            ("200 OK", r#"{"data":{"label":"x"}}"#),
            ("200 OK", r#"{"data":[{"id":"a/model"},{"id":"b/model"}]}"#),
        ]);
        let keys = OwnKeys::ephemeral().with_bases(&base, &base);
        assert_eq!(keys.verify(KeyKind::Openrouter, "sk-or-good", "a/model"), Ok(vec!["a/model".to_string()]));

        let (base, _rx) = crate::testhttp::serve(vec![
            ("200 OK", r#"{"data":{"label":"x"}}"#),
            ("200 OK", r#"{"data":[{"id":"a/model"}]}"#),
        ]);
        let keys = OwnKeys::ephemeral().with_bases(&base, &base);
        assert_eq!(keys.verify(KeyKind::Openrouter, "sk-or-good", "gone/model"), Err(Refusal::Model));

        let catalogue = r#"{"data":[
            {"id":"gpt-5","created":30},{"id":"gpt-5-mini","created":20},{"id":"gpt-4o-2024-08-06","created":10},
            {"id":"whisper-1","created":5},{"id":"gpt-4o-realtime-preview","created":40},{"id":"o4-mini","created":25},
            {"id":"text-embedding-3-small","created":1}]}"#;
        let (base, _rx) = crate::testhttp::serve(vec![("200 OK", catalogue)]);
        let keys = OwnKeys::ephemeral().with_bases(&base, &base);
        assert_eq!(keys.verify(KeyKind::Openai, "sk-good", "gpt-5-mini"), Ok(vec!["gpt-5-mini".into(), "gpt-5".into(), "o4-mini".into()]));
    }

    #[test]
    fn calls_on_an_own_key_are_metered_as_such() {
        let (base, _rx) = crate::testhttp::serve(vec![(
            "200 OK",
            r#"{"choices":[{"message":{"content":"hi"}}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#,
        )]);
        let keys = OwnKeys::ephemeral().with_bases(&base, &base);
        let conn = db();
        keys.store(&crate::db_guard(&conn), 1, KeyKind::Openai, "sk-own", "gpt-5", &[], now()).unwrap();
        let server: Arc<dyn LLMProvider> = Arc::new(crate::providers::mock::NullLLM);
        let llm = keys.llm(&conn, 1, &server);
        let _meter = crate::usage::Meter::open();
        let req = ChatRequest { system: "", messages: &[], tools: &[], background: false, plain: false, must_call: None, model: None, effort: None };
        assert_eq!(llm.chat(&req).unwrap().text, "hi");
        let spent = crate::usage::take();
        assert_eq!((spent.calls, spent.byok, spent.prompt), (1, 1, 3));
    }

    #[test]
    fn an_openai_key_sends_openai_fields() {
        let (base, rx) = crate::testhttp::serve(vec![("200 OK", r#"{"choices":[{"message":{"content":"ok"}}]}"#)]);
        let keys = OwnKeys::ephemeral().with_bases(&base, &base);
        let conn = db();
        keys.store(&crate::db_guard(&conn), 1, KeyKind::Openai, "sk-own", "gpt-5", &[], now()).unwrap();
        let server: Arc<dyn LLMProvider> = Arc::new(crate::providers::mock::NullLLM);
        let req = ChatRequest { system: "", messages: &[], tools: &[], background: false, plain: false, must_call: None, model: None, effort: Some(Effort::Low) };
        keys.llm(&conn, 1, &server).chat(&req).unwrap();
        let raw = rx.recv().unwrap();
        let body: serde_json::Value = serde_json::from_str(raw.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["reasoning_effort"], "low");
        assert_eq!(body["max_completion_tokens"], 16_384);
        assert!(body.get("reasoning").is_none() && body.get("max_tokens").is_none() && body.get("usage").is_none());
    }

    #[test]
    fn seeding_gives_every_keyless_user_the_server_key_once() {
        let tmp = tempfile::tempdir().unwrap();
        let key_file = tmp.path().join("server.key");
        std::fs::write(&key_file, "sk-or-server-7777\n").unwrap();
        let cfg = crate::config::ProvidersConfig {
            llm: Some(toml::from_str(&format!(
                "kind = \"openai\"\nbase_url = \"https://openrouter.ai/api/v1\"\nmodel = \"cfg/model\"\napi_key_file = \"{}\"",
                key_file.display()
            ))
            .unwrap()),
            ..Default::default()
        };
        let keys = OwnKeys::ephemeral();
        let db = db();
        let conn = crate::db_guard(&db);
        keys.store(&conn, 2, KeyKind::Openai, "sk-mine-0000", "gpt-5", &[], now()).unwrap();
        assert_eq!(keys.seed_from_server(&conn, &cfg, Some("live/model".into()), now()).unwrap(), 1);
        assert_eq!(
            OwnKeys::info(&conn, 1).unwrap().unwrap(),
            KeyInfo { kind: KeyKind::Openrouter, model: "live/model".into(), key_hint: "7777".into() }
        );
        assert_eq!(OwnKeys::info(&conn, 2).unwrap().unwrap().key_hint, "0000", "a saved key stays");
        conn.execute("DELETE FROM llm_keys WHERE user_id = 1", []).unwrap();
        assert_eq!(keys.seed_from_server(&conn, &cfg, None, now()).unwrap(), 0, "it runs once");
        assert_eq!(OwnKeys::info(&conn, 1).unwrap(), None);
    }

    #[test]
    fn seeding_without_a_server_key_does_nothing_and_is_done() {
        let keys = OwnKeys::ephemeral();
        let db = db();
        let conn = crate::db_guard(&db);
        assert_eq!(keys.seed_from_server(&conn, &crate::config::ProvidersConfig::default(), None, now()).unwrap(), 0);
        assert_eq!(crate::db::server_setting(&conn, SEEDED_SETTING).unwrap().as_deref(), Some("1"));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM llm_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }
}
