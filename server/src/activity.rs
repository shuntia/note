//! Whether the user has done anything since unattended work last looked, so
//! that work can pass a night or a tidy-up by when nothing changed. Only the
//! user's own doing counts: Note's sessions stamp none of these.

use rusqlite::{Connection, OptionalExtension};

/// Records a write the user made outside the app: through one of their API
/// tokens, or a button on a notification or chat message.
pub fn touch_external(conn: &Connection, user_id: i64, now: jiff::Timestamp) -> rusqlite::Result<()> {
    conn.prepare_cached("UPDATE users SET last_external_at = ?1 WHERE id = ?2")?
        .execute((crate::presence::stamp(now), user_id))
        .map(|_| ())
}

/// The newest of the user's presence or writes in the app, their writes
/// from outside it, and their messages in any thread, calls and chat
/// channels included.
pub fn last_user_activity(conn: &Connection, user_id: i64) -> rusqlite::Result<Option<jiff::Timestamp>> {
    let stamps: Option<(Option<String>, Option<String>)> = conn
        .query_row("SELECT last_active_at, last_external_at FROM users WHERE id = ?1", [user_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let (active, external) = stamps.unwrap_or_default();
    let said: Option<String> = conn.query_row(
        "SELECT MAX(m.created_at) FROM talk_messages m JOIN conversations c ON c.id = m.conversation_id
         WHERE c.user_id = ?1 AND m.role = 'user'",
        [user_id],
        |r| r.get(0),
    )?;
    Ok([active, external, said].into_iter().flatten().filter_map(|s| s.parse().ok()).max())
}

/// True when there was a last look and the user has done nothing since.
pub fn quiet_since(conn: &Connection, user_id: i64, since: Option<jiff::Timestamp>) -> rusqlite::Result<bool> {
    let Some(since) = since else { return Ok(false) };
    Ok(last_user_activity(conn, user_id)?.is_none_or(|at| at <= since))
}

/// The newest parseable stamp among `rows`.
pub(crate) fn newest(rows: impl IntoIterator<Item = Option<String>>) -> Option<jiff::Timestamp> {
    rows.into_iter().flatten().filter_map(|s| s.parse().ok()).max()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    #[test]
    fn presence_api_writes_and_messages_all_count_and_the_newest_wins() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aitest", "p", false).unwrap();
        assert_eq!(last_user_activity(&conn, uid).unwrap(), None);
        assert!(!quiet_since(&conn, uid, None).unwrap(), "with no last look there is nothing to compare");
        assert!(quiet_since(&conn, uid, Some(at("2026-11-01T00:00:00Z"))).unwrap());

        crate::presence::touch(&conn, uid, at("2026-11-02T08:00:00Z")).unwrap();
        assert_eq!(last_user_activity(&conn, uid).unwrap(), Some(at("2026-11-02T08:00:00Z")));
        touch_external(&conn, uid, at("2026-11-02T09:00:00Z")).unwrap();
        assert_eq!(last_user_activity(&conn, uid).unwrap(), Some(at("2026-11-02T09:00:00Z")));
        let thread = crate::talk::create(&conn, uid, "t", at("2026-11-02T10:00:00Z")).unwrap();
        crate::talk::append_text(&conn, thread, "assistant", "a check-in", at("2026-11-02T11:00:00Z")).unwrap();
        assert_eq!(last_user_activity(&conn, uid).unwrap(), Some(at("2026-11-02T09:00:00Z")), "Note's words are not the user's");
        crate::talk::append_text(&conn, thread, "user", "on it", at("2026-11-02T12:00:00Z")).unwrap();
        assert_eq!(last_user_activity(&conn, uid).unwrap(), Some(at("2026-11-02T12:00:00Z")));
        assert!(!quiet_since(&conn, uid, Some(at("2026-11-02T11:59:00Z"))).unwrap());
        assert!(quiet_since(&conn, uid, Some(at("2026-11-02T12:00:00Z"))).unwrap());
    }

    #[test]
    fn notes_own_edits_leave_the_user_quiet() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aitest", "p", false).unwrap();
        crate::presence::touch(&conn, uid, at("2026-11-02T08:00:00Z")).unwrap();
        crate::tasks::create(&conn, uid, crate::tasks::NewTask { title: "tidy".into(), ..Default::default() }, "agent", crate::tasks::Actor::Agent)
            .unwrap();
        conn.execute("UPDATE tasks SET updated_at = '2026-11-03T03:00:00Z'", []).unwrap();
        crate::log::record(&conn, Some(uid), "agent_session", "Nightly").unwrap();
        assert!(quiet_since(&conn, uid, Some(at("2026-11-02T09:00:00Z"))).unwrap());
    }
}
