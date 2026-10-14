use super::{ToolCtx, ToolError};
use crate::notes::{Change, NoteError, Outcome};
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
    /// The IANA zone a from or until without an offset is read in, when not the user's own.
    pub tz: Option<String>,
    /// For add: write it even though a note with the same title exists.
    #[serde(default)]
    pub also: bool,
}

pub(super) fn note_error(e: NoteError) -> ToolError {
    match e {
        NoteError::Invalid(m) => ToolError::rejected(m),
        NoteError::NotFound(id) => ToolError::not_found(format!("no note {id}")),
        NoteError::Other(e) => ToolError::internal(e.to_string()),
    }
}

/// `None` leaves the bound alone, `Some(None)` clears it. A time read in
/// `there` is stored with the user's own offset.
fn when(
    raw: Option<&str>,
    there: &jiff::tz::TimeZone,
    user: &jiff::tz::TimeZone,
) -> Result<Option<Option<String>>, ToolError> {
    match raw.map(str::trim) {
        None => Ok(None),
        Some("") => Ok(Some(None)),
        Some(s) => crate::notes::parse_when(s, there)
            .and_then(|t| crate::notes::parse_when(&t, user))
            .map(|t| Some(Some(t)))
            .map_err(note_error),
    }
}

fn shown(n: &crate::notes::Note) -> serde_json::Value {
    serde_json::json!({ "id": crate::notes::short(&n.id), "title": n.title, "from": n.from, "until": n.until })
}

pub fn write(conn: &Connection, ctx: &ToolCtx, args: &WriteArgs) -> Result<serde_json::Value, ToolError> {
    super::task_query::unscoped(ctx)?;
    let now = jiff::Timestamp::now();
    let tz = crate::triggers::timezone(ctx.config_dir, ctx.username);
    let there = super::trigger_ops::zone_arg(args.tz.as_deref())?.unwrap_or_else(|| tz.clone());
    let from = when(args.from.as_deref(), &there, &tz)?;
    let until = when(args.until.as_deref(), &there, &tz)?;
    let (data_dir, user) = (ctx.data_dir, ctx.username);
    let id = || -> Result<String, ToolError> {
        let id = args.id.as_deref().ok_or_else(|| ToolError::rejected("this op needs the note's id"))?;
        crate::notes::resolve(conn, data_dir, user, id).map_err(note_error)
    };
    match args.op {
        WriteOp::Add => {
            if args.id.is_some() {
                return Err(ToolError::rejected("add makes a new note; leave id out"));
            }
            let title = args.title.as_deref().ok_or_else(|| ToolError::rejected("add needs a title"))?;
            if !args.also {
                let notes = crate::notes::all(conn, data_dir, user).map_err(|e| ToolError::internal(e.to_string()))?;
                if let Some(n) = notes.iter().find(|n| super::same_title(&n.title, title)) {
                    return Err(ToolError::duplicate(format!(
                        "note {} already reads {:?}; keep or update it, or pass also: true when this is \
                         a second one",
                        crate::notes::short(&n.id), n.title
                    )));
                }
            }
            let n = crate::notes::add(conn, data_dir, user, title, from.flatten(), until.flatten(), now)
                .map_err(note_error)?;
            Ok(shown(&n))
        }
        WriteOp::Update => {
            if args.title.is_none() && from.is_none() && until.is_none() {
                return Err(ToolError::rejected("set title, from or until"));
            }
            let change = Change { title: args.title.clone(), from, until };
            let n = crate::notes::update(conn, data_dir, user, &id()?, change, now).map_err(note_error)?;
            Ok(shown(&n))
        }
        WriteOp::Remove => {
            let id = id()?;
            crate::notes::remove(conn, data_dir, user, &id).map_err(note_error)?;
            Ok(serde_json::json!({ "id": crate::notes::short(&id), "removed": true }))
        }
        WriteOp::Keep => {
            let id = id()?;
            let kept = crate::notes::touch(conn, data_dir, user, std::slice::from_ref(&id), now)
                .map_err(|e| ToolError::internal(e.to_string()))?;
            if kept == 0 {
                return Err(ToolError::not_found(format!("no note {}", crate::notes::short(&id))));
            }
            Ok(serde_json::json!({ "id": crate::notes::short(&id), "kept": true }))
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettleOutcome {
    Memory,
    Drop,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettleArgs {
    pub id: String,
    pub outcome: SettleOutcome,
    /// For memory: episodic by default for a note with a time window, semantic otherwise.
    pub category: Option<super::memory_ops::Category>,
    /// For memory: the fact's one line; the note's title when left out.
    pub summary: Option<String>,
    /// For memory: what the fact says beyond its summary.
    pub body: Option<String>,
}

pub fn settle(conn: &Connection, ctx: &ToolCtx, args: &SettleArgs) -> Result<serde_json::Value, ToolError> {
    let id = crate::notes::resolve(conn, ctx.data_dir, ctx.username, &args.id).map_err(note_error)?;
    let outcome = match args.outcome {
        SettleOutcome::Drop => {
            if args.category.is_some() || args.summary.is_some() || args.body.is_some() {
                return Err(ToolError::rejected("category, summary and body go with outcome memory"));
            }
            Outcome::Drop
        }
        SettleOutcome::Memory => {
            let max = super::memory_ops::MAX_SUMMARY;
            if args.summary.as_deref().is_some_and(|s| s.trim().is_empty() || s.len() > max) {
                return Err(ToolError::rejected(format!("summary must be 1..={max} bytes")));
            }
            if let Some(body) = &args.body {
                super::check_text("body", body)?;
            }
            Outcome::Memory {
                category: args.category.as_ref().map(super::memory_ops::Category::as_str),
                summary: args.summary.as_deref(),
                body: args.body.as_deref(),
            }
        }
    };
    let memory_id = crate::notes::settle(conn, ctx.data_dir, ctx.username, &id, &outcome).map_err(note_error)?;
    Ok(serde_json::json!({ "id": crate::notes::short(&id), "memory_id": memory_id }))
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
        assert!(crate::notes::all(&conn, tmp.path(), "aki").unwrap().is_empty());
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
            SessionKind::Review,
            SessionKind::Share,
        ] {
            assert!(!registry(kind).contains(&"note_write"), "note_write reached {kind:?}");
        }
    }

    #[test]
    fn note_settle_keeps_or_drops_a_note_and_refuses_a_mixed_drop() {
        let (conn, tmp) = env();
        let nightly = |args: &str| dispatch(&conn, &ctx(&tmp, 1), SessionKind::Nightly, "note_settle", args);
        let add = |title: &str| {
            call(&conn, &tmp, &format!(r#"{{"op":"add","title":"{title}"}}"#))["id"].as_str().unwrap().to_string()
        };
        let kept = add("prefers short check-ins");
        let out = nightly(&format!(r#"{{"id":"{kept}","outcome":"memory","category":"procedural"}}"#)).unwrap();
        let f = crate::memory::read(tmp.path(), "aki", out["memory_id"].as_str().unwrap()).unwrap().unwrap();
        assert_eq!((f.category.as_str(), f.summary.as_str()), ("procedural", "prefers short check-ins"));

        let dropped = add("milk");
        assert_eq!(nightly(&format!(r#"{{"id":"{dropped}","outcome":"drop","summary":"x"}}"#)).unwrap_err().kind, "rejected");
        assert_eq!(nightly(&format!(r#"{{"id":"{dropped}","outcome":"memory","summary":" "}}"#)).unwrap_err().kind, "rejected");
        assert!(nightly(&format!(r#"{{"id":"{dropped}","outcome":"drop"}}"#)).unwrap()["memory_id"].is_null());
        assert!(crate::notes::all(&conn, tmp.path(), "aki").unwrap().is_empty());
        assert_eq!(nightly(&format!(r#"{{"id":"{dropped}","outcome":"drop"}}"#)).unwrap_err().kind, "not_found");
        assert_eq!(nightly(r#"{"id":"nope","outcome":"drop"}"#).unwrap_err().kind, "rejected");
    }

    #[test]
    fn the_same_note_twice_is_shown_the_first() {
        let (conn, tmp) = env();
        let first = call(&conn, &tmp, r#"{"op":"add","title":"Call the bank"}"#)["id"].as_str().unwrap().to_string();
        let e = dispatch(&conn, &ctx(&tmp, 1), SessionKind::Talk, "note_write",
            r#"{"op":"add","title":"call the bank."}"#).unwrap_err();
        assert_eq!(e.kind, "duplicate");
        assert!(e.message.contains(&first), "{}", e.message);
        call(&conn, &tmp, r#"{"op":"add","title":"call the bank","also":true}"#);
        assert_eq!(crate::notes::all(&conn, tmp.path(), "aki").unwrap().len(), 2);
    }

    /// Five in the afternoon in New York is six the next morning in Tokyo.
    #[test]
    fn a_time_given_in_another_zone_is_kept_on_the_users_clock() {
        let (conn, tmp) = env();
        let out = call(&conn, &tmp,
            r#"{"op":"add","title":"flight home","until":"2026-10-07T17:00","tz":"America/New_York"}"#);
        assert_eq!(out["until"], "2026-10-08T06:00:00+09:00");
    }
}
