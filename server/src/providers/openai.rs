use super::{ChatRequest, ChatResponse, EmbeddingsProvider, LLMProvider, ToolCall};
use anyhow::{Context, Result};

pub struct OpenAILLM {
    agents: super::ChatAgents,
    base_url: String,
    model: String,
    api_key: String,
    reasoning: Option<String>,
}

impl OpenAILLM {
    pub fn new(
        base_url: &str,
        model: &str,
        api_key: &str,
        agents: super::ChatAgents,
        reasoning: Option<&str>,
    ) -> Self {
        Self {
            agents,
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key: api_key.to_string(),
            reasoning: reasoning.map(String::from),
        }
    }

    fn post(&self, req: &ChatRequest) -> Result<serde_json::Value> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = body(&self.model, req, self.reasoning.as_deref());
        self.agents.post_json(req.background, "openai", &body, |agent| {
            let request = agent.post(&url);
            if self.api_key.is_empty() { request } else { request.header("Authorization", format!("Bearer {}", self.api_key)) }
        })
    }
}

pub struct OpenAIEmbeddings {
    agent: ureq::Agent,
    base_url: String,
    model: String,
    api_key: String,
}

impl OpenAIEmbeddings {
    pub fn new(base_url: &str, model: &str, api_key: &str) -> Self {
        Self {
            agent: super::embeddings_http_agent(),
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key: api_key.to_string(),
        }
    }
}

fn wrap_tool(t: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": t["name"],
            "description": t["description"],
            "parameters": t["input_schema"],
        }
    })
}

/// `reasoning` asks an endpoint that supports it (`OpenRouter`) for the model's
/// reasoning text; the field is absent when no effort is configured.
pub fn body(model: &str, req: &ChatRequest, reasoning: Option<&str>) -> serde_json::Value {
    let mut messages: Vec<serde_json::Value> = vec![serde_json::json!({"role": "system", "content": req.system})];
    for m in req.messages {
        match m {
            super::Message::User(t) => messages.push(serde_json::json!({"role": "user", "content": t})),
            super::Message::Assistant { text, tool_calls } => {
                let content = if text.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(text.clone()) };
                let mut msg = serde_json::json!({"role": "assistant", "content": content});
                if !tool_calls.is_empty() {
                    let calls: Vec<serde_json::Value> = tool_calls
                        .iter()
                        .map(|c| serde_json::json!({
                            "id": c.id, "type": "function",
                            "function": {"name": c.name, "arguments": c.args}
                        }))
                        .collect();
                    msg["tool_calls"] = serde_json::Value::Array(calls);
                }
                messages.push(msg);
            }
            super::Message::ToolResult { call_id, content, is_error } => {
                let text = if *is_error { format!("ERROR: {content}") } else { content.clone() };
                messages.push(serde_json::json!({"role": "tool", "tool_call_id": call_id, "content": text}));
            }
        }
    }
    let mut v = serde_json::json!({"model": model, "messages": messages});
    if let Some(effort) = reasoning {
        v["reasoning"] = serde_json::json!({ "effort": effort });
    }
    if !req.tools.is_empty() {
        let tools: Vec<serde_json::Value> = req.tools.iter().map(wrap_tool).collect();
        v["tools"] = serde_json::Value::Array(tools);
    }
    v
}

/// `content` is a plain string in the `OpenAI` spec, but some compatible servers
/// send the multi-part array shape back; the parts' texts are concatenated.
fn content_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => {
            parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("")
        }
        _ => String::new(),
    }
}

/// llama.cpp-family servers send tool-call `arguments` as an object rather than
/// the spec's JSON-encoded string; either way the dispatcher gets JSON text.
fn tool_args(v: &serde_json::Value) -> String {
    match v.as_str() {
        Some(s) => s.to_string(),
        None if v.is_null() => String::new(),
        None => v.to_string(),
    }
}

/// `OpenRouter` returns the model's reasoning in `message.reasoning`; some
/// OpenAI-compatible servers name it `reasoning_content`.
pub fn reasoning_text(v: &serde_json::Value) -> String {
    let message = &v["choices"][0]["message"];
    for key in ["reasoning", "reasoning_content"] {
        let text = content_text(&message[key]);
        if !text.trim().is_empty() {
            return text;
        }
    }
    String::new()
}

pub fn parse(v: &serde_json::Value) -> Result<ChatResponse> {
    let message = &v["choices"][0]["message"];
    anyhow::ensure!(message.is_object(), "openai response missing choices[0].message");
    let text = content_text(&message["content"]);
    let mut tool_calls = Vec::new();
    if let Some(calls) = message["tool_calls"].as_array() {
        for c in calls {
            tool_calls.push(ToolCall {
                id: c["id"].as_str().unwrap_or("").to_string(),
                name: c["function"]["name"].as_str().unwrap_or("").to_string(),
                args: tool_args(&c["function"]["arguments"]),
            });
        }
    }
    Ok(ChatResponse { text, tool_calls })
}

#[expect(clippy::cast_possible_truncation, reason = "an embedding's components are f32 wherever they are used")]
fn narrow(x: f64) -> f32 {
    x as f32
}

pub fn parse_embeddings(v: &serde_json::Value) -> Result<Vec<Vec<f32>>> {
    let mut rows: Vec<(i64, Vec<f32>)> = v["data"]
        .as_array()
        .context("embeddings response has no data array")?
        .iter()
        .map(|d| {
            let vec = d["embedding"]
                .as_array()
                .context("no embedding")?
                .iter()
                .map(|x| narrow(x.as_f64().unwrap_or(0.0)))
                .collect();
            Ok((d["index"].as_i64().unwrap_or(0), vec))
        })
        .collect::<Result<_>>()?;
    rows.sort_by_key(|(i, _)| *i);
    Ok(rows.into_iter().map(|(_, v)| v).collect())
}

impl LLMProvider for OpenAILLM {
    fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        parse(&self.post(req)?)
    }

    fn chat_with_reasoning(&self, req: &ChatRequest) -> Result<(ChatResponse, String)> {
        let resp = self.post(req)?;
        let reasoning = if self.reasoning.is_some() { reasoning_text(&resp) } else { String::new() };
        Ok((parse(&resp)?, reasoning))
    }
}

impl EmbeddingsProvider for OpenAIEmbeddings {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut request = self.agent.post(&format!("{}/embeddings", self.base_url));
        if !self.api_key.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.api_key));
        }
        let resp: serde_json::Value = request
            .send_json(serde_json::json!({"model": self.model, "input": texts}))
            .context("openai embeddings request failed")?
            .body_mut()
            .read_json()?;
        parse_embeddings(&resp)
    }
}

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
        let b = body("gpt-x", &ChatRequest { system: "sys", messages: &msgs, tools: &tools, background: false }, None);
        let m = b["messages"].as_array().unwrap();
        assert_eq!(m[0]["role"], "system");
        assert_eq!(m[2]["tool_calls"][0]["function"]["name"], "task_create");
        assert_eq!(m[3]["role"], "tool");
        assert_eq!(m[4]["content"].as_str().unwrap(), "ERROR: {\"kind\":\"rejected\"}");
        assert_eq!(b["tools"][0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn body_omits_tools_key_when_empty() {
        let msgs = vec![Message::User("hi".into())];
        let b = body("gpt-x", &ChatRequest { system: "sys", messages: &msgs, tools: &[], background: false }, None);
        assert!(b.get("tools").is_none());
        assert!(b.get("reasoning").is_none());
    }

    #[test]
    fn body_carries_the_reasoning_effort_only_when_configured() {
        let msgs = vec![Message::User("hi".into())];
        let req = ChatRequest { system: "sys", messages: &msgs, tools: &[], background: false };
        assert!(body("gpt-x", &req, None).get("reasoning").is_none());
        assert_eq!(body("gpt-x", &req, Some("high"))["reasoning"]["effort"], "high");
    }

    #[test]
    fn reasoning_is_read_from_either_field_name() {
        let with_reasoning = serde_json::json!({"choices":[{"message":{
            "content": "sure", "reasoning": "the user wants a task, so I will make one"
        }}]});
        assert_eq!(reasoning_text(&with_reasoning), "the user wants a task, so I will make one");
        let compat = serde_json::json!({"choices":[{"message":{
            "content": "sure", "reasoning_content": "thinking"
        }}]});
        assert_eq!(reasoning_text(&compat), "thinking");
        let plain = serde_json::json!({"choices":[{"message":{"content":"sure"}}]});
        assert_eq!(reasoning_text(&plain), "");
        assert_eq!(parse(&with_reasoning).unwrap().text, "sure");
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
    fn parse_serializes_object_shaped_tool_arguments() {
        let v = serde_json::json!({"choices":[{"message":{
            "content": "",
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"task_create","arguments":{"title":"buy milk"}}}]
        }}]});
        let r = parse(&v).unwrap();
        assert_eq!(r.tool_calls[0].args, "{\"title\":\"buy milk\"}");
    }

    #[test]
    fn parse_joins_array_shaped_content() {
        let v = serde_json::json!({"choices":[{"message":{
            "content": [{"type":"text","text":"hello "},{"type":"text","text":"there"}]
        }}]});
        assert_eq!(parse(&v).unwrap().text, "hello there");
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
