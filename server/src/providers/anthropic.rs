use super::{ChatRequest, ChatResponse, LLMProvider, ToolCall};
use anyhow::{Context, Result};

pub struct AnthropicLLM {
    agent: ureq::Agent,
    base_url: String,
    model: String,
    api_key: String,
}

impl AnthropicLLM {
    pub fn new(base_url: &str, model: &str, api_key: &str, timeout_secs: u64) -> Self {
        let base = if base_url.is_empty() { "https://api.anthropic.com" } else { base_url };
        Self {
            agent: super::http_agent(timeout_secs),
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
    let mut v = serde_json::json!({
        "model": model, "max_tokens": 4096, "system": req.system,
        "messages": messages
    });
    if !req.tools.is_empty() {
        v["tools"] = serde_json::Value::from(req.tools);
    }
    v
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
            .set("content-type", "application/json")
            .send_json(body(&self.model, req))
            .context("anthropic request failed")?
            .into_json()?;
        parse(&resp)
    }
}

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
    fn body_omits_tools_key_when_empty() {
        let msgs = vec![Message::User("hi".into())];
        let b = body("claude-sonnet-5", &ChatRequest { system: "", messages: &msgs, tools: &[] });
        assert!(b.get("tools").is_none());
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
