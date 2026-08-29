use super::{ChatRequest, ChatResponse, EmbeddingsProvider, LLMProvider};
use anyhow::Result;
use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct RecordedChat {
    pub system: String,
    pub n_messages: usize,
    pub tool_names: Vec<String>,
}

#[derive(Default)]
pub struct MockLLM {
    script: Mutex<VecDeque<ChatResponse>>,
    seen: Mutex<Vec<RecordedChat>>,
}

impl MockLLM {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn scripted(responses: Vec<ChatResponse>) -> Self {
        Self { script: Mutex::new(responses.into()), seen: Mutex::new(Vec::new()) }
    }

    pub fn seen(&self) -> Vec<RecordedChat> {
        self.seen.lock().unwrap().clone()
    }
}

impl LLMProvider for MockLLM {
    fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        self.seen.lock().unwrap().push(RecordedChat {
            system: req.system.to_string(),
            n_messages: req.messages.len(),
            tool_names: req
                .tools
                .iter()
                .filter_map(|t| t["name"].as_str().map(String::from))
                .collect(),
        });
        Ok(self.script.lock().unwrap().pop_front().unwrap_or(ChatResponse {
            text: "(mock: no scripted response)".into(),
            tool_calls: vec![],
        }))
    }
}

/// Dim-8 letter-count vectors: deterministic, and texts sharing letters get
/// positive cosine similarity — enough to test ranking without a real model.
pub struct MockEmbeddings;

impl EmbeddingsProvider for MockEmbeddings {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = [0f32; 8];
                for b in t.to_lowercase().bytes() {
                    if (b'a'..=b'h').contains(&b) {
                        v[(b - b'a') as usize] += 1.0;
                    }
                }
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 0.0 {
                    v.iter_mut().for_each(|x| *x /= norm);
                }
                v.to_vec()
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ChatRequest, ChatResponse, EmbeddingsProvider, LLMProvider, Message, ToolCall};

    #[test]
    fn scripted_llm_pops_in_order_then_defaults() {
        let llm = MockLLM::scripted(vec![
            ChatResponse { text: "one".into(), tool_calls: vec![] },
            ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall { id: "c1".into(), name: "task_create".into(), args: "{}".into() }],
            },
        ]);
        let req = ChatRequest { system: "sys", messages: &[Message::User("hi".into())], tools: &[] };
        assert_eq!(llm.chat(&req).unwrap().text, "one");
        assert_eq!(llm.chat(&req).unwrap().tool_calls[0].name, "task_create");
        assert!(llm.chat(&req).unwrap().text.contains("no scripted response"));
        let seen = llm.seen();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].system, "sys");
        assert_eq!(seen[0].n_messages, 1);
    }

    #[test]
    fn mock_embeddings_are_deterministic_and_similarity_ordered() {
        let e = MockEmbeddings;
        let vs = e.embed(&["aaab", "aab", "hhh"]).unwrap();
        assert_eq!(vs[0], e.embed(&["aaab"]).unwrap()[0]);
        let cos = |a: &Vec<f32>, b: &Vec<f32>| -> f32 {
            a.iter().zip(b).map(|(x, y)| x * y).sum()
        };
        assert!(cos(&vs[0], &vs[1]) > cos(&vs[0], &vs[2]));
    }
}
