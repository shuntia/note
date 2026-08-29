use super::{check_text, ToolCtx, ToolError};
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

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateArgs {
    pub title: String,
    #[serde(default)]
    pub description: String,
}

pub fn create(
    conn: &Connection,
    ctx: &ToolCtx,
    args: CreateArgs,
) -> Result<serde_json::Value, ToolError> {
    let title = checked_title(&args.title)?;
    check_text("description", &args.description)?;
    let task = crate::tasks::create(conn, ctx.user_id, title, "agent")
        .map_err(|e| ToolError::internal(e.to_string()))?;
    if !args.description.is_empty() {
        crate::tasks::update(
            conn,
            ctx.user_id,
            task.id,
            crate::tasks::TaskPatch {
                description: Some(args.description),
                ..Default::default()
            },
        )
        .map_err(|e| ToolError::internal(e.to_string()))?;
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
    let patch = crate::tasks::TaskPatch {
        title,
        description: args.description,
        state: args.state,
        notes: args.notes,
    };
    match crate::tasks::update(conn, ctx.user_id, args.task_id, patch) {
        Ok(Some(t)) => Ok(serde_json::json!({ "task_id": t.id, "state": t.state })),
        Ok(None) => Err(ToolError::not_found(format!("no task {}", args.task_id))),
        Err(crate::tasks::UpdateError::InvalidState(s)) => {
            Err(ToolError::rejected(format!("invalid state: {s}")))
        }
        Err(e) => Err(ToolError::internal(e.to_string())),
    }
}
