pub mod anthropic;
pub mod mock;
pub mod openai;

use anyhow::Result;
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
}

#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
}

pub trait LLMProvider: Send + Sync {
    fn chat(&self, req: &ChatRequest) -> Result<ChatResponse>;
}

pub trait EmbeddingsProvider: Send + Sync {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
}

/// Agent config for chat providers: bounded connect time, and a generous
/// read/write timeout since LLM responses can be slow, so a stalled provider
/// can't hang forever. The overall `timeout` also caps a drip-feeding endpoint
/// that keeps resetting the per-operation ones. Chat calls hold no locks.
pub(crate) fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout_read(std::time::Duration::from_secs(120))
        .timeout_write(std::time::Duration::from_secs(120))
        .timeout(std::time::Duration::from_secs(120))
        .build()
}

/// Embedding calls run in the agent loop's prepare pass with no lock held, so
/// a stalled endpoint delays only its own talk turn. The tight caps bound how
/// long that stall can last.
pub(crate) fn embeddings_http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout_read(std::time::Duration::from_secs(10))
        .timeout_write(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(10))
        .build()
}

/// Builds providers from config. Absent or "mock" LLM config yields a
/// null provider, so the system is fully runnable with no tokens.
pub fn build(cfg: &crate::config::ProvidersConfig) -> Result<(Arc<dyn LLMProvider>, Option<Arc<dyn EmbeddingsProvider>>)> {
    let llm: Arc<dyn LLMProvider> = match &cfg.llm {
        None => Arc::new(mock::NullLLM),
        Some(p) => match p.kind.as_str() {
            "mock" => Arc::new(mock::NullLLM),
            "anthropic" => {
                let key = read_key(&p.api_key_env, true)?;
                Arc::new(anthropic::AnthropicLLM::new(&p.base_url, &p.model, &key))
            }
            "openai" => {
                anyhow::ensure!(!p.base_url.is_empty(), "openai llm provider requires base_url");
                let key = read_key(&p.api_key_env, false)?;
                Arc::new(openai::OpenAILLM::new(&p.base_url, &p.model, &key))
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
                let key = read_key(&p.api_key_env, false)?;
                Some(Arc::new(openai::OpenAIEmbeddings::new(&p.base_url, &p.model, &key)))
            }
            other => anyhow::bail!("unknown embeddings provider kind: {other}"),
        },
    };
    Ok((llm, emb))
}

/// `required` distinguishes Anthropic (key mandatory) from OpenAI-compatible
/// local endpoints that accept no key.
fn read_key(env_name: &str, required: bool) -> Result<String> {
    if env_name.is_empty() {
        anyhow::ensure!(!required, "provider requires api_key_env in config");
        return Ok(String::new());
    }
    match std::env::var(env_name) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ if required => anyhow::bail!("api key env var {env_name} is not set"),
        _ => Ok(String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_defaults_to_null_llm_and_no_embeddings() {
        let cfg = crate::config::ProvidersConfig::default();
        let (llm, emb) = build(&cfg).unwrap();
        let req = ChatRequest { system: "", messages: &[], tools: &[] };
        let resp = llm.chat(&req).unwrap();
        assert!(resp.text.is_empty());
        assert!(resp.tool_calls.is_empty());
        assert!(emb.is_none());
    }

    #[test]
    fn build_rejects_unknown_kind() {
        let cfg = crate::config::ProvidersConfig {
            llm: Some(crate::config::ProviderConfig {
                kind: "carrier-pigeon".into(),
                base_url: String::new(), model: String::new(), api_key_env: String::new(),
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
            }),
            embeddings: None,
        };
        let err = match build(&cfg) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("missing key env must be rejected"),
        };
        assert!(err.contains("NOTE_TEST_MISSING_KEY"), "{err}");
    }
}
