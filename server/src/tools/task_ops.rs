use super::{check_text, ToolCtx, ToolError};
use crate::tasks::{DurationActor, NewTask, Step, TaskPatch, UpdateError};
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

fn task_error(e: UpdateError) -> ToolError {
    match e {
        UpdateError::InvalidState(s) => ToolError::rejected(format!("invalid state: {s}")),
        UpdateError::InvalidDuration(m) | UpdateError::InvalidHierarchy(m) => {
            ToolError::rejected(m)
        }
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
        },
        "agent",
        DurationActor::Agent,
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
    pub state: Option<String>,
    pub notes: Option<String>,
    /// Rough estimate in whole 5-minute blocks.
    pub duration_min: Option<u32>,
}

pub fn update(
    conn: &Connection,
    ctx: &ToolCtx,
    args: UpdateArgs,
) -> Result<serde_json::Value, ToolError> {
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
        duration_actor: DurationActor::Agent,
        ..Default::default()
    };
    match crate::tasks::update(conn, ctx.user_id, args.task_id, patch) {
        Ok(Some(t)) => Ok(serde_json::json!({ "task_id": t.task.id, "state": t.task.state })),
        Ok(None) => Err(ToolError::not_found(format!("no task {}", args.task_id))),
        Err(e) => Err(task_error(e)),
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
    for s in &args.steps {
        checked_title(&s.title)?;
    }
    match crate::tasks::split(conn, ctx.user_id, args.task_id, args.steps, DurationActor::Agent) {
        Ok(Some(n)) => Ok(serde_json::json!({
            "task_id": n.task.id,
            "duration_min": n.task.duration_min,
            "step_ids": n.children.iter().map(|c| c.id).collect::<Vec<_>>(),
        })),
        Ok(None) => Err(ToolError::not_found(format!("no task {}", args.task_id))),
        Err(e) => Err(task_error(e)),
    }
}
