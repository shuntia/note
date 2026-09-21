use super::{check_text, ToolCtx, ToolError};
use crate::tasks::{Actor, NewTask, Step, TaskPatch, UpdateError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

const MAX_TITLE_BYTES: usize = 500;

/// Trims and length-checks a title; shared so `create` and `update` cannot drift.
fn checked_title(title: &str) -> Result<&str, ToolError> {
    let title = title.trim();
    if title.is_empty() || title.len() > MAX_TITLE_BYTES {
        return Err(ToolError::rejected(format!(
            "title must be 1..={MAX_TITLE_BYTES} bytes"
        )));
    }
    Ok(title)
}

/// In a scoped session the model may only reach the scoped task itself, and —
/// when `steps_too` — its steps. Unscoped sessions reach everything.
fn in_scope(
    conn: &Connection,
    ctx: &ToolCtx,
    task_id: i64,
    steps_too: bool,
) -> Result<(), ToolError> {
    let Some(scope) = ctx.task_scope else { return Ok(()) };
    if task_id == scope {
        return Ok(());
    }
    let parent = steps_too
        .then(|| crate::tasks::get(conn, ctx.user_id, task_id))
        .transpose()
        .map_err(|e| ToolError::internal(e.to_string()))?
        .flatten()
        .and_then(|t| t.parent_id);
    if parent == Some(scope) {
        return Ok(());
    }
    Err(ToolError::rejected(format!(
        "this session may only change task {scope}{}",
        if steps_too { " and its steps" } else { "" }
    )))
}

/// The timezone the user's days are measured in; a bare day means the end of
/// one of those.
fn user_tz(ctx: &ToolCtx) -> jiff::tz::TimeZone {
    crate::config::UserConfig::load(ctx.config_dir, ctx.username)
        .ok()
        .and_then(|c| jiff::tz::TimeZone::get(&c.timezone).ok())
        .unwrap_or(jiff::tz::TimeZone::UTC)
}

/// An instant stays one; a bare `YYYY-MM-DD` becomes the end of that day where
/// the user lives; an empty string clears the date.
fn checked_due(ctx: &ToolCtx, raw: &str) -> Result<Option<String>, ToolError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    if let Ok(ts) = raw.parse::<jiff::Timestamp>() {
        return Ok(Some(ts.to_string()));
    }
    let day: jiff::civil::Date = raw
        .parse()
        .map_err(|_| ToolError::rejected(format!(
            "due_at must be an RFC 3339 instant or a YYYY-MM-DD day, got {raw:?}"
        )))?;
    let end = day
        .at(23, 59, 0, 0)
        .to_zoned(user_tz(ctx))
        .map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(Some(end.timestamp().to_string()))
}

fn task_error(e: UpdateError) -> ToolError {
    match e {
        UpdateError::InvalidState(s) => ToolError::rejected(format!("invalid state: {s}")),
        UpdateError::InvalidDuration(m)
        | UpdateError::InvalidHierarchy(m)
        | UpdateError::NowFull(m)
        | UpdateError::Invalid(m)
        | UpdateError::ExternalIdTaken(m) => ToolError::rejected(m),
        UpdateError::Db(e) => ToolError::internal(e.to_string()),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateArgs {
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// Rough estimate in whole 5-minute blocks.
    #[serde(default)]
    pub duration_min: Option<u32>,
    /// Put the task straight in Now, the user's short list of at most 3.
    #[serde(default)]
    pub is_now: bool,
    /// When it is due: an RFC 3339 instant, or a bare YYYY-MM-DD day, which
    /// means the end of that day where the user lives.
    #[serde(default)]
    pub due_at: Option<String>,
    /// How the block holding this task announces itself when it starts: none,
    /// chat, or notify.
    #[serde(default)]
    pub notify: Option<String>,
    /// How far along it already is, 0 to 100. Leave it out for work not started.
    #[serde(default)]
    pub progress: Option<u32>,
}

pub fn create(
    conn: &Connection,
    ctx: &ToolCtx,
    args: CreateArgs,
) -> Result<serde_json::Value, ToolError> {
    let title = checked_title(&args.title)?;
    check_text("description", &args.description)?;
    let due_at = args.due_at.as_deref().map(|raw| checked_due(ctx, raw)).transpose()?.flatten();
    let task = crate::tasks::create(
        conn,
        ctx.user_id,
        NewTask {
            title: title.to_owned(),
            duration_min: args.duration_min,
            is_now: args.is_now,
            due_at: due_at.map(Some),
            notify: args.notify,
            progress: args.progress,
            ..NewTask::default()
        },
        "agent",
        Actor::Agent,
    )
    .map_err(task_error)?;
    if !args.description.is_empty() {
        crate::tasks::update(
            conn,
            ctx.user_id,
            task.id,
            TaskPatch {
                description: Some(args.description),
                ..Default::default()
            },
        )
        .map_err(task_error)?;
    }
    Ok(serde_json::json!({ "task_id": task.id }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateArgs {
    pub task_id: i64,
    pub title: Option<String>,
    pub description: Option<String>,
    /// One of open, in_progress, done, dropped.
    pub state: Option<String>,
    pub notes: Option<String>,
    /// Rough estimate in whole 5-minute blocks.
    pub duration_min: Option<u32>,
    /// True moves the task into Now, false moves it back to Later.
    pub is_now: Option<bool>,
    /// When it is due: an RFC 3339 instant, or a bare YYYY-MM-DD day, which
    /// means the end of that day where the user lives. An empty string clears it.
    pub due_at: Option<String>,
    /// How the block holding this task announces itself when it starts: none,
    /// chat, or notify.
    pub notify: Option<String>,
    /// How far along it is, 0 to 100. Finishing a task fills it on its own.
    pub progress: Option<u32>,
}

pub fn update(
    conn: &Connection,
    ctx: &ToolCtx,
    args: UpdateArgs,
) -> Result<serde_json::Value, ToolError> {
    in_scope(conn, ctx, args.task_id, true)?;
    if let Some(scope) = ctx.task_scope {
        if args.is_now.is_some() {
            return Err(ToolError::rejected("this session cannot move a task in or out of Now"));
        }
        if let Some(wanted) = args.state.as_deref() {
            if wanted != "dropped" {
                return Err(ToolError::rejected(format!(
                    "this session may only drop a task, not set its state to {wanted}"
                )));
            }
            let state = crate::tasks::get(conn, ctx.user_id, scope)
                .map_err(|e| ToolError::internal(e.to_string()))?
                .map(|t| t.state)
                .unwrap_or_default();
            if state == "in_progress" || state == "done" {
                return Err(ToolError::rejected(format!(
                    "task {scope} is already {state}; only an open task can be dropped"
                )));
            }
        }
    }
    let title = match &args.title {
        Some(t) => Some(checked_title(t)?.to_owned()),
        None => None,
    };
    if let Some(d) = &args.description {
        check_text("description", d)?;
    }
    if let Some(n) = &args.notes {
        check_text("notes", n)?;
    }
    let due_at = match &args.due_at {
        Some(raw) => Some(checked_due(ctx, raw)?),
        None => None,
    };
    let patch = TaskPatch {
        title,
        description: args.description,
        state: args.state,
        notes: args.notes,
        duration_min: args.duration_min.map(Some),
        is_now: args.is_now,
        due_at,
        notify: args.notify,
        progress: args.progress,
        actor: Actor::Agent,
        ..Default::default()
    };
    match crate::tasks::update(conn, ctx.user_id, args.task_id, patch) {
        Ok(Some(t)) => Ok(serde_json::json!({
            "task_id": t.task.id,
            "state": t.task.state,
            "is_now": t.task.is_now,
            "progress": t.task.progress,
            "remaining_min": t.task.remaining_min,
            "demoted_from_now": t.demoted_from_now,
        })),
        Ok(None) => Err(ToolError::not_found(format!("no task {}", args.task_id))),
        Err(e) => Err(task_error(e)),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteArgs {
    pub task_id: i64,
}

pub fn delete(
    conn: &Connection,
    ctx: &ToolCtx,
    args: DeleteArgs,
) -> Result<serde_json::Value, ToolError> {
    in_scope(conn, ctx, args.task_id, true)?;
    match crate::tasks::delete_within(conn, ctx.user_id, args.task_id) {
        Ok(true) => Ok(serde_json::json!({ "task_id": args.task_id, "deleted": true })),
        Ok(false) => Err(ToolError::not_found(format!("no task {}", args.task_id))),
        Err(e) => Err(ToolError::internal(e.to_string())),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SplitArgs {
    pub task_id: i64,
    /// 2 to 5 steps, each with a duration in whole 5-minute blocks.
    pub steps: Vec<Step>,
}

pub fn split(
    conn: &Connection,
    ctx: &ToolCtx,
    args: SplitArgs,
) -> Result<serde_json::Value, ToolError> {
    in_scope(conn, ctx, args.task_id, false)?;
    for s in &args.steps {
        checked_title(&s.title)?;
    }
    match crate::tasks::split(conn, ctx.user_id, args.task_id, args.steps, Actor::Agent) {
        Ok(Some(n)) => Ok(serde_json::json!({
            "task_id": n.task.id,
            "duration_min": n.task.duration_min,
            "step_ids": n.children.iter().map(|c| c.id).collect::<Vec<_>>(),
        })),
        Ok(None) => Err(ToolError::not_found(format!("no task {}", args.task_id))),
        Err(e) => Err(task_error(e)),
    }
}

const MAX_BRIEF_DESCRIPTION: usize = 2 * 1024;
const MAX_REASON_CHARS: usize = 100;
const MAX_STEP_TITLE_CHARS: usize = 100;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BriefArgs {
    pub task_id: i64,
    /// False drops the task as not homework; true writes the brief.
    pub homework: bool,
    /// Why it is not homework, under 100 characters. Required when homework is false.
    #[serde(default)]
    pub reason: Option<String>,
    /// The brief: at most 6 short plain-text lines, no blank lines, no markdown.
    #[serde(default)]
    pub description: Option<String>,
    /// Focused minutes, in whole 5-minute blocks.
    #[serde(default)]
    pub duration_min: Option<u32>,
    /// 2 to 5 steps, each under 100 characters. Ignored when the task already has steps.
    #[serde(default)]
    pub steps: Option<Vec<Step>>,
}

/// The whole import session in one call: either the task is dropped as not
/// homework, or it gets its description, duration and — for a task with no
/// steps yet — its steps.
pub fn brief(
    conn: &Connection,
    ctx: &ToolCtx,
    args: BriefArgs,
) -> Result<serde_json::Value, ToolError> {
    in_scope(conn, ctx, args.task_id, false)?;
    let Some(node) = crate::tasks::node(conn, ctx.user_id, args.task_id).map_err(task_error)?
    else {
        return Err(ToolError::not_found(format!("no task {}", args.task_id)));
    };
    if !args.homework {
        return drop_as_not_homework(conn, ctx, &node.task, args.reason.as_deref().unwrap_or(""));
    }

    let description = match args.description {
        Some(d) => {
            if d.len() > MAX_BRIEF_DESCRIPTION {
                return Err(ToolError::rejected(format!(
                    "description must be at most {MAX_BRIEF_DESCRIPTION} bytes"
                )));
            }
            if d.trim().is_empty() {
                return Err(ToolError::rejected("description must say something"));
            }
            Some(d)
        }
        None => None,
    };
    if description.is_some() || args.duration_min.is_some() {
        patch(
            conn,
            ctx,
            args.task_id,
            TaskPatch {
                description,
                duration_min: args.duration_min.map(Some),
                actor: Actor::Agent,
                ..Default::default()
            },
        )?;
    }

    let steps_applied = if !node.children.is_empty() {
        serde_json::json!("kept")
    } else if let Some(steps) = args.steps {
        let steps = steps
            .into_iter()
            .map(|s| {
                Ok(Step { title: checked_step_title(&s.title)?.to_owned(), ..s })
            })
            .collect::<Result<Vec<_>, ToolError>>()?;
        let n = steps.len();
        match crate::tasks::split(conn, ctx.user_id, args.task_id, steps, Actor::Agent) {
            Ok(Some(_)) => serde_json::json!(n),
            Ok(None) => return Err(ToolError::not_found(format!("no task {}", args.task_id))),
            Err(e) => return Err(task_error(e)),
        }
    } else {
        serde_json::json!(0)
    };
    Ok(serde_json::json!({
        "task_id": args.task_id,
        "outcome": "briefed",
        "steps_applied": steps_applied,
    }))
}

fn drop_as_not_homework(
    conn: &Connection,
    ctx: &ToolCtx,
    task: &crate::tasks::Task,
    reason: &str,
) -> Result<serde_json::Value, ToolError> {
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > MAX_REASON_CHARS {
        return Err(ToolError::rejected(format!(
            "homework=false needs a reason of 1 to {MAX_REASON_CHARS} characters"
        )));
    }
    if task.state != "open" {
        return Err(ToolError::rejected(format!(
            "task {} is already {}; only an open task can be dropped",
            task.id, task.state
        )));
    }
    patch(
        conn,
        ctx,
        task.id,
        TaskPatch {
            description: Some(format!("Not homework: {reason}")),
            state: Some("dropped".into()),
            actor: Actor::Agent,
            ..Default::default()
        },
    )?;
    Ok(serde_json::json!({ "task_id": task.id, "outcome": "dropped", "steps_applied": 0 }))
}

fn checked_step_title(title: &str) -> Result<&str, ToolError> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > MAX_STEP_TITLE_CHARS {
        return Err(ToolError::rejected(format!(
            "each step title must be 1 to {MAX_STEP_TITLE_CHARS} characters"
        )));
    }
    Ok(title)
}

fn patch(conn: &Connection, ctx: &ToolCtx, task_id: i64, p: TaskPatch) -> Result<(), ToolError> {
    match crate::tasks::update(conn, ctx.user_id, task_id, p) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(ToolError::not_found(format!("no task {task_id}"))),
        Err(e) => Err(task_error(e)),
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, SessionKind, ToolCtx};

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        for title in ["scoped", "someone elses"] {
            crate::tasks::create(
                &conn,
                1,
                crate::tasks::NewTask {
                    title: title.into(),
                    ..crate::tasks::NewTask::default()
                },
                "manual",
                crate::tasks::Actor::User,
            )
            .unwrap();
        }
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir, scope: Option<i64>) -> ToolCtx<'a> {
        ToolCtx {
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            user_id: 1,
            username: "aki",
            vectors: crate::tools::PreparedVectors::default(),
            task_scope: scope,
            inbox_source: None,
            memory_source: None,
        }
    }

    /// A config dir whose user lives in `tz`, which is what a bare day means.
    fn tz_dir(tz: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("defaults")).unwrap();
        std::fs::write(
            tmp.path().join("defaults/user.toml"),
            format!("display_name = \"X\"\ntimezone = \"{tz}\"\ntemplate = \"default\"\n"),
        )
        .unwrap();
        tmp
    }

    fn due_of(conn: &rusqlite::Connection, id: i64) -> Option<String> {
        crate::tasks::get(conn, 1, id).unwrap().unwrap().due_at
    }

    #[test]
    fn a_due_date_is_an_instant_or_the_end_of_a_day_where_the_user_lives() {
        let (conn, _tmp) = env();
        let tmp = tz_dir("Asia/Tokyo");
        let call = |args: &str| {
            dispatch(&conn, &ctx(&tmp, None), SessionKind::Talk, "task_create", args)
        };
        let id = call(r#"{"title":"essay","due_at":"2026-09-19T23:59:00+09:00"}"#).unwrap()
            ["task_id"]
            .as_i64()
            .unwrap();
        assert_eq!(due_of(&conn, id).as_deref(), Some("2026-09-19T14:59:00Z"));

        let bare = call(r#"{"title":"worksheet","due_at":"2026-09-19"}"#).unwrap()["task_id"]
            .as_i64()
            .unwrap();
        assert_eq!(
            due_of(&conn, bare).as_deref(),
            Some("2026-09-19T14:59:00Z"),
            "a bare day is the end of that day in Tokyo"
        );

        dispatch(
            &conn,
            &ctx(&tmp, None),
            SessionKind::Talk,
            "task_update",
            &format!(r#"{{"task_id":{bare},"due_at":"2026-09-20"}}"#),
        )
        .unwrap();
        assert_eq!(due_of(&conn, bare).as_deref(), Some("2026-09-20T14:59:00Z"));

        dispatch(
            &conn,
            &ctx(&tmp, None),
            SessionKind::Talk,
            "task_update",
            &format!(r#"{{"task_id":{bare},"due_at":""}}"#),
        )
        .unwrap();
        assert_eq!(due_of(&conn, bare), None, "an empty string clears it");

        assert_eq!(call(r#"{"title":"x","due_at":"friday"}"#).unwrap_err().kind, "rejected");
    }

    #[test]
    fn a_scoped_session_cannot_delete_a_task_outside_its_scope() {
        let (conn, tmp) = env();
        let e = dispatch(
            &conn,
            &ctx(&tmp, Some(1)),
            SessionKind::Talk,
            "task_delete",
            r#"{"task_id":2}"#,
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");
        assert!(crate::tasks::get(&conn, 1, 2).unwrap().is_some());

        dispatch(&conn, &ctx(&tmp, Some(1)), SessionKind::Talk, "task_delete", r#"{"task_id":1}"#)
            .unwrap();
        assert!(crate::tasks::get(&conn, 1, 1).unwrap().is_none());
    }

    #[test]
    fn an_unscoped_session_deletes_any_of_the_users_tasks() {
        let (conn, tmp) = env();
        dispatch(&conn, &ctx(&tmp, None), SessionKind::Talk, "task_delete", r#"{"task_id":2}"#)
            .unwrap();
        assert!(crate::tasks::get(&conn, 1, 2).unwrap().is_none());
    }

    fn brief(
        conn: &rusqlite::Connection,
        tmp: &tempfile::TempDir,
        args: &str,
    ) -> Result<serde_json::Value, crate::tools::ToolError> {
        dispatch(conn, &ctx(tmp, Some(1)), SessionKind::Import, "task_brief", args)
    }

    fn description(conn: &rusqlite::Connection, id: i64) -> String {
        crate::tasks::get(conn, 1, id).unwrap().unwrap().description
    }

    fn split_it(conn: &rusqlite::Connection, id: i64) {
        crate::tasks::split(
            conn,
            1,
            id,
            vec![
                crate::tasks::Step { title: "draft".into(), duration_min: 20 },
                crate::tasks::Step { title: "edit".into(), duration_min: 10 },
            ],
            crate::tasks::Actor::User,
        )
        .unwrap();
    }

    #[test]
    fn one_brief_writes_the_description_the_duration_and_the_steps() {
        let (conn, tmp) = env();
        let out = brief(
            &conn,
            &tmp,
            r#"{"task_id":1,"homework":true,"description":"Read chapter 4.\nHand in: worksheet.",
                "duration_min":45,
                "steps":[{"title":"read chapter 4","duration_min":25},
                         {"title":"answer the questions","duration_min":20}]}"#,
        )
        .unwrap();
        assert_eq!(out["task_id"], 1);
        assert_eq!(out["outcome"], "briefed");
        assert_eq!(out["steps_applied"], 2);

        let node = crate::tasks::node(&conn, 1, 1).unwrap().unwrap();
        assert!(node.task.description.starts_with("Read chapter 4."));
        assert_eq!(node.task.duration_min, Some(45));
        assert_eq!(node.task.duration_source, "agent");
        assert_eq!(node.task.state, "open");
        assert_eq!(node.task.title, "scoped");
        assert!(!node.task.is_now);
        assert_eq!(node.children.len(), 2);
    }

    #[test]
    fn not_homework_drops_the_task_with_its_reason() {
        let (conn, tmp) = env();
        let out = brief(
            &conn,
            &tmp,
            r#"{"task_id":1,"homework":false,"reason":"class calendar, nothing to hand in",
                "description":"ignored","duration_min":30}"#,
        )
        .unwrap();
        assert_eq!(out["outcome"], "dropped");
        assert_eq!(out["steps_applied"], 0);

        let task = crate::tasks::get(&conn, 1, 1).unwrap().unwrap();
        assert_eq!(task.state, "dropped");
        assert_eq!(task.description, "Not homework: class calendar, nothing to hand in");
        assert_eq!(task.duration_min, None);
    }

    #[test]
    fn not_homework_needs_a_short_reason() {
        let (conn, tmp) = env();
        for args in [
            r#"{"task_id":1,"homework":false}"#,
            r#"{"task_id":1,"homework":false,"reason":"   "}"#,
        ] {
            assert_eq!(brief(&conn, &tmp, args).unwrap_err().kind, "rejected", "{args}");
        }
        let long = format!(
            r#"{{"task_id":1,"homework":false,"reason":"{}"}}"#,
            "why".repeat(40)
        );
        assert_eq!(brief(&conn, &tmp, &long).unwrap_err().kind, "rejected");
        assert_eq!(crate::tasks::get(&conn, 1, 1).unwrap().unwrap().state, "open");
    }

    #[test]
    fn a_started_task_is_briefed_never_dropped() {
        let (conn, tmp) = env();
        for state in ["in_progress", "done"] {
            crate::tasks::update(
                &conn,
                1,
                1,
                crate::tasks::TaskPatch { state: Some(state.into()), ..Default::default() },
            )
            .unwrap();
            let e = brief(&conn, &tmp, r#"{"task_id":1,"homework":false,"reason":"a rubric"}"#)
                .unwrap_err();
            assert_eq!(e.kind, "rejected", "dropped a task that was {state}");
            assert_eq!(crate::tasks::get(&conn, 1, 1).unwrap().unwrap().state, state);
        }
        brief(&conn, &tmp, r#"{"task_id":1,"homework":true,"description":"still work"}"#).unwrap();
        assert_eq!(crate::tasks::get(&conn, 1, 1).unwrap().unwrap().state, "done");
    }

    #[test]
    fn existing_steps_are_kept_and_the_new_ones_ignored() {
        let (conn, tmp) = env();
        split_it(&conn, 1);
        let out = brief(
            &conn,
            &tmp,
            r#"{"task_id":1,"homework":true,"description":"a fresher brief",
                "steps":[{"title":"a","duration_min":5},{"title":"b","duration_min":5}]}"#,
        )
        .unwrap();
        assert_eq!(out["steps_applied"], "kept");
        let node = crate::tasks::node(&conn, 1, 1).unwrap().unwrap();
        assert_eq!(node.task.description, "a fresher brief");
        assert_eq!(node.children.len(), 2);
        assert_eq!(node.children[0].title, "draft");
    }

    #[test]
    fn a_blank_description_or_an_odd_duration_is_rejected() {
        let (conn, tmp) = env();
        for args in [
            r#"{"task_id":1,"homework":true,"description":"\n  \n"}"#,
            r#"{"task_id":1,"homework":true,"description":"fine","duration_min":23}"#,
            r#"{"task_id":1,"homework":true,"description":"fine","surprise":1}"#,
        ] {
            assert!(brief(&conn, &tmp, args).is_err(), "{args}");
        }
        assert_eq!(description(&conn, 1), "");
    }

    #[test]
    fn steps_are_two_to_five_short_titles_in_five_minute_blocks() {
        let (conn, tmp) = env();
        let one = r#"{"title":"only","duration_min":5}"#;
        let long = format!(r#"{{"title":"{}","duration_min":5}}"#, "x".repeat(101));
        for steps in [
            one.to_string(),
            [one; 6].join(","),
            format!("{one},{long}"),
            format!(r#"{one},{{"title":"odd","duration_min":7}}"#),
        ] {
            let args = format!(r#"{{"task_id":1,"homework":true,"steps":[{steps}]}}"#);
            assert_eq!(brief(&conn, &tmp, &args).unwrap_err().kind, "rejected", "{args}");
        }
        assert!(crate::tasks::node(&conn, 1, 1).unwrap().unwrap().children.is_empty());
    }

    #[test]
    fn a_failing_split_leaves_the_description_alone() {
        let (conn, tmp) = env();
        let e = brief(
            &conn,
            &tmp,
            r#"{"task_id":1,"homework":true,"description":"half a brief",
                "steps":[{"title":"only one","duration_min":5}]}"#,
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");
        assert_eq!(description(&conn, 1), "");
    }

    #[test]
    fn a_brief_reaches_only_the_scoped_task_itself() {
        let (conn, tmp) = env();
        split_it(&conn, 1);
        let step = crate::tasks::node(&conn, 1, 1).unwrap().unwrap().children[0].id;
        for target in [2, step, 9999] {
            let args = format!(r#"{{"task_id":{target},"homework":true,"description":"nope"}}"#);
            assert_eq!(brief(&conn, &tmp, &args).unwrap_err().kind, "rejected", "reached {target}");
        }
        assert_eq!(description(&conn, 2), "");
    }
}
