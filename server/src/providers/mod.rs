pub mod mock;

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

/// Builds providers from config. Absent or "mock" LLM config yields an
/// unscripted mock, so the system is fully runnable with no tokens.
pub fn build(cfg: &crate::config::ProvidersConfig) -> Result<(Arc<dyn LLMProvider>, Option<Arc<dyn EmbeddingsProvider>>)> {
    let llm: Arc<dyn LLMProvider> = match &cfg.llm {
        None => Arc::new(mock::MockLLM::empty()),
        Some(p) => match p.kind.as_str() {
            "mock" => Arc::new(mock::MockLLM::empty()),
            other => anyhow::bail!("unknown llm provider kind: {other}"),
        },
    };
    let emb: Option<Arc<dyn EmbeddingsProvider>> = match &cfg.embeddings {
        None => None,
        Some(p) => match p.kind.as_str() {
            "mock" => Some(Arc::new(mock::MockEmbeddings)),
            other => anyhow::bail!("unknown embeddings provider kind: {other}"),
        },
    };
    Ok((llm, emb))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_defaults_to_mock_llm_and_no_embeddings() {
        let cfg = crate::config::ProvidersConfig::default();
        let (llm, emb) = build(&cfg).unwrap();
        let req = ChatRequest { system: "", messages: &[], tools: &[] };
        assert!(llm.chat(&req).unwrap().text.contains("no scripted response"));
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
}
