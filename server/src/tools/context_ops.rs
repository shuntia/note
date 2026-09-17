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

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NightlyNotesArgs {
    /// The whole brief: plain lines, no markdown headers.
    pub text: String,
}

/// Replaces the brief the next day's sessions read. Rejections are `rejected`
/// so the model reformats and calls again rather than giving up.
pub fn nightly_notes_write(
    conn: &Connection,
    ctx: &ToolCtx,
    args: NightlyNotesArgs,
) -> Result<serde_json::Value, ToolError> {
    let _ = conn;
    let text = args.text.trim();
    if text.is_empty() {
        return Err(ToolError::rejected("text must not be empty"));
    }
    if text.len() > crate::context::MAX_NIGHTLY_NOTES_BYTES {
        return Err(ToolError::rejected(format!(
            "text must be at most {} bytes",
            crate::context::MAX_NIGHTLY_NOTES_BYTES
        )));
    }
    if text.lines().any(|l| l.trim_start().starts_with('#')) {
        return Err(ToolError::rejected(
            "no markdown headers; the notes are injected under a header of their own",
        ));
    }
    let ucfg = crate::config::UserConfig::load(ctx.config_dir, ctx.username)
        .map_err(|e| ToolError::internal(e.to_string()))?;
    let tz = jiff::tz::TimeZone::get(&ucfg.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
    let date = jiff::Timestamp::now().to_zoned(tz).date();
    crate::context::write_nightly_notes(ctx.config_dir, ctx.username, text, date)
        .map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(serde_json::json!({ "ok": true, "bytes": text.len() }))
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
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("defaults")).unwrap();
        std::fs::write(
            tmp.path().join("defaults/user.toml"),
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n",
        )
        .unwrap();
        (conn, tmp)
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir) -> ToolCtx<'a> {
        ToolCtx { config_dir: tmp.path(), data_dir: tmp.path(), user_id: 1, username: "aki", vectors: crate::tools::PreparedVectors::default(), task_scope: None, inbox_source: None }
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

    const NOTES: &str = "the essay is the one that matters\nlow energy after 21:00";

    fn today_utc() -> String {
        jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date().to_string()
    }

    #[test]
    fn nightly_notes_write_replaces_the_file_under_a_dated_marker() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "nightly_notes_write",
            &serde_json::json!({ "text": NOTES }).to_string()).unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["bytes"].as_u64().unwrap() as usize, NOTES.len());

        let path = crate::context::nightly_notes_path(tmp.path(), "aki");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("<!-- written {} -->\n{NOTES}\n", today_utc()),
        );

        dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "nightly_notes_write",
            r#"{"text":"  start with the dishes  "}"#).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("<!-- written {} -->\nstart with the dishes\n", today_utc()),
        );
        let mut tmp_name = path.as_os_str().to_owned();
        tmp_name.push(".tmp");
        assert!(!std::path::PathBuf::from(tmp_name).exists(), "a temp file was left behind");
    }

    #[test]
    fn nightly_notes_write_rejects_empty_oversized_and_headed_text() {
        let (conn, tmp) = env();
        let big = "x".repeat(crate::context::MAX_NIGHTLY_NOTES_BYTES + 1);
        for raw in [
            r#"{"text":""}"#.to_string(),
            r#"{"text":"   \n  "}"#.to_string(),
            serde_json::json!({ "text": big }).to_string(),
            serde_json::json!({ "text": "# Tomorrow\nthe essay first" }).to_string(),
            serde_json::json!({ "text": "the essay first\n  ## energy" }).to_string(),
        ] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "nightly_notes_write", &raw)
                .unwrap_err();
            assert_eq!(e.kind, "rejected", "raw={raw}");
        }
        assert!(!crate::context::nightly_notes_path(tmp.path(), "aki").exists());
    }

    #[test]
    fn a_rejected_note_leaves_last_nights_file_alone() {
        let (conn, tmp) = env();
        dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "nightly_notes_write",
            &serde_json::json!({ "text": NOTES }).to_string()).unwrap();
        let before =
            std::fs::read_to_string(crate::context::nightly_notes_path(tmp.path(), "aki")).unwrap();
        dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "nightly_notes_write",
            r##"{"text":"# headed"}"##).unwrap_err();
        assert_eq!(
            std::fs::read_to_string(crate::context::nightly_notes_path(tmp.path(), "aki")).unwrap(),
            before,
        );
    }
}
