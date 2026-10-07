use super::{ToolCtx, ToolError};
use crate::notes::{Change, NoteError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WriteOp {
    Add,
    Update,
    Remove,
    Keep,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteArgs {
    pub op: WriteOp,
    /// The note's id; every op but add needs it.
    pub id: Option<String>,
    /// One self-explanatory line, at most 80 characters.
    pub title: Option<String>,
    /// When the note starts to matter: RFC 3339, or YYYY-MM-DDTHH:MM or YYYY-MM-DD in the user's own zone. An empty string clears it.
    pub from: Option<String>,
    /// When it stops mattering, in the same forms. An empty string clears it.
    pub until: Option<String>,
}

pub(super) fn note_error(e: NoteError) -> ToolError {
    match e {
        NoteError::Invalid(m) => ToolError::rejected(m),
        NoteError::NotFound(id) => ToolError::not_found(format!("no note {id}")),
        NoteError::Other(e) => ToolError::internal(e.to_string()),
    }
}

/// `None` leaves the bound alone, `Some(None)` clears it.
fn when(raw: Option<&str>, tz: &jiff::tz::TimeZone) -> Result<Option<Option<String>>, ToolError> {
    match raw.map(str::trim) {
        None => Ok(None),
        Some("") => Ok(Some(None)),
        Some(s) => crate::notes::parse_when(s, tz).map(|t| Some(Some(t))).map_err(note_error),
    }
}

fn shown(n: &crate::notes::Note) -> serde_json::Value {
    serde_json::json!({ "id": n.id, "title": n.title, "from": n.from, "until": n.until })
}

pub fn write(conn: &Connection, ctx: &ToolCtx, args: &WriteArgs) -> Result<serde_json::Value, ToolError> {
    super::task_query::unscoped(ctx)?;
    let now = jiff::Timestamp::now();
    let tz = crate::triggers::timezone(ctx.config_dir, ctx.username);
    let from = when(args.from.as_deref(), &tz)?;
    let until = when(args.until.as_deref(), &tz)?;
    let (data_dir, user) = (ctx.data_dir, ctx.username);
    let id = || -> Result<&str, ToolError> {
        let id = args.id.as_deref().ok_or_else(|| ToolError::rejected("this op needs the note's id"))?;
        if !crate::memory::valid_id(id) {
            return Err(ToolError::rejected("malformed note id"));
        }
        Ok(id)
    };
    match args.op {
        WriteOp::Add => {
            if args.id.is_some() {
                return Err(ToolError::rejected("add makes a new note; leave id out"));
            }
            let title = args.title.as_deref().ok_or_else(|| ToolError::rejected("add needs a title"))?;
            let n = crate::notes::add(conn, data_dir, user, title, from.flatten(), until.flatten(), now)
                .map_err(note_error)?;
            Ok(shown(&n))
        }
        WriteOp::Update => {
            if args.title.is_none() && from.is_none() && until.is_none() {
                return Err(ToolError::rejected("set title, from or until"));
            }
            let change = Change { title: args.title.clone(), from, until };
            let n = crate::notes::update(conn, data_dir, user, id()?, change, now).map_err(note_error)?;
            Ok(shown(&n))
        }
        WriteOp::Remove => {
            let id = id()?;
            crate::notes::remove(conn, data_dir, user, id).map_err(note_error)?;
            Ok(serde_json::json!({ "id": id, "removed": true }))
        }
        WriteOp::Keep => {
            let id = id()?;
            let kept = crate::notes::touch(conn, data_dir, user, &[id.to_owned()], now)
                .map_err(|e| ToolError::internal(e.to_string()))?;
            if kept == 0 {
                return Err(ToolError::not_found(format!("no note {id}")));
            }
            Ok(serde_json::json!({ "id": id, "kept": true }))
        }
    }
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
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("defaults/user.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "display_name = \"X\"\ntimezone = \"Asia/Tokyo\"\ntemplate = \"default\"\n").unwrap();
        (conn, tmp)
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

    fn call(conn: &Connection, tmp: &tempfile::TempDir, args: &str) -> Value {
        dispatch(conn, &ctx(tmp, 1), SessionKind::Talk, "note_write", args).unwrap()
    }

    #[test]
    fn a_note_is_added_reworded_kept_and_removed() {
        let (conn, tmp) = env();
        let out = call(&conn, &tmp, r#"{"op":"add","title":"  call the bank\nbefore five ","until":"2026-10-07T17:00"}"#);
        let id = out["id"].as_str().unwrap().to_string();
        assert_eq!(out["title"], "call the bank before five");
        assert_eq!(out["until"], "2026-10-07T17:00:00+09:00", "a wall time is the user's own");

        let out = call(&conn, &tmp, &format!(r#"{{"op":"update","id":"{id}","title":"call the bank","until":""}}"#));
        assert_eq!(out["title"], "call the bank");
        assert!(out["until"].is_null(), "an empty until clears it");

        assert_eq!(call(&conn, &tmp, &format!(r#"{{"op":"keep","id":"{id}"}}"#))["kept"], true);
        assert_eq!(call(&conn, &tmp, &format!(r#"{{"op":"remove","id":"{id}"}}"#))["removed"], true);
        assert!(crate::notes::all(tmp.path(), "aki").unwrap().is_empty());
    }

    #[test]
    fn note_write_refuses_what_is_not_a_note() {
        let (conn, tmp) = env();
        let bad = |args: &str| dispatch(&conn, &ctx(&tmp, 1), SessionKind::Talk, "note_write", args).unwrap_err().kind;
        assert_eq!(bad(r#"{"op":"add","title":"   "}"#), "rejected");
        assert_eq!(bad(&format!(r#"{{"op":"add","title":"{}"}}"#, "x".repeat(81))), "rejected");
        assert_eq!(bad(r#"{"op":"add"}"#), "rejected");
        assert_eq!(bad(r#"{"op":"add","title":"x","id":"00000000-0000-4000-8000-000000000001"}"#), "rejected");
        assert_eq!(bad(r#"{"op":"add","title":"x","from":"friday"}"#), "rejected");
        assert_eq!(bad(r#"{"op":"add","title":"x","from":"2026-10-07T15:00","until":"2026-10-07T14:00"}"#), "rejected");
        assert_eq!(bad(r#"{"op":"remove"}"#), "rejected");
        assert_eq!(bad(r#"{"op":"keep","id":"../../etc/passwd"}"#), "rejected");
        assert_eq!(bad(r#"{"op":"keep","id":"00000000-0000-4000-8000-000000000404"}"#), "not_found");
        assert_eq!(bad(r#"{"op":"forget","id":"x"}"#), "invalid_args");

        let theirs = dispatch(&conn, &ctx(&tmp, 2), SessionKind::Talk, "note_write", r#"{"op":"add","title":"theirs"}"#)
            .unwrap()["id"].as_str().unwrap().to_string();
        assert_eq!(bad(&format!(r#"{{"op":"remove","id":"{theirs}"}}"#)), "not_found");
        let fact = crate::memory::add(&conn, tmp.path(), "aki", "semantic", "s", "b", None).unwrap();
        assert_eq!(bad(&format!(r#"{{"op":"remove","id":"{fact}"}}"#)), "not_found", "a long-term memory is out of reach");
        let mine = call(&conn, &tmp, r#"{"op":"add","title":"mine"}"#)["id"].as_str().unwrap().to_string();
        assert_eq!(bad(&format!(r#"{{"op":"update","id":"{mine}"}}"#)), "rejected");

        let scoped = ToolCtx { task_scope: Some(1), ..ctx(&tmp, 1) };
        let e = dispatch(&conn, &scoped, SessionKind::Talk, "note_write", r#"{"op":"add","title":"x"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected", "a scoped session reaches its task alone");
    }

    #[test]
    fn note_write_sits_where_the_user_a_check_in_a_trigger_or_a_call_can_reach_it() {
        for kind in [SessionKind::Talk, SessionKind::Checkin, SessionKind::Trigger, SessionKind::Nightly, SessionKind::Call] {
            assert!(registry(kind).contains(&"note_write"), "note_write missing from {kind:?}");
        }
        for kind in [
            SessionKind::Import,
            SessionKind::Inbox,
            SessionKind::Summarize,
            SessionKind::Harvest,
            SessionKind::Review,
            SessionKind::Share,
        ] {
            assert!(!registry(kind).contains(&"note_write"), "note_write reached {kind:?}");
        }
    }
}
