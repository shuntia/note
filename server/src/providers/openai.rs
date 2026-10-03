use super::{ChatRequest, ChatResponse, EmbeddingsProvider, FirstTokenTimeout, LLMProvider, StreamOpts, StreamSink, ToolCall};
use anyhow::{Context, Result};
use std::io::BufRead;
use std::time::{Duration, Instant};

pub struct OpenAILLM {
    agents: super::ChatAgents,
    base_url: String,
    model: super::LiveModel,
    api_key: String,
    reasoning: Option<String>,
    extra: serde_json::Map<String, serde_json::Value>,
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
            model: super::LiveModel::new(model),
            api_key: api_key.to_string(),
            reasoning: reasoning.map(String::from),
            extra: serde_json::Map::new(),
        }
    }

    /// Top-level fields of `extra` go into every request body.
    #[must_use]
    pub fn with_extra(mut self, extra: serde_json::Value) -> Self {
        if let serde_json::Value::Object(fields) = extra {
            self.extra.extend(fields);
        }
        self
    }

    #[must_use]
    pub fn without_reasoning(mut self) -> Self {
        self.reasoning = None;
        self
    }

    fn url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    fn request_body(&self, req: &ChatRequest) -> serde_json::Value {
        let mut v = body(&self.model.get(), req, self.reasoning.as_deref());
        for (k, field) in &self.extra {
            v[k] = field.clone();
        }
        v
    }

    fn authorized(&self, request: ureq::RequestBuilder<ureq::typestate::WithBody>) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        if self.api_key.is_empty() { request } else { request.header("Authorization", format!("Bearer {}", self.api_key)) }
    }

    fn post(&self, req: &ChatRequest) -> Result<serde_json::Value> {
        let url = self.url();
        let body = self.request_body(req);
        self.agents.post_json(req.background, "openai", &body, |agent| self.authorized(agent.post(&url)))
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

/// Reads Server-Sent Events of a streamed chat completion into `sink`. Tool
/// calls arrive in pieces keyed by `index`; one is complete once another index
/// begins or the stream ends.
fn read_stream(
    reader: impl BufRead,
    started: Instant,
    first_token: Duration,
    sink: &mut dyn StreamSink,
) -> Result<ChatResponse> {
    let mut resp = ChatResponse::default();
    let mut pending: Option<(u64, ToolCall)> = None;
    let mut heard = false;
    for line in reader.lines() {
        let line = line.context("reading the openai stream")?;
        if !heard && started.elapsed() > first_token {
            return Err(FirstTokenTimeout(first_token).into());
        }
        let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
        if data == "[DONE]" {
            break;
        }
        let event: serde_json::Value =
            serde_json::from_str(data).with_context(|| format!("openai stream event {data:?}"))?;
        if let Some(err) = event.get("error") {
            anyhow::bail!("openai stream failed: {err}");
        }
        let delta = &event["choices"][0]["delta"];
        let text = content_text(&delta["content"]);
        if !text.is_empty() {
            heard = true;
            resp.text.push_str(&text);
            if !sink.text(&text) {
                return Ok(resp);
            }
        }
        for piece in delta["tool_calls"].as_array().into_iter().flatten() {
            heard = true;
            let index = piece["index"].as_u64().or(pending.as_ref().map(|(i, _)| *i)).unwrap_or(0);
            if let Some((_, done)) = pending.take_if(|(i, _)| *i != index) {
                if !complete_call(done, &mut resp, sink) {
                    return Ok(resp);
                }
            }
            let (_, call) = pending.get_or_insert_with(|| (index, ToolCall { id: String::new(), name: String::new(), args: String::new() }));
            if let Some(id) = piece["id"].as_str().filter(|_| call.id.is_empty()) {
                call.id = id.to_string();
            }
            if let Some(name) = piece["function"]["name"].as_str().filter(|_| call.name.is_empty()) {
                call.name = name.to_string();
            }
            call.args.push_str(&tool_args(&piece["function"]["arguments"]));
        }
    }
    if let Some((_, done)) = pending {
        complete_call(done, &mut resp, sink);
    }
    Ok(resp)
}

fn complete_call(call: ToolCall, resp: &mut ChatResponse, sink: &mut dyn StreamSink) -> bool {
    let go_on = sink.tool_call(&call);
    resp.tool_calls.push(call);
    go_on
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
                .map(|x| x.as_f64().unwrap_or(0.0) as f32)
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

    fn chat_stream(&self, req: &ChatRequest, opts: &StreamOpts, sink: &mut dyn StreamSink) -> Result<ChatResponse> {
        let mut body = self.request_body(req);
        body["stream"] = serde_json::Value::Bool(true);
        let agent = self.agents.stream_agent(req.background, opts.first_token);
        let started = Instant::now();
        let mut resp = match self.authorized(agent.post(&self.url())).send_json(&body) {
            Ok(resp) => resp,
            Err(ureq::Error::Timeout(ureq::Timeout::RecvResponse)) => {
                return Err(FirstTokenTimeout(opts.first_token).into());
            }
            Err(e) => anyhow::bail!("openai stream request failed: {e}"),
        };
        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let head: String = resp.body_mut().read_to_string().unwrap_or_default().chars().take(300).collect();
            anyhow::bail!("openai stream request failed: status {code}: {head}");
        }
        let reader = std::io::BufReader::new(resp.body_mut().as_reader());
        read_stream(reader, started, opts.first_token, sink)
    }

    fn model(&self) -> Option<String> {
        Some(self.model.get())
    }

    fn set_model(&self, model: &str) -> bool {
        self.model.set(model);
        true
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

    use crate::providers::{ChatAgents, FirstTokenTimeout, StreamOpts, StreamSink};
    use std::time::Duration;

    const SSE: &str = concat!(
        ": OPENROUTER PROCESSING\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"On it, \"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"looking now.\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"web_search\",\"arguments\":\"{\\\"q\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\":\\\"train\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"c2\",\"function\":{\"name\":\"task_list\",\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: [DONE]\n\n",
    );

    #[derive(Default)]
    struct Collect {
        text: String,
        calls: Vec<ToolCall>,
        order: Vec<String>,
        stop_after: Option<usize>,
    }

    impl Collect {
        fn go_on(&self) -> bool {
            self.stop_after.is_none_or(|n| self.order.len() < n)
        }
    }

    impl StreamSink for Collect {
        fn text(&mut self, delta: &str) -> bool {
            self.text.push_str(delta);
            self.order.push("text".into());
            self.go_on()
        }

        fn tool_call(&mut self, call: &ToolCall) -> bool {
            self.calls.push(call.clone());
            self.order.push(format!("call:{}", call.id));
            self.go_on()
        }
    }

    fn stub_sse(sse: &'static str) -> String {
        crate::testhttp::serve_stream(sse, Duration::ZERO).0
    }

    fn req() -> ChatRequest<'static> {
        static MSGS: std::sync::LazyLock<Vec<Message>> = std::sync::LazyLock::new(|| vec![Message::User("hi".into())]);
        ChatRequest { system: "sys", messages: &MSGS, tools: &[], background: false }
    }

    fn opts(first_token: Duration) -> StreamOpts {
        StreamOpts { first_token }
    }

    #[test]
    fn streams_text_and_assembles_tool_calls_by_index() {
        let base = stub_sse(SSE);
        let llm = OpenAILLM::new(&base, "m", "", ChatAgents::new(30, 30), None);
        let mut sink = Collect::default();
        let resp = llm.chat_stream(&req(), &opts(Duration::from_secs(5)), &mut sink).unwrap();
        assert_eq!(sink.text, "On it, looking now.");
        assert_eq!(
            sink.calls.iter().map(|c| (c.id.as_str(), c.name.as_str(), c.args.as_str())).collect::<Vec<_>>(),
            vec![("c1", "web_search", r#"{"q":"train"}"#), ("c2", "task_list", "{}")]
        );
        assert_eq!(sink.order, vec!["text", "text", "call:c1", "call:c2"], "c1 completes when index 1 begins");
        assert_eq!(resp.text, "On it, looking now.");
        assert_eq!(resp.tool_calls.len(), 2);
    }

    #[test]
    fn a_sink_that_stops_ends_the_stream_early() {
        let base = stub_sse(SSE);
        let llm = OpenAILLM::new(&base, "m", "", ChatAgents::new(30, 30), None);
        let mut sink = Collect { stop_after: Some(1), ..Collect::default() };
        let resp = llm.chat_stream(&req(), &opts(Duration::from_secs(5)), &mut sink).unwrap();
        assert_eq!(resp.text, "On it, ");
        assert!(resp.tool_calls.is_empty());
        assert_eq!(sink.order, vec!["text"]);
    }

    #[test]
    fn no_first_token_in_time_is_a_first_token_timeout() {
        let (base, _rx) = crate::testhttp::serve_stream(SSE, Duration::from_secs(2));
        let llm = OpenAILLM::new(&base, "m", "", ChatAgents::new(30, 30), None);
        let started = std::time::Instant::now();
        let err = llm.chat_stream(&req(), &opts(Duration::from_millis(300)), &mut Collect::default()).unwrap_err();
        assert!(err.is::<FirstTokenTimeout>(), "{err:#}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn extra_body_fields_reach_the_request() {
        let (base, rx) = crate::testhttp::serve_stream(SSE, Duration::ZERO);
        let llm = OpenAILLM::new(&base, "m", "", ChatAgents::new(30, 30), Some("high"))
            .with_extra(serde_json::json!({"provider": {"sort": "latency"}}))
            .without_reasoning();
        llm.chat_stream(&req(), &opts(Duration::from_secs(5)), &mut Collect::default()).unwrap();
        let sent = crate::testhttp::body_json(&rx.recv().unwrap());
        assert_eq!(sent["provider"]["sort"], "latency");
        assert_eq!(sent["stream"], true);
        assert!(sent.get("reasoning").is_none());
    }
}
