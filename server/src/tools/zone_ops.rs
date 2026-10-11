use super::{ToolCtx, ToolError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetArgs {
    /// The IANA zone the user is in now, like "America/New_York".
    pub zone: String,
}

/// Moves the account to the zone the user says they are in; every pending
/// check keeps the instant it was laid for.
pub fn set(conn: &Connection, ctx: &ToolCtx, args: &SetArgs) -> Result<serde_json::Value, ToolError> {
    let zone = args.zone.trim();
    if jiff::tz::TimeZone::get(zone).is_err() {
        return Err(ToolError::rejected(format!(
            "zone must be an IANA zone name like America/New_York, got {zone:?}"
        )));
    }
    let internal = |e: anyhow::Error| ToolError::internal(e.to_string());
    let mut cfg = crate::config::UserConfig::load(ctx.config_dir, ctx.username).map_err(internal)?;
    if cfg.timezone == zone {
        return Ok(serde_json::json!({ "timezone": zone, "changed": false }));
    }
    let was = std::mem::replace(&mut cfg.timezone, zone.to_string());
    let moved = crate::zone::rezone(
        conn,
        ctx.config_dir,
        ctx.username,
        ctx.user_id,
        &was,
        zone,
        jiff::Timestamp::now(),
    )
    .map_err(internal)?;
    cfg.save(ctx.config_dir, ctx.username).map_err(internal)?;
    Ok(serde_json::json!({ "timezone": zone, "was": was, "changed": true, "kept_their_time": moved }))
}

#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, registry, PreparedVectors, SessionKind, ToolCtx};

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aitest', 'x', 'member')", [])
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("defaults/user.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "display_name = \"X\"\ntimezone = \"America/Los_Angeles\"\ntemplate = \"default\"\n")
            .unwrap();
        (conn, tmp)
    }

    fn ctx(tmp: &tempfile::TempDir) -> ToolCtx<'_> {
        ToolCtx {
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            user_id: 1,
            username: "aitest",
            vectors: PreparedVectors::default(),
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            share: None,
            share_thread: None,
        }
    }

    #[test]
    fn the_user_says_where_they_are_and_their_checks_keep_their_instant() {
        let (conn, tmp) = env();
        let day = jiff::Timestamp::now()
            .to_zoned(jiff::tz::TimeZone::get("America/Los_Angeles").unwrap())
            .date()
            .checked_add(jiff::Span::new().days(2))
            .unwrap();
        let laid = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "trigger_set",
            &format!(r#"{{"at":"11:00","date":"{day}","prompt":"call them"}}"#)).unwrap();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "timezone_set",
            r#"{"zone":"America/New_York"}"#).unwrap();
        assert_eq!(out["was"], "America/Los_Angeles");
        assert_eq!(out["kept_their_time"], 1);
        let cfg = crate::config::UserConfig::load(tmp.path(), "aitest").unwrap();
        assert_eq!(cfg.timezone, "America/New_York");
        let wall: String = conn
            .query_row("SELECT wall_time FROM events WHERE id = ?1", [laid["event_id"].as_i64().unwrap()], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(wall, "14:00");

        let again = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "timezone_set",
            r#"{"zone":"America/New_York"}"#).unwrap();
        assert_eq!(again["changed"], false);
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "timezone_set", r#"{"zone":"Eastern"}"#)
            .unwrap_err();
        assert_eq!(e.kind, "rejected");
    }

    #[test]
    fn only_a_session_the_user_speaks_in_can_move_their_zone() {
        for kind in [SessionKind::Talk, SessionKind::Checkin, SessionKind::Call] {
            assert!(registry(kind).contains(&"timezone_set"), "{kind:?}");
        }
        for kind in [SessionKind::Nightly, SessionKind::Trigger, SessionKind::Share, SessionKind::Harvest] {
            assert!(!registry(kind).contains(&"timezone_set"), "{kind:?}");
        }
    }
}
