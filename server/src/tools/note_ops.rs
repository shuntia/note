use super::task_ops::task_error;
use super::{ToolCtx, ToolError};
use crate::legacy_notes::{NewNote, NotePatch};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddArgs {
    /// One line, at most 200 characters.
    pub text: String,
}

pub fn add(conn: &Connection, ctx: &ToolCtx, args: AddArgs) -> Result<serde_json::Value, ToolError> {
    super::task_query::unscoped(ctx)?;
    let note = crate::legacy_notes::create(conn, ctx.user_id, &NewNote { text: args.text }, jiff::Timestamp::now())
        .map_err(task_error)?;
    Ok(serde_json::json!({ "note_id": note.id, "text": note.text }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateArgs {
    pub note_id: i64,
    /// One line, at most 200 characters.
    pub text: Option<String>,
    /// A pinned note is never nudged about.
    pub pinned: Option<bool>,
}

pub fn update(conn: &Connection, ctx: &ToolCtx, args: UpdateArgs) -> Result<serde_json::Value, ToolError> {
    super::task_query::unscoped(ctx)?;
    if args.text.is_none() && args.pinned.is_none() {
        return Err(ToolError::rejected("set text, pinned or both"));
    }
    let patch = NotePatch { text: args.text, pinned: args.pinned, done: None };
    match crate::legacy_notes::update(conn, ctx.user_id, args.note_id, &patch, jiff::Timestamp::now()) {
        Ok(Some(n)) => Ok(serde_json::json!({ "note_id": n.id, "text": n.text, "pinned": n.pinned })),
        Ok(None) => Err(ToolError::not_found(format!("no note {}", args.note_id))),
        Err(e) => Err(task_error(e)),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DoneArgs {
    pub note_id: i64,
}

pub fn done(conn: &Connection, ctx: &ToolCtx, args: &DoneArgs) -> Result<serde_json::Value, ToolError> {
    super::task_query::unscoped(ctx)?;
    let patch = NotePatch { done: Some(true), ..Default::default() };
    match crate::legacy_notes::update(conn, ctx.user_id, args.note_id, &patch, jiff::Timestamp::now()) {
        Ok(Some(n)) => Ok(serde_json::json!({ "note_id": n.id, "text": n.text, "done_at": n.done_at })),
        Ok(None) => Err(ToolError::not_found(format!("no note {}", args.note_id))),
        Err(e) => Err(task_error(e)),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {}

pub fn list(conn: &Connection, ctx: &ToolCtx, _args: ListArgs) -> Result<serde_json::Value, ToolError> {
    super::task_query::unscoped(ctx)?;
    let notes = crate::legacy_notes::list(conn, ctx.user_id, jiff::Timestamp::now())
        .map_err(|e| ToolError::internal(e.to_string()))?;
    let notes: Vec<serde_json::Value> = notes
        .into_iter()
        .filter(|n| n.done_at.is_none())
        .map(|n| {
            serde_json::json!({
                "note_id": n.id,
                "text": n.text,
                "pinned": n.pinned,
                "created_at": n.created_at,
                "last_nudged_at": n.last_nudged_at,
            })
        })
        .collect();
    Ok(serde_json::json!({ "notes": notes }))
}

#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, registry, PreparedVectors, SessionKind, ToolCtx};
    use rusqlite::Connection;
    use serde_json::Value;

    fn env() -> (Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        for name in ["aki", "bo"] {
            conn.execute(
                "INSERT INTO users (username, pass_hash, role) VALUES (?1, 'x', 'member')",
                [name],
            )
            .unwrap();
        }
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx(tmp: &tempfile::TempDir, user_id: i64) -> ToolCtx<'_> {
        ToolCtx {
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            user_id,
            username: if user_id == 1 { "aki" } else { "bo" },
            vectors: PreparedVectors::default(),
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            share: None,
            share_thread: None,
        }
    }

    fn call(conn: &Connection, tmp: &tempfile::TempDir, name: &str, args: &str) -> Value {
        dispatch(conn, &ctx(tmp, 1), SessionKind::Talk, name, args).unwrap()
    }

    #[test]
    fn a_note_is_added_pinned_reworded_and_checked_off() {
        let (conn, tmp) = env();
        let id = call(&conn, &tmp, "note_add", r#"{"text":"  call the bank\nbefore five "}"#)
            ["note_id"]
            .as_i64()
            .unwrap();
        let out = call(&conn, &tmp, "note_list", "{}");
        assert_eq!(out["notes"][0]["note_id"], id);
        assert_eq!(out["notes"][0]["text"], "call the bank before five");
        assert_eq!(out["notes"][0]["pinned"], false);

        let out = call(
            &conn,
            &tmp,
            "note_update",
            &format!(r#"{{"note_id":{id},"text":"call the bank","pinned":true}}"#),
        );
        assert_eq!((out["text"].as_str(), out["pinned"].as_bool()), (Some("call the bank"), Some(true)));

        let out = call(&conn, &tmp, "note_done", &format!(r#"{{"note_id":{id}}}"#));
        assert!(out["done_at"].is_string());
        assert!(call(&conn, &tmp, "note_list", "{}")["notes"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_note_tool_refuses_what_is_not_a_note() {
        let (conn, tmp) = env();
        let bad = |name: &str, args: &str| {
            dispatch(&conn, &ctx(&tmp, 1), SessionKind::Talk, name, args).unwrap_err()
        };
        assert_eq!(bad("note_add", r#"{"text":"   "}"#).kind, "rejected");
        assert_eq!(bad("note_add", &format!(r#"{{"text":"{}"}}"#, "x".repeat(201))).kind, "rejected");
        assert_eq!(bad("note_update", r#"{"note_id":99,"pinned":true}"#).kind, "not_found");
        assert_eq!(bad("note_done", r#"{"note_id":99}"#).kind, "not_found");

        let theirs = dispatch(&conn, &ctx(&tmp, 2), SessionKind::Talk, "note_add", r#"{"text":"theirs"}"#)
            .unwrap()["note_id"]
            .as_i64()
            .unwrap();
        assert_eq!(bad("note_done", &format!(r#"{{"note_id":{theirs}}}"#)).kind, "not_found");
        assert!(call(&conn, &tmp, "note_list", "{}")["notes"].as_array().unwrap().is_empty());

        let mine = call(&conn, &tmp, "note_add", r#"{"text":"mine"}"#)["note_id"].as_i64().unwrap();
        assert_eq!(bad("note_update", &format!(r#"{{"note_id":{mine}}}"#)).kind, "rejected");

        let scoped = ToolCtx { task_scope: Some(1), ..ctx(&tmp, 1) };
        let e = dispatch(&conn, &scoped, SessionKind::Talk, "note_list", "{}").unwrap_err();
        assert_eq!(e.kind, "rejected", "a scoped session reaches its task alone");
    }

    #[test]
    fn the_note_tools_sit_where_the_user_or_a_check_in_can_reach_them() {
        for name in ["note_add", "note_update", "note_done", "note_list"] {
            for kind in [SessionKind::Talk, SessionKind::Checkin, SessionKind::Trigger, SessionKind::Nightly] {
                assert!(registry(kind).contains(&name), "{name} missing from {kind:?}");
            }
            for kind in [
                SessionKind::Import,
                SessionKind::Inbox,
                SessionKind::Summarize,
                SessionKind::Harvest,
                SessionKind::Review,
                SessionKind::Share,
            ] {
                assert!(!registry(kind).contains(&name), "{name} reached {kind:?}");
            }
        }
    }
}
