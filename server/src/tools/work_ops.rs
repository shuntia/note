use super::{ToolCtx, ToolError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EndArgs {
    /// done when the work got finished, stopped when it was left.
    pub outcome: Outcome,
    /// Why it ends now, for the log.
    pub reason: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Done,
    Stopped,
}

/// Closes the open work session with no farewell of its own: the wake-up that
/// ends it says whatever is said.
pub fn end(conn: &Connection, ctx: &ToolCtx, args: &EndArgs) -> Result<serde_json::Value, ToolError> {
    let reason = args.reason.trim();
    if reason.is_empty() {
        return Err(ToolError::rejected("reason must say why the session ends now"));
    }
    super::check_text("reason", reason)?;
    let internal = |e: anyhow::Error| ToolError::internal(e.to_string());
    let now = jiff::Timestamp::now();
    let Some(session) = crate::work::open(conn, ctx.user_id).map_err(|e| ToolError::internal(e.to_string()))? else {
        return Err(ToolError::not_found("no work session is open"));
    };
    let outcome = match args.outcome {
        Outcome::Done => "done",
        Outcome::Stopped => "stopped",
    };
    crate::work::end_with(
        conn,
        ctx.config_dir,
        ctx.user_id,
        ctx.username,
        Some(session.id),
        outcome,
        crate::work::Farewell::None,
        now,
    )
    .map_err(internal)?;
    crate::log::record(
        conn,
        Some(ctx.user_id),
        "work_session_ended_by_note",
        &format!("session {} {outcome} ({reason})", session.id),
    )
    .map_err(internal)?;
    Ok(serde_json::json!({
        "ended": session.id,
        "title": session.title,
        "outcome": outcome,
        "minutes": session.elapsed_ms(now) / 60_000,
    }))
}

#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, PreparedVectors, SessionKind, ToolCtx};

    #[test]
    fn a_wakeup_ends_the_open_session_without_a_farewell_of_its_own() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("defaults/user.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n").unwrap();
        let ctx = ToolCtx {
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            user_id: uid,
            username: "aki",
            vectors: PreparedVectors::default(),
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            share: None,
            share_thread: None,
        };
        let args = r#"{"outcome":"stopped","reason":"paused all afternoon"}"#;
        let e = dispatch(&conn, &ctx, SessionKind::Trigger, "work_end", args).unwrap_err();
        assert_eq!(e.kind, "not_found");

        let session = crate::work::start(
            &conn,
            tmp.path(),
            uid,
            "aki",
            crate::work::NewSession { title: "essay".into(), ..Default::default() },
            jiff::Timestamp::now(),
        )
        .unwrap();
        let out = dispatch(&conn, &ctx, SessionKind::Trigger, "work_end", args).unwrap();
        assert_eq!(out["ended"], session.id);
        assert!(crate::work::open(&conn, uid).unwrap().is_none());
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE work_session_id = ?1 AND status = 'pending'",
                [session.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending, 0, "its checks are dropped and no farewell is laid");

        let e = dispatch(&conn, &ctx, SessionKind::Talk, "work_end", args).unwrap_err();
        assert_eq!(e.kind, "unknown_tool", "only a wake-up ends a session for the user");
    }
}
