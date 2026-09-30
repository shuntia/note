use super::{ToolCtx, ToolError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

const MAX_NOTE: usize = 600;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DoneArgs {
    pub written: u32,
    #[serde(default)]
    pub note: String,
}

pub fn done(
    conn: &Connection,
    ctx: &ToolCtx,
    args: &DoneArgs,
) -> Result<serde_json::Value, ToolError> {
    let _ = (conn, ctx);
    let note = args.note.trim();
    if note.len() > MAX_NOTE {
        return Err(ToolError::rejected(format!("note must be at most {MAX_NOTE} bytes")));
    }
    Ok(serde_json::json!({ "written": args.written, "note": note }))
}
