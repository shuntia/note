use super::{check_text, ToolCtx, ToolError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendArgs {
    /// The nudge text shown to the user.
    pub text: String,
}

/// Inserts an immediate droppable nudge into today's plan; the runner delivers
/// it on its next sweep, so dispatch itself never touches the network.
///
/// Unlike `schedule_insert`, which rejects a day with no plan, this generates
/// today's plan when the nightly job has not run yet: an outreach nudge is
/// about right now, so it must land whether or not the day was planned.
pub fn send(conn: &Connection, ctx: &ToolCtx, args: SendArgs) -> Result<serde_json::Value, ToolError> {
    check_text("text", &args.text)?;
    let text = args.text.trim();
    if text.is_empty() {
        return Err(ToolError::rejected("text must not be empty"));
    }
    let ucfg = crate::config::UserConfig::load(ctx.config_dir, ctx.username)
        .map_err(|e| ToolError::internal(e.to_string()))?;
    let tz = jiff::tz::TimeZone::get(&ucfg.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
    let local = jiff::Timestamp::now().to_zoned(tz);
    let wall = format!("{:02}:{:02}", local.hour(), local.minute());
    let tmpl = crate::templates::Template::load(ctx.config_dir, ctx.username, &ucfg.template)
        .map_err(|e| ToolError::internal(e.to_string()))?;
    let plan_id = crate::plan::generate(conn, ctx.user_id, &tmpl, local.date())
        .map_err(|e| ToolError::internal(e.to_string()))?;
    conn.execute(
        "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, flexibility, slide_window_min, channel, message)
         VALUES (?1, 'nudge', ?2, ?2, 'drop', 0, 'push', ?3)",
        (plan_id, &wall, text),
    )
    .map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(serde_json::json!({ "event_id": conn.last_insert_rowid(), "wall_time": wall }))
}

#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, SessionKind, ToolCtx};

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        env_with("UTC", "events = []\n")
    }

    fn env_with(timezone: &str, template: &str) -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let write = |rel: &str, c: &str| {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        };
        write(
            "defaults/user.toml",
            &format!("display_name = \"X\"\ntimezone = \"{timezone}\"\ntemplate = \"default\"\n"),
        );
        write("defaults/templates/default.toml", template);
        (conn, tmp)
    }

    fn ctx(tmp: &tempfile::TempDir) -> ToolCtx<'_> {
        ToolCtx {
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            user_id: 1,
            username: "aki",
            vectors: crate::tools::PreparedVectors::default(),
        }
    }

    #[test]
    fn notify_send_inserts_an_immediate_droppable_nudge() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "notify_send",
            r#"{"text":"stretch break, you asked for it"}"#).unwrap();
        let id = out["event_id"].as_i64().unwrap();
        let (kind, flex, message, status): (String, String, String, String) = conn
            .query_row(
                "SELECT kind, flexibility, message, status FROM events WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(kind, "nudge");
        assert_eq!(flex, "drop");
        assert_eq!(message, "stretch break, you asked for it");
        assert_eq!(status, "pending");
    }

    #[test]
    fn notify_send_is_nightly_only_and_validates_text() {
        let (conn, tmp) = env();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "notify_send", r#"{"text":"x"}"#)
            .unwrap_err();
        assert_eq!(e.kind, "forbidden");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "notify_send", r#"{"text":"  "}"#)
            .unwrap_err();
        assert_eq!(e.kind, "rejected");
        let big = format!(r#"{{"text":"{}"}}"#, "x".repeat(16 * 1024 + 1));
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "notify_send", &big).unwrap_err();
        assert_eq!(e.kind, "rejected");
    }

    /// The nudge lands on the user's wall clock, not the host's: on a UTC host
    /// a Tokyo user's nudge is nine hours off, and near either midnight it is
    /// filed under a different date.
    #[test]
    fn notify_send_places_the_nudge_in_the_user_timezone() {
        let (conn, tmp) = env_with("Asia/Tokyo", "events = []\n");
        let tokyo = jiff::tz::TimeZone::get("Asia/Tokyo").unwrap();
        let stamp = |z: &jiff::Zoned| (format!("{:02}:{:02}", z.hour(), z.minute()), z.date().to_string());
        let before = stamp(&jiff::Timestamp::now().to_zoned(tokyo.clone()));
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "notify_send",
            r#"{"text":"stretch"}"#).unwrap();
        let after = stamp(&jiff::Timestamp::now().to_zoned(tokyo));

        let date: String = conn
            .query_row(
                "SELECT p.date FROM plans p JOIN events e ON e.plan_id = p.id WHERE e.id = ?1",
                [out["event_id"].as_i64().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        let got = (out["wall_time"].as_str().unwrap().to_string(), date);
        assert!(got == before || got == after, "got {got:?}, expected {before:?} or {after:?}");
    }

    #[test]
    fn notify_send_writes_no_plan_when_the_day_cannot_be_built() {
        let (conn, tmp) = env_with(
            "UTC",
            "[[events]]\nkind='nudge'\ntime='09:00'\ndays=['mon']\nflexibility='soft'\n",
        );
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "notify_send",
            r#"{"text":"stretch"}"#).unwrap_err();
        assert_eq!(e.kind, "internal");
        let plans: i64 = conn.query_row("SELECT COUNT(*) FROM plans", [], |r| r.get(0)).unwrap();
        assert_eq!(plans, 0);
        let events: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(events, 0);
    }
}
