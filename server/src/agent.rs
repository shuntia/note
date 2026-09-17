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
    /// Confines the session's task tools to one task and its steps.
    pub task_scope: Option<i64>,
    /// The API token the caller presented, so the session's log row says which
    /// credential spent it.
    pub token_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct SessionStep {
    pub name: String,
    pub args: String,
    pub result: String,
    pub is_error: bool,
}

#[derive(Debug)]
pub struct SessionOutcome {
    pub reply: String,
    pub turns: usize,
    pub tool_calls: usize,
    pub steps: Vec<SessionStep>,
}

/// Runs one agent session: chat, dispatch tool calls, feed results back, until
/// the model answers in text or MAX_TURNS is hit. The DB lock is held only for
/// assembly, individual dispatches, and log writes — never across a provider call.
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
    // An import session briefs one task on an importer's behalf: it gets its own
    // instructions and none of the user's standing context.
    let mut system = if kind == SessionKind::Import {
        crate::prompts::load(deps.config_dir, username, "import")?
    } else {
        crate::prompts::load(deps.config_dir, username, "persona")?
    };
    if kind == SessionKind::Nightly {
        system.push_str("\n\n");
        system.push_str(&crate::prompts::load(deps.config_dir, username, "planning")?);
    }
    if kind != SessionKind::Import {
        let conn = deps.db.lock().unwrap();
        let context = crate::context::assemble(&conn, deps.config_dir, user_id, username, now)?;
        system.push_str("\n\n");
        system.push_str(&context);
    }

    let schemas = tools::schemas(kind);
    let mut messages = history.to_vec();
    messages.push(Message::User(opening.to_string()));
    let mut turns = 0;
    let mut tool_calls = 0;
    let mut steps = Vec::new();
    let mut last_text = String::new();

    while turns < MAX_TURNS {
        let resp = deps
            .llm
            .chat(&ChatRequest { system: &system, messages: &messages, tools: &schemas })?;
        turns += 1;
        last_text = resp.text;
        if resp.tool_calls.is_empty() {
            finish(deps, user_id, kind, turns, tool_calls, "agent_session")?;
            return Ok(SessionOutcome { reply: last_text, turns, tool_calls, steps });
        }
        let calls = resp.tool_calls.clone();
        messages.push(Message::Assistant { text: last_text.clone(), tool_calls: resp.tool_calls });
        for call in calls {
            tool_calls += 1;
            let vectors = tools::prepare(deps.embeddings, &call.name, &call.args);
            let (content, is_error) = {
                let conn = deps.db.lock().unwrap();
                let ctx = ToolCtx {
                    config_dir: deps.config_dir,
                    data_dir: deps.data_dir,
                    user_id,
                    username,
                    vectors,
                    task_scope: deps.task_scope,
                };
                match tools::dispatch(&conn, &ctx, kind, &call.name, &call.args) {
                    Ok(v) => (v.to_string(), false),
                    Err(e) => (
                        serde_json::to_string(&e)
                            .unwrap_or_else(|_| r#"{"kind":"internal"}"#.into()),
                        true,
                    ),
                }
            };
            steps.push(SessionStep {
                name: call.name,
                args: call.args,
                result: content.clone(),
                is_error,
            });
            messages.push(Message::ToolResult { call_id: call.id, content, is_error });
        }
    }
    finish(deps, user_id, kind, turns, tool_calls, "agent_max_turns")?;
    Ok(SessionOutcome { reply: last_text, turns, tool_calls, steps })
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
        detail.push_str(&format!(" token={id}"));
    }
    let conn = deps.db.lock().unwrap();
    crate::log::record(&conn, Some(user_id), log_kind, &detail)
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
        (Mutex::new(conn), tmp)
    }

    fn deps<'a>(
        db: &'a Mutex<rusqlite::Connection>,
        tmp: &'a tempfile::TempDir,
        llm: &'a MockLLM,
    ) -> SessionDeps<'a> {
        SessionDeps { db, config_dir: tmp.path(), data_dir: tmp.path(), llm, embeddings: None, task_scope: None, token_id: None }
    }

    fn now() -> jiff::Timestamp {
        "2026-08-31T04:00:00Z".parse().unwrap()
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
            task_scope: None,
            token_id: None,
        };
        assert!(run_session(&d, 1, "aki", SessionKind::Talk, now(), &[], "hi").is_err());
    }
}
