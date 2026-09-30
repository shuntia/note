use super::{ToolCtx, ToolError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

pub const MAX_SUMMARY: usize = 1200;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteArgs {
    pub summary: String,
    /// What to call the thread: 3 to 6 words naming its subject.
    #[serde(default)]
    pub title: Option<String>,
}

/// Validates the summary and hands it back; the caller owns the conversation
/// it belongs to and stores it once the session ends.
pub fn write(
    conn: &Connection,
    ctx: &ToolCtx,
    args: &WriteArgs,
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
    let title = match args.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        None => None,
        Some(raw) => Some(crate::talk::normalize_title(raw).ok_or_else(|| {
            ToolError::rejected("title is a name of 3 to 6 words on one line, no quotes")
        })?),
    };
    Ok(serde_json::json!({ "summary": summary, "title": title }))
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
    fn a_title_comes_back_normalised_and_prose_is_refused() {
        let (conn, tmp) = env();
        let out = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Summarize,
            "summary_write",
            r#"{"summary":"Aki asked about the essay.","title":"  \"Essay due on Friday\".  "}"#,
        )
        .unwrap();
        assert_eq!(out["title"], "Essay due on Friday");

        let out = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Summarize,
            "summary_write",
            r#"{"summary":"Aki asked about the essay."}"#,
        )
        .unwrap();
        assert!(out["title"].is_null(), "a summary without a title is still a summary");

        let e = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Summarize,
            "summary_write",
            r#"{"summary":"Aki asked.","title":"Aki asked what to do about the essay that is due"}"#,
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");
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
