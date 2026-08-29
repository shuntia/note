# Note Providers, Agent Runtime & Nightly Job Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The provider layer (`LLMProvider` for Anthropic + OpenAI-compatible endpoints, `EmbeddingsProvider`, deterministic mocks), vector-augmented hybrid memory retrieval, the agent runtime (tool loop over `tools::dispatch`), a `POST /api/talk` text conversation route, and the per-user nightly plan+debrief job — fully buildable and testable with no live tokens.

**Architecture:** Providers are synchronous trait objects (`Arc<dyn LLMProvider>`) using `ureq` (rustls); async call sites wrap sessions in `spawn_blocking`. The agent runtime is pure orchestration: build system prompt (persona files + `context::assemble`), loop `llm.chat` → `tools::dispatch` per tool call → tool-result messages, bounded turns. The DB mutex is held per-operation, never across a network call. The nightly job generates the day's plan from the template in code first (the fallback guarantee), then runs a Nightly agent session and stores a debrief row; idempotency is the debrief's `UNIQUE(user_id, date)`. Embeddings write vectors beside the memory index; query fuses lexical FTS and cosine ranks, degrading to lexical-only when no provider is configured or the embed call fails.

**Tech Stack:** Existing stack plus `ureq = { version = "2", features = ["json"] }`. No other new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-29-note-design.md` (Provider layer, Agent runtime, Memory layer, Context injection, Error handling, Testing). Prior phases: `docs/superpowers/plans/2026-08-29-note-foundation.md`, `docs/superpowers/plans/2026-08-29-note-agent-tools.md`.

## Global Constraints

- All prior constraints hold: wall-clock `HH:MM` + IANA tz; pragmas on open; comments only at fn declarations where code can't speak; `cargo test` green from repo root before every commit; imperative one-line commit messages.
- **No model in the plumbing:** context assembly, scheduling, and delivery stay pure code. Models plan, converse, and decide — through `tools::dispatch` only. The agent runtime must not touch the DB or filesystem except via `context::assemble`, `prompts::load`, `log::record`, and `tools::dispatch`.
- **Fully testable with no live tokens:** mocks are the default provider; every test runs offline. HTTP provider impls are tested via pure request-builder/response-parser functions on JSON fixtures, never live calls.
- **Never hold the DB lock across a network call.** Lock for `assemble`, lock per `dispatch`, lock for plan generation/debrief writes — release before/after each `llm.chat`/`embed`.
- **Error handling per spec:** scheduler/UI/delivery keep running when providers are down. Nightly falls back to "template plan + fallback debrief" on LLM failure (logged `nightly_fallback`); embed failures degrade to lexical (logged `memory_embed_error`); every degradation is logged via `log::record`.
- Tool-result content returned to the model is the serialized `Ok` JSON value or the serialized `ToolError` with `is_error: true` — typed rejections reach the model, never abort the session.
- `MAX_TURNS = 16` per session; hitting the cap logs `agent_max_turns` and returns the last text.
- This plan also retires the phase-2 deferred hardening debt (each folded into the task touching that file): atomic file writes (write-temp+rename), `edit_append` non-NotFound error propagation, UTC-fallback tz labeling, `reindex_all` root-error logging, `check_text` consolidation in memory_ops, and a registry-subset test (CHECKIN ⊆ TALK ⊆ NIGHTLY).

---

### Task 1: Provider traits, config, mocks, factory

**Files:**
- Create: `server/src/providers/mod.rs`, `server/src/providers/mock.rs`
- Modify: `server/src/config.rs` (providers section), `server/src/lib.rs` (add `pub mod providers;`)
- Test: inline in both new files + `config.rs`

**Interfaces:**
- Produces:
  - `providers::ToolCall { id: String, name: String, args: String }` (args = raw JSON string).
  - `providers::Message { User(String), Assistant { text: String, tool_calls: Vec<ToolCall> }, ToolResult { call_id: String, content: String, is_error: bool } }` (all Clone + Debug).
  - `providers::ChatRequest<'a> { system: &'a str, messages: &'a [Message], tools: &'a [serde_json::Value] }`.
  - `providers::ChatResponse { text: String, tool_calls: Vec<ToolCall> }` (Clone, Debug, Default).
  - `providers::LLMProvider: Send + Sync { fn chat(&self, req: &ChatRequest) -> anyhow::Result<ChatResponse>; }`.
  - `providers::EmbeddingsProvider: Send + Sync { fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>>; }`.
  - `providers::build(cfg: &config::ProvidersConfig) -> anyhow::Result<(Arc<dyn LLMProvider>, Option<Arc<dyn EmbeddingsProvider>>)>` — this task wires `mock`/absent kinds; Task 2 adds `anthropic`/`openai` arms (an unknown kind is an error naming the kind).
  - `mock::MockLLM` — `empty()`, `scripted(Vec<ChatResponse>)`; `chat` pops the script front (empty script → `ChatResponse { text: "(mock: no scripted response)".into(), tool_calls: vec![] }`) and records `RecordedChat { system: String, n_messages: usize, tool_names: Vec<String> }`; `seen() -> Vec<RecordedChat>`.
  - `mock::MockEmbeddings` — deterministic dim-8 vectors: counts of bytes `a`–`h` in the lowercased text, L2-normalized (zero vector for text with none); same text ⇒ same vector, shared letters ⇒ positive cosine.
  - `config::ProviderConfig { kind: String, base_url: String (default ""), model: String (default ""), api_key_env: String (default "") }` (Deserialize, Clone).
  - `config::ProvidersConfig { llm: Option<ProviderConfig>, embeddings: Option<ProviderConfig> }` (Deserialize, Default, `#[serde(default)]`).
  - `ServerConfig` gains `#[serde(default)] pub providers: ProvidersConfig` — existing `server.toml` files without the section keep loading.

- [ ] **Step 1: Write the failing tests**

Inline in `server/src/providers/mock.rs`:

```rust
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
```

Inline in `server/src/providers/mod.rs`:

```rust
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
        let err = build(&cfg).unwrap_err().to_string();
        assert!(err.contains("carrier-pigeon"), "{err}");
    }
}
```

Add to `config.rs` tests:

```rust
    #[test]
    fn server_config_without_providers_section_still_loads() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "server.toml",
            "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n");
        let cfg = ServerConfig::load(tmp.path()).unwrap();
        assert!(cfg.providers.llm.is_none());
    }

    #[test]
    fn providers_section_parses() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "server.toml", concat!(
            "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n",
            "[providers.llm]\nkind = \"anthropic\"\nmodel = \"claude-sonnet-5\"\napi_key_env = \"ANTHROPIC_API_KEY\"\n",
            "[providers.embeddings]\nkind = \"openai\"\nbase_url = \"http://localhost:8080/v1\"\nmodel = \"embeddinggemma\"\n")); 
        let cfg = ServerConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.providers.llm.unwrap().kind, "anthropic");
        assert_eq!(cfg.providers.embeddings.unwrap().base_url, "http://localhost:8080/v1");
    }
```

(`config.rs` tests currently have a local `write` helper inside the tests module of that file — reuse it; if it doesn't exist there, add the same 4-line helper used by the existing tests.)

- [ ] **Step 2: Run to verify failure** — `cargo test --lib providers config` — expected: compile FAIL.

- [ ] **Step 3: Implement**

`server/src/providers/mod.rs`:

```rust
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
```

`server/src/providers/mock.rs`:

```rust
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
```

`server/src/config.rs` additions:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub kind: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub api_key_env: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ProvidersConfig {
    pub llm: Option<ProviderConfig>,
    pub embeddings: Option<ProviderConfig>,
}
```

and on `ServerConfig`: `#[serde(default)] pub providers: ProvidersConfig`.

Add `pub mod providers;` to `server/src/lib.rs`.

- [ ] **Step 4: Run** `cargo test` — expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat: provider traits with mock llm and embeddings"`

---

### Task 2: Anthropic + OpenAI-compatible providers

**Files:**
- Create: `server/src/providers/anthropic.rs`, `server/src/providers/openai.rs`
- Modify: `server/Cargo.toml` (add `ureq = { version = "2", features = ["json"] }`), `server/src/providers/mod.rs` (module decls + factory arms)
- Test: inline fixture tests in both files (no network)

**Interfaces:**
- Consumes: Task 1's types.
- Produces:
  - `anthropic::AnthropicLLM::new(base_url: &str, model: &str, api_key: &str) -> Self` (empty `base_url` defaults to `https://api.anthropic.com`); implements `LLMProvider` via `POST {base}/v1/messages` with headers `x-api-key`, `anthropic-version: 2023-06-01`, `content-type: application/json`.
  - `openai::OpenAILLM::new(base_url: &str, model: &str, api_key: &str) -> Self` — `POST {base}/chat/completions`, `Authorization: Bearer …` only when key non-empty (local llama.cpp needs none).
  - `openai::OpenAIEmbeddings::new(base_url, model, api_key)` — `POST {base}/embeddings`, request `{model, input: [texts…]}`, response `data[i].embedding`, returned **in index order** (sort by `.index`).
  - Pure mapping fns (the testable core; the trait impls are thin HTTP shells around them):
    - `anthropic::body(model: &str, req: &ChatRequest) -> serde_json::Value`
    - `anthropic::parse(v: &serde_json::Value) -> anyhow::Result<ChatResponse>`
    - `openai::body(model: &str, req: &ChatRequest) -> serde_json::Value`
    - `openai::parse(v: &serde_json::Value) -> anyhow::Result<ChatResponse>`
  - Factory arms in `providers::build`: `"anthropic"` (requires `api_key_env` set and the env var non-empty, else error naming the var), `"openai"` for LLM (requires `base_url`; key optional via `api_key_env`), `"openai"` for embeddings (same rules).

Mapping rules (binding):
- **Anthropic:** `system` top-level; `Message::User` → `{role:"user",content:[{type:"text",text}]}`; `Assistant{text,tool_calls}` → assistant content: text block (only if non-empty) + one `{type:"tool_use", id, name, input: <parsed args JSON, or {} if unparseable>}` per call; **consecutive `ToolResult`s merge into one user message** of `{type:"tool_result", tool_use_id, content, is_error}` blocks. Tools pass through as-is (`{name, description, input_schema}` is already Anthropic's format). `max_tokens: 4096`. Parse: concatenate `text` blocks into `text`, each `tool_use` block → `ToolCall { id, name, args: input serialized back to a JSON string }`.
- **OpenAI:** `system` becomes the first message `{role:"system"}`; `Assistant` → `{role:"assistant", content: text-or-null, tool_calls:[{id, type:"function", function:{name, arguments: args}}]}` (omit `tool_calls` when empty); each `ToolResult` → its own `{role:"tool", tool_call_id, content}` (`is_error` has no OpenAI slot — prefix content with `"ERROR: "` when set). Tools wrap: `{type:"function", function:{name, description, parameters: input_schema}}`. Parse `choices[0].message`: `content` (null → empty) + `tool_calls[].function` → `ToolCall { id, name, args: arguments }`.

- [ ] **Step 1: Write the failing fixture tests**

Inline in `server/src/providers/anthropic.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ChatRequest, Message, ToolCall};

    fn req_fixture() -> Vec<Message> {
        vec![
            Message::User("plan my day".into()),
            Message::Assistant {
                text: "checking".into(),
                tool_calls: vec![ToolCall { id: "t1".into(), name: "memory_query".into(), args: r#"{"query":"day"}"#.into() }],
            },
            Message::ToolResult { call_id: "t1".into(), content: r#"{"results":[]}"#.into(), is_error: false },
            Message::ToolResult { call_id: "t2".into(), content: r#"{"kind":"rejected"}"#.into(), is_error: true },
        ]
    }

    #[test]
    fn body_maps_roles_tools_and_merges_tool_results() {
        let tools = vec![serde_json::json!({"name":"memory_query","description":"d","input_schema":{"type":"object"}})];
        let msgs = req_fixture();
        let b = body("claude-sonnet-5", &ChatRequest { system: "be kind", messages: &msgs, tools: &tools });
        assert_eq!(b["system"], "be kind");
        assert_eq!(b["max_tokens"], 4096);
        assert_eq!(b["tools"][0]["name"], "memory_query");
        let m = b["messages"].as_array().unwrap();
        assert_eq!(m.len(), 3, "two consecutive tool results must merge into one user message");
        assert_eq!(m[1]["content"][1]["type"], "tool_use");
        assert_eq!(m[1]["content"][1]["input"]["query"], "day");
        assert_eq!(m[2]["role"], "user");
        assert_eq!(m[2]["content"][1]["is_error"], true);
    }

    #[test]
    fn parse_extracts_text_and_tool_use() {
        let v = serde_json::json!({"content":[
            {"type":"text","text":"on it. "},
            {"type":"tool_use","id":"x1","name":"task_create","input":{"title":"milk"}},
            {"type":"text","text":"done"}
        ]});
        let r = parse(&v).unwrap();
        assert_eq!(r.text, "on it. done");
        assert_eq!(r.tool_calls[0].name, "task_create");
        assert!(r.tool_calls[0].args.contains("milk"));
    }

    #[test]
    fn parse_rejects_shapeless_response() {
        assert!(parse(&serde_json::json!({"error":{"message":"overloaded"}})).is_err());
    }
}
```

Inline in `server/src/providers/openai.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ChatRequest, Message, ToolCall};

    #[test]
    fn body_wraps_tools_and_maps_tool_results() {
        let tools = vec![serde_json::json!({"name":"task_create","description":"d","input_schema":{"type":"object"}})];
        let msgs = vec![
            Message::User("hi".into()),
            Message::Assistant { text: String::new(), tool_calls: vec![ToolCall { id: "c1".into(), name: "task_create".into(), args: "{}".into() }] },
            Message::ToolResult { call_id: "c1".into(), content: "{\"task_id\":1}".into(), is_error: false },
            Message::ToolResult { call_id: "c2".into(), content: "{\"kind\":\"rejected\"}".into(), is_error: true },
        ];
        let b = body("gpt-x", &ChatRequest { system: "sys", messages: &msgs, tools: &tools });
        let m = b["messages"].as_array().unwrap();
        assert_eq!(m[0]["role"], "system");
        assert_eq!(m[2]["tool_calls"][0]["function"]["name"], "task_create");
        assert_eq!(m[3]["role"], "tool");
        assert_eq!(m[4]["content"].as_str().unwrap(), "ERROR: {\"kind\":\"rejected\"}");
        assert_eq!(b["tools"][0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn parse_reads_choices_message() {
        let v = serde_json::json!({"choices":[{"message":{
            "content": null,
            "tool_calls":[{"id":"c9","type":"function","function":{"name":"schedule_drop","arguments":"{\"event_id\":2}"}}]
        }}]});
        let r = parse(&v).unwrap();
        assert_eq!(r.text, "");
        assert_eq!(r.tool_calls[0].args, "{\"event_id\":2}");
    }

    #[test]
    fn embeddings_response_sorted_by_index() {
        let v = serde_json::json!({"data":[
            {"index":1,"embedding":[2.0]},
            {"index":0,"embedding":[1.0]}
        ]});
        let vs = parse_embeddings(&v).unwrap();
        assert_eq!(vs, vec![vec![1.0f32], vec![2.0f32]]);
    }
}
```

Factory test appended to `providers/mod.rs` tests:

```rust
    #[test]
    fn anthropic_without_key_env_errors() {
        let cfg = crate::config::ProvidersConfig {
            llm: Some(crate::config::ProviderConfig {
                kind: "anthropic".into(), base_url: String::new(),
                model: "m".into(), api_key_env: "NOTE_TEST_MISSING_KEY".into(),
            }),
            embeddings: None,
        };
        let err = build(&cfg).unwrap_err().to_string();
        assert!(err.contains("NOTE_TEST_MISSING_KEY"), "{err}");
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib providers` — expected: compile FAIL.

- [ ] **Step 3: Implement**

`server/src/providers/anthropic.rs` core (the HTTP shell + pure fns):

```rust
use super::{ChatRequest, ChatResponse, LLMProvider, ToolCall};
use anyhow::{Context, Result};

pub struct AnthropicLLM {
    agent: ureq::Agent,
    base_url: String,
    model: String,
    api_key: String,
}

impl AnthropicLLM {
    pub fn new(base_url: &str, model: &str, api_key: &str) -> Self {
        let base = if base_url.is_empty() { "https://api.anthropic.com" } else { base_url };
        Self {
            agent: ureq::Agent::new(),
            base_url: base.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key: api_key.to_string(),
        }
    }
}

pub fn body(model: &str, req: &ChatRequest) -> serde_json::Value {
    let mut messages: Vec<serde_json::Value> = Vec::new();
    for m in req.messages {
        match m {
            super::Message::User(t) => messages.push(serde_json::json!({
                "role": "user", "content": [{"type": "text", "text": t}]
            })),
            super::Message::Assistant { text, tool_calls } => {
                let mut content = Vec::new();
                if !text.is_empty() {
                    content.push(serde_json::json!({"type": "text", "text": text}));
                }
                for c in tool_calls {
                    let input: serde_json::Value =
                        serde_json::from_str(&c.args).unwrap_or_else(|_| serde_json::json!({}));
                    content.push(serde_json::json!({
                        "type": "tool_use", "id": c.id, "name": c.name, "input": input
                    }));
                }
                messages.push(serde_json::json!({"role": "assistant", "content": content}));
            }
            super::Message::ToolResult { call_id, content, is_error } => {
                let block = serde_json::json!({
                    "type": "tool_result", "tool_use_id": call_id,
                    "content": content, "is_error": is_error
                });
                match messages.last_mut() {
                    Some(last) if last["role"] == "user" && last["content"][0]["type"] == "tool_result" => {
                        last["content"].as_array_mut().unwrap().push(block);
                    }
                    _ => messages.push(serde_json::json!({"role": "user", "content": [block]})),
                }
            }
        }
    }
    serde_json::json!({
        "model": model, "max_tokens": 4096, "system": req.system,
        "messages": messages, "tools": req.tools
    })
}

pub fn parse(v: &serde_json::Value) -> Result<ChatResponse> {
    let blocks = v["content"].as_array().context("anthropic response has no content array")?;
    let mut out = ChatResponse::default();
    for b in blocks {
        match b["type"].as_str() {
            Some("text") => out.text.push_str(b["text"].as_str().unwrap_or("")),
            Some("tool_use") => out.tool_calls.push(ToolCall {
                id: b["id"].as_str().unwrap_or("").to_string(),
                name: b["name"].as_str().unwrap_or("").to_string(),
                args: b["input"].to_string(),
            }),
            _ => {}
        }
    }
    Ok(out)
}

impl LLMProvider for AnthropicLLM {
    fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let resp: serde_json::Value = self
            .agent
            .post(&format!("{}/v1/messages", self.base_url))
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", "2023-06-01")
            .send_json(body(&self.model, req))
            .context("anthropic request failed")?
            .into_json()?;
        parse(&resp)
    }
}
```

`server/src/providers/openai.rs` in the same shape: `OpenAILLM` (`Authorization: Bearer` header only when `api_key` non-empty), pure `body`/`parse` per the binding mapping rules, plus:

```rust
pub struct OpenAIEmbeddings { /* agent, base_url, model, api_key */ }

pub fn parse_embeddings(v: &serde_json::Value) -> Result<Vec<Vec<f32>>> {
    let mut rows: Vec<(i64, Vec<f32>)> = v["data"]
        .as_array()
        .context("embeddings response has no data array")?
        .iter()
        .map(|d| {
            let vec = d["embedding"].as_array().context("no embedding")?
                .iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect();
            Ok((d["index"].as_i64().unwrap_or(0), vec))
        })
        .collect::<Result<_>>()?;
    rows.sort_by_key(|(i, _)| *i);
    Ok(rows.into_iter().map(|(_, v)| v).collect())
}

impl super::EmbeddingsProvider for OpenAIEmbeddings {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let resp: serde_json::Value = /* POST {base}/embeddings with {model, input: texts} */
        parse_embeddings(&resp)
    }
}
```

Factory arms in `providers::build` (replacing the bail arms):

```rust
            "anthropic" => {
                let key = read_key(&p.api_key_env, true)?;
                Arc::new(anthropic::AnthropicLLM::new(&p.base_url, &p.model, &key))
            }
            "openai" => {
                anyhow::ensure!(!p.base_url.is_empty(), "openai llm provider requires base_url");
                let key = read_key(&p.api_key_env, false)?;
                Arc::new(openai::OpenAILLM::new(&p.base_url, &p.model, &key))
            }
```

with:

```rust
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
```

and the embeddings `"openai"` arm mirroring the LLM one. Add `pub mod anthropic; pub mod openai;` to `providers/mod.rs`.

- [ ] **Step 4: Run** `cargo test` — expected: PASS (all offline).

- [ ] **Step 5: Commit** — `git commit -am "feat: anthropic and openai-compatible providers"`

---

### Task 3: Migration v3 — memory_vectors + debriefs

**Files:**
- Modify: `server/src/db.rs`
- Test: inline in `db.rs`

**Interfaces:**
- Produces:
  - `memory_vectors (user TEXT NOT NULL, id TEXT NOT NULL, vector BLOB NOT NULL, PRIMARY KEY (user, id))`.
  - `debriefs (id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id), date TEXT NOT NULL, content TEXT NOT NULL, created_at TEXT NOT NULL, UNIQUE (user_id, date))`.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn v3_creates_vector_and_debrief_tables() {
        let conn = open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')", [],
        ).unwrap();
        conn.execute(
            "INSERT INTO memory_vectors (user, id, vector) VALUES ('a', 'x', X'00000000')", [],
        ).unwrap();
        conn.execute(
            "INSERT INTO debriefs (user_id, date, content, created_at) VALUES (1, '2026-08-31', 'ok', 't')", [],
        ).unwrap();
        // second debrief for the same (user, date) must be refused
        assert!(conn.execute(
            "INSERT INTO debriefs (user_id, date, content, created_at) VALUES (1, '2026-08-31', 'dup', 't')", [],
        ).is_err());
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib db` — expected: FAIL (no such table).

- [ ] **Step 3: Implement** — append to `MIGRATIONS`:

```rust
    // v3
    "
    CREATE TABLE memory_vectors (
        user TEXT NOT NULL,
        id TEXT NOT NULL,
        vector BLOB NOT NULL,
        PRIMARY KEY (user, id)
    );
    CREATE TABLE debriefs (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        date TEXT NOT NULL,
        content TEXT NOT NULL,
        created_at TEXT NOT NULL,
        UNIQUE (user_id, date)
    );
    ",
```

- [ ] **Step 4: Run** `cargo test` — expected: PASS (the `migrations_apply_and_are_idempotent` test's version assertion uses `MIGRATIONS.len()`, so it stays green).

- [ ] **Step 5: Commit** — `git commit -am "feat: schema v3 with memory vectors and debriefs"`

---

### Task 4: Vector memory — embed on write, hybrid query, hardening

**Files:**
- Modify: `server/src/memory.rs`, `server/src/tools/mod.rs` (ToolCtx field), `server/src/tools/memory_ops.rs` (pass-through + `check_text` consolidation), `server/tests/tool_fuzz.rs` + `server/tests/tool_invariants.rs` (ToolCtx construction sites)
- Test: inline in `memory.rs`

**Interfaces:**
- Consumes: `providers::EmbeddingsProvider`, `mock::MockEmbeddings`, `memory_vectors` table.
- Produces (signature changes — every listed caller updates in this task):
  - `memory::add(conn, data_dir, user, category, summary, body, emb: Option<&dyn EmbeddingsProvider>) -> Result<String>`
  - `memory::update(conn, data_dir, user, id, summary, body, emb: Option<&dyn EmbeddingsProvider>) -> Result<Option<()>, WriteError>`
  - `memory::supersede(conn, data_dir, user, old_id, summary, body, emb: Option<&dyn EmbeddingsProvider>) -> Result<Option<String>, WriteError>`
  - `memory::query(conn, user, q, limit, emb: Option<&dyn EmbeddingsProvider>) -> Result<Vec<QueryHit>>`
  - `tools::ToolCtx` gains `pub embeddings: Option<&'a dyn crate::providers::EmbeddingsProvider>` — construction sites to update: `tools/mod.rs` tests, `tools/memory_ops.rs` tests, `tools/context_ops.rs` tests, `tools/schedule_ops.rs` tests, `tests/tool_fuzz.rs`, `tests/tool_invariants.rs` (all add `embeddings: None`).
  - `memory_ops::{query, write}` pass `ctx.embeddings` through.
- Behavior (binding):
  - On add/update: embed `"{summary}\n{body}"`; store as little-endian f32 BLOB in `memory_vectors`. On supersede: delete the old id's vector row, insert the new one. Embed failure never fails the write: log `memory_embed_error` via `log::record` and continue.
  - Query: lexical FTS top 32 as today. If `emb` is Some and embedding the query succeeds: load all non-archived vectors for the user, cosine-rank top 32, fuse with reciprocal rank fusion (`score = Σ 1/(60 + rank)`, ranks 0-based per list), return top `limit` by fused score. Lexical-only otherwise (including embed failure, logged).
  - `reindex_user` additionally prunes stale vectors: `DELETE FROM memory_vectors WHERE user = ?1 AND id NOT IN (SELECT id FROM memory_index WHERE user = ?1 AND archived = 0)` (run after the rebuild).
- Hardening folded in (phase-2 debt):
  - New private `fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()>` — write to `path.with_extension("md.tmp")` then `fs::rename`; used by every memory file write (`add`, `update`, `supersede`).
  - `reindex_all`: an unreadable `data_dir/memory` root that exists is no longer silently swallowed — attempt `read_dir`; on `Err(e)` where `data_dir.join("memory").exists()`, log `memory_index_error` with the error before returning `Ok(())` (first boot with no directory stays silent).
  - `memory_ops`: replace the inline summary/body size checks with the shared `tools::check_text` for the body (summary keeps its own 200-byte message, unchanged).

- [ ] **Step 1: Write the failing tests**

Add to `memory.rs` tests (existing tests updated mechanically to pass `None` as the new `emb` argument):

```rust
    #[test]
    fn vectors_written_on_add_and_pruned_on_supersede() {
        let (conn, tmp) = env();
        let e = crate::providers::mock::MockEmbeddings;
        let id = add(&conn, tmp.path(), "aki", "semantic", "abc", "abc", Some(&e)).unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_vectors WHERE user='aki' AND id=?1", [&id], |r| r.get(0),
        ).unwrap();
        assert_eq!(n, 1);
        let new_id = supersede(&conn, tmp.path(), "aki", &id, "abc", "abc", Some(&e)).unwrap().unwrap();
        let old_n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_vectors WHERE id=?1", [&id], |r| r.get(0),
        ).unwrap();
        assert_eq!(old_n, 0, "superseded vector must be pruned");
        let new_n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_vectors WHERE id=?1", [&new_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(new_n, 1);
    }

    #[test]
    fn hybrid_query_ranks_vector_similar_fact_first() {
        let (conn, tmp) = env();
        let e = crate::providers::mock::MockEmbeddings;
        // lexically, neither fact contains the query token "aaab"; only vectors can rank them
        let close = add(&conn, tmp.path(), "aki", "semantic", "zz", "aaaa", Some(&e)).unwrap();
        let _far = add(&conn, tmp.path(), "aki", "semantic", "zz", "hhhh", Some(&e)).unwrap();
        let hits = query(&conn, "aki", "aaab", 2, Some(&e)).unwrap();
        assert!(!hits.is_empty(), "vector arm must contribute hits with no lexical match");
        assert_eq!(hits[0].id, close);
    }

    #[test]
    fn query_without_provider_stays_lexical() {
        let (conn, tmp) = env();
        let id = add(&conn, tmp.path(), "aki", "semantic", "dentist", "molar", None).unwrap();
        let hits = query(&conn, "aki", "molar", 10, None).unwrap();
        assert_eq!(hits[0].id, id);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM memory_vectors", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn atomic_write_leaves_no_tmp_files() {
        let (conn, tmp) = env();
        add(&conn, tmp.path(), "aki", "semantic", "s", "b", None).unwrap();
        let dir = tmp.path().join("memory/aki/semantic");
        let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap()
            .flatten()
            .filter(|f| f.path().extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib memory` — expected: compile FAIL (arity).

- [ ] **Step 3: Implement**

In `memory.rs` (key pieces; the rest is mechanical threading of `emb`):

```rust
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() { return 0.0; }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na * nb) }
}

/// Embed failure degrades to no vector — the write itself already succeeded.
fn store_vector(conn: &Connection, user: &str, f: &MemoryFile, emb: Option<&dyn crate::providers::EmbeddingsProvider>) {
    let Some(emb) = emb else { return };
    match emb.embed(&[&format!("{}\n{}", f.summary, f.body)]) {
        Ok(vs) if !vs.is_empty() => {
            let _ = conn.execute(
                "INSERT OR REPLACE INTO memory_vectors (user, id, vector) VALUES (?1, ?2, ?3)",
                (user, &f.id, vec_to_blob(&vs[0])),
            );
        }
        Ok(_) => {}
        Err(e) => {
            let _ = crate::log::record(conn, None, "memory_embed_error", &format!("{}: {e}", f.id));
        }
    }
}
```

`add`/`update` call `store_vector` after `index_insert`; `supersede` runs `conn.execute("DELETE FROM memory_vectors WHERE user = ?1 AND id = ?2", (user, &old.id))?` then `store_vector` for the new file. All three swap `std::fs::write` for `write_atomic`.

Hybrid `query`:

```rust
pub fn query(conn: &Connection, user: &str, q: &str, limit: i64, emb: Option<&dyn crate::providers::EmbeddingsProvider>) -> Result<Vec<QueryHit>> {
    let lexical = lexical_query(conn, user, q, 32)?; // existing body, renamed, LIMIT 32
    let vector = vector_query(conn, user, q, 32, emb)?; // Vec<String> of ids, best first; empty when degraded
    if vector.is_empty() {
        return Ok(lexical.into_iter().take(limit as usize).collect());
    }
    let mut scores: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for (rank, hit) in lexical.iter().enumerate() {
        *scores.entry(hit.id.clone()).or_default() += 1.0 / (60.0 + rank as f64);
    }
    for (rank, id) in vector.iter().enumerate() {
        *scores.entry(id.clone()).or_default() += 1.0 / (60.0 + rank as f64);
    }
    let mut ids: Vec<(String, f64)> = scores.into_iter().collect();
    ids.sort_by(|a, b| b.1.total_cmp(&a.1));
    ids.truncate(limit as usize);
    ids.into_iter().map(|(id, _)| hit_for(conn, user, &id, &lexical)).collect()
}
```

where `vector_query` embeds `q` (on `Err`: log `memory_embed_error`, return empty), loads `SELECT v.id, v.vector FROM memory_vectors v JOIN memory_index i ON i.user = v.user AND i.id = v.id WHERE v.user = ?1 AND i.archived = 0`, ranks by `cosine` descending, takes 32; and `hit_for` returns the `QueryHit` from `lexical` when present, else one row `SELECT category, summary FROM memory_index WHERE user = ?1 AND id = ?2`.

`reindex_user` appends the prune DELETE; `reindex_all` logs an unreadable existing root. `ToolCtx` gains the field; `memory_ops::query`/`write` pass `ctx.embeddings`; `memory_ops` body check switches to `super::check_text("body", &args.body)?`. Update every ToolCtx construction site listed in Interfaces with `embeddings: None`.

- [ ] **Step 4: Run** `cargo test` — expected: PASS (fuzz + invariant suites included).

- [ ] **Step 5: Commit** — `git commit -am "feat: hybrid memory retrieval with embedded vectors"`

---

### Task 5: Prompt files, nightly_time config, context fixes

**Files:**
- Create: `server/src/prompts.rs`, `config/defaults/prompts/persona.md`, `config/defaults/prompts/planning.md`
- Modify: `server/src/lib.rs` (add `pub mod prompts;`), `server/src/config.rs` (UserConfig.nightly_time), `server/src/context.rs` (tz label + edit_append hardening)
- Test: inline in `prompts.rs`, `config.rs`, `context.rs`

**Interfaces:**
- Produces:
  - `prompts::load(config_dir: &Path, user: &str, name: &str) -> anyhow::Result<String>` — reads `config_dir/users/<user>/prompts/<name>.md`, falling back to `config_dir/defaults/prompts/<name>.md`; error names the prompt when neither exists.
  - `config::UserConfig` gains `#[serde(default = "default_nightly_time")] pub nightly_time: String` (default `"03:00"`); `UserConfig::load` validates it with `crate::templates::valid_time` and errors on a malformed value.
  - `context::edit_append` propagates any read error other than NotFound as `EditError::Io`; both context writes go through the same write-temp+rename pattern as memory (`fn write_atomic` — duplicate the 4-line helper locally; the two modules stay dependency-free of each other).
  - `context::assemble` labels the time `UTC (configured timezone invalid)` when the tz fallback fires.
  - Checked-in default prompts (real content, guilt-free rules per spec): `persona.md` — role, tone, one-tap/one-sentence responses, never scold, memory-pass instruction (search first, then NOOP/ADD/UPDATE/SUPERSEDE via memory tools); `planning.md` — nightly instructions: review today's plan and open tasks, adjust events with schedule tools where warranted, then reply with a short morning debrief of yesterday and the day ahead.

- [ ] **Step 1: Write the failing tests**

`prompts.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn user_prompt_overrides_default() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/prompts/persona.md", "default persona");
        write(tmp.path(), "users/aki/prompts/persona.md", "aki persona");
        assert_eq!(load(tmp.path(), "aki", "persona").unwrap(), "aki persona");
        assert_eq!(load(tmp.path(), "bob", "persona").unwrap(), "default persona");
        let err = load(tmp.path(), "aki", "missing").unwrap_err().to_string();
        assert!(err.contains("missing"), "{err}");
    }
}
```

`config.rs` tests:

```rust
    #[test]
    fn nightly_time_defaults_and_validates() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/user.toml",
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");
        assert_eq!(UserConfig::load(tmp.path(), "a").unwrap().nightly_time, "03:00");
        write(tmp.path(), "users/aki/user.toml", "nightly_time = \"4:00\"\n");
        assert!(UserConfig::load(tmp.path(), "aki").is_err());
    }
```

`context.rs` tests:

```rust
    #[test]
    fn invalid_tz_is_labeled_as_utc_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let write = |rel: &str, c: &str| {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        };
        write("defaults/user.toml",
            "display_name = \"X\"\ntimezone = \"Not/AZone\"\ntemplate = \"default\"\n");
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        let now: jiff::Timestamp = "2026-08-31T12:00:00Z".parse().unwrap();
        let out = assemble(&conn, tmp.path(), uid, "aki", now).unwrap();
        assert!(out.contains("UTC (configured timezone invalid)"), "{out}");
        assert!(!out.contains("Not/AZone"), "{out}");
    }
```

(Also verify by reading `edit_append` that the NotFound arm is explicit — its new behavior is exercised implicitly by all existing append tests.)

- [ ] **Step 2: Run to verify failure** — `cargo test --lib prompts config context` — expected: FAIL.

- [ ] **Step 3: Implement**

`server/src/prompts.rs`:

```rust
use anyhow::{Context, Result};
use std::path::Path;

/// Per-user prompt override with shipped-default fallback, mirroring
/// Template::load's resolution order.
pub fn load(config_dir: &Path, user: &str, name: &str) -> Result<String> {
    let user_path = config_dir.join("users").join(user).join("prompts").join(format!("{name}.md"));
    let default_path = config_dir.join("defaults/prompts").join(format!("{name}.md"));
    let path = if user_path.exists() { user_path } else { default_path };
    std::fs::read_to_string(&path).with_context(|| format!("reading prompt {name} ({})", path.display()))
}
```

`config.rs`: add the field + `fn default_nightly_time() -> String { "03:00".into() }`; in `UserConfig::load`, after `try_into`, `anyhow::ensure!(crate::templates::valid_time(&cfg.nightly_time), "invalid nightly_time {:?}", cfg.nightly_time)`.

`context.rs`: in `assemble`, resolve tz as:

```rust
    let (tz, tz_label) = match jiff::tz::TimeZone::get(&ucfg.timezone) {
        Ok(tz) => (tz, ucfg.timezone.clone()),
        Err(_) => (jiff::tz::TimeZone::UTC, "UTC (configured timezone invalid)".into()),
    };
```

and print `tz_label`. In `edit_append`:

```rust
    let mut cur = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
```

Both `edit_replace` and `edit_append` write via a local `write_atomic` (same 4-line temp+rename helper as memory's, with a `.md.tmp` extension).

`config/defaults/prompts/persona.md`:

```markdown
You are Note, a personal accountability companion for someone with ADHD.

Rules that never bend:
- Guilt-free, always. Never scold, never mention streaks or how many times
  something slipped. A missed check-in is just "want to pick a new time?"
- Keep every reply short enough to act on in one tap or one sentence.
- You externalize their working memory: capture tasks the moment they come up
  (task_create), update states when they tell you, and keep the standing
  context current.
- Memory pass: before answering anything that might touch the past, search
  (memory_query, then memory_read). After a conversation that taught you
  something durable, write it back — add a new fact, update one, or
  supersede one that is now wrong. Do nothing when nothing changed.
```

`config/defaults/prompts/planning.md`:

```markdown
Nightly planning session. The day plan for today has already been generated
from the template.

1. Look at today's plan and the open tasks in the standing context.
2. Adjust where it clearly helps: slide or drop flexible events, insert an
   event for anything urgent (schedule_insert), keeping the day realistic —
   an emptier plan that happens beats a full one that doesn't.
3. Then reply with the morning debrief: two or three warm sentences —
   yesterday in one line (no guilt), today's shape in one or two.

Your final reply text becomes the debrief delivered this morning.
```

- [ ] **Step 4: Run** `cargo test` — expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat: prompt files, nightly_time config, context hardening"`

---

### Task 6: Agent runtime — the tool loop

**Files:**
- Create: `server/src/agent.rs`
- Modify: `server/src/lib.rs` (add `pub mod agent;`), `server/src/tools/mod.rs` (registry-subset test only)
- Test: inline in `agent.rs` + the subset test

**Interfaces:**
- Consumes: `providers::{LLMProvider, EmbeddingsProvider, Message, ChatRequest, ChatResponse}`, `tools::{dispatch, schemas, SessionKind, ToolCtx}`, `context::assemble`, `prompts::load`, `log::record`.
- Produces:
  - `agent::MAX_TURNS: usize = 16`.
  - `agent::SessionDeps<'a> { db: &'a std::sync::Mutex<rusqlite::Connection>, config_dir: &'a Path, data_dir: &'a Path, llm: &'a dyn LLMProvider, embeddings: Option<&'a dyn EmbeddingsProvider> }`.
  - `agent::SessionOutcome { reply: String, turns: usize, tool_calls: usize }`.
  - `agent::run_session(deps: &SessionDeps, user_id: i64, username: &str, kind: SessionKind, opening: &str) -> anyhow::Result<SessionOutcome>`.
- Behavior (binding):
  - System prompt = `prompts::load(persona)` + (for `SessionKind::Nightly` only) `"\n\n"` + `prompts::load(planning)` + `"\n\n"` + `context::assemble(…)` (assemble under a short lock; released before any chat call).
  - Loop: `chat` with `tools::schemas(kind)`; response with no tool calls ends the session with its text. Otherwise push `Assistant`, then for each call in order: one lock, build `ToolCtx { config_dir, data_dir, user_id, username, embeddings }`, `dispatch`; result content = `serde_json::to_string` of the `Ok` value (`is_error: false`) or of the `ToolError` (`is_error: true`); push `ToolResult`s; next turn.
  - Turn cap: after `MAX_TURNS` chat calls, log `agent_max_turns` and return the last response's text (possibly empty).
  - On success, `log::record(conn, Some(user_id), "agent_session", "kind=<kind:?> turns=<n> tools=<m>")` under a short lock.
  - The runtime never touches DB/filesystem except through the named crate functions (Global Constraints).

- [ ] **Step 1: Write the failing tests**

`agent.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{mock::MockLLM, ChatResponse, ToolCall};
    use crate::tools::SessionKind;
    use std::sync::Mutex;

    fn env() -> (Mutex<rusqlite::Connection>, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", [],
        ).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let write = |rel: &str, c: &str| {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        };
        write("defaults/user.toml",
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");
        write("defaults/prompts/persona.md", "you are note, be kind");
        write("defaults/prompts/planning.md", "plan the day");
        (Mutex::new(conn), tmp)
    }

    fn deps<'a>(db: &'a Mutex<rusqlite::Connection>, tmp: &'a tempfile::TempDir, llm: &'a MockLLM) -> SessionDeps<'a> {
        SessionDeps { db, config_dir: tmp.path(), data_dir: tmp.path(), llm, embeddings: None }
    }

    #[test]
    fn tool_call_round_trip_creates_task_and_returns_reply() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![
            ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall { id: "c1".into(), name: "task_create".into(), args: r#"{"title":"buy milk"}"#.into() }],
            },
            ChatResponse { text: "added buy milk!".into(), tool_calls: vec![] },
        ]);
        let out = run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, "add buy milk").unwrap();
        assert_eq!(out.reply, "added buy milk!");
        assert_eq!(out.turns, 2);
        assert_eq!(out.tool_calls, 1);
        let title: String = db.lock().unwrap()
            .query_row("SELECT title FROM tasks WHERE user_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(title, "buy milk");
        // the model saw persona + context and the talk tool surface
        let seen = llm.seen();
        assert!(seen[0].system.contains("you are note"));
        assert!(seen[0].system.contains("# Today's plan"));
        assert!(seen[0].tool_names.contains(&"context_edit".to_string()));
        assert!(!seen[0].tool_names.contains(&"schedule_insert".to_string()));
        // second turn carried the tool result back
        assert_eq!(seen[1].n_messages, 3);
    }

    #[test]
    fn rejected_tool_call_reaches_model_as_error_and_session_continues() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![
            ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall { id: "c1".into(), name: "schedule_insert".into(), args: "{}".into() }],
            },
            ChatResponse { text: "sorry, couldn't".into(), tool_calls: vec![] },
        ]);
        // Talk surface: schedule_insert is forbidden — dispatch returns a typed error
        let out = run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, "hi").unwrap();
        assert_eq!(out.reply, "sorry, couldn't");
    }

    #[test]
    fn turn_cap_ends_the_session_and_logs() {
        let (db, tmp) = env();
        // every response asks for another tool call — the loop must stop at MAX_TURNS
        let resp = ChatResponse {
            text: "looping".into(),
            tool_calls: vec![ToolCall { id: "c".into(), name: "memory_query".into(), args: r#"{"query":"x"}"#.into() }],
        };
        let llm = MockLLM::scripted(vec![resp; MAX_TURNS + 4]);
        let out = run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, "hi").unwrap();
        assert_eq!(out.turns, MAX_TURNS);
        let n: i64 = db.lock().unwrap().query_row(
            "SELECT COUNT(*) FROM event_log WHERE kind = 'agent_max_turns'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn nightly_session_includes_planning_prompt() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![ChatResponse { text: "debrief".into(), tool_calls: vec![] }]);
        run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Nightly, "night").unwrap();
        assert!(llm.seen()[0].system.contains("plan the day"));
    }

    #[test]
    fn llm_failure_propagates() {
        struct Failing;
        impl crate::providers::LLMProvider for Failing {
            fn chat(&self, _: &crate::providers::ChatRequest) -> anyhow::Result<crate::providers::ChatResponse> {
                anyhow::bail!("provider down")
            }
        }
        let (db, tmp) = env();
        let llm = Failing;
        let d = SessionDeps { db: &db, config_dir: tmp.path(), data_dir: tmp.path(), llm: &llm, embeddings: None };
        assert!(run_session(&d, 1, "aki", SessionKind::Talk, "hi").is_err());
    }
}
```

Add to `tools/mod.rs` tests (phase-2 debt):

```rust
    #[test]
    fn session_surfaces_are_nested_subsets() {
        let is_subset = |a: &[&str], b: &[&str]| a.iter().all(|t| b.contains(t));
        assert!(is_subset(registry(SessionKind::Checkin), registry(SessionKind::Talk)));
        assert!(is_subset(registry(SessionKind::Talk), registry(SessionKind::Nightly)));
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib agent` — expected: compile FAIL.

- [ ] **Step 3: Implement `server/src/agent.rs`**

```rust
use crate::providers::{ChatRequest, EmbeddingsProvider, LLMProvider, Message};
use crate::tools::{self, SessionKind, ToolCtx};
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;

pub const MAX_TURNS: usize = 16;

pub struct SessionDeps<'a> {
    pub db: &'a Mutex<Connection>,
    pub config_dir: &'a Path,
    pub data_dir: &'a Path,
    pub llm: &'a dyn LLMProvider,
    pub embeddings: Option<&'a dyn EmbeddingsProvider>,
}

#[derive(Debug)]
pub struct SessionOutcome {
    pub reply: String,
    pub turns: usize,
    pub tool_calls: usize,
}

/// Runs one agent session: chat → dispatch tool calls → feed results back,
/// until the model answers in text or MAX_TURNS is hit. The DB lock is held
/// only for assembly, individual dispatches, and log writes — never across a
/// provider call.
pub fn run_session(deps: &SessionDeps, user_id: i64, username: &str, kind: SessionKind, opening: &str) -> Result<SessionOutcome> {
    let mut system = crate::prompts::load(deps.config_dir, username, "persona")?;
    if kind == SessionKind::Nightly {
        system.push_str("\n\n");
        system.push_str(&crate::prompts::load(deps.config_dir, username, "planning")?);
    }
    {
        let conn = deps.db.lock().unwrap();
        system.push_str("\n\n");
        system.push_str(&crate::context::assemble(&conn, deps.config_dir, user_id, username, jiff::Timestamp::now())?);
    }
    let schemas = tools::schemas(kind);
    let mut messages = vec![Message::User(opening.to_string())];
    let mut turns = 0;
    let mut total_calls = 0;
    let mut last_text = String::new();

    while turns < MAX_TURNS {
        let resp = deps.llm.chat(&ChatRequest { system: &system, messages: &messages, tools: &schemas })?;
        turns += 1;
        last_text = resp.text.clone();
        if resp.tool_calls.is_empty() {
            finish(deps, user_id, kind, turns, total_calls, "agent_session")?;
            return Ok(SessionOutcome { reply: resp.text, turns, tool_calls: total_calls });
        }
        let calls = resp.tool_calls.clone();
        messages.push(Message::Assistant { text: resp.text, tool_calls: resp.tool_calls });
        for call in calls {
            total_calls += 1;
            let conn = deps.db.lock().unwrap();
            let ctx = ToolCtx {
                config_dir: deps.config_dir,
                data_dir: deps.data_dir,
                user_id,
                username,
                embeddings: deps.embeddings,
            };
            let (content, is_error) = match tools::dispatch(&conn, &ctx, kind, &call.name, &call.args) {
                Ok(v) => (v.to_string(), false),
                Err(e) => (serde_json::to_string(&e).unwrap_or_else(|_| "{\"kind\":\"internal\"}".into()), true),
            };
            drop(conn);
            messages.push(Message::ToolResult { call_id: call.id, content, is_error });
        }
    }
    finish(deps, user_id, kind, turns, total_calls, "agent_max_turns")?;
    Ok(SessionOutcome { reply: last_text, turns, tool_calls: total_calls })
}

fn finish(deps: &SessionDeps, user_id: i64, kind: SessionKind, turns: usize, calls: usize, log_kind: &str) -> Result<()> {
    let conn = deps.db.lock().unwrap();
    crate::log::record(&conn, Some(user_id), log_kind, &format!("kind={kind:?} turns={turns} tools={calls}"))
}
```

(`SessionKind` needs `Debug` — it already derives it.)

- [ ] **Step 4: Run** `cargo test` — expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat: agent runtime tool loop over session surfaces"`

---

### Task 7: AppState providers + /api/talk

**Files:**
- Modify: `server/src/lib.rs` (AppState), `server/src/api.rs` (talk route), `server/src/main.rs` (build providers + data_dir), `server/tests/common/mod.rs`, `server/tests/health.rs`, `server/tests/auth.rs`, `server/tests/tasks_api.rs`, `server/tests/admin_api.rs`, `server/tests/plan_api.rs` (constructor arity)
- Test: `server/tests/talk_api.rs`

**Interfaces:**
- Produces:
  - `AppState { db: Arc<Mutex<Connection>>, config_dir: PathBuf, data_dir: PathBuf, llm: Arc<dyn LLMProvider>, embeddings: Option<Arc<dyn EmbeddingsProvider>> }`.
  - `AppState::new(conn: Connection, config_dir: PathBuf, data_dir: PathBuf) -> Self` — defaults `llm` to `Arc::new(MockLLM::empty())`, `embeddings` to `None`.
  - `AppState::with_providers(self, llm: Arc<dyn LLMProvider>, embeddings: Option<Arc<dyn EmbeddingsProvider>>) -> Self`.
  - Route `POST /api/talk {message}` behind `CurrentUser` → `{"reply": …}`; empty/whitespace or > 16KiB message → 400; session failure → 500. The session runs inside `tokio::task::spawn_blocking` (providers are blocking).
  - `main.rs`: `let (llm, embeddings) = providers::build(&cfg.providers)?;` and `AppState::new(conn, config_dir, cfg.data_dir.clone()).with_providers(llm, embeddings)`.
- Every existing `AppState::new(conn, path)` call site gains a `data_dir` argument: in tests, pass the same tempdir path used for `config_dir` (they're separate roots in production only). `tests/common/mod.rs` keeps its single constructor so most test files change by zero lines.

- [ ] **Step 1: Write the failing test**

`server/tests/talk_api.rs`:

```rust
mod common;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::providers::{mock::MockLLM, ChatResponse, ToolCall};
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn talk_runs_a_session_and_returns_the_reply() {
    let llm = Arc::new(MockLLM::scripted(vec![
        ChatResponse {
            text: String::new(),
            tool_calls: vec![ToolCall { id: "c1".into(), name: "task_create".into(), args: r#"{"title":"call mom"}"#.into() }],
        },
        ChatResponse { text: "done — added call mom".into(), tool_calls: vec![] },
    ]));
    let (app, cookie) = common::app_with_logged_in_user_and_llm(llm).await;

    let res = app.clone().oneshot(
        Request::post("/api/talk")
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"message":"remind me to call mom"}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["reply"], "done — added call mom");

    let res = app.clone().oneshot(
        Request::get("/api/tasks").header(header::COOKIE, &cookie).body(Body::empty()).unwrap(),
    ).await.unwrap();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v[0]["title"], "call mom");
}

#[tokio::test]
async fn empty_message_is_400_and_no_cookie_is_401() {
    let (app, cookie) = common::app_with_logged_in_user().await;
    let res = app.clone().oneshot(
        Request::post("/api/talk")
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"message":"   "}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = app.oneshot(
        Request::post("/api/talk")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"message":"hi"}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
```

`tests/common/mod.rs` additions: `app_with_logged_in_user_and_llm(llm: Arc<MockLLM>)` — same body as the existing helper, but the state is `AppState::new(conn, dir.clone(), dir).with_providers(llm, None)`, and the helper also writes `defaults/prompts/persona.md` ("you are note") so sessions load. Update the existing helper to write the persona file too and to pass the new `data_dir` argument.

- [ ] **Step 2: Run to verify failure** — `cargo test --test talk_api` — expected: compile FAIL.

- [ ] **Step 3: Implement**

`lib.rs`:

```rust
use crate::providers::{EmbeddingsProvider, LLMProvider};

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub llm: Arc<dyn LLMProvider>,
    pub embeddings: Option<Arc<dyn EmbeddingsProvider>>,
}

impl AppState {
    pub fn new(conn: Connection, config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            db: Arc::new(Mutex::new(conn)),
            config_dir,
            data_dir,
            llm: Arc::new(crate::providers::mock::MockLLM::empty()),
            embeddings: None,
        }
    }

    pub fn with_providers(mut self, llm: Arc<dyn LLMProvider>, embeddings: Option<Arc<dyn EmbeddingsProvider>>) -> Self {
        self.llm = llm;
        self.embeddings = embeddings;
        self
    }
}
```

`api.rs` handler + route `.route("/api/talk", post(talk))`:

```rust
#[derive(Deserialize)]
struct TalkReq { message: String }

async fn talk(user: CurrentUser, State(state): State<AppState>, Json(req): Json<TalkReq>) -> impl IntoResponse {
    let message = req.message.trim().to_string();
    if message.is_empty() || message.len() > 16 * 1024 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let result = tokio::task::spawn_blocking(move || {
        let deps = crate::agent::SessionDeps {
            db: &state.db,
            config_dir: &state.config_dir,
            data_dir: &state.data_dir,
            llm: state.llm.as_ref(),
            embeddings: state.embeddings.as_deref(),
        };
        crate::agent::run_session(&deps, user.id, &user.username, crate::tools::SessionKind::Talk, &message)
    })
    .await;
    match result {
        Ok(Ok(out)) => Json(serde_json::json!({ "reply": out.reply })).into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
```

`main.rs`: build providers before state, pass `cfg.data_dir.clone()` as data_dir. Update every listed test constructor call site.

- [ ] **Step 4: Run** `cargo test` — expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat: talk endpoint running agent sessions with configured providers"`

---

### Task 8: Nightly job — plan, session, debrief, fallback

**Files:**
- Create: `server/src/nightly.rs`
- Modify: `server/src/lib.rs` (add `pub mod nightly;`), `server/src/main.rs` (spawn)
- Test: inline in `nightly.rs`

**Interfaces:**
- Consumes: `agent::run_session`, `plan::generate`, `templates::Template`, `config::UserConfig`, `debriefs` table, `providers` via `SessionDeps`.
- Produces:
  - `nightly::run_for_user(deps: &agent::SessionDeps, user_id: i64, username: &str, now: jiff::Timestamp) -> anyhow::Result<()>` — behavior below.
  - `nightly::due(conn: &Connection, config_dir: &Path, now: jiff::Timestamp) -> anyhow::Result<Vec<(i64, String)>>` — every user whose local time (their tz; invalid → UTC) is at or past their `nightly_time` and who has **no debrief row for their local date**.
  - `nightly::spawn(state: AppState)` — tokio task, 60s interval: compute `due`, run `run_for_user` for each inside `spawn_blocking`; any `Err` logged as `nightly_error` with the username.
- `run_for_user` behavior (binding):
  1. Resolve the user's tz and local `date` from `now`. Idempotency: if a debrief row exists for `(user_id, date)`, return Ok immediately.
  2. **Fallback guarantee first, pure code:** load the user's template and `plan::generate(conn, user_id, &tmpl, date)` (no-op if the plan exists). A template/DB failure here is an `Err` (retried next sweep; noisy-by-design like the runner).
  3. Run a `SessionKind::Nightly` session with opening `"Nightly run for {date}."`. On `Ok(outcome)`: debrief content = `outcome.reply` (empty reply → the fallback text). On `Err`: log `nightly_fallback` with the error; debrief content = `"(Plan generated from your template. The assistant was unavailable overnight.)"`.
  4. Insert the debrief row (`INSERT OR IGNORE` on the unique key) with `created_at = now`.
  - Locks: steps 2 and 4 each take the lock briefly; the session in step 3 manages its own locking. Never hold across the session.

- [ ] **Step 1: Write the failing tests**

`nightly.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{mock::MockLLM, ChatResponse};
    use std::sync::Mutex;

    fn env(tz: &str, nightly_time: &str) -> (Mutex<rusqlite::Connection>, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", [],
        ).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let write = |rel: &str, c: &str| {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        };
        write("defaults/user.toml", &format!(
            "display_name = \"X\"\ntimezone = \"{tz}\"\ntemplate = \"default\"\nnightly_time = \"{nightly_time}\"\n"));
        write("defaults/templates/default.toml",
            "[[events]]\nkind='checkin'\ntime='09:00'\ndays=['mon','tue','wed','thu','fri','sat','sun']\nflexibility='slide'\n");
        write("defaults/prompts/persona.md", "persona");
        write("defaults/prompts/planning.md", "planning");
        (Mutex::new(conn), tmp)
    }

    fn deps<'a>(db: &'a Mutex<rusqlite::Connection>, tmp: &'a tempfile::TempDir, llm: &'a dyn crate::providers::LLMProvider) -> crate::agent::SessionDeps<'a> {
        crate::agent::SessionDeps { db, config_dir: tmp.path(), data_dir: tmp.path(), llm, embeddings: None }
    }

    #[test]
    fn nightly_generates_plan_and_stores_debrief_idempotently() {
        let (db, tmp) = env("UTC", "03:00");
        let llm = MockLLM::scripted(vec![
            ChatResponse { text: "good morning! light day ahead".into(), tool_calls: vec![] },
            ChatResponse { text: "should never be consumed".into(), tool_calls: vec![] },
        ]);
        let now: jiff::Timestamp = "2026-08-31T04:00:00Z".parse().unwrap();
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", now).unwrap();
        {
            let conn = db.lock().unwrap();
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM events e JOIN plans p ON p.id = e.plan_id WHERE p.date='2026-08-31'",
                [], |r| r.get(0)).unwrap();
            assert_eq!(n, 1);
            let content: String = conn.query_row(
                "SELECT content FROM debriefs WHERE user_id=1 AND date='2026-08-31'", [], |r| r.get(0)).unwrap();
            assert!(content.contains("light day"));
        }
        // second run same day: no second session, no duplicate rows
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", now).unwrap();
        assert_eq!(llm.seen().len(), 1);
    }

    #[test]
    fn llm_failure_still_leaves_plan_and_fallback_debrief() {
        struct Failing;
        impl crate::providers::LLMProvider for Failing {
            fn chat(&self, _: &crate::providers::ChatRequest) -> anyhow::Result<crate::providers::ChatResponse> {
                anyhow::bail!("down")
            }
        }
        let (db, tmp) = env("UTC", "03:00");
        let now: jiff::Timestamp = "2026-08-31T04:00:00Z".parse().unwrap();
        run_for_user(&deps(&db, &tmp, &Failing), 1, "aki", now).unwrap();
        let conn = db.lock().unwrap();
        let plans: i64 = conn.query_row("SELECT COUNT(*) FROM plans", [], |r| r.get(0)).unwrap();
        assert_eq!(plans, 1);
        let content: String = conn.query_row(
            "SELECT content FROM debriefs WHERE user_id=1", [], |r| r.get(0)).unwrap();
        assert!(content.contains("template"));
        let logged: i64 = conn.query_row(
            "SELECT COUNT(*) FROM event_log WHERE kind='nightly_fallback'", [], |r| r.get(0)).unwrap();
        assert_eq!(logged, 1);
    }

    #[test]
    fn due_respects_local_time_and_existing_debriefs() {
        let (db, tmp) = env("Asia/Tokyo", "03:00");
        // 2026-08-30T17:00Z = 2026-08-31 02:00 JST → not yet due
        let early: jiff::Timestamp = "2026-08-30T17:00:00Z".parse().unwrap();
        assert!(due(&db.lock().unwrap(), tmp.path(), early).unwrap().is_empty());
        // 2026-08-30T19:00Z = 04:00 JST on the 31st → due
        let later: jiff::Timestamp = "2026-08-30T19:00:00Z".parse().unwrap();
        let d = due(&db.lock().unwrap(), tmp.path(), later).unwrap();
        assert_eq!(d, vec![(1, "aki".to_string())]);
        db.lock().unwrap().execute(
            "INSERT INTO debriefs (user_id, date, content, created_at) VALUES (1, '2026-08-31', 'x', 't')", [],
        ).unwrap();
        assert!(due(&db.lock().unwrap(), tmp.path(), later).unwrap().is_empty());
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib nightly` — expected: compile FAIL.

- [ ] **Step 3: Implement `server/src/nightly.rs`**

```rust
use crate::agent::SessionDeps;
use crate::config::UserConfig;
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

const FALLBACK_DEBRIEF: &str =
    "(Plan generated from your template. The assistant was unavailable overnight.)";

fn local_date(config_dir: &Path, username: &str, now: jiff::Timestamp) -> Result<jiff::civil::Date> {
    let ucfg = UserConfig::load(config_dir, username)?;
    let tz = jiff::tz::TimeZone::get(&ucfg.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
    Ok(now.to_zoned(tz).date())
}

/// One user's nightly run: template plan first (the always-a-morning-plan
/// guarantee is pure code), then the agent session; the debrief row is the
/// idempotency marker, so an LLM failure writes the fallback debrief rather
/// than retrying forever.
pub fn run_for_user(deps: &SessionDeps, user_id: i64, username: &str, now: jiff::Timestamp) -> Result<()> {
    let date = local_date(deps.config_dir, username, now)?;
    {
        let conn = deps.db.lock().unwrap();
        let done: i64 = conn.query_row(
            "SELECT COUNT(*) FROM debriefs WHERE user_id = ?1 AND date = ?2",
            (user_id, date.to_string()), |r| r.get(0),
        )?;
        if done > 0 {
            return Ok(());
        }
        let ucfg = UserConfig::load(deps.config_dir, username)?;
        let tmpl = crate::templates::Template::load(deps.config_dir, username, &ucfg.template)?;
        crate::plan::generate(&conn, user_id, &tmpl, date)?;
    }
    let content = match crate::agent::run_session(
        deps, user_id, username, crate::tools::SessionKind::Nightly,
        &format!("Nightly run for {date}."),
    ) {
        Ok(out) if !out.reply.trim().is_empty() => out.reply,
        Ok(_) => FALLBACK_DEBRIEF.to_string(),
        Err(e) => {
            let conn = deps.db.lock().unwrap();
            let _ = crate::log::record(&conn, Some(user_id), "nightly_fallback", &e.to_string());
            FALLBACK_DEBRIEF.to_string()
        }
    };
    let conn = deps.db.lock().unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO debriefs (user_id, date, content, created_at) VALUES (?1, ?2, ?3, ?4)",
        (user_id, date.to_string(), content, now.to_string()),
    )?;
    Ok(())
}

pub fn due(conn: &Connection, config_dir: &Path, now: jiff::Timestamp) -> Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare("SELECT id, username FROM users")?;
    let users: Vec<(i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for (id, username) in users {
        let Ok(ucfg) = UserConfig::load(config_dir, &username) else { continue };
        let tz = jiff::tz::TimeZone::get(&ucfg.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
        let local = now.to_zoned(tz);
        let (h, m) = ucfg.nightly_time.split_once(':').unwrap_or(("03", "00"));
        let due_time = jiff::civil::time(h.parse().unwrap_or(3), m.parse().unwrap_or(0), 0, 0);
        if local.time() < due_time {
            continue;
        }
        let has: i64 = conn.query_row(
            "SELECT COUNT(*) FROM debriefs WHERE user_id = ?1 AND date = ?2",
            (id, local.date().to_string()), |r| r.get(0),
        )?;
        if has == 0 {
            out.push((id, username));
        }
    }
    Ok(out)
}

pub fn spawn(state: crate::AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tick.tick().await;
            let now = jiff::Timestamp::now();
            let users = {
                let conn = state.db.lock().unwrap();
                due(&conn, &state.config_dir, now)
            };
            let users = match users {
                Ok(u) => u,
                Err(e) => {
                    let conn = state.db.lock().unwrap();
                    let _ = crate::log::record(&conn, None, "nightly_error", &e.to_string());
                    continue;
                }
            };
            for (user_id, username) in users {
                let st = state.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let deps = SessionDeps {
                        db: &st.db,
                        config_dir: &st.config_dir,
                        data_dir: &st.data_dir,
                        llm: st.llm.as_ref(),
                        embeddings: st.embeddings.as_deref(),
                    };
                    let r = run_for_user(&deps, user_id, &username, now);
                    (r, username)
                })
                .await;
                if let Ok((Err(e), username)) = result {
                    let conn = state.db.lock().unwrap();
                    let _ = crate::log::record(&conn, None, "nightly_error", &format!("{username}: {e}"));
                }
            }
        }
    });
}
```

`main.rs`: `nightly::spawn(state.clone());` next to `runner::spawn`.

- [ ] **Step 4: Run** `cargo test` — expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat: nightly job generates plans and stores debriefs with fallback"`

---

### Task 9: Full simulated-day integration test

**Files:**
- Test: `server/tests/full_day.rs`

**Interfaces:**
- Consumes: everything. This is the spec's "one test driving a full simulated day": nightly run → morning plan + debrief, a talk session capturing a task, the runner firing the day's events.

- [ ] **Step 1: Write the test**

`server/tests/full_day.rs`:

```rust
use note_server::agent::SessionDeps;
use note_server::providers::{mock::MockLLM, ChatResponse, ToolCall};
use note_server::tools::SessionKind;
use std::sync::Mutex;

fn write(dir: &std::path::Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

#[test]
fn a_full_simulated_day() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "defaults/user.toml",
        "display_name = \"Aki\"\ntimezone = \"Asia/Tokyo\"\ntemplate = \"default\"\n");
    write(tmp.path(), "defaults/templates/default.toml",
        "[[events]]\nkind='checkin_call'\ntime='09:00'\ndays=['mon','tue','wed','thu','fri','sat','sun']\nflexibility='slide'\nslide_window_min=60\nchannel='voice'\n");
    write(tmp.path(), "defaults/prompts/persona.md", "you are note");
    write(tmp.path(), "defaults/prompts/planning.md", "plan the day, then debrief");
    let conn = note_server::db::open_memory().unwrap();
    conn.execute(
        "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", [],
    ).unwrap();
    let db = Mutex::new(conn);

    // --- 03:30 JST, 2026-08-31 (Monday): nightly run.
    // The agent inserts an afternoon nudge, then debriefs.
    let nightly_llm = MockLLM::scripted(vec![
        ChatResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "n1".into(), name: "schedule_insert".into(),
                args: r#"{"date":"2026-08-31","kind":"nudge","time":"16:00","flexibility":"drop","channel":"push"}"#.into(),
            }],
        },
        ChatResponse { text: "yesterday was quiet. today: checkin at nine, nudge at four.".into(), tool_calls: vec![] },
    ]);
    let now: jiff::Timestamp = "2026-08-30T18:30:00Z".parse().unwrap(); // 03:30 JST on the 31st
    {
        let deps = SessionDeps { db: &db, config_dir: tmp.path(), data_dir: tmp.path(), llm: &nightly_llm, embeddings: None };
        assert_eq!(note_server::nightly::due(&db.lock().unwrap(), tmp.path(), now).unwrap().len(), 1);
        note_server::nightly::run_for_user(&deps, 1, "aki", now).unwrap();
    }
    {
        let conn = db.lock().unwrap();
        let kinds: Vec<String> = conn
            .prepare("SELECT e.kind FROM events e JOIN plans p ON p.id = e.plan_id WHERE p.date='2026-08-31' ORDER BY e.wall_time")
            .unwrap()
            .query_map([], |r| r.get(0)).unwrap()
            .collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(kinds, vec!["checkin_call".to_string(), "nudge".to_string()]);
        let debrief: String = conn.query_row(
            "SELECT content FROM debriefs WHERE user_id=1 AND date='2026-08-31'", [], |r| r.get(0)).unwrap();
        assert!(debrief.contains("checkin at nine"));
    }

    // --- 10:00 JST: the user talks; the agent captures a task and remembers a fact.
    let talk_llm = MockLLM::scripted(vec![
        ChatResponse {
            text: String::new(),
            tool_calls: vec![
                ToolCall { id: "t1".into(), name: "task_create".into(), args: r#"{"title":"submit report"}"#.into() },
                ToolCall { id: "t2".into(), name: "memory_write".into(),
                    args: r#"{"op":"add","category":"episodic","summary":"report due friday","body":"mentioned during monday talk"}"#.into() },
            ],
        },
        ChatResponse { text: "got it — report's on the list.".into(), tool_calls: vec![] },
    ]);
    {
        let deps = SessionDeps { db: &db, config_dir: tmp.path(), data_dir: tmp.path(), llm: &talk_llm, embeddings: None };
        let out = note_server::agent::run_session(&deps, 1, "aki", SessionKind::Talk, "the report is due friday, remind me").unwrap();
        assert!(out.reply.contains("on the list"));
    }
    {
        let conn = db.lock().unwrap();
        let title: String = conn.query_row("SELECT title FROM tasks WHERE user_id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(title, "submit report");
        let hits = note_server::memory::query(&conn, "aki", "report", 5, None).unwrap();
        assert_eq!(hits.len(), 1);
    }

    // --- 16:05 JST: the runner fires everything due (09:00 checkin + 16:00 nudge).
    let fire_at: jiff::Timestamp = "2026-08-31T07:05:00Z".parse().unwrap(); // 16:05 JST
    let fired = {
        let conn = db.lock().unwrap();
        note_server::runner::fire_due(&conn, tmp.path(), fire_at).unwrap()
    };
    assert_eq!(fired.len(), 2);
    {
        let conn = db.lock().unwrap();
        let logged: i64 = conn.query_row(
            "SELECT COUNT(*) FROM event_log WHERE kind='event_fired'", [], |r| r.get(0)).unwrap();
        assert_eq!(logged, 2);
    }
}
```

- [ ] **Step 2: Run** `cargo test --test full_day` — expected: PASS. If it fails, the bug is in the integration seams — debug the seam, not the test (the per-layer suites are green).

- [ ] **Step 3: Run the whole suite** — `cargo test` — expected: PASS.

- [ ] **Step 4: Commit** — `git commit -am "test: full simulated day from nightly run to fired events"`

---

### Task 10: Starter config, README, final verification

**Files:**
- Modify: `README.md`, `config/server.toml`
- (The default prompt files were checked in by Task 5.)

- [ ] **Step 1: Starter config**

Append to `config/server.toml` (commented out — mock is the default and needs no section):

```toml
# Providers. Omit this whole section to run with the built-in mock (no tokens).
#
# [providers.llm]
# kind = "anthropic"            # or "openai" for any OpenAI-compatible endpoint
# model = "claude-sonnet-5"
# api_key_env = "ANTHROPIC_API_KEY"
#
# [providers.embeddings]        # optional; enables hybrid memory search
# kind = "openai"
# base_url = "http://localhost:8080/v1"   # e.g. a local llama.cpp router
# model = "embeddinggemma"
```

- [ ] **Step 2: README section**

Append after the "Memory & agent tools" section, adapted to the README's voice:

```markdown
## Providers & the agent

The server runs with deterministic mock providers by default — no API keys,
fully testable offline. Configure real ones in `config/server.toml`
(`[providers.llm]`, `[providers.embeddings]`): Anthropic or any
OpenAI-compatible endpoint for chat, OpenAI-compatible for embeddings (a
local llama.cpp router works). Keys are read from the env var named in
`api_key_env`, never from config files.

With an embeddings provider configured, memory search becomes hybrid
(lexical + vector) and degrades back to lexical automatically when the
provider is down.

`POST /api/talk {message}` runs a text conversation with the agent. Agent
behavior lives in editable prompt files (`config/defaults/prompts/`,
overridable per user under `config/users/<user>/prompts/`) — changing tone
or policy is a file edit, not a deploy.

Every night at each user's `nightly_time` (default 03:00, their timezone),
the server generates the day's plan from their template, lets the agent
adjust it and write a morning debrief, and stores the debrief. If the model
is unreachable, the plan still exists and a fallback debrief says so — a
plainer day, never a missing one.
```

- [ ] **Step 3: Full verification** — `cargo test` (full PASS) and `cargo build --release` (clean).

- [ ] **Step 4: Commit** — `git commit -am "docs: provider configuration and nightly job in README"`

---

## Follow-on plans (not in this document)

4. Channels: web-push (VAPID), Twilio voice bridge (SpeechProvider joins here), WebSocket delivery; outreach tools (`send_nudge`, `place_call`) join the registries; morning debrief delivery; auth-hardening pass with public ingress.
5. Web PWA.
