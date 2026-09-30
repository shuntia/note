use rusqlite::{Connection, OptionalExtension};

pub const EVERY_SECS: i64 = 60;

/// Whole seconds, so two stamps compare exactly as text.
pub fn stamp(at: jiff::Timestamp) -> String {
    jiff::Timestamp::from_second(at.as_second())
        .expect("a timestamp's own second is in range")
        .to_string()
}

/// Records that the user is here: a session write, a message on their socket,
/// or a chat turn on any channel that runs through `talk::run_turn` (web,
/// Telegram, Matrix). Returns whether the stamp moved; it moves at most once
/// every `EVERY_SECS`.
pub fn touch(conn: &Connection, user_id: i64, now: jiff::Timestamp) -> rusqlite::Result<bool> {
    let cutoff = stamp(now - jiff::Span::new().seconds(EVERY_SECS));
    let n = conn.execute(
        "UPDATE users SET last_active_at = ?1
         WHERE id = ?2 AND (last_active_at IS NULL OR last_active_at <= ?3)",
        (stamp(now), user_id, cutoff),
    )?;
    Ok(n > 0)
}

pub fn last_active(conn: &Connection, user_id: i64) -> rusqlite::Result<Option<jiff::Timestamp>> {
    let raw: Option<String> = conn
        .query_row("SELECT last_active_at FROM users WHERE id = ?1", [user_id], |r| r.get(0))
        .optional()?
        .flatten();
    Ok(raw.and_then(|s| s.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    #[test]
    fn a_stamp_is_whole_seconds() {
        assert_eq!(stamp(at("2026-09-30T12:00:00.123456Z")), "2026-09-30T12:00:00Z");
    }

    #[test]
    fn a_stamp_moves_at_most_once_a_minute() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        assert_eq!(last_active(&conn, uid).unwrap(), None);

        assert!(touch(&conn, uid, at("2026-09-30T12:00:00.700Z")).unwrap());
        assert_eq!(last_active(&conn, uid).unwrap(), Some(at("2026-09-30T12:00:00Z")));
        assert!(!touch(&conn, uid, at("2026-09-30T12:00:59Z")).unwrap());
        assert_eq!(last_active(&conn, uid).unwrap(), Some(at("2026-09-30T12:00:00Z")));
        assert!(touch(&conn, uid, at("2026-09-30T12:01:00Z")).unwrap());
        assert_eq!(last_active(&conn, uid).unwrap(), Some(at("2026-09-30T12:01:00Z")));
    }

    #[test]
    fn one_users_stamp_leaves_another_alone() {
        let conn = crate::db::open_memory().unwrap();
        let aki = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        let bo = crate::auth::create_user(&conn, "bo", "p", false).unwrap();
        touch(&conn, aki, at("2026-09-30T12:00:00Z")).unwrap();
        assert_eq!(last_active(&conn, bo).unwrap(), None);
    }
}
