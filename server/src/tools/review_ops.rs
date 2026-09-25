use super::{ToolCtx, ToolError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

pub const MAX_TEXT: usize = 4000;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteArgs {
    pub text: String,
}

/// Validates the week's letter and hands it back; the caller owns the week it
/// belongs to and stores it once the session ends.
pub fn write(
    conn: &Connection,
    ctx: &ToolCtx,
    args: WriteArgs,
) -> Result<serde_json::Value, ToolError> {
    let _ = (conn, ctx);
    let text = args.text.trim();
    if text.is_empty() || text.len() > MAX_TEXT {
        return Err(ToolError::rejected(format!("text must be 1..={MAX_TEXT} bytes")));
    }
    Ok(serde_json::json!({ "text": text }))
}

#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, PreparedVectors, SessionKind, ToolCtx};

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir) -> ToolCtx<'a> {
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

    #[test]
    fn the_week_is_one_letter_between_one_and_four_thousand_bytes() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Review, "review_write",
            r#"{"text":"  You finished the essay.  "}"#).unwrap();
        assert_eq!(out["text"], "You finished the essay.");

        for bad in [String::from("   "), "x".repeat(super::MAX_TEXT + 1)] {
            let args = serde_json::json!({ "text": bad }).to_string();
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Review, "review_write", &args)
                .unwrap_err();
            assert_eq!(e.kind, "rejected");
        }
    }
}
