use super::task_query::unscoped;
use super::{ToolCtx, ToolError};
use rusqlite::{Connection, OptionalExtension};
use schemars::JsonSchema;
use serde::Deserialize;

const MAX_TASKS: usize = 10;
const MAX_DAYS_AHEAD: i64 = 14;
const DEFAULT_BLOCK_MIN: i64 = 25;
const DEFAULT_GAP_MIN: u32 = 5;
const MAX_GAP_MIN: u32 = 120;
const MAX_KIND_CHARS: usize = 60;
const END_OF_DAY_MIN: i64 = 23 * 60 + 59;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanTasksArgs {
    /// The day to lay them out on, YYYY-MM-DD: today or one of the next 14 days.
    pub date: String,
    /// 1 to 10 task ids, laid out in the order given.
    pub task_ids: Vec<i64>,
    /// When the first block starts, zero-padded HH:MM.
    pub start: String,
    /// The latest the last block may end, zero-padded HH:MM. Omit for no ceiling.
    #[serde(default)]
    pub end: Option<String>,
    /// Minutes of air between one block and the next. Default 5.
    #[serde(default)]
    pub gap_min: Option<u32>,
}

struct Planned {
    id: i64,
    title: String,
    minutes: i64,
}

fn internal(e: impl std::fmt::Display) -> ToolError {
    ToolError::internal(e.to_string())
}

fn wall(minutes: i64) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// The day the user is living in, which is what "today or later" is measured
/// against.
fn today(ctx: &ToolCtx) -> jiff::civil::Date {
    let tz = crate::config::UserConfig::load(ctx.config_dir, ctx.username)
        .ok()
        .and_then(|c| jiff::tz::TimeZone::get(&c.timezone).ok())
        .unwrap_or(jiff::tz::TimeZone::UTC);
    jiff::Timestamp::now().to_zoned(tz).date()
}

/// The day's plan, generated from the user's template when the day has none —
/// the same row the nightly run would have made.
fn plan_row(conn: &Connection, ctx: &ToolCtx, date: jiff::civil::Date) -> Result<i64, ToolError> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM plans WHERE user_id = ?1 AND date = ?2",
            (ctx.user_id, date.to_string()),
            |r| r.get(0),
        )
        .optional()
        .map_err(internal)?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let template = crate::config::UserConfig::load(ctx.config_dir, ctx.username)
        .ok()
        .and_then(|c| crate::templates::Template::load(ctx.config_dir, ctx.username, &c.template).ok())
        .unwrap_or(crate::templates::Template { events: Vec::new() });
    crate::plan::generate(conn, ctx.user_id, &template, date).map_err(internal)
}

fn resolve_tasks(conn: &Connection, ctx: &ToolCtx, ids: &[i64]) -> Result<Vec<Planned>, ToolError> {
    ids.iter()
        .map(|id| {
            let Some(task) = crate::tasks::get(conn, ctx.user_id, *id).map_err(internal)? else {
                return Err(ToolError::not_found(format!("no task {id}; nothing was planned")));
            };
            Ok(Planned {
                id: task.id,
                title: task.title,
                minutes: task.duration_min.map_or(DEFAULT_BLOCK_MIN, i64::from),
            })
        })
        .collect()
}

/// Tasks that already hold a slot on that day; planning one twice is a mistake
/// the model should see rather than a second block.
fn already_planned(
    conn: &Connection,
    ctx: &ToolCtx,
    date: jiff::civil::Date,
    tasks: &[Planned],
) -> Result<Vec<String>, ToolError> {
    let mut found = Vec::new();
    for t in tasks {
        let linked: bool = conn
            .query_row(
                "SELECT EXISTS (
                    SELECT 1 FROM event_tasks et
                    JOIN events e ON e.id = et.event_id
                    JOIN plans p ON p.id = e.plan_id
                    WHERE p.user_id = ?1 AND p.date = ?2 AND et.task_id = ?3
                      AND e.status != 'dropped')",
                (ctx.user_id, date.to_string(), t.id),
                |r| r.get(0),
            )
            .map_err(internal)?;
        if linked {
            found.push(format!("{} ({})", t.id, t.title));
        }
    }
    Ok(found)
}

/// Everything already occupying time on that day, as half-open minute ranges.
fn occupied(
    conn: &Connection,
    ctx: &ToolCtx,
    date: jiff::civil::Date,
) -> Result<Vec<(String, i64, i64)>, ToolError> {
    let mut stmt = conn
        .prepare(
            "SELECT e.kind, e.wall_time, e.end_wall_time, e.span_min FROM events e
             JOIN plans p ON p.id = e.plan_id
             WHERE p.user_id = ?1 AND p.date = ?2 AND e.status != 'dropped'",
        )
        .map_err(internal)?;
    stmt.query_map((ctx.user_id, date.to_string()), |r| {
        let kind: String = r.get(0)?;
        let start: String = r.get(1)?;
        let end: Option<String> = r.get(2)?;
        let span: i64 = r.get(3)?;
        let from = crate::templates::wall_minutes(&start);
        let to = end.map_or(from + span, |e| crate::templates::wall_minutes(&e));
        Ok((format!("{kind} at {start}-{}", wall(to)), from, to))
    })
    .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
    .map_err(internal)
}

/// Lays the tasks out as consecutive blocks on one day's plan: each as long as
/// its own duration, in the order given, and never over something already
/// planned.
pub fn plan_tasks(
    conn: &Connection,
    ctx: &ToolCtx,
    args: PlanTasksArgs,
) -> Result<serde_json::Value, ToolError> {
    unscoped(ctx)?;
    let date: jiff::civil::Date = args.date.parse().map_err(|_| {
        ToolError::rejected(format!("date must be YYYY-MM-DD, got {:?}", args.date))
    })?;
    let today = today(ctx);
    let horizon = today
        .checked_add(jiff::Span::new().days(MAX_DAYS_AHEAD))
        .map_err(internal)?;
    if date < today || date > horizon {
        return Err(ToolError::rejected(format!(
            "date must be from {today} to {horizon}, got {date}"
        )));
    }
    if !crate::templates::valid_time(&args.start) {
        return Err(ToolError::rejected(format!(
            "start must be zero-padded HH:MM, got {:?}",
            args.start
        )));
    }
    let first = crate::templates::wall_minutes(&args.start);
    let ceiling = match &args.end {
        None => None,
        Some(end) if !crate::templates::valid_time(end) => {
            return Err(ToolError::rejected(format!(
                "end must be zero-padded HH:MM, got {end:?}"
            )))
        }
        Some(end) => {
            let last = crate::templates::wall_minutes(end);
            if last <= first {
                return Err(ToolError::rejected(format!(
                    "end must be after start, got {}-{end}",
                    args.start
                )));
            }
            Some(last)
        }
    };
    let gap = i64::from(args.gap_min.unwrap_or(DEFAULT_GAP_MIN));
    if gap > i64::from(MAX_GAP_MIN) {
        return Err(ToolError::rejected(format!("gap_min must be at most {MAX_GAP_MIN}")));
    }
    if args.task_ids.is_empty() || args.task_ids.len() > MAX_TASKS {
        return Err(ToolError::rejected(format!("task_ids must hold 1 to {MAX_TASKS} ids")));
    }
    let mut seen = std::collections::HashSet::new();
    if let Some(dup) = args.task_ids.iter().find(|id| !seen.insert(**id)) {
        return Err(ToolError::rejected(format!("task_ids names {dup} twice")));
    }

    let tasks = resolve_tasks(conn, ctx, &args.task_ids)?;
    let clashing = already_planned(conn, ctx, date, &tasks)?;
    if !clashing.is_empty() {
        return Err(ToolError::rejected(format!(
            "already on the plan for {date}: {}",
            clashing.join(", ")
        )));
    }

    let mut laid: Vec<(&Planned, i64, i64)> = Vec::new();
    let mut cursor = first;
    for (i, task) in tasks.iter().enumerate() {
        let end = cursor + task.minutes;
        if end > END_OF_DAY_MIN || ceiling.is_some_and(|c| end > c) {
            let left: Vec<&str> = tasks[i..].iter().map(|t| t.title.as_str()).collect();
            return Err(ToolError::rejected(format!(
                "{} does not fit before {}: {}",
                if left.len() == 1 { "one task" } else { "some tasks" },
                ceiling.map_or_else(|| "midnight".into(), wall),
                left.join(", ")
            )));
        }
        laid.push((task, cursor, end));
        cursor = end + gap;
    }

    let plan_id = plan_row(conn, ctx, date)?;
    let mut clashes: Vec<String> = occupied(conn, ctx, date)?
        .into_iter()
        .filter(|(_, from, to)| laid.iter().any(|(_, s, e)| s < to && from < e))
        .map(|(label, _, _)| label)
        .collect();
    clashes.sort();
    clashes.dedup();
    if !clashes.is_empty() {
        return Err(ToolError::rejected(format!(
            "these would overlap what is already planned for {date}: {}",
            clashes.join(", ")
        )));
    }

    let mut events = Vec::new();
    for (task, start, end) in laid {
        let kind: String = task.title.chars().take(MAX_KIND_CHARS).collect();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, end_wall_time,
                                 flexibility, slide_window_min, channel, alert, span_min)
             VALUES (?1, ?2, ?3, ?3, ?4, 'slide', 0, 'push', 0, ?5)",
            (plan_id, &kind, wall(start), wall(end), task.minutes),
        )
        .map_err(internal)?;
        let event_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO event_tasks (event_id, task_id) VALUES (?1, ?2)",
            (event_id, task.id),
        )
        .map_err(internal)?;
        events.push(serde_json::json!({
            "event_id": event_id,
            "task_id": task.id,
            "start": wall(start),
            "end": wall(end),
        }));
    }
    Ok(serde_json::json!({ "plan_date": date.to_string(), "events": events }))
}

#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, registry, PreparedVectors, SessionKind, ToolCtx, ToolError};
    use rusqlite::Connection;
    use serde_json::Value;

    fn env() -> (Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir, scope: Option<i64>) -> ToolCtx<'a> {
        ToolCtx {
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            user_id: 1,
            username: "aki",
            vectors: PreparedVectors::default(),
            task_scope: scope,
        }
    }

    fn call(
        conn: &Connection,
        tmp: &tempfile::TempDir,
        name: &str,
        args: &str,
    ) -> Result<Value, ToolError> {
        dispatch(conn, &ctx(tmp, None), SessionKind::Talk, name, args)
    }

    fn task(conn: &Connection, tmp: &tempfile::TempDir, args: &str) -> i64 {
        call(conn, tmp, "task_create", args).unwrap()["task_id"].as_i64().unwrap()
    }

    /// The tools resolve "today" in the user's zone, and a config-less test
    /// user is in UTC.
    fn today() -> jiff::civil::Date {
        jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date()
    }

    fn tomorrow() -> jiff::civil::Date {
        today().tomorrow().unwrap()
    }

    fn every_day() -> Vec<String> {
        ["mon", "tue", "wed", "thu", "fri", "sat", "sun"].iter().map(|d| (*d).into()).collect()
    }

    /// A plan for `date` holding a 09:00 routine and a 13:00-15:00 block.
    fn seed_plan(conn: &Connection, date: jiff::civil::Date) {
        let tmpl = crate::templates::Template {
            events: vec![
                crate::templates::TemplateEvent {
                    kind: "morning checkin".into(),
                    time: "09:00".into(),
                    days: every_day(),
                    channel: "push".into(),
                    ..Default::default()
                },
                crate::templates::TemplateEvent {
                    kind: "deep work".into(),
                    time: "13:00".into(),
                    days: every_day(),
                    entry: crate::templates::Entry::Block,
                    end_time: Some("15:00".into()),
                    channel: "push".into(),
                    ..Default::default()
                },
            ],
        };
        crate::plan::generate(conn, 1, &tmpl, date).unwrap();
    }

    fn events_on(conn: &Connection, date: jiff::civil::Date) -> Vec<(String, String, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT e.kind, e.wall_time, COALESCE(e.end_wall_time, '') FROM events e
                 JOIN plans p ON p.id = e.plan_id
                 WHERE p.user_id = 1 AND p.date = ?1 ORDER BY e.wall_time, e.id",
            )
            .unwrap();
        stmt.query_map([date.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn plan_tasks_lays_the_tasks_out_back_to_back_and_creates_the_plan() {
        let (conn, tmp) = env();
        let short = task(&conn, &tmp, r#"{"title":"call dentist"}"#);
        let long = task(&conn, &tmp, r#"{"title":"email landlord","duration_min":45}"#);
        let date = tomorrow();

        let out = call(
            &conn,
            &tmp,
            "plan_tasks",
            &format!(r#"{{"date":"{date}","task_ids":[{short},{long}],"start":"09:00"}}"#),
        )
        .unwrap();
        assert_eq!(out["plan_date"], date.to_string());
        let laid = out["events"].as_array().unwrap();
        assert_eq!(laid.len(), 2);
        assert_eq!(laid[0]["task_id"], short);
        assert_eq!((&laid[0]["start"], &laid[0]["end"]), (&"09:00".into(), &"09:25".into()));
        assert_eq!(laid[1]["task_id"], long);
        assert_eq!((&laid[1]["start"], &laid[1]["end"]), (&"09:30".into(), &"10:15".into()));

        let plans: i64 = conn
            .query_row("SELECT COUNT(*) FROM plans WHERE date = ?1", [date.to_string()], |r| r.get(0))
            .unwrap();
        assert_eq!(plans, 1, "the day's plan row was created");
        assert_eq!(
            events_on(&conn, date),
            vec![
                ("call dentist".into(), "09:00".into(), "09:25".into()),
                ("email landlord".into(), "09:30".into(), "10:15".into()),
            ]
        );

        for (i, event) in laid.iter().enumerate() {
            let id = event["event_id"].as_i64().unwrap();
            let (alert, span, flex, channel): (i64, i64, String, String) = conn
                .query_row(
                    "SELECT alert, span_min, flexibility, channel FROM events WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
            assert_eq!(alert, 0, "a block never pings");
            assert_eq!(span, if i == 0 { 25 } else { 45 });
            assert_eq!((flex.as_str(), channel.as_str()), ("slide", "push"));
            let linked: i64 = conn
                .query_row("SELECT task_id FROM event_tasks WHERE event_id = ?1", [id], |r| r.get(0))
                .unwrap();
            assert_eq!(linked, event["task_id"].as_i64().unwrap());
        }
    }

    #[test]
    fn planned_blocks_never_reach_the_delivery_sweep() {
        let (conn, tmp) = env();
        let id = task(&conn, &tmp, r#"{"title":"call dentist"}"#);
        let date = today();
        call(
            &conn,
            &tmp,
            "plan_tasks",
            &format!(r#"{{"date":"{date}","task_ids":[{id}],"start":"00:05"}}"#),
        )
        .unwrap();
        let end_of_day = date
            .at(23, 59, 0, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap()
            .timestamp();
        let fired = crate::runner::fire_due(&conn, tmp.path(), end_of_day).unwrap();
        assert!(fired.is_empty(), "a block was delivered: {fired:?}");
    }

    #[test]
    fn plan_tasks_rejects_what_does_not_fit_before_the_end() {
        let (conn, tmp) = env();
        let first = task(&conn, &tmp, r#"{"title":"call dentist"}"#);
        let second = task(&conn, &tmp, r#"{"title":"email landlord"}"#);
        let date = tomorrow();

        let e = call(
            &conn,
            &tmp,
            "plan_tasks",
            &format!(
                r#"{{"date":"{date}","task_ids":[{first},{second}],"start":"09:00","end":"09:30"}}"#
            ),
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");
        assert!(e.message.contains("email landlord"), "names what did not fit: {}", e.message);
        assert!(events_on(&conn, date).is_empty());
        let plans: i64 =
            conn.query_row("SELECT COUNT(*) FROM plans", [], |r| r.get(0)).unwrap();
        assert_eq!(plans, 0, "a rejected call leaves no plan row behind");
    }

    #[test]
    fn plan_tasks_refuses_to_overlap_what_is_already_on_the_day() {
        let (conn, tmp) = env();
        let date = tomorrow();
        seed_plan(&conn, date);
        let id = task(&conn, &tmp, r#"{"title":"call dentist"}"#);

        for (start, clash) in [("09:10", "morning checkin"), ("14:00", "deep work")] {
            let e = call(
                &conn,
                &tmp,
                "plan_tasks",
                &format!(r#"{{"date":"{date}","task_ids":[{id}],"start":"{start}"}}"#),
            )
            .unwrap_err();
            assert_eq!(e.kind, "rejected", "{start}");
            assert!(e.message.contains(clash), "names the clash: {}", e.message);
        }
        assert_eq!(events_on(&conn, date).len(), 2, "nothing was added");

        call(
            &conn,
            &tmp,
            "plan_tasks",
            &format!(r#"{{"date":"{date}","task_ids":[{id}],"start":"10:00"}}"#),
        )
        .unwrap();
        assert_eq!(events_on(&conn, date).len(), 3, "a free slot is taken");
    }

    #[test]
    fn a_task_is_planned_on_a_day_only_once() {
        let (conn, tmp) = env();
        let date = tomorrow();
        let id = task(&conn, &tmp, r#"{"title":"call dentist"}"#);
        call(
            &conn,
            &tmp,
            "plan_tasks",
            &format!(r#"{{"date":"{date}","task_ids":[{id}],"start":"09:00"}}"#),
        )
        .unwrap();

        let e = call(
            &conn,
            &tmp,
            "plan_tasks",
            &format!(r#"{{"date":"{date}","task_ids":[{id}],"start":"11:00"}}"#),
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");
        assert!(e.message.contains(&id.to_string()), "names the task: {}", e.message);
        assert_eq!(events_on(&conn, date).len(), 1);

        let other = tomorrow().tomorrow().unwrap();
        call(
            &conn,
            &tmp,
            "plan_tasks",
            &format!(r#"{{"date":"{other}","task_ids":[{id}],"start":"09:00"}}"#),
        )
        .unwrap();
    }

    #[test]
    fn plan_tasks_checks_its_day_its_times_and_its_tasks() {
        let (conn, tmp) = env();
        let id = task(&conn, &tmp, r#"{"title":"call dentist"}"#);
        let date = tomorrow();
        let yesterday = today().yesterday().unwrap();
        let far = today().checked_add(jiff::Span::new().days(15)).unwrap();
        let eleven = (0..11).map(|_| id.to_string()).collect::<Vec<_>>().join(",");

        for (args, why) in [
            (format!(r#"{{"date":"{yesterday}","task_ids":[{id}],"start":"09:00"}}"#), "the past"),
            (format!(r#"{{"date":"{far}","task_ids":[{id}],"start":"09:00"}}"#), "past the horizon"),
            (format!(r#"{{"date":"soon","task_ids":[{id}],"start":"09:00"}}"#), "not a date"),
            (format!(r#"{{"date":"{date}","task_ids":[{id}],"start":"9:00"}}"#), "unpadded start"),
            (
                format!(r#"{{"date":"{date}","task_ids":[{id}],"start":"09:00","end":"08:00"}}"#),
                "end before start",
            ),
            (format!(r#"{{"date":"{date}","task_ids":[],"start":"09:00"}}"#), "no tasks"),
            (format!(r#"{{"date":"{date}","task_ids":[{eleven}],"start":"09:00"}}"#), "too many"),
            (
                format!(r#"{{"date":"{date}","task_ids":[{id},{id}],"start":"09:00"}}"#),
                "the same task twice",
            ),
            (
                format!(r#"{{"date":"{date}","task_ids":[{id}],"start":"09:00","gap_min":600}}"#),
                "an absurd gap",
            ),
            (
                format!(r#"{{"date":"{date}","task_ids":[{id}],"start":"23:50"}}"#),
                "past midnight",
            ),
        ] {
            assert_eq!(
                call(&conn, &tmp, "plan_tasks", &args).unwrap_err().kind,
                "rejected",
                "{why}"
            );
        }

        let e = call(
            &conn,
            &tmp,
            "plan_tasks",
            &format!(r#"{{"date":"{date}","task_ids":[{id},9999],"start":"09:00"}}"#),
        )
        .unwrap_err();
        assert_eq!(e.kind, "not_found");
        assert!(e.message.contains("9999"));
        assert!(events_on(&conn, date).is_empty());
    }

    #[test]
    fn plan_tasks_is_closed_to_a_scoped_session() {
        let (conn, tmp) = env();
        let id = task(&conn, &tmp, r#"{"title":"biology ch.4"}"#);
        let date = tomorrow();
        let e = dispatch(
            &conn,
            &ctx(&tmp, Some(id)),
            SessionKind::Talk,
            "plan_tasks",
            &format!(r#"{{"date":"{date}","task_ids":[{id}],"start":"09:00"}}"#),
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");
        assert!(events_on(&conn, date).is_empty());
    }

    #[test]
    fn plan_tasks_reaches_talk_and_nightly_only() {
        for kind in [SessionKind::Talk, SessionKind::Nightly] {
            assert!(registry(kind).contains(&"plan_tasks"), "{kind:?}");
        }
        for kind in [SessionKind::Checkin, SessionKind::Import] {
            assert!(!registry(kind).contains(&"plan_tasks"), "{kind:?}");
        }
        let (conn, tmp) = env();
        let id = task(&conn, &tmp, r#"{"title":"call dentist"}"#);
        let e = dispatch(
            &conn,
            &ctx(&tmp, None),
            SessionKind::Checkin,
            "plan_tasks",
            &format!(r#"{{"date":"{}","task_ids":[{id}],"start":"09:00"}}"#, tomorrow()),
        )
        .unwrap_err();
        assert_eq!(e.kind, "forbidden");
    }
}
