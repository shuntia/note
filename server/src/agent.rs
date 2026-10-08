use crate::providers::{ChatRequest, EmbeddingsProvider, LLMProvider, Message};
use crate::search::SearchProvider;
use crate::tools::{self, SessionKind, ToolCtx, ToolError};
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;
use std::fmt::Write as _;

pub const MAX_TURNS: usize = 16;
/// An import session is one brief, an inbox session one decision, and a
/// summary one line: the call, plus a single retry when the first one is
/// rejected.
pub const IMPORT_MAX_TURNS: usize = 2;
/// A harvest reads one digest: enough rounds to search memory before each
/// write and still finish.
pub const HARVEST_MAX_TURNS: usize = 8;
/// A review reads one week: enough rounds to search memory before each write
/// and still finish.
pub const REVIEW_MAX_TURNS: usize = 8;
/// A trigger session looks around and then speaks or does not: room to read the
/// situation, never room to hold a conversation with itself.
pub const TRIGGER_MAX_TURNS: usize = 10;

use crate::model_text::{self as mt, REPLY_IN_JAPANESE, SPEAK_JAPANESE, VISITOR_JAPANESE};
use crate::text::Lang;

/// Whom a session writes for: the visitor on a share link, the user otherwise.
fn session_lang(deps: &SessionDeps, username: &str, kind: SessionKind) -> Lang {
    match kind {
        SessionKind::Share => deps.share.as_ref().map_or_else(Lang::default, |s| s.visitor_lang),
        _ => Lang::for_user(deps.config_dir, username),
    }
}

/// `system` with the line that asks for Japanese, where the user reads it.
pub(crate) fn with_language_line(mut system: String, config_dir: &Path, username: &str) -> String {
    if Lang::for_user(config_dir, username) == Lang::Ja {
        system.push_str("\n\n");
        system.push_str(REPLY_IN_JAPANESE);
    }
    system
}


/// The sessions that run on their own instructions alone, with none of the
/// user's standing context and a single call to make.
fn single_call(kind: SessionKind) -> bool {
    matches!(kind, SessionKind::Import | SessionKind::Inbox | SessionKind::Summarize)
}

pub struct SessionDeps<'a> {
    pub db: &'a Mutex<Connection>,
    pub config_dir: &'a Path,
    pub data_dir: &'a Path,
    pub llm: &'a dyn LLMProvider,
    pub embeddings: Option<&'a dyn EmbeddingsProvider>,
    /// Absent where the server has no search configured; the session then never
    /// offers the `web_search` tool.
    pub search: Option<&'a dyn SearchProvider>,
    /// Confines the session's task tools to one task and its steps.
    pub task_scope: Option<i64>,
    /// Confines the session's inbox decision to one source id.
    pub inbox_source: Option<String>,
    /// Records every fact the session writes against this source id.
    pub memory_source: Option<String>,
    /// The API token the caller presented, so the session's log row says which
    /// credential spent it.
    pub token_id: Option<i64>,
    /// What the model should know about the thread it is replying in, appended
    /// after the context block.
    pub thread_note: Option<String>,
    /// Set on a visitor's session: the link whose scope its tools are held to.
    pub share: Option<ShareSession>,
}

/// The link a share session answers for.
#[derive(Debug, Clone)]
pub struct ShareSession {
    pub id: i64,
    pub thread_id: i64,
    /// The owner's per-link instruction, appended under `# From {owner}`.
    pub brief: String,
    pub scope: crate::shares::ShareScope,
    /// The visitor's language, from their browser.
    pub visitor_lang: crate::text::Lang,
}

#[derive(Debug, Clone)]
pub struct SessionStep {
    pub name: String,
    pub args: String,
    pub result: String,
    pub is_error: bool,
    /// The reasoning of the round that made this call, on the round's first
    /// step and `None` on the rest, so a transcript can show the thinking where
    /// it happened.
    pub thinking: Option<String>,
}

#[derive(Debug)]
pub struct SessionOutcome {
    pub reply: String,
    pub turns: usize,
    pub tool_calls: usize,
    pub steps: Vec<SessionStep>,
    /// The reasoning of the final round, the one that answered in text; every
    /// earlier round's reasoning rides on its first step instead. Empty when a
    /// terminal tool or the turn cap ended the session.
    pub reasoning: String,
    /// Wall clock from the first provider call to the reply, in milliseconds.
    pub thought_ms: u64,
}

/// One step of a session as it happens, for a caller that shows progress while
/// the session runs. `index` addresses the matching entry of `steps`.
#[derive(Debug, Clone, Copy)]
pub enum AgentEvent<'a> {
    Thinking { text: &'a str },
    ToolCall { index: usize, name: &'a str, args: &'a str },
    ToolResult { index: usize, name: &'a str, result: &'a str, is_error: bool },
    Reply { text: &'a str },
    Error { reason: crate::failure::Reason },
}

/// Runs one agent session: chat, dispatch tool calls, feed results back, until
/// the model answers in text, a terminal tool succeeds, or the turn cap is hit.
/// The DB lock is held only for assembly, individual dispatches, and log writes
/// — never across a provider call.
/// `now` is the caller's clock so a nightly run assembles context for the same
/// local date its plan was generated for.
pub fn run_session(
    deps: &SessionDeps,
    user_id: i64,
    username: &str,
    kind: SessionKind,
    now: jiff::Timestamp,
    history: &[Message],
    opening: &str,
) -> Result<SessionOutcome> {
    run_session_watched(deps, user_id, username, kind, now, history, opening, &|_| {})
}

/// `run_session` with a progress sink: every event is handed over as it
/// happens, on the session's own thread, before the session moves on.
/// However it ends, the session leaves one `agent_traces` row behind; failing
/// to write that row changes nothing about the session's own result.
#[allow(clippy::too_many_arguments)]
pub fn run_session_watched(
    deps: &SessionDeps,
    user_id: i64,
    username: &str,
    kind: SessionKind,
    now: jiff::Timestamp,
    history: &[Message],
    opening: &str,
    on_event: &dyn Fn(AgentEvent),
) -> Result<SessionOutcome> {
    let mut trace = crate::trace::Builder::new(kind, opening);
    let result =
        run_traced(deps, user_id, username, kind, now, history, opening, on_event, &mut trace);
    if let Err(e) = &result {
        trace.failed(&format!("{e:#}"));
    }
    let _ = trace.insert(&crate::db_guard(deps.db), user_id);
    result
}

/// The system prompt a session of `kind` runs on: its own instructions, then
/// the context its kind is allowed to see.
pub(crate) fn system_prompt(
    deps: &SessionDeps,
    user_id: i64,
    username: &str,
    kind: SessionKind,
    now: jiff::Timestamp,
) -> Result<String> {
    system_prompt_in(deps, user_id, username, kind, now, session_lang(deps, username, kind))
}

/// `system_prompt` in `lang`, whatever the reader's setting.
pub(crate) fn system_prompt_in(
    deps: &SessionDeps,
    user_id: i64,
    username: &str,
    kind: SessionKind,
    now: jiff::Timestamp,
    lang: Lang,
) -> Result<String> {
    // An import session briefs one task and an inbox session judges one item,
    // both on a caller's behalf: each gets its own instructions and none of the
    // user's standing context.
    let load = |name| crate::prompts::load_in(deps.config_dir, username, name, lang);
    let mut system = match kind {
        SessionKind::Import => load("import")?,
        SessionKind::Inbox => load("inbox")?,
        SessionKind::Summarize => load("summarize")?,
        SessionKind::Harvest => load("harvest")?,
        SessionKind::Review => load("review")?,
        SessionKind::Share => load("share")?,
        SessionKind::Call => {
            let display = crate::config::UserConfig::load(deps.config_dir, username).map_or_else(|_| username.to_string(), |c| c.display_name);
            let voice = load("voice")?.replace("{name}", &display);
            let briefly = mt::call_briefly(lang);
            match lang {
                Lang::Ja => format!("{briefly}\n{SPEAK_JAPANESE}\n\n{voice}"),
                Lang::En => format!("{briefly}\n\n{voice}"),
            }
        }
        _ => load("persona")?,
    };
    if kind == SessionKind::Nightly {
        system.push_str("\n\n");
        system.push_str(&load("planning")?);
    }
    if kind == SessionKind::Trigger {
        system.push_str("\n\n");
        system.push_str(&load("trigger")?);
    }
    if lang == Lang::Ja && kind != SessionKind::Call {
        system.push_str("\n\n");
        system.push_str(if kind == SessionKind::Share { VISITOR_JAPANESE } else { REPLY_IN_JAPANESE });
    }
    if kind == SessionKind::Share {
        let share = deps.share.as_ref().ok_or_else(|| anyhow::anyhow!("a share session needs its link"))?;
        let conn = crate::db_guard(deps.db);
        let display = crate::config::UserConfig::load(deps.config_dir, username).map_or_else(|_| username.to_string(), |c| c.display_name);
        system = system.replace("{owner}", &display);
        if !share.brief.trim().is_empty() {
            let _ = write!(system, "\n\n{}\n\n{}", mt::share_from(lang, &display), share.brief.trim());
        }
        let rendered = crate::shares::render(&conn, deps.config_dir, user_id, username, &share.scope, lang, now)?;
        system.push_str("\n\n");
        system.push_str(&rendered.text);
        if share.scope.notes {
            let _ = write!(system, "\n\n{}", mt::share_note_line(lang, &display));
        }
    } else if !single_call(kind) {
        let conn = crate::db_guard(deps.db);
        let context = crate::context::assemble(&conn, deps.config_dir, deps.data_dir, user_id, username, now)?;
        system.push_str("\n\n");
        system.push_str(&context);
        if let Some(note) = &deps.thread_note {
            system.push_str("\n\n");
            system.push_str(note);
        }
    }
    Ok(system)
}

#[allow(clippy::too_many_arguments)]
fn run_traced(
    deps: &SessionDeps,
    user_id: i64,
    username: &str,
    kind: SessionKind,
    now: jiff::Timestamp,
    history: &[Message],
    opening: &str,
    on_event: &dyn Fn(AgentEvent),
    trace: &mut crate::trace::Builder,
) -> Result<SessionOutcome> {
    let system = system_prompt(deps, user_id, username, kind, now)?;
    let mut schemas = match (kind, &deps.share) {
        (SessionKind::Share, Some(s)) => tools::share_schemas(&s.scope),
        _ => tools::schemas(kind),
    };
    if deps.search.is_none() {
        schemas.retain(|s| s["name"] != "web_search");
    }
    let mut messages = history.to_vec();
    messages.push(Message::User(opening.to_string()));
    let mut turns = 0;
    let mut tool_calls = 0;
    let mut steps = Vec::new();
    let mut last_text = String::new();
    let max_turns = match kind {
        _ if single_call(kind) => IMPORT_MAX_TURNS,
        SessionKind::Harvest => HARVEST_MAX_TURNS,
        SessionKind::Review => REVIEW_MAX_TURNS,
        SessionKind::Trigger => TRIGGER_MAX_TURNS,
        SessionKind::Share => tools::SHARE_MAX_TURNS,
        _ => MAX_TURNS,
    };
    let background = is_background(kind);
    let env = CallEnv { deps, user_id, username, kind };
    let started = std::time::Instant::now();

    while turns < max_turns {
        let req = ChatRequest { system: &system, messages: &messages, tools: &schemas, background };
        let round = std::time::Instant::now();
        let (resp, thinking) = match deps.llm.chat_with_reasoning(&req) {
            Ok(v) => {
                trace.round(round.elapsed().as_millis() as u64);
                v
            }
            Err(e) => {
                trace.round_failed(round.elapsed().as_millis() as u64, &format!("{e:#}"));
                on_event(AgentEvent::Error { reason: crate::failure::Reason::of(&e) });
                return Err(e);
            }
        };
        turns += 1;
        let mut thinking = match thinking.trim() {
            "" => None,
            text => {
                on_event(AgentEvent::Thinking { text });
                Some(text.to_string())
            }
        };
        last_text = resp.text;
        if resp.tool_calls.is_empty() {
            on_event(AgentEvent::Reply { text: &last_text });
            trace.ok(&last_text);
            finish(deps, user_id, kind, turns, tool_calls, log_kind(kind, "agent_session"))?;
            let thought_ms = started.elapsed().as_millis() as u64;
            return Ok(SessionOutcome {
                reply: last_text,
                turns,
                tool_calls,
                steps,
                reasoning: thinking.unwrap_or_default(),
                thought_ms,
            });
        }
        let calls = resp.tool_calls.clone();
        messages.push(Message::Assistant { text: last_text.clone(), tool_calls: resp.tool_calls });
        for call in calls {
            if call.name == "batch" {
                let (content, is_error) = run_batch(
                    &env,
                    &call.args,
                    &mut steps,
                    &mut tool_calls,
                    &mut thinking,
                    trace,
                    on_event,
                );
                messages.push(Message::ToolResult { call_id: call.id, content, is_error });
                continue;
            }
            tool_calls += 1;
            let (content, is_error) =
                env.step(&call.name, &call.args, &mut steps, &mut thinking, trace, on_event);
            if !is_error && tools::is_terminal(kind, &call.name) {
                on_event(AgentEvent::Reply { text: &content });
                trace.ok(&content);
                finish(deps, user_id, kind, turns, tool_calls, log_kind(kind, "agent_session"))?;
                let thought_ms = started.elapsed().as_millis() as u64;
                return Ok(SessionOutcome {
                    reply: content,
                    turns,
                    tool_calls,
                    steps,
                    reasoning: String::new(),
                    thought_ms,
                });
            }
            messages.push(Message::ToolResult { call_id: call.id, content, is_error });
        }
    }
    if last_text.trim().is_empty()
        && matches!(kind, SessionKind::Talk | SessionKind::Checkin | SessionKind::Share)
    {
        last_text = crate::text::max_turns_reply(session_lang(deps, username, kind));
    }
    on_event(AgentEvent::Reply { text: &last_text });
    trace.max_turns(&last_text);
    finish(deps, user_id, kind, turns, tool_calls, log_kind(kind, "agent_max_turns"))?;
    let thought_ms = started.elapsed().as_millis() as u64;
    Ok(SessionOutcome {
        reply: last_text,
        turns,
        tool_calls,
        steps,
        reasoning: String::new(),
        thought_ms,
    })
}

/// Talk, import, inbox and share sessions answer a caller who is waiting on them.
fn is_background(kind: SessionKind) -> bool {
    !matches!(
        kind,
        SessionKind::Talk | SessionKind::Import | SessionKind::Inbox | SessionKind::Share
    )
}

/// Runs one tool call as the model made it and returns its result text and
/// whether it failed. Embeddings first, outside the DB lock, then one dispatch
/// inside it; `once` makes that dispatch `dispatch_once` under its
/// `(call_id, op_key)`. `web_search` reaches the network instead, never takes
/// the lock and ignores `once`.
pub(crate) fn run_tool(
    deps: &SessionDeps,
    user_id: i64,
    username: &str,
    kind: SessionKind,
    name: &str,
    args: &str,
    once: Option<(&str, &str)>,
) -> (String, bool) {
    let result = match name {
        "web_search" if !tools::registry(kind).contains(&name) => Err(ToolError::forbidden(
            format!("tool {name} is not available in this session type"),
        )),
        "web_search" => crate::search::run_tool(deps, user_id, username, is_background(kind), args),
        _ => {
            let vectors = tools::prepare(deps.embeddings, name, args);
            let conn = crate::db_guard(deps.db);
            let ctx = ToolCtx {
                config_dir: deps.config_dir,
                data_dir: deps.data_dir,
                user_id,
                username,
                vectors,
                task_scope: deps.task_scope,
                inbox_source: deps.inbox_source.clone(),
                memory_source: deps.memory_source.clone(),
                share: deps.share.as_ref().map(|s| s.scope.clone()),
                share_thread: deps.share.as_ref().map(|s| s.thread_id),
            };
            match once {
                Some((call_id, op_key)) => {
                    tools::dispatch_once(&conn, &ctx, kind, name, args, call_id, op_key)
                }
                None => tools::dispatch(&conn, &ctx, kind, name, args),
            }
        }
    };
    match result {
        Ok(v) => (v.to_string(), false),
        Err(e) => (error_json(&e), true),
    }
}

/// What every tool call of one session shares, so a call the model made and a
/// call inside a `batch` take exactly the same path.
struct CallEnv<'a> {
    deps: &'a SessionDeps<'a>,
    user_id: i64,
    username: &'a str,
    kind: SessionKind,
}

impl CallEnv<'_> {
    fn run(&self, name: &str, args: &str) -> (String, bool) {
        run_tool(self.deps, self.user_id, self.username, self.kind, name, args, None)
    }

    /// `run` as a session step: its own index, its own pair of events, its own
    /// entry in `steps`, and its own timed call on the round's trace. The first
    /// step of a round takes `thinking` and leaves the rest of the round none.
    fn step(
        &self,
        name: &str,
        args: &str,
        steps: &mut Vec<SessionStep>,
        thinking: &mut Option<String>,
        trace: &mut crate::trace::Builder,
        on_event: &dyn Fn(AgentEvent),
    ) -> (String, bool) {
        let index = steps.len();
        on_event(AgentEvent::ToolCall { index, name, args });
        let started = std::time::Instant::now();
        let (content, is_error) = self.run(name, args);
        trace.call(name, args, &content, is_error, started.elapsed().as_millis() as u64);
        on_event(AgentEvent::ToolResult { index, name, result: &content, is_error });
        steps.push(SessionStep {
            name: name.to_string(),
            args: args.to_string(),
            result: content.clone(),
            is_error,
            thinking: thinking.take(),
        });
        (content, is_error)
    }
}

fn error_json(e: &ToolError) -> String {
    serde_json::to_string(e).unwrap_or_else(|_| r#"{"kind":"internal"}"#.into())
}

/// Runs a `batch` call's sub-calls in order, each its own step, and returns the
/// single tool result the model gets back. `is_error` is set only when the
/// batch arguments themselves are unusable: a sub-call that fails or is refused
/// is an entry in `results` and leaves the rest of the batch running.
fn run_batch(
    env: &CallEnv,
    raw_args: &str,
    steps: &mut Vec<SessionStep>,
    tool_calls: &mut usize,
    thinking: &mut Option<String>,
    trace: &mut crate::trace::Builder,
    on_event: &dyn Fn(AgentEvent),
) -> (String, bool) {
    if !tools::registry(env.kind).contains(&"batch") {
        let e = ToolError::forbidden("tool batch is not available in this session type");
        return (error_json(&e), true);
    }
    if raw_args.len() > tools::MAX_ARGS_BYTES {
        let e = ToolError::rejected(format!("arguments exceed {} bytes", tools::MAX_ARGS_BYTES));
        return (error_json(&e), true);
    }
    let args: tools::BatchArgs = match serde_json::from_str(raw_args) {
        Ok(a) => a,
        Err(e) => return (error_json(&ToolError::invalid_args(e.to_string())), true),
    };
    if args.calls.is_empty() || args.calls.len() > tools::MAX_BATCH_CALLS {
        let e = ToolError::rejected(format!(
            "batch takes 1 to {} calls, not {}",
            tools::MAX_BATCH_CALLS,
            args.calls.len()
        ));
        return (error_json(&e), true);
    }
    let mut results = Vec::new();
    for sub in args.calls {
        let refused = if sub.tool == "batch" {
            Some(ToolError::rejected("a batch cannot hold another batch"))
        } else if tools::is_terminal(env.kind, &sub.tool) {
            Some(ToolError::rejected(format!(
                "{} ends the session and must be called on its own",
                sub.tool
            )))
        } else {
            None
        };
        if let Some(e) = refused {
            results.push(serde_json::json!({ "tool": sub.tool, "ok": false, "error": e }));
            continue;
        }
        *tool_calls += 1;
        let sub_args = match sub.args {
            serde_json::Value::Null => "{}".to_string(),
            v if tools::registry(env.kind).contains(&sub.tool.as_str()) => {
                tools::coerce_batch_args(&sub.tool, v).to_string()
            }
            v => v.to_string(),
        };
        let (content, is_error) = env.step(&sub.tool, &sub_args, steps, thinking, trace, on_event);
        let value: serde_json::Value =
            serde_json::from_str(&content).unwrap_or(serde_json::Value::String(content));
        results.push(if is_error {
            serde_json::json!({ "tool": sub.tool, "ok": false, "error": value })
        } else {
            serde_json::json!({ "tool": sub.tool, "ok": true, "result": value })
        });
    }
    (serde_json::json!({ "results": results }).to_string(), false)
}

fn finish(
    deps: &SessionDeps,
    user_id: i64,
    kind: SessionKind,
    turns: usize,
    calls: usize,
    log_kind: &str,
) -> Result<()> {
    let mut detail = format!("kind={kind:?} turns={turns} tools={calls}");
    if let Some(id) = deps.token_id {
        let _ = write!(detail, " token={id}");
    }
    if let Some(share) = &deps.share {
        let _ = write!(detail, " share={}", share.id);
    }
    let conn = crate::db_guard(deps.db);
    crate::log::record(&conn, Some(user_id), log_kind, &detail)
}

/// A share session logs under its own kinds, so it never counts against the
/// owner's session budget or shows in their activity.
fn log_kind(kind: SessionKind, base: &'static str) -> &'static str {
    match (kind, base) {
        (SessionKind::Share, "agent_session") => "share_session",
        (SessionKind::Share, _) => "share_max_turns",
        _ => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{mock::MockLLM, ChatResponse, ToolCall};
    use crate::tools::SessionKind;
    use std::sync::Mutex;

    fn env() -> (Mutex<rusqlite::Connection>, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let write = |rel: &str, c: &str| {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        };
        write(
            "defaults/user.toml",
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n",
        );
        write("defaults/prompts/persona.md", "you are note, be kind");
        write("defaults/prompts/planning.md", "plan the day");
        write("defaults/prompts/import.md", "brief the assignment");
        write("defaults/prompts/inbox.md", "read the item");
        write("defaults/prompts/trigger.md", "you are following up on your own plan");
        write("defaults/prompts/search.md", "answer from the hits alone");
        (Mutex::new(conn), tmp)
    }

    fn deps<'a>(
        db: &'a Mutex<rusqlite::Connection>,
        tmp: &'a tempfile::TempDir,
        llm: &'a dyn LLMProvider,
    ) -> SessionDeps<'a> {
        SessionDeps { db, config_dir: tmp.path(), data_dir: tmp.path(), llm, embeddings: None,
            search: None, task_scope: None, inbox_source: None, memory_source: None, token_id: None, thread_note: None,
            share: None }
    }

    fn now() -> jiff::Timestamp {
        "2026-08-31T04:00:00Z".parse().unwrap()
    }

    #[test]
    fn a_japanese_reader_is_answered_in_japanese() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![]);
        let prompt = |kind| system_prompt(&deps(&db, &tmp, &llm), 1, "aki", kind, now()).unwrap();
        assert!(!prompt(SessionKind::Talk).contains(REPLY_IN_JAPANESE));
        let dir = tmp.path().join("users/aki");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("user.toml"), "language = \"ja\"\n").unwrap();
        assert!(prompt(SessionKind::Talk).contains(REPLY_IN_JAPANESE));
        let talk = prompt(SessionKind::Talk);
        for heading in ["# 常設コンテキスト", "# 現在", "# 今日の予定", "# タスク", "# 設定", "# 最近の動き"] {
            assert!(talk.contains(heading), "{talk}");
        }
        assert!(!talk.contains("# Standing context") && !talk.contains("# Settings"), "{talk}");
        std::fs::create_dir_all(tmp.path().join("defaults/prompts/ja")).unwrap();
        std::fs::write(tmp.path().join("defaults/prompts/ja/persona.md"), "ノートです").unwrap();
        assert!(prompt(SessionKind::Talk).starts_with("ノートです"));
        assert!(prompt(SessionKind::Nightly).contains(REPLY_IN_JAPANESE));
        assert!(prompt(SessionKind::Import).contains(REPLY_IN_JAPANESE));
        std::fs::write(tmp.path().join("defaults/prompts/voice.md"), "on the phone with {name}").unwrap();
        let call = prompt(SessionKind::Call);
        assert!(call.starts_with(&format!("{}\n{SPEAK_JAPANESE}\n\non the phone with X", mt::call_briefly(Lang::Ja))), "{call}");
        assert!(!call.contains(REPLY_IN_JAPANESE), "{call}");
    }

    #[test]
    fn a_call_prompt_opens_by_asking_for_short_replies() {
        let (db, tmp) = env();
        std::fs::write(tmp.path().join("defaults/prompts/voice.md"), "on the phone with {name}").unwrap();
        let llm = MockLLM::scripted(vec![]);
        let call = system_prompt(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Call, now()).unwrap();
        assert!(call.starts_with(&format!("{}\n\non the phone with X", mt::call_briefly(Lang::En))), "{call}");
        assert!(!call.contains(SPEAK_JAPANESE));
    }

    #[test]
    fn a_visitor_is_answered_in_their_own_language() {
        let (db, tmp) = env();
        std::fs::write(tmp.path().join("defaults/prompts/share.md"), "answer for {owner}").unwrap();
        let llm = MockLLM::scripted(vec![]);
        let prompt = |visitor_lang| {
            let share = ShareSession { id: 1, thread_id: 1, brief: String::new(), scope: crate::shares::ShareScope::default(), visitor_lang };
            let deps = SessionDeps { share: Some(share), ..deps(&db, &tmp, &llm) };
            system_prompt(&deps, 1, "aki", SessionKind::Share, now()).unwrap()
        };
        assert!(prompt(crate::text::Lang::Ja).contains(VISITOR_JAPANESE));
        let dir = tmp.path().join("users/aki");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("user.toml"), "language = \"ja\"\n").unwrap();
        let english = prompt(crate::text::Lang::En);
        assert!(!english.contains(VISITOR_JAPANESE) && !english.contains(REPLY_IN_JAPANESE), "{english}");
    }

    /// Every event as `kind:detail`, in the order the session emitted it.
    fn trace(ev: AgentEvent) -> String {
        match ev {
            AgentEvent::Thinking { text } => format!("thinking:{text}"),
            AgentEvent::ToolCall { index, name, .. } => format!("call:{index}:{name}"),
            AgentEvent::ToolResult { index, name, is_error, .. } => {
                format!("result:{index}:{name}:{is_error}")
            }
            AgentEvent::Reply { text } => format!("reply:{text}"),
            AgentEvent::Error { reason } => format!("error:{}", reason.code()),
        }
    }

    #[test]
    fn a_watched_session_reports_thinking_calls_results_then_the_reply() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![
            ChatResponse {
                text: String::new(),
                tool_calls: vec![
                    ToolCall {
                        id: "c1".into(),
                        name: "task_create".into(),
                        args: r#"{"title":"buy milk"}"#.into(),
                    },
                    ToolCall { id: "c2".into(), name: "memory_query".into(), args: "{}".into() },
                ],
            },
            ChatResponse { text: "done".into(), tool_calls: vec![] },
        ])
        .thinking(vec!["a task, then a lookup"]);
        let seen = std::cell::RefCell::new(Vec::new());
        let out = run_session_watched(
            &deps(&db, &tmp, &llm),
            1,
            "aki",
            SessionKind::Talk,
            now(),
            &[],
            "add buy milk",
            &|ev| seen.borrow_mut().push(trace(ev)),
        )
        .unwrap();
        assert_eq!(
            seen.into_inner(),
            vec![
                "thinking:a task, then a lookup",
                "call:0:task_create",
                "result:0:task_create:false",
                "call:1:memory_query",
                "result:1:memory_query:true",
                "reply:done",
            ]
        );
        assert_eq!(out.steps.len(), 2);
        assert_eq!(out.steps[0].thinking.as_deref(), Some("a task, then a lookup"));
        assert_eq!(out.steps[1].thinking, None, "one round's thinking rides on its first call");
        assert_eq!(out.reasoning, "", "no round answered in text with thinking of its own");
    }

    #[test]
    fn each_rounds_thinking_rides_on_its_first_call_and_the_last_rounds_stays_in_the_outcome() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![
            ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "task_create".into(),
                    args: r#"{"title":"buy milk"}"#.into(),
                }],
            },
            ChatResponse { text: "added it".into(), tool_calls: vec![] },
        ])
        .thinking(vec!["first, a task", "that covers it"]);
        let out =
            run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, now(), &[], "milk")
                .unwrap();

        assert_eq!(out.steps.len(), 1);
        assert_eq!(out.steps[0].thinking.as_deref(), Some("first, a task"));
        assert_eq!(out.reasoning, "that covers it");
    }

    #[test]
    fn a_batchs_thinking_rides_on_its_first_sub_call() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![
            batch_call(
                "b1",
                &serde_json::json!([
                    { "tool": "task_create", "args": { "title": "buy milk" } },
                    { "tool": "task_create", "args": { "title": "call the dentist" } },
                ]),
            ),
            ChatResponse { text: "both added".into(), tool_calls: vec![] },
        ])
        .thinking(vec!["two at once"]);
        let out =
            run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, now(), &[], "two")
                .unwrap();

        assert_eq!(out.steps.len(), 2);
        assert_eq!(out.steps[0].thinking.as_deref(), Some("two at once"));
        assert_eq!(out.steps[1].thinking, None);
        assert_eq!(out.reasoning, "");
    }

    #[test]
    fn a_session_a_terminal_tool_ended_keeps_its_thinking_on_the_step() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![ChatResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "stay_quiet".into(),
                args: r#"{"reason":"nothing to add"}"#.into(),
            }],
        }])
        .thinking(vec!["nothing worth saying"]);
        let out =
            run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Trigger, now(), &[], "check")
                .unwrap();

        assert_eq!(out.steps.len(), 1);
        assert_eq!(out.steps[0].thinking.as_deref(), Some("nothing worth saying"));
        assert_eq!(out.reasoning, "");
    }

    #[test]
    fn a_failing_provider_reports_an_error_event() {
        struct Broken;
        impl crate::providers::LLMProvider for Broken {
            fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse> {
                anyhow::bail!("provider is down")
            }
        }
        let (db, tmp) = env();
        let seen = std::cell::RefCell::new(Vec::new());
        let deps = SessionDeps {
            db: &db,
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            llm: &Broken,
            embeddings: None,
            search: None,
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            token_id: None,
            thread_note: None,
            share: None,
        };
        let err = run_session_watched(
            &deps,
            1,
            "aki",
            SessionKind::Talk,
            now(),
            &[],
            "hi",
            &|ev| seen.borrow_mut().push(trace(ev)),
        )
        .unwrap_err();
        assert!(err.to_string().contains("provider is down"));
        assert_eq!(seen.into_inner(), vec!["error:internal"]);
    }

    #[test]
    fn tool_call_round_trip_creates_task_and_returns_reply() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![
            ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "task_create".into(),
                    args: r#"{"title":"buy milk"}"#.into(),
                }],
            },
            ChatResponse { text: "added buy milk!".into(), tool_calls: vec![] },
        ]);
        let out = run_session(
            &deps(&db, &tmp, &llm),
            1,
            "aki",
            SessionKind::Talk,
            now(),
            &[],
            "add buy milk",
        )
        .unwrap();
        assert_eq!(out.reply, "added buy milk!");
        assert_eq!(out.turns, 2);
        assert_eq!(out.tool_calls, 1);
        assert_eq!(out.steps.len(), 1);
        let step = &out.steps[0];
        assert_eq!(step.name, "task_create");
        assert_eq!(step.args, r#"{"title":"buy milk"}"#);
        assert!(step.result.contains("task_id"), "{}", step.result);
        assert!(!step.is_error);
        let title: String = db
            .lock()
            .unwrap()
            .query_row("SELECT title FROM tasks WHERE user_id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "buy milk");
        // the model saw persona + context and the talk tool surface
        let seen = llm.seen();
        assert!(seen[0].system.contains("you are note"));
        assert!(seen[0].system.contains("# Today's plan"));
        assert!(seen[0].tool_names.contains(&"context_edit".to_string()));
        assert!(!seen[0].tool_names.contains(&"schedule_insert".to_string()));
        // second turn carried the tool result back
        assert_eq!(seen[1].n_messages, 3);
        match &seen[1].messages[2] {
            Message::ToolResult { call_id, content, is_error } => {
                assert_eq!(call_id, "c1");
                assert!(!is_error);
                assert!(content.contains("task_id"), "{content}");
            }
            other => panic!("expected a tool result, got {other:?}"),
        }
    }

    #[test]
    fn rejected_tool_call_reaches_model_as_error_and_session_continues() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![
            ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "schedule_insert".into(),
                    args: "{}".into(),
                }],
            },
            ChatResponse { text: "sorry, couldn't".into(), tool_calls: vec![] },
        ]);
        // Talk surface: schedule_insert is forbidden — dispatch returns a typed error
        let out =
            run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, now(), &[], "hi")
                .unwrap();
        assert_eq!(out.reply, "sorry, couldn't");
        assert_eq!(out.steps.len(), 1);
        assert!(out.steps[0].is_error);
        assert!(out.steps[0].result.contains("forbidden"), "{}", out.steps[0].result);
        match &llm.seen()[1].messages[2] {
            Message::ToolResult { content, is_error, .. } => {
                assert!(is_error);
                assert!(content.contains("forbidden"), "{content}");
            }
            other => panic!("expected a tool result, got {other:?}"),
        }
    }

    #[test]
    fn history_precedes_the_opening_message() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![ChatResponse { text: "ok".into(), tool_calls: vec![] }]);
        let history = vec![
            Message::User("earlier question".into()),
            Message::Assistant { text: "earlier answer".into(), tool_calls: vec![] },
        ];
        run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, now(), &history, "follow up")
            .unwrap();
        let seen = llm.seen();
        assert_eq!(seen[0].n_messages, 3);
        assert!(matches!(&seen[0].messages[0], Message::User(t) if t == "earlier question"));
        assert!(
            matches!(&seen[0].messages[1], Message::Assistant { text, .. } if text == "earlier answer")
        );
        assert!(matches!(&seen[0].messages[2], Message::User(t) if t == "follow up"));
    }

    #[test]
    fn turn_cap_ends_the_session_and_logs() {
        let (db, tmp) = env();
        // every response asks for another tool call — the loop must stop at MAX_TURNS
        let resp = ChatResponse {
            text: "looping".into(),
            tool_calls: vec![ToolCall {
                id: "c".into(),
                name: "memory_query".into(),
                args: r#"{"query":"x"}"#.into(),
            }],
        };
        let llm = MockLLM::scripted(vec![resp; MAX_TURNS + 4]);
        let out =
            run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, now(), &[], "hi")
                .unwrap();
        assert_eq!(out.turns, MAX_TURNS);
        assert_eq!(out.steps.len(), MAX_TURNS);
        let n: i64 = db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM event_log WHERE kind = 'agent_max_turns'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn a_turn_cap_with_no_text_says_it_ran_out_of_steps() {
        let (db, tmp) = env();
        let resp = ChatResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c".into(),
                name: "memory_query".into(),
                args: r#"{"query":"x"}"#.into(),
            }],
        };
        let llm = MockLLM::scripted(vec![resp.clone(); MAX_TURNS]);
        let out =
            run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, now(), &[], "hi")
                .unwrap();
        assert_eq!(out.reply, crate::text::max_turns_reply(crate::text::Lang::En));

        let llm = MockLLM::scripted(vec![resp; MAX_TURNS]);
        let out =
            run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Nightly, now(), &[], "hi")
                .unwrap();
        assert_eq!(out.reply, "", "the nightly run keeps its own fallback");
    }

    #[test]
    fn a_trigger_session_reads_its_own_instructions_and_stops_early() {
        let (db, tmp) = env();
        let resp = ChatResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c".into(),
                name: "memory_query".into(),
                args: r#"{"query":"x"}"#.into(),
            }],
        };
        let llm = MockLLM::scripted(vec![resp; TRIGGER_MAX_TURNS + 2]);
        let out =
            run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Trigger, now(), &[], "check")
                .unwrap();
        assert_eq!(out.turns, TRIGGER_MAX_TURNS);
        let seen = llm.seen();
        assert!(seen[0].system.contains("you are note"));
        assert!(seen[0].system.contains("following up on your own plan"));
        assert!(seen[0].system.contains("# Today's plan"));
        assert!(seen[0].tool_names.contains(&"stay_quiet".to_string()));
        assert!(!seen[0].tool_names.contains(&"calendar_add".to_string()));
    }

    #[test]
    fn nightly_session_includes_planning_prompt() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![ChatResponse {
            text: "debrief".into(),
            tool_calls: vec![],
        }]);
        run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Nightly, now(), &[], "night")
            .unwrap();
        assert!(llm.seen()[0].system.contains("plan the day"));
    }

    fn import_env(
        db: &Mutex<rusqlite::Connection>,
    ) -> i64 {
        let conn = db.lock().unwrap();
        crate::tasks::create(
            &conn,
            1,
            crate::tasks::NewTask {
                title: "Biology ch.4".into(),
                ..crate::tasks::NewTask::default()
            },
            "import",
            crate::tasks::Actor::User,
        )
        .unwrap()
        .id
    }

    fn brief_call(id: &str, args: String) -> ChatResponse {
        ChatResponse {
            text: String::new(),
            tool_calls: vec![ToolCall { id: id.into(), name: "task_brief".into(), args }],
        }
    }

    fn log_rows(db: &Mutex<rusqlite::Connection>, kind: &str) -> i64 {
        db.lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind = ?1", [kind], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn a_successful_brief_ends_the_import_session_in_one_round() {
        let (db, tmp) = env();
        let id = import_env(&db);
        let llm = MockLLM::scripted(vec![brief_call(
            "c1",
            format!(r#"{{"task_id":{id},"homework":true,"description":"one line of brief"}}"#),
        )]);
        let mut d = deps(&db, &tmp, &llm);
        d.task_scope = Some(id);
        let out = run_session(&d, 1, "aki", SessionKind::Import, now(), &[], "brief it").unwrap();

        assert_eq!(llm.seen().len(), 1, "the model was asked again after a successful brief");
        assert_eq!(out.turns, 1);
        assert_eq!(out.tool_calls, 1);
        assert_eq!(out.steps.len(), 1);
        assert!(!out.steps[0].is_error);
        assert!(out.reply.contains("briefed"), "{}", out.reply);
        assert_eq!(log_rows(&db, "agent_session"), 1);
        assert_eq!(log_rows(&db, "agent_max_turns"), 0);
    }

    #[test]
    fn a_rejected_brief_buys_exactly_one_retry() {
        let (db, tmp) = env();
        let id = import_env(&db);
        let llm = MockLLM::scripted(vec![
            brief_call("c1", format!(r#"{{"task_id":{id},"homework":false}}"#)),
            brief_call(
                "c2",
                format!(r#"{{"task_id":{id},"homework":true,"description":"second try"}}"#),
            ),
            brief_call("c3", format!(r#"{{"task_id":{id},"homework":true}}"#)),
        ]);
        let mut d = deps(&db, &tmp, &llm);
        d.task_scope = Some(id);
        let out = run_session(&d, 1, "aki", SessionKind::Import, now(), &[], "brief it").unwrap();

        assert_eq!(llm.seen().len(), 2);
        assert_eq!(out.steps.len(), 2);
        assert!(out.steps[0].is_error);
        assert!(!out.steps[1].is_error);
    }

    #[test]
    fn two_rejected_briefs_end_the_import_session() {
        let (db, tmp) = env();
        let id = import_env(&db);
        let bad = brief_call("c", format!(r#"{{"task_id":{id},"homework":false}}"#));
        let llm = MockLLM::scripted(vec![bad.clone(), bad.clone(), bad]);
        let mut d = deps(&db, &tmp, &llm);
        d.task_scope = Some(id);
        let out = run_session(&d, 1, "aki", SessionKind::Import, now(), &[], "brief it").unwrap();

        assert_eq!(llm.seen().len(), IMPORT_MAX_TURNS);
        assert_eq!(out.turns, IMPORT_MAX_TURNS);
        assert!(out.steps.iter().all(|s| s.is_error));
        assert_eq!(log_rows(&db, "agent_max_turns"), 1);
    }

    fn batch_call(id: &str, calls: &serde_json::Value) -> ChatResponse {
        ChatResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: "batch".into(),
                args: serde_json::json!({ "calls": calls }).to_string(),
            }],
        }
    }

    /// The one tool result a batch hands back, parsed.
    fn batch_result(llm: &MockLLM, round: usize) -> (serde_json::Value, bool) {
        let seen = llm.seen();
        let last = seen[round].messages.last().expect("a message");
        match last {
            Message::ToolResult { content, is_error, .. } => {
                (serde_json::from_str(content).expect("a JSON tool result"), *is_error)
            }
            other => panic!("expected a tool result, got {other:?}"),
        }
    }

    #[test]
    fn a_batch_runs_every_call_in_one_round_and_a_failure_leaves_the_rest_standing() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![
            batch_call(
                "b1",
                &serde_json::json!([
                    { "tool": "task_create", "args": { "title": "buy milk" } },
                    { "tool": "task_update", "args": { "task_id": 999, "state": "done" } },
                    { "tool": "task_create", "args": { "title": "call the dentist" } },
                ]),
            ),
            ChatResponse { text: "both added".into(), tool_calls: vec![] },
        ]);
        let seen = std::cell::RefCell::new(Vec::new());
        let out = run_session_watched(
            &deps(&db, &tmp, &llm),
            1,
            "aki",
            SessionKind::Talk,
            now(),
            &[],
            "two things",
            &|ev| seen.borrow_mut().push(trace(ev)),
        )
        .unwrap();

        assert_eq!(out.turns, 2, "three calls cost one round");
        assert_eq!(out.tool_calls, 3);
        assert_eq!(out.steps.len(), 3, "the batch itself is not a step");
        assert_eq!(
            seen.into_inner(),
            vec![
                "call:0:task_create",
                "result:0:task_create:false",
                "call:1:task_update",
                "result:1:task_update:true",
                "call:2:task_create",
                "result:2:task_create:false",
                "reply:both added",
            ]
        );

        let (result, is_error) = batch_result(&llm, 1);
        assert!(!is_error, "a failing call inside a batch is not a failed batch");
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0]["tool"], "task_create");
        assert_eq!(results[0]["ok"], true);
        assert!(results[0]["result"]["task_id"].is_i64());
        assert_eq!(results[1]["ok"], false);
        assert_eq!(results[1]["error"]["kind"], "not_found");
        assert_eq!(results[2]["ok"], true);

        let titles: Vec<String> = {
            let conn = db.lock().unwrap();
            let mut stmt = conn.prepare("SELECT title FROM tasks ORDER BY id").unwrap();
            let rows = stmt.query_map([], |r| r.get(0)).unwrap();
            rows.collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(titles, vec!["buy milk", "call the dentist"]);
    }

    #[test]
    fn a_batch_reads_quoted_numbers_and_stringified_args_by_the_tool_schema() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![
            batch_call(
                "b1",
                &serde_json::json!([
                    { "tool": "task_create", "args": { "title": "buy milk", "is_now": "true" } },
                    { "tool": "task_update", "args": { "task_id": "1", "duration_min": "30", "state": "in_progress" } },
                    { "tool": "task_update", "args": "{\"task_id\": 1, \"progress\": \"40\"}" },
                    { "tool": "task_update", "args": { "task_id": "one", "state": "done" } },
                ]),
            ),
            ChatResponse { text: "done".into(), tool_calls: vec![] },
        ]);
        run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, now(), &[], "milk").unwrap();

        let (result, _) = batch_result(&llm, 1);
        let results = result["results"].as_array().unwrap();
        for (i, r) in results.iter().enumerate().take(3) {
            assert_eq!(r["ok"], true, "call {i}: {r}");
        }
        assert_eq!(results[3]["ok"], false, "a string that is no number stays a string");

        let conn = db.lock().unwrap();
        let row: (String, i64, i64) = conn
            .query_row("SELECT state, duration_min, progress FROM tasks WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(row, ("in_progress".into(), 30, 40));
    }

    #[test]
    fn a_batch_refuses_a_nested_batch_and_a_tool_that_would_end_the_session() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![
            batch_call(
                "b1",
                &serde_json::json!([
                    { "tool": "batch", "args": { "calls": [] } },
                    { "tool": "say", "args": { "text": "hello" } },
                    { "tool": "memory_query", "args": { "query": "school" } },
                ]),
            ),
            ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c2".into(),
                    name: "stay_quiet".into(),
                    args: r#"{"reason":"nothing to add"}"#.into(),
                }],
            },
        ]);
        let out =
            run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Trigger, now(), &[], "check")
                .unwrap();

        let (result, is_error) = batch_result(&llm, 1);
        assert!(!is_error);
        let results = result["results"].as_array().unwrap();
        assert_eq!(results[0]["ok"], false);
        assert!(
            results[0]["error"]["message"].as_str().unwrap().contains("batch"),
            "{}",
            results[0]["error"]
        );
        assert_eq!(results[1]["ok"], false);
        assert!(
            results[1]["error"]["message"].as_str().unwrap().contains("ends the session"),
            "{}",
            results[1]["error"]
        );
        assert_eq!(results[2]["ok"], true, "{}", results[2]);

        assert_eq!(out.tool_calls, 2, "a refused call never runs");
        let names: Vec<&str> = out.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["memory_query", "stay_quiet"]);
        assert!(out.reply.contains("quiet"), "a batch ended the session: {}", out.reply);
    }

    #[test]
    fn a_batch_over_the_cap_or_empty_runs_nothing() {
        let (db, tmp) = env();
        let one = serde_json::json!({ "tool": "task_create", "args": { "title": "x" } });
        for calls in [
            serde_json::Value::Array(vec![one; tools::MAX_BATCH_CALLS + 1]),
            serde_json::json!([]),
        ] {
            let llm = MockLLM::scripted(vec![
                batch_call("b1", &calls),
                ChatResponse { text: "fine".into(), tool_calls: vec![] },
            ]);
            let out =
                run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, now(), &[], "hi")
                    .unwrap();
            let (result, is_error) = batch_result(&llm, 1);
            assert!(is_error, "malformed batch arguments are the batch call's own error");
            assert_eq!(result["kind"], "rejected");
            assert_eq!(out.tool_calls, 0);
            assert!(out.steps.is_empty());
        }
        let n: i64 =
            db.lock().unwrap().query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn the_sessions_that_answer_one_call_are_never_offered_a_batch() {
        for kind in [SessionKind::Import, SessionKind::Inbox, SessionKind::Summarize] {
            assert!(!tools::registry(kind).contains(&"batch"), "{kind:?} is offered a batch");
        }
        for kind in [
            SessionKind::Talk,
            SessionKind::Checkin,
            SessionKind::Nightly,
            SessionKind::Harvest,
            SessionKind::Review,
            SessionKind::Trigger,
        ] {
            assert!(tools::registry(kind).contains(&"batch"), "{kind:?} has no batch");
        }
    }

    struct FakeSearch(Vec<crate::search::SearchHit>);

    impl crate::search::SearchProvider for FakeSearch {
        fn search(&self, _query: &str) -> Result<Vec<crate::search::SearchHit>> {
            Ok(self.0.clone())
        }
    }

    fn hits(n: usize) -> FakeSearch {
        FakeSearch(
            (1..=n)
                .map(|i| crate::search::SearchHit {
                    title: format!("result {i}"),
                    url: format!("http://example.test/{i}"),
                    snippet: format!("what page {i} says"),
                })
                .collect(),
        )
    }

    fn searching<'a>(
        db: &'a Mutex<rusqlite::Connection>,
        tmp: &'a tempfile::TempDir,
        llm: &'a dyn LLMProvider,
        search: &'a dyn crate::search::SearchProvider,
    ) -> SessionDeps<'a> {
        SessionDeps { search: Some(search), ..deps(db, tmp, llm) }
    }

    fn search_call(args: &str) -> ChatResponse {
        ChatResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "s1".into(),
                name: "web_search".into(),
                args: args.into(),
            }],
        }
    }

    fn search_log(db: &Mutex<rusqlite::Connection>) -> String {
        db.lock()
            .unwrap()
            .query_row("SELECT detail FROM event_log WHERE kind = 'web_search'", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn a_web_search_hands_back_a_summary_written_from_the_hits() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![
            search_call(r#"{"query":"kyoto rain","question":"is it raining in kyoto?"}"#),
            ChatResponse { text: "Rain until Thursday (1,2).".into(), tool_calls: vec![] },
            ChatResponse { text: "take an umbrella".into(), tool_calls: vec![] },
        ]);
        let search = hits(2);
        let out = run_session(
            &searching(&db, &tmp, &llm, &search),
            1,
            "aki",
            SessionKind::Talk,
            now(),
            &[],
            "is it raining in kyoto?",
        )
        .unwrap();

        assert_eq!(out.reply, "take an umbrella");
        assert_eq!(out.steps.len(), 1);
        assert_eq!(out.steps[0].name, "web_search");
        assert!(!out.steps[0].is_error);
        let result: serde_json::Value = serde_json::from_str(&out.steps[0].result).unwrap();
        assert_eq!(result["summary"], "Rain until Thursday (1,2).");
        assert_eq!(result["sources"].as_array().unwrap().len(), 2);
        assert_eq!(result["sources"][0]["n"], 1);
        assert_eq!(result["sources"][1]["url"], "http://example.test/2");
        assert!(result["sources"][0]["snippet"].is_null(), "a summary carries no page text");

        // the summarizer is one tool-less call of its own, between the rounds
        let seen = llm.seen();
        assert_eq!(seen.len(), 3);
        assert!(seen[1].tool_names.is_empty(), "the summarizer was handed tools");
        assert!(seen[1].system.contains("answer from the hits alone"));
        assert_eq!(seen[1].n_messages, 1);
        match &seen[1].messages[0] {
            Message::User(text) => {
                assert!(text.contains("kyoto rain"), "{text}");
                assert!(text.contains("is it raining in kyoto?"), "{text}");
                assert!(text.contains("1. result 1") && text.contains("what page 2 says"), "{text}");
            }
            other => panic!("expected the hits as a user message, got {other:?}"),
        }
        assert_eq!(search_log(&db), "hits=2 summary=ok");
    }

    #[test]
    fn a_summarizer_that_cannot_answer_falls_back_to_the_hits_themselves() {
        /// Answers the session from the script and fails the tool-less
        /// summarizer call.
        struct NoSummary(MockLLM);
        impl LLMProvider for NoSummary {
            fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
                anyhow::ensure!(!req.tools.is_empty(), "the summarizer is down");
                self.0.chat(req)
            }
        }
        let (db, tmp) = env();
        let llm = NoSummary(MockLLM::scripted(vec![
            search_call(r#"{"query":"kyoto rain"}"#),
            ChatResponse { text: "here is what I found".into(), tool_calls: vec![] },
        ]));
        let search = hits(7);
        let out = run_session(
            &searching(&db, &tmp, &llm, &search),
            1,
            "aki",
            SessionKind::Talk,
            now(),
            &[],
            "kyoto rain",
        )
        .unwrap();

        assert!(!out.steps[0].is_error, "a silent summarizer is not a failed search");
        let result: serde_json::Value = serde_json::from_str(&out.steps[0].result).unwrap();
        assert!(result["summary"].is_null());
        let raw = result["hits"].as_array().unwrap();
        assert_eq!(raw.len(), 5, "the fallback is the top five, text and all");
        assert_eq!(raw[0]["snippet"], "what page 1 says");
        assert_eq!(search_log(&db), "hits=7 summary=fallback");
    }

    #[test]
    fn a_search_with_no_hits_costs_nothing_more() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![
            search_call(r#"{"query":"kyoto rain"}"#),
            ChatResponse { text: "nothing came back".into(), tool_calls: vec![] },
        ]);
        let search = hits(0);
        let out = run_session(
            &searching(&db, &tmp, &llm, &search),
            1,
            "aki",
            SessionKind::Talk,
            now(),
            &[],
            "kyoto rain",
        )
        .unwrap();

        assert_eq!(out.reply, "nothing came back");
        let result: serde_json::Value = serde_json::from_str(&out.steps[0].result).unwrap();
        assert_eq!(result["summary"], "no results");
        assert_eq!(result["sources"].as_array().unwrap().len(), 0);
        assert_eq!(llm.seen().len(), 2, "the summarizer was called with nothing to read");
        assert_eq!(search_log(&db), "hits=0 summary=none");
    }

    #[test]
    fn the_search_tool_is_offered_and_answered_only_where_search_is_configured() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![
            search_call(r#"{"query":"kyoto rain"}"#),
            ChatResponse { text: "I can't look that up".into(), tool_calls: vec![] },
        ]);
        let out =
            run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, now(), &[], "hi")
                .unwrap();
        assert!(!llm.seen()[0].tool_names.contains(&"web_search".to_string()));
        assert!(out.steps[0].is_error);
        assert!(out.steps[0].result.contains("not configured"), "{}", out.steps[0].result);
        let n: i64 = db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind = 'web_search'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);

        let llm = MockLLM::scripted(vec![ChatResponse { text: "hi".into(), tool_calls: vec![] }]);
        let search = hits(1);
        run_session(&searching(&db, &tmp, &llm, &search), 1, "aki", SessionKind::Talk, now(), &[], "hi")
            .unwrap();
        assert!(llm.seen()[0].tool_names.contains(&"web_search".to_string()));

        let llm = MockLLM::scripted(vec![ChatResponse { text: "hi".into(), tool_calls: vec![] }]);
        run_session(
            &searching(&db, &tmp, &llm, &search),
            1,
            "aki",
            SessionKind::Trigger,
            now(),
            &[],
            "check",
        )
        .unwrap();
        assert!(
            !llm.seen()[0].tool_names.contains(&"web_search".to_string()),
            "a trigger session searches the user's own world, not the web"
        );
    }

    /// The one trace the session left, as (outcome, turns, `tool_calls`, error, detail).
    fn trace_row(
        db: &Mutex<rusqlite::Connection>,
    ) -> (String, i64, i64, Option<String>, serde_json::Value) {
        let conn = db.lock().unwrap();
        let n: i64 =
            conn.query_row("SELECT COUNT(*) FROM agent_traces", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "one session leaves one trace");
        conn.query_row(
            "SELECT outcome, turns, tool_calls, error, detail FROM agent_traces",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    serde_json::from_str(&r.get::<_, String>(4)?).unwrap(),
                ))
            },
        )
        .unwrap()
    }

    fn rounds(detail: &serde_json::Value) -> &Vec<serde_json::Value> {
        detail["rounds"].as_array().unwrap()
    }

    #[test]
    fn a_finished_session_leaves_one_trace_of_its_rounds_and_calls() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![
            ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "task_create".into(),
                    args: r#"{"title":"buy milk"}"#.into(),
                }],
            },
            ChatResponse { text: "added buy milk!".into(), tool_calls: vec![] },
        ]);
        let out =
            run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, now(), &[], "add milk")
                .unwrap();

        let (outcome, turns, tool_calls, error, detail) = trace_row(&db);
        assert_eq!(outcome, "ok");
        assert_eq!(turns, out.turns as i64);
        assert_eq!(tool_calls, out.tool_calls as i64);
        assert!(error.is_none());
        assert_eq!(detail["opening"], "add milk");
        assert_eq!(detail["reply"], "added buy milk!");
        assert_eq!(rounds(&detail).len(), turns as usize, "every answered round is a turn");
        let calls: Vec<&serde_json::Value> =
            rounds(&detail).iter().flat_map(|r| r["calls"].as_array().unwrap()).collect();
        assert_eq!(calls.len(), out.steps.len());
        assert_eq!(calls[0]["name"], out.steps[0].name);
        assert_eq!(calls[0]["args"], out.steps[0].args);
        assert_eq!(calls[0]["result"], out.steps[0].result);
        assert_eq!(calls[0]["is_error"], false);
        assert!(calls[0]["error_kind"].is_null());
        assert!(rounds(&detail)[1]["calls"].as_array().unwrap().is_empty());
        let kind: String =
            db.lock().unwrap().query_row("SELECT kind FROM agent_traces", [], |r| r.get(0)).unwrap();
        assert_eq!(kind, "Talk");
    }

    #[test]
    fn a_session_that_runs_out_of_turns_says_so_in_its_trace() {
        let (db, tmp) = env();
        let resp = ChatResponse {
            text: "looping".into(),
            tool_calls: vec![ToolCall {
                id: "c".into(),
                name: "memory_query".into(),
                args: r#"{"query":"x"}"#.into(),
            }],
        };
        let llm = MockLLM::scripted(vec![resp; MAX_TURNS + 2]);
        run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, now(), &[], "hi").unwrap();

        let (outcome, turns, tool_calls, _, detail) = trace_row(&db);
        assert_eq!(outcome, "max_turns");
        assert_eq!(turns, MAX_TURNS as i64);
        assert_eq!(tool_calls, MAX_TURNS as i64);
        assert_eq!(rounds(&detail).len(), MAX_TURNS);
        assert_eq!(detail["reply"], "looping");
    }

    /// Answers `rounds` calls from the script, then fails the way a provider
    /// that has gone away does.
    struct FailsAfter {
        inner: MockLLM,
        rounds: usize,
        seen: std::sync::atomic::AtomicUsize,
    }

    impl LLMProvider for FailsAfter {
        fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
            if self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= self.rounds {
                anyhow::bail!("timed out reading response");
            }
            self.inner.chat(req)
        }
    }

    #[test]
    fn a_provider_that_dies_mid_session_leaves_the_rounds_it_finished() {
        let (db, tmp) = env();
        let llm = FailsAfter {
            inner: MockLLM::scripted(vec![ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "task_create".into(),
                    args: r#"{"title":"buy milk"}"#.into(),
                }],
            }]),
            rounds: 1,
            seen: std::sync::atomic::AtomicUsize::new(0),
        };
        let err =
            run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Nightly, now(), &[], "night")
                .unwrap_err();
        assert!(err.to_string().contains("timed out"));

        let (outcome, turns, tool_calls, error, detail) = trace_row(&db);
        assert_eq!(outcome, "error");
        assert_eq!(turns, 1, "only the answered round counts");
        assert_eq!(tool_calls, 1);
        assert!(error.unwrap().contains("timed out reading response"));
        assert_eq!(rounds(&detail).len(), 2);
        assert!(rounds(&detail)[0]["error"].is_null());
        assert_eq!(rounds(&detail)[0]["calls"][0]["name"], "task_create");
        assert!(rounds(&detail)[1]["error"].as_str().unwrap().contains("timed out"));
        assert!(rounds(&detail)[1]["calls"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_session_that_never_reaches_the_provider_is_traced_all_the_same() {
        let (db, tmp) = env();
        std::fs::remove_file(tmp.path().join("defaults/prompts/persona.md")).unwrap();
        let llm = MockLLM::scripted(vec![ChatResponse { text: "hi".into(), tool_calls: vec![] }]);
        assert!(run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, now(), &[], "hi")
            .is_err());

        let (outcome, turns, tool_calls, error, detail) = trace_row(&db);
        assert_eq!((outcome.as_str(), turns, tool_calls), ("error", 0, 0));
        assert!(error.is_some());
        assert!(rounds(&detail).is_empty());
    }

    #[test]
    fn a_batch_lands_as_the_calls_of_the_round_that_ran_it() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![
            batch_call(
                "b1",
                &serde_json::json!([
                    { "tool": "task_create", "args": { "title": "buy milk" } },
                    { "tool": "task_update", "args": { "task_id": 999, "state": "done" } },
                ]),
            ),
            ChatResponse { text: "one added".into(), tool_calls: vec![] },
        ]);
        run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, now(), &[], "two things")
            .unwrap();

        let (_, turns, tool_calls, _, detail) = trace_row(&db);
        assert_eq!((turns, tool_calls), (2, 2));
        let calls = rounds(&detail)[0]["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 2, "the batch envelope itself is not a call");
        assert_eq!(calls[0]["name"], "task_create");
        assert_eq!(calls[1]["name"], "task_update");
        assert_eq!(calls[1]["is_error"], true);
        assert_eq!(calls[1]["error_kind"], "not_found");
    }

    #[test]
    fn a_runaway_argument_is_clipped_before_it_is_stored() {
        let (db, tmp) = env();
        let title = "x".repeat(crate::trace::MAX_FIELD_BYTES * 2);
        let llm = MockLLM::scripted(vec![
            ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "task_create".into(),
                    args: serde_json::json!({ "title": title }).to_string(),
                }],
            },
            ChatResponse { text: "done".into(), tool_calls: vec![] },
        ]);
        run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, now(), &[], "big").unwrap();

        let (_, _, _, _, detail) = trace_row(&db);
        let args = rounds(&detail)[0]["calls"][0]["args"].as_str().unwrap();
        assert!(args.ends_with('…'));
        assert!(args.len() <= crate::trace::MAX_FIELD_BYTES + '…'.len_utf8());
    }

    #[test]
    fn a_trace_that_cannot_be_written_leaves_the_session_alone() {
        let (db, tmp) = env();
        db.lock().unwrap().execute("DROP TABLE agent_traces", []).unwrap();
        let llm = MockLLM::scripted(vec![ChatResponse { text: "hi".into(), tool_calls: vec![] }]);
        let out =
            run_session(&deps(&db, &tmp, &llm), 1, "aki", SessionKind::Talk, now(), &[], "hello")
                .unwrap();
        assert_eq!(out.reply, "hi");
    }

    #[test]
    fn llm_failure_propagates() {
        struct Failing;
        impl crate::providers::LLMProvider for Failing {
            fn chat(
                &self,
                _: &crate::providers::ChatRequest,
            ) -> anyhow::Result<crate::providers::ChatResponse> {
                anyhow::bail!("provider down")
            }
        }
        let (db, tmp) = env();
        let llm = Failing;
        let d = SessionDeps {
            db: &db,
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            llm: &llm,
            embeddings: None,
            search: None,
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            token_id: None,
            thread_note: None,
            share: None,
        };
        assert!(run_session(&d, 1, "aki", SessionKind::Talk, now(), &[], "hi").is_err());
    }

    #[test]
    fn a_share_session_gets_the_share_prompt_the_opener_and_no_context_block() {
        let (db, tmp) = env();
        std::fs::write(tmp.path().join("defaults/prompts/share.md"), "answer for {owner}").unwrap();
        let standing = crate::context::standing_path(tmp.path(), "aki");
        std::fs::create_dir_all(standing.parent().unwrap()).unwrap();
        std::fs::write(standing, "SECRET STANDING LINE").unwrap();
        {
            let conn = db.lock().unwrap();
            crate::tasks::create(&conn, 1, crate::tasks::NewTask { title: "lab report".into(), category: Some("school".into()), ..Default::default() }, "manual", crate::tasks::Actor::User).unwrap();
        }
        let llm = MockLLM::scripted(vec![ChatResponse { text: "Aki has a lab report.".into(), tool_calls: vec![] }]);
        let share = ShareSession { id: 1, thread_id: 1, brief: "be warm".into(), scope: crate::shares::ShareScope::default(), visitor_lang: crate::text::Lang::En };
        let deps = SessionDeps { db: &db, config_dir: tmp.path(), data_dir: tmp.path(), llm: &llm, embeddings: None, search: None, task_scope: None, inbox_source: None, memory_source: None, token_id: None, thread_note: Some("SECRET NOTE".into()), share: Some(share) };
        let out = run_session(&deps, 1, "aki", SessionKind::Share, jiff::Timestamp::now(), &[], "what does aki have?").unwrap();
        assert_eq!(out.reply, "Aki has a lab report.");
        let seen = llm.seen();
        assert!(seen[0].system.starts_with("answer for X"), "{}", seen[0].system);
        assert!(seen[0].system.contains("# From X\n\nbe warm"));
        assert!(seen[0].system.contains("# What is shared"));
        assert!(seen[0].system.contains("lab report"));
        assert!(!seen[0].system.contains("SECRET STANDING LINE"));
        assert!(!seen[0].system.contains("# Standing context"));
        assert!(!seen[0].system.contains("SECRET NOTE"));
        assert!(!seen[0].tool_names.iter().any(|n| n.starts_with("memory_") || n == "task_create" || n == "share_note"));
        let conn = db.lock().unwrap();
        let (kind, detail): (String, String) = conn.query_row("SELECT kind, detail FROM event_log WHERE kind LIKE 'share%'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(kind, "share_session");
        assert!(detail.contains("share=1"), "{detail}");
        assert_eq!(crate::log::agent_sessions_since(&conn, 1, jiff::Timestamp::now() - jiff::Span::new().hours(1)).unwrap(), 0);
    }

    #[test]
    fn a_tool_run_once_per_key_lands_its_change_once() {
        let (db, tmp) = env();
        db.lock()
            .unwrap()
            .execute(
                "INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at)
                 VALUES ('c1', 1, 'outbound', 'answered', 'x', 'x')",
                [],
            )
            .unwrap();
        let llm = MockLLM::scripted(vec![]);
        let d = deps(&db, &tmp, &llm);
        let run = |key| {
            run_tool(&d, 1, "aki", SessionKind::Call, "task_create", r#"{"title":"essay"}"#, Some(("c1", key)))
        };
        let first = run("1:0");
        assert!(!first.1, "{}", first.0);
        assert_eq!(run("1:0"), first);
        let tasks: i64 =
            db.lock().unwrap().query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(tasks, 1);
    }
}
