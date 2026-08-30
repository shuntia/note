use super::{check_text, ToolCtx, ToolError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditArgs {
    pub find: Option<String>,
    pub replace: Option<String>,
    pub append: Option<String>,
}

pub fn edit(
    conn: &Connection,
    ctx: &ToolCtx,
    args: EditArgs,
) -> Result<serde_json::Value, ToolError> {
    let _ = conn;
    let result = match (args.find, args.replace, args.append) {
        (Some(find), Some(replace), None) => {
            check_text("find", &find)?;
            check_text("replace", &replace)?;
            crate::context::edit_replace(ctx.config_dir, ctx.username, &find, &replace)
        }
        (None, None, Some(text)) => {
            check_text("append", &text)?;
            crate::context::edit_append(ctx.config_dir, ctx.username, &text)
        }
        _ => {
            return Err(ToolError::rejected(
                "provide either find+replace or append, not a mix",
            ))
        }
    };
    match result {
        Ok(()) => Ok(serde_json::json!({ "ok": true })),
        Err(crate::context::EditError::Io(e)) => Err(ToolError::internal(e.to_string())),
        Err(e) => Err(ToolError::rejected(e.to_string())),
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
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir) -> ToolCtx<'a> {
        ToolCtx { config_dir: tmp.path(), data_dir: tmp.path(), user_id: 1, username: "aki", vectors: crate::tools::PreparedVectors::default() }
    }

    #[test]
    fn append_then_replace_edits_standing() {
        let (conn, tmp) = env();
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "context_edit",
            r#"{"append":"- exam week"}"#).unwrap();
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "context_edit",
            r#"{"find":"exam week","replace":"exams done"}"#).unwrap();
        let text = std::fs::read_to_string(
            crate::context::standing_path(tmp.path(), "aki")).unwrap();
        assert!(text.contains("exams done"));
    }

    #[test]
    fn oversized_text_is_rejected_before_touching_standing() {
        let (conn, tmp) = env();
        let big = "x".repeat(crate::tools::MAX_TEXT_BYTES + 1);
        for raw in [
            serde_json::json!({ "append": &big }).to_string(),
            serde_json::json!({ "find": &big, "replace": "y" }).to_string(),
            serde_json::json!({ "find": "y", "replace": &big }).to_string(),
        ] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "context_edit", &raw).unwrap_err();
            assert_eq!(e.kind, "rejected");
        }
        assert!(!crate::context::standing_path(tmp.path(), "aki").exists());
    }

    #[test]
    fn bad_combinations_and_misses_are_typed() {
        let (conn, tmp) = env();
        for raw in [
            r#"{}"#,
            r#"{"find":"x"}"#,
            r#"{"append":"a","find":"x","replace":"y"}"#,
        ] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "context_edit", raw).unwrap_err();
            assert_eq!(e.kind, "rejected", "raw={raw}");
        }
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "context_edit",
            r#"{"find":"nope","replace":"y"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
        // context_edit is not on the check-in surface
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "context_edit",
            r#"{"append":"x"}"#).unwrap_err();
        assert_eq!(e.kind, "forbidden");
    }
}
