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
}

pub const MAX_ARGS_BYTES: usize = 64 * 1024;

const CHECKIN: &[&str] = &["task_create", "task_update"];
const TALK: &[&str] = &["task_create", "task_update"];
const NIGHTLY: &[&str] = &["task_create", "task_update"];

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
            "Create a new task for the current user.",
            schema::<task_ops::CreateArgs>(),
        ),
        "task_update" => (
            "Update a task's title, description, state, or notes.",
            schema::<task_ops::UpdateArgs>(),
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
        ToolCtx { config_dir: tmp.path(), data_dir: tmp.path(), user_id: 1, username: "aki" }
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
