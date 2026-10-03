pub mod anthropic;
pub mod mock;
pub mod openai;

use anyhow::{Context, Result};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: String,
}

#[derive(Debug, Clone)]
pub enum Message {
    User(String),
    Assistant { text: String, tool_calls: Vec<ToolCall> },
    ToolResult { call_id: String, content: String, is_error: bool },
}

#[derive(Debug, Clone)]
pub struct ChatRequest<'a> {
    pub system: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [serde_json::Value],
    /// Nobody is waiting on the reply, so the call may take the provider's
    /// longer `background_timeout_secs`.
    pub background: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
}

pub trait StreamSink {
    /// A piece of assistant text. Returning false stops the stream.
    fn text(&mut self, delta: &str) -> bool;
    /// A tool call whose arguments are complete. Returning false stops the stream.
    fn tool_call(&mut self, call: &ToolCall) -> bool;
}

pub struct StreamOpts {
    /// Fails the call if no text or tool call has arrived by then.
    pub first_token: std::time::Duration,
}

#[derive(Debug, thiserror::Error)]
#[error("no first token within {0:?}")]
pub struct FirstTokenTimeout(pub std::time::Duration);

pub trait LLMProvider: Send + Sync {
    fn chat(&self, req: &ChatRequest) -> Result<ChatResponse>;

    /// Streams one round. The returned response holds everything that arrived,
    /// also when the sink stopped the stream early.
    fn chat_stream(&self, req: &ChatRequest, _opts: &StreamOpts, sink: &mut dyn StreamSink) -> Result<ChatResponse> {
        let resp = self.chat(req)?;
        if !resp.text.is_empty() && !sink.text(&resp.text) {
            return Ok(resp);
        }
        for c in &resp.tool_calls {
            if !sink.tool_call(c) {
                break;
            }
        }
        Ok(resp)
    }

    /// The reply plus the model's reasoning text for this round, blank when the
    /// provider returns none or reasoning is off.
    fn chat_with_reasoning(&self, req: &ChatRequest) -> Result<(ChatResponse, String)> {
        Ok((self.chat(req)?, String::new()))
    }

    fn model(&self) -> Option<String> {
        None
    }

    /// Whether `chat_stream` streams; the default runs a whole `chat`.
    fn streams(&self) -> bool {
        false
    }

    /// Swaps the model later calls ask for; false for a provider with none.
    fn set_model(&self, _model: &str) -> bool {
        false
    }
}

/// A model name the admin can swap while calls are in flight.
pub(crate) struct LiveModel(std::sync::RwLock<String>);

impl LiveModel {
    pub(crate) fn new(model: &str) -> Self {
        Self(std::sync::RwLock::new(model.to_string()))
    }

    pub(crate) fn get(&self) -> String {
        self.0.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub(crate) fn set(&self, model: &str) {
        *self.0.write().unwrap_or_else(std::sync::PoisonError::into_inner) = model.to_string();
    }
}

pub trait EmbeddingsProvider: Send + Sync {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
}

/// Agent config for chat providers: bounded connect time, and a caller-set
/// read/write cap so a stalled provider can't hang a session. The overall
/// `timeout` also caps a drip-feeding endpoint that keeps resetting the
/// per-operation ones. Chat calls hold no locks.
pub(crate) fn http_agent(timeout_secs: u64) -> ureq::Agent {
    let cap = Some(std::time::Duration::from_secs(timeout_secs));
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(std::time::Duration::from_secs(10)))
        .timeout_send_request(cap)
        .timeout_send_body(cap)
        .timeout_recv_response(cap)
        .timeout_recv_body(cap)
        .timeout_global(cap)
        .build()
        .into()
}

/// Waits before the second and third attempt of a chat call.
const RETRY_DELAYS_MS: [u64; 2] = [1_500, 4_000];

/// The interactive and the background agent of one chat provider.
pub struct ChatAgents {
    interactive: ureq::Agent,
    background: ureq::Agent,
    timeout_secs: u64,
    background_timeout_secs: u64,
    stream_agents: std::sync::Mutex<std::collections::HashMap<(bool, std::time::Duration), ureq::Agent>>,
    retry_delays_ms: &'static [u64],
}

impl ChatAgents {
    pub fn new(timeout_secs: u64, background_timeout_secs: u64) -> Self {
        let background_timeout_secs = background_timeout_secs.max(timeout_secs);
        Self {
            interactive: http_agent(timeout_secs),
            background: http_agent(background_timeout_secs),
            timeout_secs,
            background_timeout_secs,
            stream_agents: std::sync::Mutex::default(),
            retry_delays_ms: &RETRY_DELAYS_MS,
        }
    }

    /// The agent for streamed calls with this deadline: the response head must
    /// arrive within `first_token`, and the whole body within the usual chat cap.
    pub(crate) fn stream_agent(&self, background: bool, first_token: std::time::Duration) -> ureq::Agent {
        let mut agents = self.stream_agents.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        agents.entry((background, first_token)).or_insert_with(|| self.new_stream_agent(background, first_token)).clone()
    }

    fn new_stream_agent(&self, background: bool, first_token: std::time::Duration) -> ureq::Agent {
        let secs = if background { self.background_timeout_secs } else { self.timeout_secs };
        let cap = Some(std::time::Duration::from_secs(secs));
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(std::time::Duration::from_secs(5)))
            .timeout_send_request(cap)
            .timeout_send_body(cap)
            .timeout_recv_response(Some(first_token))
            .timeout_recv_body(cap)
            .build()
            .into()
    }

    /// Posts `body` and reads the JSON reply. A transport failure, a reply that
    /// stalls mid-read, 429 and 5xx are tried again; any other status fails at
    /// once. The error carries the status and the head of the body, since the
    /// log line it ends up in is all anyone sees of it.
    pub(crate) fn post_json(
        &self,
        background: bool,
        what: &str,
        body: &serde_json::Value,
        request: impl Fn(&ureq::Agent) -> ureq::RequestBuilder<ureq::typestate::WithBody>,
    ) -> Result<serde_json::Value> {
        let agent = if background { &self.background } else { &self.interactive };
        let mut attempt = 0;
        loop {
            let failure = match request(agent).send_json(body) {
                Ok(mut resp) if resp.status().is_success() => {
                    match resp.body_mut().read_json::<serde_json::Value>() {
                        Ok(v) => return Ok(v),
                        Err(e) => format!("reading the response: {e}"),
                    }
                }
                Ok(mut resp) => {
                    let code = resp.status().as_u16();
                    let head: String =
                        resp.body_mut().read_to_string().unwrap_or_default().chars().take(300).collect();
                    let failure = format!("status {code}: {head}");
                    if code != 429 && code < 500 {
                        anyhow::bail!("{what} request failed: {failure}");
                    }
                    failure
                }
                Err(e) => e.to_string(),
            };
            let Some(delay) = self.retry_delays_ms.get(attempt) else {
                anyhow::bail!("{what} request failed after {} attempts: {failure}", attempt + 1);
            };
            std::thread::sleep(std::time::Duration::from_millis(*delay));
            attempt += 1;
        }
    }
}

/// Embedding calls run in the agent loop's prepare pass with no lock held, so
/// a stalled endpoint delays only its own talk turn. The tight caps bound how
/// long that stall can last.
pub(crate) fn embeddings_http_agent() -> ureq::Agent {
    let cap = Some(std::time::Duration::from_secs(10));
    ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(5)))
        .timeout_send_request(cap)
        .timeout_send_body(cap)
        .timeout_recv_response(cap)
        .timeout_recv_body(cap)
        .timeout_global(cap)
        .build()
        .into()
}

pub type Providers = (Arc<dyn LLMProvider>, Option<Arc<dyn EmbeddingsProvider>>);

/// Builds providers from config. Absent or "mock" LLM config yields a
/// null provider, so the system is fully runnable with no tokens.
pub fn build(cfg: &crate::config::ProvidersConfig) -> Result<Providers> {
    let llm: Arc<dyn LLMProvider> = match &cfg.llm {
        None => Arc::new(mock::NullLLM),
        Some(p) => match p.kind.as_str() {
            "mock" => Arc::new(mock::NullLLM),
            "anthropic" => {
                let key = read_key(p, true)?;
                Arc::new(anthropic::AnthropicLLM::new(
                    &p.base_url,
                    &p.model,
                    &key,
                    ChatAgents::new(p.timeout_secs, p.background_timeout_secs),
                ))
            }
            "openai" => {
                anyhow::ensure!(!p.base_url.is_empty(), "openai llm provider requires base_url");
                let key = read_key(p, false)?;
                Arc::new(openai::OpenAILLM::new(
                    &p.base_url,
                    &p.model,
                    &key,
                    ChatAgents::new(p.timeout_secs, p.background_timeout_secs),
                    reasoning_effort(p)?,
                ))
            }
            other => anyhow::bail!("unknown llm provider kind: {other}"),
        },
    };
    let emb: Option<Arc<dyn EmbeddingsProvider>> = match &cfg.embeddings {
        None => None,
        Some(p) => match p.kind.as_str() {
            "mock" => Some(Arc::new(mock::MockEmbeddings)),
            "openai" => {
                anyhow::ensure!(!p.base_url.is_empty(), "openai embeddings provider requires base_url");
                let key = read_key(p, false)?;
                Some(Arc::new(openai::OpenAIEmbeddings::new(&p.base_url, &p.model, &key)))
            }
            other => anyhow::bail!("unknown embeddings provider kind: {other}"),
        },
    };
    Ok((llm, emb))
}

/// A call's reply model: for an `openai` provider, `[voice] model` (else the
/// main model) with reasoning off unless `[voice] reasoning_off = false`, and
/// `OpenRouter`'s `provider.sort`; any other
/// provider speaks on `main` as is.
pub fn build_voice(
    cfg: &crate::config::ProvidersConfig,
    voice: &crate::config::VoiceConfig,
    main: &Arc<dyn LLMProvider>,
) -> Result<Arc<dyn LLMProvider>> {
    let Some(p) = cfg.llm.as_ref().filter(|p| p.kind == "openai") else {
        return Ok(main.clone());
    };
    let model = voice.model.clone().or_else(|| main.model()).unwrap_or_else(|| p.model.clone());
    let key = read_key(p, false)?;
    let mut llm = openai::OpenAILLM::new(
        &p.base_url,
        &model,
        &key,
        ChatAgents::new(p.timeout_secs, p.background_timeout_secs),
        reasoning_effort(p)?,
    );
    if voice.reasoning_off {
        llm = llm.without_reasoning();
    }
    if let Some(sort) = &voice.provider_sort {
        llm = llm.with_extra(serde_json::json!({"provider": {"sort": sort}}));
    }
    Ok(Arc::new(llm))
}

/// The configured reasoning effort, or `None` for the default "none".
fn reasoning_effort(p: &crate::config::ProviderConfig) -> Result<Option<&str>> {
    match p.reasoning.as_str() {
        "" | "none" => Ok(None),
        e @ ("low" | "medium" | "high") => Ok(Some(e)),
        other => anyhow::bail!("unknown reasoning effort: {other}"),
    }
}

/// Key resolution: `api_key_file` wins when set (a configured file that is
/// unreadable or blank is always an error), then `api_key_env`. `required`
/// distinguishes Anthropic (key mandatory) from OpenAI-compatible local
/// endpoints that accept no key.
fn read_key(p: &crate::config::ProviderConfig, required: bool) -> Result<String> {
    if !p.api_key_file.as_os_str().is_empty() {
        let key = std::fs::read_to_string(&p.api_key_file)
            .with_context(|| format!("reading api key file {}", p.api_key_file.display()))?
            .trim()
            .to_string();
        anyhow::ensure!(!key.is_empty(), "api key file {} is empty", p.api_key_file.display());
        return Ok(key);
    }
    if p.api_key_env.is_empty() {
        anyhow::ensure!(!required, "provider requires api_key_env or api_key_file in config");
        return Ok(String::new());
    }
    match std::env::var(&p.api_key_env) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ if required => anyhow::bail!("api key env var {} is not set", p.api_key_env),
        _ => Ok(String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents() -> ChatAgents {
        ChatAgents { retry_delays_ms: &[0, 0], ..ChatAgents::new(5, 5) }
    }

    fn post(base: &str) -> Result<serde_json::Value> {
        let url = format!("{base}/chat");
        agents().post_json(false, "openai", &serde_json::json!({}), |a| a.post(&url))
    }

    #[test]
    fn a_chat_call_outlives_a_failing_attempt() {
        let (base, rx) = crate::testhttp::serve(vec![
            ("502 Bad Gateway", "{}"),
            ("429 Too Many Requests", "{}"),
            ("200 OK", r#"{"ok":true}"#),
        ]);
        assert_eq!(post(&base).unwrap()["ok"], true);
        assert_eq!(rx.try_iter().count(), 3);
    }

    #[test]
    fn a_refused_chat_call_fails_at_once_and_says_why() {
        let (base, rx) = crate::testhttp::serve(vec![
            ("400 Bad Request", r#"{"error":"context too long"}"#),
            ("200 OK", "{}"),
        ]);
        let err = post(&base).unwrap_err().to_string();
        assert!(err.contains("status 400") && err.contains("context too long"), "{err}");
        assert_eq!(rx.try_iter().count(), 1);
    }

    #[test]
    fn a_chat_call_gives_up_after_its_retries() {
        let (base, _rx) = crate::testhttp::serve(vec![("500 Oops", "{}"); 3]);
        let err = post(&base).unwrap_err().to_string();
        assert!(err.contains("after 3 attempts") && err.contains("status 500"), "{err}");
    }

    #[test]
    fn build_defaults_to_null_llm_and_no_embeddings() {
        let cfg = crate::config::ProvidersConfig::default();
        let (llm, emb) = build(&cfg).unwrap();
        let req = ChatRequest { system: "", messages: &[], tools: &[], background: false };
        let resp = llm.chat(&req).unwrap();
        assert!(resp.text.is_empty());
        assert_eq!(llm.chat_with_reasoning(&req).unwrap().1, "");
        assert!(resp.tool_calls.is_empty());
        assert!(emb.is_none());
    }

    #[test]
    fn build_rejects_unknown_kind() {
        let cfg = crate::config::ProvidersConfig {
            llm: Some(crate::config::ProviderConfig {
                kind: "carrier-pigeon".into(),
                base_url: String::new(), model: String::new(), api_key_env: String::new(),
                api_key_file: std::path::PathBuf::new(), timeout_secs: 45, background_timeout_secs: 180,
                reasoning: String::new(), cache_ttl_min: None,
            }),
            embeddings: None,
        };
        let err = match build(&cfg) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("unknown kind must be rejected"),
        };
        assert!(err.contains("carrier-pigeon"), "{err}");
    }

    #[test]
    fn anthropic_without_key_env_errors() {
        let cfg = crate::config::ProvidersConfig {
            llm: Some(crate::config::ProviderConfig {
                kind: "anthropic".into(), base_url: String::new(),
                model: "m".into(), api_key_env: "NOTE_TEST_MISSING_KEY".into(),
                api_key_file: std::path::PathBuf::new(), timeout_secs: 45, background_timeout_secs: 180,
                reasoning: String::new(), cache_ttl_min: None,
            }),
            embeddings: None,
        };
        let err = match build(&cfg) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("missing key env must be rejected"),
        };
        assert!(err.contains("NOTE_TEST_MISSING_KEY"), "{err}");
    }

    fn file_key_config(path: std::path::PathBuf) -> crate::config::ProviderConfig {
        crate::config::ProviderConfig {
            kind: "openai".into(), base_url: "http://localhost:1/v1".into(),
            model: "m".into(), api_key_env: String::new(), api_key_file: path,
            timeout_secs: 45, background_timeout_secs: 180, reasoning: String::new(), cache_ttl_min: None,
        }
    }

    #[test]
    fn a_call_speaks_on_its_own_model_only_through_openai() {
        let voice: crate::config::VoiceConfig =
            toml::from_str("socket = \"/run/v.sock\"\nmodel = \"fast\"").unwrap();
        let main: Arc<dyn LLMProvider> = Arc::new(mock::NullLLM);
        let mut cfg = crate::config::ProvidersConfig { llm: None, embeddings: None };
        assert!(Arc::ptr_eq(&build_voice(&cfg, &voice, &main).unwrap(), &main));
        cfg.llm = Some(file_key_config(std::path::PathBuf::new()));
        let llm = build_voice(&cfg, &voice, &main).unwrap();
        assert!(!Arc::ptr_eq(&llm, &main));
        assert_eq!(llm.model().as_deref(), Some("fast"));
        let unset: crate::config::VoiceConfig = toml::from_str("socket = \"/run/v.sock\"").unwrap();
        assert_eq!(build_voice(&cfg, &unset, &main).unwrap().model().as_deref(), Some("m"));
    }

    #[test]
    fn a_call_switches_reasoning_off_unless_told_not_to() {
        let reply = r#"{"choices":[{"message":{"content":"hi"}}]}"#;
        let main: Arc<dyn LLMProvider> = Arc::new(mock::NullLLM);
        let req = ChatRequest { system: "s", messages: &[], tools: &[], background: false };
        let sent = |toml: &str| {
            let (base, rx) = crate::testhttp::serve(vec![("200 OK", reply)]);
            let mut p = file_key_config(std::path::PathBuf::new());
            p.base_url = base;
            let cfg = crate::config::ProvidersConfig { llm: Some(p), embeddings: None };
            let voice: crate::config::VoiceConfig = toml::from_str(toml).unwrap();
            build_voice(&cfg, &voice, &main).unwrap().chat(&req).unwrap();
            crate::testhttp::body_json(&rx.recv().unwrap())
        };
        let on = sent("socket = \"/run/v.sock\"");
        assert_eq!(on["reasoning"], serde_json::json!({"enabled": false}));
        let off = sent("socket = \"/run/v.sock\"\nreasoning_off = false");
        assert!(off.get("reasoning").is_none(), "{off}");
    }

    #[test]
    fn reasoning_effort_defaults_to_none_and_rejects_junk() {
        let mut cfg = file_key_config(std::path::PathBuf::new());
        assert_eq!(reasoning_effort(&cfg).unwrap(), None);
        cfg.reasoning = "none".into();
        assert_eq!(reasoning_effort(&cfg).unwrap(), None);
        cfg.reasoning = "high".into();
        assert_eq!(reasoning_effort(&cfg).unwrap(), Some("high"));
        cfg.reasoning = "extreme".into();
        assert!(reasoning_effort(&cfg).is_err());
    }

    #[test]
    fn key_file_wins_and_is_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        std::fs::write(&path, "  nvapi-secret\n").unwrap();
        let key = read_key(&file_key_config(path), true).unwrap();
        assert_eq!(key, "nvapi-secret");
    }

    #[test]
    fn missing_key_file_errors_with_path() {
        let err = read_key(&file_key_config("/nonexistent/note-key".into()), false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("/nonexistent/note-key"), "{err}");
    }

    #[test]
    fn blank_key_file_errors_even_when_optional() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        std::fs::write(&path, "\n").unwrap();
        let err = read_key(&file_key_config(path), false).unwrap_err().to_string();
        assert!(err.contains("empty"), "{err}");
    }
}
