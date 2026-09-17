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

fn task_error(e: UpdateError) -> ToolError {
    match e {
        UpdateError::InvalidState(s) => ToolError::rejected(format!("invalid state: {s}")),
        UpdateError::InvalidDuration(m)
        | UpdateError::InvalidHierarchy(m)
        | UpdateError::NowFull(m) => ToolError::rejected(m),
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
}

pub fn create(
    conn: &Connection,
    ctx: &ToolCtx,
    args: CreateArgs,
) -> Result<serde_json::Value, ToolError> {
    let title = checked_title(&args.title)?;
    check_text("description", &args.description)?;
    let task = crate::tasks::create(
        conn,
        ctx.user_id,
        NewTask {
            title: title.to_owned(),
            duration_min: args.duration_min,
            parent_id: None,
            is_now: args.is_now,
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
        if args.state.as_deref() == Some("dropped") {
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
    let patch = TaskPatch {
        title,
        description: args.description,
        state: args.state,
        notes: args.notes,
        duration_min: args.duration_min.map(Some),
        is_now: args.is_now,
        actor: Actor::Agent,
        ..Default::default()
    };
    match crate::tasks::update(conn, ctx.user_id, args.task_id, patch) {
        Ok(Some(t)) => Ok(serde_json::json!({
            "task_id": t.task.id,
            "state": t.task.state,
            "is_now": t.task.is_now,
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
                    duration_min: None,
                    parent_id: None,
                    is_now: false,
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
        }
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
}
