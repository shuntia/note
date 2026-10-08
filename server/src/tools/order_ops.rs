use super::task_query::unscoped;
use super::{SessionKind, ToolCtx, ToolError};
use crate::order::{self, OrderError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetArgs {
    /// Task or step ids, first to last; an empty list clears the order.
    pub task_ids: Vec<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveArgs {
    pub task_id: i64,
    /// The item it goes in front of. Omit to put it last.
    #[serde(default)]
    pub before_task_id: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DropArgs {
    pub task_id: i64,
}

fn internal(e: impl std::fmt::Display) -> ToolError {
    ToolError::internal(e.to_string())
}

fn refused(e: OrderError) -> ToolError {
    match e {
        OrderError::NotFound(_) => ToolError::not_found(e.to_string()),
        OrderError::Twice(_) | OrderError::TooMany => ToolError::rejected(e.to_string()),
        OrderError::Db(e) => internal(e),
    }
}

/// The running work session's task with its parent and its steps; empty when
/// no session runs or it holds no task.
pub(crate) fn held(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<i64>> {
    let Some(id) = crate::work::open(conn, user_id)?.and_then(|s| s.task_id) else {
        return Ok(Vec::new());
    };
    let mut out = vec![id];
    if let Some(parent) = crate::tasks::get(conn, user_id, id)?.and_then(|t| t.parent_id) {
        out.push(parent);
    }
    let mut stmt = conn.prepare("SELECT id FROM tasks WHERE parent_id = ?1")?;
    let steps = stmt.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
    out.extend(steps);
    Ok(out)
}

fn write(
    conn: &Connection,
    ctx: &ToolCtx,
    date: jiff::civil::Date,
    before: &[i64],
    after: &[i64],
) -> Result<serde_json::Value, ToolError> {
    let held = held(conn, ctx.user_id).map_err(internal)?;
    if let Some(first) = before.first().filter(|f| held.contains(f)) {
        if !after.first().is_some_and(|a| held.contains(a)) {
            return Err(ToolError::rejected(format!(
                "task {first} is being worked on right now and stays first"
            )));
        }
    }
    let items = order::replace(conn, ctx.user_id, date, after).map_err(refused)?;
    let line = after.iter().map(i64::to_string).collect::<Vec<_>>().join(", ");
    crate::log::record(conn, Some(ctx.user_id), "order_changed", &format!("{date}: {line}"))
        .map_err(internal)?;
    Ok(serde_json::json!({ "date": date.to_string(), "order": items }))
}

fn day(ctx: &ToolCtx, kind: SessionKind) -> jiff::civil::Date {
    super::trigger_ops::target_date(ctx, kind, jiff::Timestamp::now())
}

pub fn set(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, args: &SetArgs) -> Result<serde_json::Value, ToolError> {
    unscoped(ctx)?;
    let date = day(ctx, kind);
    let before = order::ids(conn, ctx.user_id, date).map_err(internal)?;
    write(conn, ctx, date, &before, &args.task_ids)
}

pub fn move_item(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, args: &MoveArgs) -> Result<serde_json::Value, ToolError> {
    unscoped(ctx)?;
    let date = day(ctx, kind);
    let before = order::ids(conn, ctx.user_id, date).map_err(internal)?;
    let after = order::placed(&before, args.task_id, args.before_task_id).ok_or_else(|| {
        ToolError::rejected(format!(
            "before_task_id {} is not another item of the order",
            args.before_task_id.unwrap_or_default()
        ))
    })?;
    write(conn, ctx, date, &before, &after)
}

pub fn drop_item(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, args: &DropArgs) -> Result<serde_json::Value, ToolError> {
    unscoped(ctx)?;
    let date = day(ctx, kind);
    let before = order::ids(conn, ctx.user_id, date).map_err(internal)?;
    if !before.contains(&args.task_id) {
        return Err(ToolError::not_found(format!("task {} is not in the order", args.task_id)));
    }
    let after: Vec<i64> = before.iter().copied().filter(|&i| i != args.task_id).collect();
    write(conn, ctx, date, &before, &after)
}


#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, registry, PreparedVectors, SessionKind, ToolCtx};

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", [])
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("defaults/user.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            p,
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\nnightly_time = \"22:00\"\n",
        )
        .unwrap();
        (conn, tmp)
    }

    fn ctx(tmp: &tempfile::TempDir) -> ToolCtx<'_> {
        ToolCtx {
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            user_id: 1,
            username: "aki",
            vectors: PreparedVectors::default(),
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            share: None,
            share_thread: None,
        }
    }

    fn task(conn: &rusqlite::Connection, tmp: &tempfile::TempDir, title: &str) -> i64 {
        dispatch(conn, &ctx(tmp), SessionKind::Talk, "task_create", &format!(r#"{{"title":"{title}"}}"#))
            .unwrap()["task_id"]
            .as_i64()
            .unwrap()
    }

    fn today() -> jiff::civil::Date {
        jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date()
    }

    fn order(conn: &rusqlite::Connection) -> Vec<i64> {
        crate::order::ids(conn, 1, today()).unwrap()
    }

    #[test]
    fn note_sets_moves_and_drops_todays_order() {
        let (conn, tmp) = env();
        let (a, b, c) = (task(&conn, &tmp, "a"), task(&conn, &tmp, "b"), task(&conn, &tmp, "c"));
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "order_set",
            &format!(r#"{{"task_ids":[{a},{b}]}}"#)).unwrap();
        assert_eq!(out["date"], today().to_string());
        assert_eq!(out["order"][1]["title"], "b");
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "order_move",
            &format!(r#"{{"task_id":{c},"before_task_id":{a}}}"#)).unwrap();
        assert_eq!(order(&conn), [c, a, b], "a task not yet placed joins where it is put");
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "order_move", &format!(r#"{{"task_id":{c}}}"#)).unwrap();
        assert_eq!(order(&conn), [a, b, c]);
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "order_drop", &format!(r#"{{"task_id":{a}}}"#)).unwrap();
        assert_eq!(order(&conn), [b, c]);
        let logged: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind = 'order_changed'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(logged, 4);
    }

    #[test]
    fn a_bad_order_call_is_named_and_changes_nothing() {
        let (conn, tmp) = env();
        let a = task(&conn, &tmp, "a");
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "order_set", &format!(r#"{{"task_ids":[{a}]}}"#)).unwrap();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "order_set", r#"{"task_ids":[404]}"#).unwrap_err();
        assert_eq!(e.kind, "not_found");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "order_set",
            &format!(r#"{{"task_ids":[{a},{a}]}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "order_move",
            &format!(r#"{{"task_id":{a},"before_task_id":404}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "order_drop", r#"{"task_id":404}"#).unwrap_err();
        assert_eq!(e.kind, "not_found");
        assert_eq!(order(&conn), [a]);
    }

    /// An evening run plans tomorrow, so its order lands there.
    #[test]
    fn the_nightly_sets_the_order_of_the_day_it_is_planning() {
        let (conn, tmp) = env();
        let a = task(&conn, &tmp, "a");
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "order_set",
            &format!(r#"{{"task_ids":[{a}]}}"#)).unwrap();
        assert_eq!(out["date"], today().tomorrow().unwrap().to_string());
        assert!(order(&conn).is_empty());
    }

    #[test]
    fn the_task_being_worked_on_stays_first() {
        let (conn, tmp) = env();
        let (a, b) = (task(&conn, &tmp, "essay"), task(&conn, &tmp, "laundry"));
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "order_set", &format!(r#"{{"task_ids":[{a},{b}]}}"#)).unwrap();
        conn.execute(
            "INSERT INTO work_sessions (user_id, task_id, title, started_at) VALUES (1, ?1, 'essay', '2026-10-07T09:00:00Z')",
            [a],
        )
        .unwrap();
        for (name, args) in [
            ("order_move", format!(r#"{{"task_id":{a}}}"#)),
            ("order_move", format!(r#"{{"task_id":{b},"before_task_id":{a}}}"#)),
            ("order_drop", format!(r#"{{"task_id":{a}}}"#)),
            ("order_set", format!(r#"{{"task_ids":[{b},{a}]}}"#)),
        ] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, name, &args).unwrap_err();
            assert_eq!(e.kind, "rejected", "{name} {args}");
        }
        assert_eq!(order(&conn), [a, b]);
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "order_drop", &format!(r#"{{"task_id":{b}}}"#)).unwrap();
        assert_eq!(order(&conn), [a]);
    }

    #[test]
    fn the_order_tools_reach_the_sessions_that_plan() {
        for kind in [SessionKind::Talk, SessionKind::Checkin, SessionKind::Nightly] {
            for name in ["order_set", "order_move", "order_drop"] {
                assert!(registry(kind).contains(&name), "{kind:?} {name}");
            }
        }
        for kind in [SessionKind::Share, SessionKind::Import, SessionKind::Call] {
            assert!(!registry(kind).contains(&"order_set"), "{kind:?}");
        }
    }
}
