use super::{ToolCtx, ToolError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

pub const MAX_SUMMARY: usize = 1200;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteArgs {
    pub summary: String,
}

/// Validates the summary and hands it back; the caller owns the conversation
/// it belongs to and stores it once the session ends.
pub fn write(
    conn: &Connection,
    ctx: &ToolCtx,
    args: WriteArgs,
) -> Result<serde_json::Value, ToolError> {
    let _ = (conn, ctx);
    let summary = args.summary.trim();
    if summary.is_empty() || summary.len() > MAX_SUMMARY {
        return Err(ToolError::rejected(format!(
            "summary must be 1..={MAX_SUMMARY} bytes"
        )));
    }
    if summary.lines().any(|l| l.trim_start().starts_with('#')) {
        return Err(ToolError::rejected("summary is plain text: no markdown headers"));
    }
    Ok(serde_json::json!({ "summary": summary }))
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
        }
    }

    #[test]
    fn a_summary_comes_back_trimmed() {
        let (conn, tmp) = env();
        let out = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Summarize,
            "summary_write",
            r#"{"summary":"  Aki asked about the essay.  "}"#,
        )
        .unwrap();
        assert_eq!(out["summary"], "Aki asked about the essay.");
    }

    #[test]
    fn blank_over_long_and_marked_up_summaries_are_rejected() {
        let (conn, tmp) = env();
        let long = "a".repeat(super::MAX_SUMMARY + 1);
        for raw in [
            r#"{"summary":"   "}"#.to_string(),
            format!(r#"{{"summary":"{long}"}}"#),
            "{\"summary\":\"## What happened\\nAki asked.\"}".to_string(),
        ] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Summarize, "summary_write", &raw)
                .unwrap_err();
            assert_eq!(e.kind, "rejected", "{raw}");
        }
    }
}
