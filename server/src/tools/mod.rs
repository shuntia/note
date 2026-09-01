pub mod context_ops;
pub mod memory_ops;
pub mod outreach_ops;
pub mod schedule_ops;
pub mod task_ops;

use rusqlite::Connection;
use serde::Serialize;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SessionKind {
    Nightly,
    Checkin,
    Talk,
}

/// A tool failure returned to the model as a value; `kind` is machine-matchable,
/// `message` is for the model to read.
#[derive(Debug, Serialize)]
pub struct ToolError {
    pub kind: &'static str,
    pub message: String,
}

impl ToolError {
    fn of(kind: &'static str) -> impl Fn(String) -> Self {
        move |message| Self { kind, message }
    }
    pub fn rejected(m: impl Into<String>) -> Self {
        Self::of("rejected")(m.into())
    }
    pub fn invalid_args(m: impl Into<String>) -> Self {
        Self::of("invalid_args")(m.into())
    }
    pub fn not_found(m: impl Into<String>) -> Self {
        Self::of("not_found")(m.into())
    }
    pub fn forbidden(m: impl Into<String>) -> Self {
        Self::of("forbidden")(m.into())
    }
    pub fn unknown_tool(m: impl Into<String>) -> Self {
        Self::of("unknown_tool")(m.into())
    }
    pub fn internal(m: impl Into<String>) -> Self {
        Self::of("internal")(m.into())
    }
}

pub struct ToolCtx<'a> {
    pub config_dir: &'a Path,
    pub data_dir: &'a Path,
    pub user_id: i64,
    pub username: &'a str,
    pub vectors: PreparedVectors,
}

pub const MAX_ARGS_BYTES: usize = 64 * 1024;

#[derive(Default, Debug)]
pub struct PreparedVectors {
    pub content: Option<Vec<f32>>,
    pub query: Option<Vec<f32>>,
    pub error: Option<String>,
}

/// Embeds any text this tool call will need, so `dispatch` itself never does
/// network I/O. Malformed args embed nothing — dispatch will reject them with
/// a typed error anyway.
pub fn prepare(
    emb: Option<&dyn crate::providers::EmbeddingsProvider>,
    name: &str,
    raw_args: &str,
) -> PreparedVectors {
    let Some(emb) = emb else { return PreparedVectors::default() };
    if raw_args.len() > MAX_ARGS_BYTES {
        return PreparedVectors::default();
    }
    let mut out = PreparedVectors::default();
    match name {
        "memory_query" => {
            #[derive(serde::Deserialize, Default)]
            #[serde(default)]
            struct Q {
                query: String,
            }
            if let Ok(q) = serde_json::from_str::<Q>(raw_args) {
                if !q.query.is_empty() {
                    match emb.embed(&[&q.query]) {
                        Ok(vs) if !vs.is_empty() => out.query = Some(vs[0].clone()),
                        Ok(_) => {}
                        Err(e) => out.error = Some(e.to_string()),
                    }
                }
            }
        }
        "memory_write" => {
            #[derive(serde::Deserialize, Default)]
            #[serde(default)]
            struct W {
                summary: String,
                body: String,
            }
            if let Ok(w) = serde_json::from_str::<W>(raw_args) {
                if !w.summary.is_empty() || !w.body.is_empty() {
                    let text = crate::memory::embed_text(&w.summary, &w.body);
                    match emb.embed(&[&text]) {
                        Ok(vs) if !vs.is_empty() => out.content = Some(vs[0].clone()),
                        Ok(_) => {}
                        Err(e) => out.error = Some(e.to_string()),
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// Ceiling for every free-text field a tool accepts, shared so the surfaces
/// cannot drift apart.
pub(crate) const MAX_TEXT_BYTES: usize = 16 * 1024;

pub(crate) fn check_text(field: &str, value: &str) -> Result<(), ToolError> {
    if value.len() > MAX_TEXT_BYTES {
        return Err(ToolError::rejected(format!(
            "{field} must be at most {MAX_TEXT_BYTES} bytes"
        )));
    }
    Ok(())
}

const CHECKIN: &[&str] = &[
    "memory_query",
    "memory_read",
    "memory_write",
    "task_create",
    "task_update",
    "task_split",
    "schedule_slide",
    "schedule_snooze",
    "schedule_drop",
    "schedule_reshape",
];
const TALK: &[&str] = &[
    "memory_query",
    "memory_read",
    "memory_write",
    "task_create",
    "task_update",
    "task_split",
    "schedule_slide",
    "schedule_snooze",
    "schedule_drop",
    "schedule_reshape",
    "context_edit",
];
const NIGHTLY: &[&str] = &[
    "memory_query",
    "memory_read",
    "memory_write",
    "task_create",
    "task_update",
    "task_split",
    "schedule_slide",
    "schedule_snooze",
    "schedule_drop",
    "schedule_reshape",
    "context_edit",
    "schedule_insert",
    "notify_send",
];

pub fn registry(kind: SessionKind) -> &'static [&'static str] {
    match kind {
        SessionKind::Nightly => NIGHTLY,
        SessionKind::Checkin => CHECKIN,
        SessionKind::Talk => TALK,
    }
}

fn schema<T: schemars::JsonSchema>() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("schema serializes")
}

fn describe(name: &str) -> (&'static str, serde_json::Value) {
    match name {
        "task_create" => (
            "Create a new task for the current user. Set is_now to put it straight in Now, \
             the user's short list of at most 3 — a fourth pushes the newest one back to Later.",
            schema::<task_ops::CreateArgs>(),
        ),
        "task_update" => (
            "Update a task's title, description, state, notes, duration (whole 5-minute blocks), \
             or whether it sits in Now — the short list of at most 3, where a fourth pushes the \
             newest one back to Later. Steps are never in Now.",
            schema::<task_ops::UpdateArgs>(),
        ),
        "task_split" => (
            "Split a task into 2-5 short steps, each with a duration in whole 5-minute blocks. \
             Only for a task that has no steps yet.",
            schema::<task_ops::SplitArgs>(),
        ),
        "memory_query" => (
            "Search the user's long-term memory; returns ids and summaries.",
            schema::<memory_ops::QueryArgs>(),
        ),
        "memory_read" => (
            "Read one memory in full by id.",
            schema::<memory_ops::ReadArgs>(),
        ),
        "memory_write" => (
            "Add, update, or supersede a memory. Superseding archives the old fact.",
            schema::<memory_ops::WriteArgs>(),
        ),
        "context_edit" => (
            "Edit the standing context document: replace a unique snippet or append a line.",
            schema::<context_ops::EditArgs>(),
        ),
        "schedule_slide" => (
            "Slide a plan event by N minutes (negative = earlier), within its slide window.",
            schema::<schedule_ops::SlideArgs>(),
        ),
        "schedule_snooze" => (
            "Postpone a plan event's delivery by N minutes without changing the schedule intent.",
            schema::<schedule_ops::SnoozeArgs>(),
        ),
        "schedule_drop" => (
            "Drop a droppable plan event for today. If it is moving rather than going away, \
             add the replacement first and name its id, so the user can see where it went.",
            schema::<schedule_ops::DropArgs>(),
        ),
        "schedule_reshape" => (
            "Move or resize a block of time on the day's plan. Blocks are the only entries \
             with a start and an end, and they never notify the user.",
            schema::<schedule_ops::ReshapeArgs>(),
        ),
        "schedule_insert" => (
            "Insert a new event into an existing day plan.",
            schema::<schedule_ops::InsertArgs>(),
        ),
        "notify_send" => (
            "Send the user a push nudge with this text (delivered within a minute).",
            schema::<outreach_ops::SendArgs>(),
        ),
        _ => unreachable!("describe covers every registered tool"),
    }
}

pub fn schemas(kind: SessionKind) -> Vec<serde_json::Value> {
    registry(kind)
        .iter()
        .map(|name| {
            let (description, input_schema) = describe(name);
            serde_json::json!({ "name": name, "description": description, "input_schema": input_schema })
        })
        .collect()
}

fn parse<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, ToolError> {
    serde_json::from_str(raw).map_err(|e| ToolError::invalid_args(e.to_string()))
}

/// Sole entrypoint for model-originated calls: enforces the payload cap and
/// the per-session registry, then runs the handler inside one transaction so
/// a failed call leaves no trace.
pub fn dispatch(
    conn: &Connection,
    ctx: &ToolCtx,
    kind: SessionKind,
    name: &str,
    raw_args: &str,
) -> Result<serde_json::Value, ToolError> {
    if raw_args.len() > MAX_ARGS_BYTES {
        return Err(ToolError::rejected(format!(
            "arguments exceed {MAX_ARGS_BYTES} bytes"
        )));
    }
    if !registry(kind).contains(&name) {
        return Err(if NIGHTLY.contains(&name) {
            ToolError::forbidden(format!("tool {name} is not available in this session type"))
        } else {
            ToolError::unknown_tool(format!("no such tool: {name}"))
        });
    }
    let tx = conn.unchecked_transaction().map_err(|e| ToolError::internal(e.to_string()))?;
    let out = run(&tx, ctx, name, raw_args)?;
    tx.commit().map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(out)
}

fn run(
    conn: &Connection,
    ctx: &ToolCtx,
    name: &str,
    raw: &str,
) -> Result<serde_json::Value, ToolError> {
    match name {
        "task_create" => task_ops::create(conn, ctx, parse(raw)?),
        "task_update" => task_ops::update(conn, ctx, parse(raw)?),
        "task_split" => task_ops::split(conn, ctx, parse(raw)?),
        "memory_query" => memory_ops::query(conn, ctx, parse(raw)?),
        "memory_read" => memory_ops::read(conn, ctx, parse(raw)?),
        "memory_write" => memory_ops::write(conn, ctx, parse(raw)?),
        "context_edit" => context_ops::edit(conn, ctx, parse(raw)?),
        "schedule_slide" => schedule_ops::slide(conn, ctx, parse(raw)?),
        "schedule_snooze" => schedule_ops::snooze(conn, ctx, parse(raw)?),
        "schedule_drop" => schedule_ops::drop_event(conn, ctx, parse(raw)?),
        "schedule_reshape" => schedule_ops::reshape(conn, ctx, parse(raw)?),
        "schedule_insert" => schedule_ops::insert(conn, ctx, parse(raw)?),
        "notify_send" => outreach_ops::send(conn, ctx, parse(raw)?),
        _ => unreachable!("registry guarantees a known name"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        // argon2 is slow and irrelevant here; insert the user row directly
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir) -> ToolCtx<'a> {
        ToolCtx { config_dir: tmp.path(), data_dir: tmp.path(), user_id: 1, username: "aki", vectors: PreparedVectors::default() }
    }

    #[test]
    fn task_create_and_update_roundtrip() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"call dentist","description":"about the molar"}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{id},"state":"done"}}"#)).unwrap();
        assert_eq!(out["state"], "done");
    }

    #[test]
    fn agent_sets_durations_in_five_minute_steps() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"email landlord","duration_min":20}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        let (dur, src): (i64, String) = conn
            .query_row("SELECT duration_min, duration_source FROM tasks WHERE id = ?1", [id],
                |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((dur, src.as_str()), (20, "agent"));

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{id},"duration_min":23}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");

        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{id},"duration_min":25}}"#)).unwrap();
        let dur: i64 = conn
            .query_row("SELECT duration_min FROM tasks WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(dur, 25);
    }

    #[test]
    fn agent_moves_tasks_in_and_out_of_now() {
        let (conn, tmp) = env();
        let flag = |id: i64| -> i64 {
            conn.query_row("SELECT is_now FROM tasks WHERE id = ?1", [id], |r| r.get(0)).unwrap()
        };
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"email landlord","is_now":true}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        assert_eq!(flag(id), 1);

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{id},"is_now":false}}"#)).unwrap();
        assert_eq!(out["is_now"], false);
        assert_eq!(flag(id), 0);

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{id},"is_now":true}}"#)).unwrap();
        assert_eq!(out["is_now"], true);
        assert_eq!(out["demoted_from_now"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_fourth_agent_write_pushes_the_newest_task_out_of_now() {
        let (conn, tmp) = env();
        let mut ids = Vec::new();
        for title in ["a", "b", "c", "d"] {
            let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
                &format!(r#"{{"title":"{title}","is_now":true}}"#)).unwrap();
            ids.push(out["task_id"].as_i64().unwrap());
        }
        let live: Vec<i64> = {
            let mut stmt = conn.prepare(
                "SELECT id FROM tasks WHERE is_now = 1 AND state IN ('open','in_progress') ORDER BY id",
            ).unwrap();
            let rows = stmt.query_map([], |r| r.get(0)).unwrap();
            rows.collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(live, vec![ids[0], ids[1], ids[3]], "the newest already in Now stepped aside");

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{},"is_now":true}}"#, ids[2])).unwrap();
        assert_eq!(out["demoted_from_now"][0], ids[3]);
    }

    #[test]
    fn the_agent_cannot_put_a_step_in_now() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"email landlord"}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            &format!(r#"{{"task_id":{id},"steps":[
                {{"title":"find the thread","duration_min":5}},
                {{"title":"write and send","duration_min":10}}]}}"#)).unwrap();
        let step = out["step_ids"][0].as_i64().unwrap();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{step},"is_now":true}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");
    }

    #[test]
    fn task_split_makes_one_level_of_steps() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"email landlord"}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            &format!(r#"{{"task_id":{id},"steps":[
                {{"title":"find the last email thread","duration_min":5}},
                {{"title":"write and send","duration_min":10}}]}}"#)).unwrap();
        assert_eq!(out["duration_min"], 15);
        assert_eq!(out["step_ids"].as_array().unwrap().len(), 2);
        let child = out["step_ids"][0].as_i64().unwrap();

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            &format!(r#"{{"task_id":{child},"steps":[
                {{"title":"a","duration_min":5}},{{"title":"b","duration_min":5}}]}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            &format!(r#"{{"task_id":{id},"steps":[{{"title":"only","duration_min":5}}]}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            r#"{"task_id":999,"steps":[{"title":"a","duration_min":5},{"title":"b","duration_min":5}]}"#).unwrap_err();
        assert_eq!(e.kind, "not_found");
    }

    #[test]
    fn unknown_tool_unknown_field_and_bad_json_are_typed() {
        let (conn, tmp) = env();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "rm_rf", "{}").unwrap_err();
        assert_eq!(e.kind, "unknown_tool");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"x","surprise":1}"#).unwrap_err();
        assert_eq!(e.kind, "invalid_args");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create", "not json").unwrap_err();
        assert_eq!(e.kind, "invalid_args");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            r#"{"task_id":999,"state":"done"}"#).unwrap_err();
        assert_eq!(e.kind, "not_found");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            r#"{"task_id":1,"state":"exploded"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
    }

    #[test]
    fn oversized_payload_is_rejected_before_parsing() {
        let (conn, tmp) = env();
        let big = format!(r#"{{"title":"{}"}}"#, "x".repeat(MAX_ARGS_BYTES));
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create", &big).unwrap_err();
        assert_eq!(e.kind, "rejected");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn oversized_text_fields_are_rejected_and_leave_the_row_unchanged() {
        let (conn, tmp) = env();
        let out = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Talk,
            "task_create",
            r#"{"title":"call dentist"}"#,
        )
        .unwrap();
        let id = out["task_id"].as_i64().unwrap();

        let e = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Talk,
            "task_update",
            &format!(r#"{{"task_id":{id},"title":"{}"}}"#, "x".repeat(501)),
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");

        let e = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Talk,
            "task_update",
            &format!(
                r#"{{"task_id":{id},"title":"renamed","notes":"{}"}}"#,
                "x".repeat(16 * 1024 + 1)
            ),
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");

        let (title, notes): (String, String) = conn
            .query_row("SELECT title, notes FROM tasks WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(title, "call dentist");
        assert_eq!(notes, "");
    }

    #[test]
    fn prepare_embeds_only_memory_tools_and_reports_failures() {
        use crate::providers::mock::MockEmbeddings;
        let emb = MockEmbeddings;

        let v = prepare(Some(&emb), "memory_query", r#"{"query":"abba"}"#);
        assert!(v.query.is_some());
        assert!(v.content.is_none() && v.error.is_none());

        let v = prepare(Some(&emb), "memory_write", r#"{"op":"add","category":"semantic","summary":"s","body":"b"}"#);
        assert!(v.content.is_some());
        assert!(v.query.is_none());

        // non-memory tools and malformed args cost nothing
        let v = prepare(Some(&emb), "task_create", r#"{"title":"x"}"#);
        assert!(v.content.is_none() && v.query.is_none() && v.error.is_none());
        let v = prepare(Some(&emb), "memory_query", "not json");
        assert!(v.query.is_none() && v.error.is_none());

        // no provider → all None
        let v = prepare(None, "memory_query", r#"{"query":"abba"}"#);
        assert!(v.query.is_none());

        struct FailingEmb;
        impl crate::providers::EmbeddingsProvider for FailingEmb {
            fn embed(&self, _: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
                anyhow::bail!("endpoint down")
            }
        }
        let v = prepare(Some(&FailingEmb), "memory_query", r#"{"query":"abba"}"#);
        assert!(v.query.is_none());
        assert!(v.error.as_deref().unwrap_or("").contains("endpoint down"));
    }

    #[test]
    fn prepared_vector_text_matches_index_text() {
        use crate::providers::{mock::MockEmbeddings, EmbeddingsProvider};
        let emb = MockEmbeddings;
        let v = prepare(Some(&emb), "memory_write",
            r#"{"op":"add","category":"semantic","summary":"line one\nline two","body":"  padded  "}"#);
        let direct = emb
            .embed(&[&crate::memory::embed_text("line one\nline two", "  padded  ")])
            .unwrap();
        assert_eq!(v.content.unwrap(), direct[0]);
    }

    #[test]
    fn session_surfaces_are_nested_subsets() {
        let is_subset = |a: &[&str], b: &[&str]| a.iter().all(|t| b.contains(t));
        assert!(is_subset(registry(SessionKind::Checkin), registry(SessionKind::Talk)));
        assert!(is_subset(registry(SessionKind::Talk), registry(SessionKind::Nightly)));
    }

    #[test]
    fn schemas_cover_the_registry_and_are_objects() {
        for kind in [SessionKind::Nightly, SessionKind::Checkin, SessionKind::Talk] {
            let schemas = schemas(kind);
            assert_eq!(schemas.len(), registry(kind).len());
            for s in schemas {
                assert!(s["name"].is_string());
                assert!(!s["description"].as_str().unwrap().is_empty());
                assert!(s["input_schema"].is_object());
            }
        }
    }
}
