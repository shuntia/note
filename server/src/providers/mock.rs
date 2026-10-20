use super::{ChatRequest, ChatResponse, EmbeddingsProvider, LLMProvider, Message, StreamOpts, StreamSink, ToolCall};
use anyhow::Result;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum StreamPiece {
    Thought(&'static str),
    Text(&'static str),
    Call(ToolCall),
    Wait(Duration),
    Fail(&'static str),
    /// A provider refusal with this status and body.
    Status(u16, &'static str),
}

#[derive(Debug, Clone)]
pub struct RecordedChat {
    pub system: String,
    pub n_messages: usize,
    pub messages: Vec<Message>,
    pub tool_names: Vec<String>,
    pub plain: bool,
    pub must_call: Option<String>,
    pub model: Option<String>,
    pub effort: Option<super::Effort>,
}

#[derive(Default)]
pub struct MockLLM {
    script: Mutex<VecDeque<ChatResponse>>,
    thinking: Mutex<VecDeque<String>>,
    rounds: Mutex<VecDeque<Vec<StreamPiece>>>,
    seen: Mutex<Vec<RecordedChat>>,
    streams: bool,
    usage: Option<crate::usage::Usage>,
    offers: Vec<String>,
}

impl MockLLM {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn scripted(responses: Vec<ChatResponse>) -> Self {
        Self { script: Mutex::new(responses.into()), ..Self::default() }
    }

    /// One round of pieces per `chat_stream` call, in order.
    pub fn streamed(rounds: Vec<Vec<StreamPiece>>) -> Self {
        Self { rounds: Mutex::new(rounds.into()), streams: true, ..Self::default() }
    }

    /// One reasoning text per scripted round, in order.
    #[must_use]
    pub fn thinking(self, texts: Vec<&str>) -> Self {
        *self.thinking.lock().unwrap() = texts.into_iter().map(String::from).collect();
        self
    }

    /// Every call reports `usage` to the open meter.
    #[must_use]
    pub fn with_usage(mut self, usage: crate::usage::Usage) -> Self {
        self.usage = Some(usage);
        self
    }

    /// Offers `models` as `offered()`, the first as `model()`.
    #[must_use]
    pub fn offering(mut self, models: &[&str]) -> Self {
        self.offers = models.iter().map(|m| (*m).to_string()).collect();
        self
    }

    pub fn seen(&self) -> Vec<RecordedChat> {
        self.seen.lock().unwrap().clone()
    }

    fn record(&self, req: &ChatRequest) {
        if let Some(usage) = self.usage {
            crate::usage::record(usage);
        }
        self.seen.lock().unwrap().push(RecordedChat {
            system: req.system.to_string(),
            n_messages: req.messages.len(),
            messages: req.messages.to_vec(),
            tool_names: req
                .tools
                .iter()
                .filter_map(|t| t["name"].as_str().map(String::from))
                .collect(),
            plain: req.plain,
            must_call: req.must_call.map(|m| match m {
                super::MustCall::Any => "*".to_string(),
                super::MustCall::Tool(name) => name.to_string(),
                super::MustCall::Nothing => "-".to_string(),
            }),
            model: req.model.map(String::from),
            effort: req.effort,
        });
    }
}

impl LLMProvider for MockLLM {
    fn streams(&self) -> bool {
        self.streams
    }

    fn model(&self) -> Option<String> {
        self.offers.first().cloned()
    }

    fn offered(&self) -> Vec<String> {
        self.offers.clone()
    }

    /// A scripted call's `$token` is filled from the latest tool result's
    /// `token`, as a model copies the one `call_user` handed back.
    fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        self.record(req);
        let mut resp = self.script.lock().unwrap().pop_front().unwrap_or(ChatResponse {
            text: "(mock: no scripted response)".into(),
            tool_calls: vec![],
        });
        let token = req.messages.iter().rev().find_map(|m| match m {
            Message::ToolResult { content, .. } => serde_json::from_str::<serde_json::Value>(content)
                .ok()
                .and_then(|v| v["token"].as_str().map(str::to_string)),
            _ => None,
        });
        if let Some(token) = token {
            for call in &mut resp.tool_calls {
                call.args = call.args.replace("$token", &token);
            }
        }
        Ok(resp)
    }

    fn chat_with_reasoning(&self, req: &ChatRequest) -> Result<(ChatResponse, String)> {
        let resp = self.chat(req)?;
        Ok((resp, self.thinking.lock().unwrap().pop_front().unwrap_or_default()))
    }

    fn chat_stream(&self, req: &ChatRequest, _opts: &StreamOpts, sink: &mut dyn StreamSink) -> Result<ChatResponse> {
        self.record(req);
        let round = self.rounds.lock().unwrap().pop_front().unwrap_or_default();
        let mut resp = ChatResponse::default();
        for piece in round {
            let go_on = match piece {
                StreamPiece::Thought(t) => sink.reasoning(t),
                StreamPiece::Text(t) => {
                    resp.text.push_str(t);
                    sink.text(t)
                }
                StreamPiece::Call(c) => {
                    let go_on = sink.tool_call(&c);
                    resp.tool_calls.push(c);
                    go_on
                }
                StreamPiece::Wait(d) => {
                    std::thread::sleep(d);
                    true
                }
                StreamPiece::Fail(why) => anyhow::bail!("{why}"),
                StreamPiece::Status(code, body) => {
                    return Err(super::ProviderError::status(code, body, format!("status {code}: {body}")).into());
                }
            };
            if !go_on {
                break;
            }
        }
        Ok(resp)
    }
}

/// The default when no LLM is configured: replies blank and records nothing, so
/// a long-running tokenless server neither grows a transcript in memory nor
/// stores placeholder text as a user's debriefs and replies.
pub struct NullLLM;

impl LLMProvider for NullLLM {
    fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse> {
        Ok(ChatResponse { text: String::new(), tool_calls: vec![] })
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
                    for x in &mut v {
                        *x /= norm;
                    }
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
        let req = ChatRequest { system: "sys", messages: &[Message::User("hi".into())], tools: &[], background: false, plain: false, must_call: None, model: None, effort: None };
        assert_eq!(llm.chat(&req).unwrap().text, "one");
        assert_eq!(llm.chat(&req).unwrap().tool_calls[0].name, "task_create");
        assert!(llm.chat(&req).unwrap().text.contains("no scripted response"));
        let seen = llm.seen();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].system, "sys");
        assert_eq!(seen[0].n_messages, 1);
    }

    #[derive(Default)]
    struct Collect(Vec<String>);

    impl crate::providers::StreamSink for Collect {
        fn text(&mut self, delta: &str) -> bool {
            self.0.push(delta.to_string());
            self.0.len() < 2
        }

        fn tool_call(&mut self, call: &ToolCall) -> bool {
            self.0.push(call.id.clone());
            true
        }
    }

    #[test]
    fn streamed_llm_plays_one_round_per_call_and_stops_with_the_sink() {
        let call = ToolCall { id: "c1".into(), name: "task_list".into(), args: "{}".into() };
        let llm = MockLLM::streamed(vec![
            vec![StreamPiece::Call(call), StreamPiece::Wait(Duration::from_millis(1)), StreamPiece::Text("a"), StreamPiece::Text("b")],
            vec![StreamPiece::Text("partial"), StreamPiece::Fail("dropped")],
        ]);
        let req = ChatRequest { system: "sys", messages: &[Message::User("hi".into())], tools: &[], background: false, plain: false, must_call: None, model: None, effort: None };
        let opts = crate::providers::StreamOpts::spoken(Duration::from_secs(1));
        let mut sink = Collect::default();
        let resp = llm.chat_stream(&req, &opts, &mut sink).unwrap();
        assert_eq!(sink.0, vec!["c1", "a"]);
        assert_eq!((resp.text.as_str(), resp.tool_calls.len()), ("a", 1));
        let err = llm.chat_stream(&req, &opts, &mut Collect::default()).unwrap_err();
        assert_eq!(err.to_string(), "dropped");
        assert_eq!(llm.seen().len(), 2);
    }

    #[test]
    fn null_llm_replies_blank_and_stays_stateless() {
        let llm = NullLLM;
        let req = ChatRequest { system: "sys", messages: &[Message::User("hi".into())], tools: &[], background: false, plain: false, must_call: None, model: None, effort: None };
        for _ in 0..3 {
            let r = llm.chat(&req).unwrap();
            assert!(r.text.is_empty());
            assert!(r.tool_calls.is_empty());
        }
        assert_eq!(std::mem::size_of::<NullLLM>(), 0);
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
