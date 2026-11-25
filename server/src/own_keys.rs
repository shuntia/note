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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

const KEY_FILE: &str = "llm_keys";
pub const SEEDED_SETTING: &str = "llm_keys_seeded";
pub const OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";
pub const OPENAI_BASE: &str = "https://api.openai.com/v1";
pub const MAX_KEY_LEN: usize = 512;
pub const MAX_MODEL_LEN: usize = 200;
pub const MAX_BASE_LEN: usize = 300;
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

/// What the API shows of a saved key; `base_url` is an `OpenAI`-compatible key's endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyInfo {
    pub kind: KeyKind,
    pub model: String,
    pub key_hint: String,
    pub base_url: Option<String>,
}

/// A key to save. `base_url` is an `OpenAI`-compatible key's endpoint, `None`
/// for `OPENAI_BASE`; an `OpenRouter` key has none.
pub struct NewKey<'a> {
    pub kind: KeyKind,
    pub key: &'a str,
    pub model: &'a str,
    pub base_url: Option<&'a str>,
    pub models: &'a [String],
}

/// Why a key was not saved.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    Key,
    Model,
    Endpoint(&'static str),
    Unreachable(String),
}

/// The providers a user with a key of their own runs on.
pub struct Own {
    pub llm: Arc<dyn LLMProvider>,
    pub voice: Arc<dyn LLMProvider>,
}

struct SealedRow {
    kind: String,
    model: String,
    models: String,
    nonce: Vec<u8>,
    sealed: Vec<u8>,
    base_url: Option<String>,
    seeded: bool,
}

enum Secret {
    /// Read from the first of these that exists, else made in the first that takes it.
    Files(Vec<PathBuf>),
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
    /// Off only in unit tests, whose providers answer on loopback.
    guard: bool,
    built: Mutex<HashMap<i64, Option<Arc<Own>>>>,
    /// Bumped by every key change, so a build that read the row before it is not kept.
    changes: AtomicU64,
}

impl OwnKeys {
    /// Seals under `llm_keys` in `secrets_dir`, or in `data_dir` when the
    /// secrets directory holds none and cannot be written, as a systemd
    /// credentials directory cannot; a fresh key is written on first use.
    pub fn new(
        secrets_dir: &Path,
        data_dir: &Path,
        providers: &crate::config::ProvidersConfig,
        voice: Option<&crate::config::VoiceConfig>,
    ) -> Self {
        let timeouts = providers.llm.as_ref().map_or(
            (crate::config::DEFAULT_PROVIDER_TIMEOUT_SECS, crate::config::DEFAULT_BACKGROUND_TIMEOUT_SECS),
            |p| (p.timeout_secs, p.background_timeout_secs),
        );
        let files = Secret::Files(vec![secrets_dir.join(KEY_FILE), data_dir.join(KEY_FILE)]);
        Self { timeouts, voice: voice.cloned(), ..Self::with_secret(files) }
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
            guard: true,
            built: Mutex::default(),
            changes: AtomicU64::new(0),
        }
    }

    #[cfg(test)]
    fn unguarded(mut self) -> Self {
        self.guard = false;
        self
    }

    #[must_use]
    pub fn with_bases(mut self, openrouter: &str, openai: &str) -> Self {
        self.openrouter_base = openrouter.trim_end_matches('/').into();
        self.openai_base = openai.trim_end_matches('/').into();
        self
    }

    /// Where a key of `kind` is sent: `base_url` for an `OpenAI`-compatible one that names it.
    fn endpoint(&self, kind: KeyKind, base_url: Option<&str>) -> String {
        match (kind, base_url) {
            (KeyKind::Openrouter, _) => self.openrouter_base.clone(),
            (KeyKind::Openai, Some(base)) => base.trim_end_matches('/').to_string(),
            (KeyKind::Openai, None) => self.openai_base.clone(),
        }
    }

    /// Whether calls to `base` skip `net::guarded`: the server's own bases, and
    /// what the admin configured, which only seeding writes.
    fn trusted(&self, base: &str, seeded: bool) -> bool {
        !self.guard || seeded || base == self.openrouter_base || base == self.openai_base
    }

    fn secret(&self) -> Result<[u8; 32]> {
        let mut loaded = self.loaded.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(key) = *loaded {
            return Ok(key);
        }
        let key = match &self.secret {
            Secret::Fixed(key) => *key,
            Secret::Files(paths) => load_or_create(paths)?,
        };
        *loaded = Some(key);
        Ok(key)
    }

    pub fn info(&self, conn: &Connection, user_id: i64) -> Result<Option<KeyInfo>> {
        let row: Option<(String, String, String, Option<String>)> = conn
            .query_row("SELECT kind, model, hint, base_url FROM llm_keys WHERE user_id = ?1", [user_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .optional()?;
        Ok(row.and_then(|(kind, model, key_hint, base_url)| {
            let kind = KeyKind::parse(&kind)?;
            let base_url = (kind == KeyKind::Openai).then(|| self.endpoint(kind, base_url.as_deref()));
            Some(KeyInfo { kind, model, key_hint, base_url })
        }))
    }

    /// Saves a key the user gave; the calls it leads to are held to `net::guarded`
    /// unless they go to one of the server's own bases.
    pub fn store(&self, conn: &Connection, user_id: i64, new: &NewKey, now: jiff::Timestamp) -> Result<()> {
        self.write(conn, user_id, new, false, now)
    }

    fn write(&self, conn: &Connection, user_id: i64, new: &NewKey, seeded: bool, now: jiff::Timestamp) -> Result<()> {
        let (nonce, sealed) = seal(&self.secret()?, user_id, new.key)?;
        let hint: String = new.key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        let base_url = new.base_url.filter(|_| new.kind == KeyKind::Openai).map(|b| b.trim_end_matches('/'));
        conn.execute(
            "INSERT INTO llm_keys (user_id, kind, model, models, nonce, sealed, hint, created_at, base_url, seeded)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (user_id) DO UPDATE SET kind = excluded.kind, model = excluded.model, models = excluded.models,
                 nonce = excluded.nonce, sealed = excluded.sealed, hint = excluded.hint, created_at = excluded.created_at,
                 base_url = excluded.base_url, seeded = excluded.seeded",
            rusqlite::params![
                user_id,
                new.kind.as_str(),
                new.model,
                serde_json::to_string(new.models)?,
                nonce,
                sealed,
                hint,
                now.to_string(),
                base_url,
                seeded
            ],
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
        let mut built = self.built.lock().unwrap_or_else(PoisonError::into_inner);
        self.changes.fetch_add(1, Ordering::SeqCst);
        built.remove(&user_id);
    }

    /// The user's providers, built once and kept until their key changes.
    /// `db` is locked only to build them, so the caller must not hold it.
    pub fn own(&self, db: &Mutex<Connection>, user_id: i64, server: &dyn LLMProvider) -> Option<Arc<Own>> {
        let changes = {
            let built = self.built.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(own) = built.get(&user_id) {
                return own.clone();
            }
            self.changes.load(Ordering::SeqCst)
        };
        let own = self.build(&crate::db_guard(db), user_id, server).ok()?.map(Arc::new);
        let mut built = self.built.lock().unwrap_or_else(PoisonError::into_inner);
        if self.changes.load(Ordering::SeqCst) == changes {
            built.insert(user_id, own.clone());
        }
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

    fn build(&self, conn: &Connection, user_id: i64, server: &dyn LLMProvider) -> rusqlite::Result<Option<Own>> {
        let row: Option<SealedRow> = conn
            .query_row(
                "SELECT kind, model, models, nonce, sealed, base_url, seeded FROM llm_keys WHERE user_id = ?1",
                [user_id],
                |r| {
                    Ok(SealedRow {
                        kind: r.get(0)?,
                        model: r.get(1)?,
                        models: r.get(2)?,
                        nonce: r.get(3)?,
                        sealed: r.get(4)?,
                        base_url: r.get(5)?,
                        seeded: r.get(6)?,
                    })
                },
            )
            .optional()?;
        let Some(row) = row else { return Ok(None) };
        let opened = KeyKind::parse(&row.kind)
            .context("unknown key kind")
            .and_then(|kind| Ok((kind, open(&self.secret()?, user_id, &row.nonce, &row.sealed)?)));
        let (kind, key) = match opened {
            Ok(opened) => opened,
            Err(e) => {
                let _ = crate::log::record(conn, Some(user_id), "llm_key_unreadable", &format!("{e:#}"));
                let broken: Arc<dyn LLMProvider> = Arc::new(Unusable(Reason::AuthInvalid, "the saved AI key cannot be read; save it again"));
                return Ok(Some(Own { llm: broken.clone(), voice: broken }));
            }
        };
        let models: Vec<String> = serde_json::from_str(&row.models).unwrap_or_default();
        let base = self.endpoint(kind, row.base_url.as_deref());
        let public = !self.trusted(&base, row.seeded);
        Ok(Some(self.providers(kind, &key, &row.model, &models, &base, public, server)))
    }

    #[allow(clippy::too_many_arguments)]
    fn providers(&self, kind: KeyKind, key: &str, model: &str, models: &[String], base: &str, public: bool, server: &dyn LLMProvider) -> Own {
        let agents = || {
            if public { ChatAgents::public(self.timeouts.0, self.timeouts.1) } else { ChatAgents::new(self.timeouts.0, self.timeouts.1) }
        };
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
                let built = if openai_proper(base) { built.with_openai_shape() } else { built };
                built.with_models(&offered)
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

    /// Checks `key` with the provider at `base_url` (see `NewKey`), cheaply and
    /// without spending, and that it offers `model`; returns what an
    /// `OpenAI`-compatible key may pick from. Blocking on DNS.
    pub fn verify(&self, kind: KeyKind, key: &str, model: &str, base_url: Option<&str>) -> Result<Vec<String>, Refusal> {
        let base = self.endpoint(kind, base_url);
        let public = !self.trusted(&base, false);
        if public {
            crate::net::public_url_ok(&base).map_err(Refusal::Endpoint)?;
        }
        let agent = crate::providers::http_agent_for(VERIFY_TIMEOUT_SECS, public);
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
                let models = std::slice::from_ref(&model);
                let new = NewKey { kind, key: &key, model: &model, base_url: Some(&p.base_url), models };
                let users: Vec<i64> = conn
                    .prepare("SELECT id FROM users WHERE id NOT IN (SELECT user_id FROM llm_keys)")?
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                let tx = conn.unchecked_transaction()?;
                for user_id in users {
                    self.write(&tx, user_id, &new, true, now)?;
                    seeded += 1;
                }
                tx.commit()?;
            }
        }
        crate::db::set_server_setting(conn, SEEDED_SETTING, "1")?;
        Ok(seeded)
    }
}

/// Whether `base` is `OpenAI`'s own API, which takes its own request shape.
fn openai_proper(base: &str) -> bool {
    crate::net::host_of(base).as_deref() == Some("api.openai.com")
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

fn load_or_create(paths: &[PathBuf]) -> Result<[u8; 32]> {
    for path in paths {
        if let Some(key) = read_key_file(path)? {
            return Ok(key);
        }
    }
    let mut refused = anyhow::anyhow!("no place is given for the key file");
    for path in paths {
        match create_key_file(path) {
            Ok(key) => return Ok(key),
            Err(e) => refused = e,
        }
    }
    Err(refused)
}

fn read_key_file(path: &Path) -> Result<Option<[u8; 32]>> {
    let engine = base64::engine::general_purpose::STANDARD;
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let bytes = engine.decode(raw.trim()).with_context(|| format!("{} is not base64", path.display()))?;
            bytes.try_into().map(Some).map_err(|_| anyhow::anyhow!("{} does not hold 32 bytes", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::new(e).context(format!("reading {}", path.display()))),
    }
}

/// Writes a fresh key beside `path` and links it into place whole, so a crash
/// never leaves a partial file and a racing writer's key wins for both.
fn create_key_file(path: &Path) -> Result<[u8; 32]> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).expect("os rng");
    let mut tag = [0u8; 8];
    getrandom::fill(&mut tag).expect("os rng");
    let staged = path.with_extension(format!("new-{}", u64::from_le_bytes(tag)));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options.open(&staged).and_then(|mut file| {
        file.write_all(base64::engine::general_purpose::STANDARD.encode(key).as_bytes())?;
        file.sync_all()
    });
    let linked = written.and_then(|()| std::fs::hard_link(&staged, path));
    let _ = std::fs::remove_file(&staged);
    match linked {
        Ok(()) => Ok(key),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            read_key_file(path)?.with_context(|| format!("{} vanished as it was made", path.display()))
        }
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
        let first = load_or_create(&[dir.join(KEY_FILE)]).unwrap();
        assert_eq!(load_or_create(&[dir.join(KEY_FILE)]).unwrap(), first);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "nothing staged is left behind");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(KEY_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let data = tmp.path().join("data");
        let keys = OwnKeys::new(&dir, &data, &crate::config::ProvidersConfig::default(), None);
        let conn = db();
        keys.store(&crate::db_guard(&conn), 1, &NewKey { kind: KeyKind::Openai, key: "sk-proj-1234", model: "gpt-5", base_url: None, models: &["gpt-5".into()] }, now()).unwrap();
        let reread = OwnKeys::new(&dir, &data, &crate::config::ProvidersConfig::default(), None);
        let server: Arc<dyn LLMProvider> = Arc::new(crate::providers::mock::NullLLM);
        assert_eq!(reread.llm(&conn, 1, &server).model().as_deref(), Some("gpt-5"));
        assert!(!data.join(KEY_FILE).exists(), "a key in the secrets directory is the one used");
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_secrets_directory_puts_the_key_file_in_the_data_directory() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let (secrets, data) = (tmp.path().join("credentials"), tmp.path().join("data"));
        std::fs::create_dir_all(&secrets).unwrap();
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o500)).unwrap();
        let conn = db();
        let new = NewKey { kind: KeyKind::Openai, key: "sk-proj-1234", model: "gpt-5", base_url: None, models: &[] };
        OwnKeys::new(&secrets, &data, &crate::config::ProvidersConfig::default(), None).store(&crate::db_guard(&conn), 1, &new, now()).unwrap();
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(data.join(KEY_FILE).exists() && !secrets.join(KEY_FILE).exists());
        let reread = OwnKeys::new(&secrets, &data, &crate::config::ProvidersConfig::default(), None);
        let server: Arc<dyn LLMProvider> = Arc::new(crate::providers::mock::NullLLM);
        assert_eq!(reread.llm(&conn, 1, &server).model().as_deref(), Some("gpt-5"));
    }

    #[test]
    fn a_user_runs_on_their_own_key_and_without_one_is_asked_for_it() {
        let keys = OwnKeys::ephemeral();
        let conn = db();
        let server: Arc<dyn LLMProvider> = Arc::new(crate::providers::mock::MockLLM::empty().offering(&["server/model", "other/model"]));
        keys.store(&crate::db_guard(&conn), 1, &NewKey { kind: KeyKind::Openrouter, key: "sk-or-1234", model: "mine/model", base_url: None, models: &[] }, now()).unwrap();
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
    fn a_key_that_could_not_be_read_is_looked_for_again() {
        let keys = OwnKeys::ephemeral();
        let conn = db();
        let server: Arc<dyn LLMProvider> = Arc::new(crate::providers::mock::NullLLM);
        keys.store(&crate::db_guard(&conn), 1, &NewKey { kind: KeyKind::Openai, key: "sk-1", model: "gpt-5", base_url: None, models: &[] }, now()).unwrap();
        keys.forget(1);
        crate::db_guard(&conn).execute_batch("ALTER TABLE llm_keys RENAME TO llm_keys_away").unwrap();
        assert!(!keys.usable(&conn, 1, server.as_ref()));
        crate::db_guard(&conn).execute_batch("ALTER TABLE llm_keys_away RENAME TO llm_keys").unwrap();
        assert!(keys.usable(&conn, 1, server.as_ref()), "a failed read is not remembered as no key");
    }

    #[test]
    fn info_shows_the_kind_model_and_last_four_only() {
        let keys = OwnKeys::ephemeral();
        let conn = db();
        let conn = crate::db_guard(&conn);
        assert_eq!(keys.info(&conn, 1).unwrap(), None);
        keys.store(&conn, 1, &NewKey { kind: KeyKind::Openai, key: "sk-proj-SECRET-9xyz", model: "gpt-5", base_url: None, models: &[] }, now()).unwrap();
        let info = keys.info(&conn, 1).unwrap().unwrap();
        assert_eq!(info, KeyInfo { kind: KeyKind::Openai, model: "gpt-5".into(), key_hint: "9xyz".into(), base_url: Some(OPENAI_BASE.into()) });
        assert!(!serde_json::to_string(&info).unwrap().contains("SECRET"));
    }

    #[test]
    fn a_refused_key_is_not_taken() {
        let (base, rx) = crate::testhttp::serve(vec![("401 Unauthorized", r#"{"error":{"message":"No auth credentials found"}}"#)]);
        let keys = OwnKeys::ephemeral().with_bases(&base, &base);
        assert_eq!(keys.verify(KeyKind::Openrouter, "sk-or-bad", "a/model", None), Err(Refusal::Key));
        let raw = rx.recv().unwrap();
        assert!(raw.starts_with("GET /key ") && raw.contains("Bearer sk-or-bad"), "{raw}");

        let (base, _rx) = crate::testhttp::serve(vec![("403 Forbidden", "{}")]);
        let keys = OwnKeys::ephemeral().with_bases(&base, &base);
        assert_eq!(keys.verify(KeyKind::Openai, "sk-bad", "gpt-5", None), Err(Refusal::Key));
    }

    #[test]
    fn a_good_key_is_taken_with_the_models_it_may_pick() {
        let (base, _rx) = crate::testhttp::serve(vec![
            ("200 OK", r#"{"data":{"label":"x"}}"#),
            ("200 OK", r#"{"data":[{"id":"a/model"},{"id":"b/model"}]}"#),
        ]);
        let keys = OwnKeys::ephemeral().with_bases(&base, &base);
        assert_eq!(keys.verify(KeyKind::Openrouter, "sk-or-good", "a/model", None), Ok(vec!["a/model".to_string()]));

        let (base, _rx) = crate::testhttp::serve(vec![
            ("200 OK", r#"{"data":{"label":"x"}}"#),
            ("200 OK", r#"{"data":[{"id":"a/model"}]}"#),
        ]);
        let keys = OwnKeys::ephemeral().with_bases(&base, &base);
        assert_eq!(keys.verify(KeyKind::Openrouter, "sk-or-good", "gone/model", None), Err(Refusal::Model));

        let catalogue = r#"{"data":[
            {"id":"gpt-5","created":30},{"id":"gpt-5-mini","created":20},{"id":"gpt-4o-2024-08-06","created":10},
            {"id":"whisper-1","created":5},{"id":"gpt-4o-realtime-preview","created":40},{"id":"o4-mini","created":25},
            {"id":"text-embedding-3-small","created":1}]}"#;
        let (base, _rx) = crate::testhttp::serve(vec![("200 OK", catalogue)]);
        let keys = OwnKeys::ephemeral().with_bases(&base, &base);
        assert_eq!(keys.verify(KeyKind::Openai, "sk-good", "gpt-5-mini", None), Ok(vec!["gpt-5-mini".into(), "gpt-5".into(), "o4-mini".into()]));
    }

    #[test]
    fn calls_on_an_own_key_are_metered_as_such() {
        let (base, _rx) = crate::testhttp::serve(vec![(
            "200 OK",
            r#"{"choices":[{"message":{"content":"hi"}}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#,
        )]);
        let keys = OwnKeys::ephemeral().with_bases(&base, &base);
        let conn = db();
        keys.store(&crate::db_guard(&conn), 1, &NewKey { kind: KeyKind::Openai, key: "sk-own", model: "gpt-5", base_url: None, models: &[] }, now()).unwrap();
        let server: Arc<dyn LLMProvider> = Arc::new(crate::providers::mock::NullLLM);
        let llm = keys.llm(&conn, 1, &server);
        let _meter = crate::usage::Meter::open();
        let req = ChatRequest { system: "", messages: &[], tools: &[], background: false, plain: false, must_call: None, model: None, effort: None };
        assert_eq!(llm.chat(&req).unwrap().text, "hi");
        let spent = crate::usage::take();
        assert_eq!((spent.calls, spent.byok, spent.prompt), (1, 1, 3));
    }

    fn body_of(raw: &str) -> serde_json::Value {
        serde_json::from_str(raw.split_once("\r\n\r\n").unwrap().1).unwrap()
    }

    #[test]
    fn only_openais_own_api_gets_its_request_shape() {
        assert!(openai_proper(OPENAI_BASE));
        assert!(!openai_proper("https://llm.example.com/v1"));
        assert!(!openai_proper("https://api.openai.com.example.com/v1"));
        assert!(!openai_proper("http://api.openai.com/v1"));
        let mut v = crate::providers::openai::body(
            "gpt-5",
            &ChatRequest { system: "", messages: &[], tools: &[], background: false, plain: false, must_call: None, model: None, effort: None },
            Some("low"),
        );
        crate::providers::openai::reshape_for_openai(&mut v);
        assert_eq!((v["reasoning_effort"].as_str(), v["max_completion_tokens"].as_u64()), (Some("low"), Some(16_384)));
        assert!(v.get("reasoning").is_none() && v.get("max_tokens").is_none());
    }

    #[test]
    fn an_openai_compatible_key_is_checked_and_spoken_to_at_its_own_endpoint() {
        let (base, rx) = crate::testhttp::serve(vec![
            ("200 OK", r#"{"data":[{"id":"local-model"}]}"#),
            ("200 OK", r#"{"choices":[{"message":{"content":"ok"}}]}"#),
        ]);
        let keys = OwnKeys::ephemeral().unguarded();
        let endpoint = format!("{base}/v1/");
        assert_eq!(keys.verify(KeyKind::Openai, "sk-local", "local-model", Some(&endpoint)), Ok(vec!["local-model".to_string()]));
        assert!(rx.recv().unwrap().starts_with("GET /v1/models "));
        let conn = db();
        let new = NewKey { kind: KeyKind::Openai, key: "sk-local", model: "local-model", base_url: Some(&endpoint), models: &[] };
        keys.store(&crate::db_guard(&conn), 1, &new, now()).unwrap();
        assert_eq!(keys.info(&crate::db_guard(&conn), 1).unwrap().unwrap().base_url, Some(format!("{base}/v1")));
        let server: Arc<dyn LLMProvider> = Arc::new(crate::providers::mock::NullLLM);
        let req = ChatRequest { system: "", messages: &[], tools: &[], background: false, plain: false, must_call: None, model: None, effort: Some(Effort::Low) };
        keys.llm(&conn, 1, &server).chat(&req).unwrap();
        let raw = rx.recv().unwrap();
        assert!(raw.starts_with("POST /v1/chat/completions "), "{raw}");
        let body = body_of(&raw);
        assert_eq!(body["max_tokens"], 16_384, "another endpoint keeps the generic shape");
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn a_typed_endpoint_must_be_public_https_on_save_and_when_called() {
        let keys = OwnKeys::ephemeral();
        for endpoint in ["http://llm.example.com/v1", "https://10.1.2.3/v1", "https://127.0.0.1:8443/v1", "https://[fd00::1]/v1", "https://100.64.0.9/v1"] {
            assert!(matches!(keys.verify(KeyKind::Openai, "sk", "m", Some(endpoint)), Err(Refusal::Endpoint(_))), "{endpoint}");
        }

        let (base, rx) = crate::testhttp::serve(vec![("200 OK", r#"{"choices":[{"message":{"content":"ok"}}]}"#)]);
        let port = base.rsplit(':').next().unwrap();
        let conn = db();
        let server: Arc<dyn LLMProvider> = Arc::new(crate::providers::mock::NullLLM);
        let req = ChatRequest { system: "", messages: &[], tools: &[], background: false, plain: false, must_call: None, model: None, effort: None };
        for (user, endpoint) in [(1, format!("https://127.0.0.1:{port}")), (2, base.clone())] {
            let new = NewKey { kind: KeyKind::Openai, key: "sk", model: "m", base_url: Some(&endpoint), models: &[] };
            keys.store(&crate::db_guard(&conn), user, &new, now()).unwrap();
            assert!(keys.llm(&conn, user, &server).chat(&req).is_err(), "{endpoint}");
        }
        assert!(rx.try_recv().is_err(), "nothing reached the private address");
    }

    #[test]
    fn only_seeding_exempts_the_servers_own_endpoint() {
        let (base, rx) = crate::testhttp::serve(vec![("200 OK", r#"{"choices":[{"message":{"content":"seeded"}}]}"#)]);
        let tmp = tempfile::tempdir().unwrap();
        let key_file = tmp.path().join("server.key");
        std::fs::write(&key_file, "sk-server\n").unwrap();
        let cfg = crate::config::ProvidersConfig {
            llm: Some(toml::from_str(&format!(
                "kind = \"openai\"\nbase_url = \"{base}\"\nmodel = \"local\"\napi_key_file = \"{}\"",
                key_file.display()
            ))
            .unwrap()),
            ..Default::default()
        };
        let keys = OwnKeys::ephemeral();
        let conn = db();
        keys.seed_from_server(&crate::db_guard(&conn), &cfg, None, now()).unwrap();
        let info = keys.info(&crate::db_guard(&conn), 1).unwrap().unwrap();
        assert_eq!((info.kind, info.base_url.as_deref()), (KeyKind::Openai, Some(base.as_str())), "the server's base is kept as written");
        let server: Arc<dyn LLMProvider> = Arc::new(crate::providers::mock::NullLLM);
        let req = ChatRequest { system: "", messages: &[], tools: &[], background: false, plain: false, must_call: None, model: None, effort: None };
        assert_eq!(keys.llm(&conn, 1, &server).chat(&req).unwrap().text, "seeded");
        rx.recv().unwrap();

        let new = NewKey { kind: KeyKind::Openai, key: "sk-mine", model: "local", base_url: Some(&base), models: &[] };
        keys.store(&crate::db_guard(&conn), 1, &new, now()).unwrap();
        assert!(keys.llm(&conn, 1, &server).chat(&req).is_err(), "the user's own save of that endpoint is held to the guard");
        assert!(rx.try_recv().is_err());
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
        keys.store(&conn, 2, &NewKey { kind: KeyKind::Openai, key: "sk-mine-0000", model: "gpt-5", base_url: None, models: &[] }, now()).unwrap();
        assert_eq!(keys.seed_from_server(&conn, &cfg, Some("live/model".into()), now()).unwrap(), 1);
        assert_eq!(
            keys.info(&conn, 1).unwrap().unwrap(),
            KeyInfo { kind: KeyKind::Openrouter, model: "live/model".into(), key_hint: "7777".into(), base_url: None }
        );
        assert_eq!(keys.info(&conn, 2).unwrap().unwrap().key_hint, "0000", "a saved key stays");
        conn.execute("DELETE FROM llm_keys WHERE user_id = 1", []).unwrap();
        assert_eq!(keys.seed_from_server(&conn, &cfg, None, now()).unwrap(), 0, "it runs once");
        assert_eq!(keys.info(&conn, 1).unwrap(), None);
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
