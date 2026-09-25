use super::{ToolCtx, ToolError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

const MAX_NOTE_BYTES: usize = 2000;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoteArgs {
    /// What to pass on, in the visitor's words.
    pub text: String,
}

/// Writes the note onto the visitor's own thread; the session that called it
/// hands it to the owner's channels once the tool has returned.
pub fn note(conn: &Connection, ctx: &ToolCtx, args: NoteArgs) -> Result<serde_json::Value, ToolError> {
    let Some(thread) = ctx.share_thread else {
        return Err(ToolError::forbidden("notes are filed from a share link alone"));
    };
    let text = args.text.trim();
    if text.is_empty() || text.len() > MAX_NOTE_BYTES {
        return Err(ToolError::rejected(format!("text must be 1 to {MAX_NOTE_BYTES} bytes")));
    }
    crate::shares::append(conn, thread, "note", text, jiff::Timestamp::now())
        .map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(serde_json::json!({ "filed": true }))
}
