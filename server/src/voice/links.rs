use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub id: i64,
    pub mxid: String,
    pub room_id: Option<String>,
    pub state: String,
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Link> {
    Ok(Link { id: r.get(0)?, mxid: r.get(1)?, room_id: r.get(2)?, state: r.get(3)? })
}

pub fn get(conn: &Connection, user_id: i64) -> Result<Option<Link>> {
    Ok(conn
        .query_row("SELECT id, mxid, room_id, state FROM voice_links WHERE user_id = ?1", [user_id], row)
        .optional()?)
}

/// A joined link with its room: the only kind a call may use.
pub fn ringable(conn: &Connection, user_id: i64) -> Result<Option<Link>> {
    Ok(get(conn, user_id)?.filter(|l| l.state == "linked" && l.room_id.is_some()))
}

/// The user whose joined link is `mxid` in `room_id`.
pub fn linked_user(conn: &Connection, room_id: &str, mxid: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT user_id FROM voice_links WHERE room_id = ?1 AND mxid = ?2 AND state = 'linked'",
            (room_id, mxid),
            |r| r.get(0),
        )
        .optional()?)
}

/// Starts, or restarts, the user's one link, keeping its id.
pub fn begin(conn: &Connection, user_id: i64, mxid: &str, now: jiff::Timestamp) -> Result<i64> {
    Ok(conn.query_row(
        "INSERT INTO voice_links (user_id, mxid, state, created_at) VALUES (?1, ?2, 'invited', ?3)
         ON CONFLICT(user_id) DO UPDATE SET
             mxid = excluded.mxid, state = 'invited', room_id = NULL, linked_at = NULL,
             created_at = excluded.created_at
         RETURNING id",
        (user_id, mxid, now.to_string()),
        |r| r.get(0),
    )?)
}

/// False when the link is gone.
pub fn set_room(conn: &Connection, link_id: i64, room_id: &str) -> Result<bool> {
    Ok(conn.execute("UPDATE voice_links SET room_id = ?2 WHERE id = ?1", (link_id, room_id))? > 0)
}

/// Links only a join of the room this link invited to, so a join from an
/// account the user has since replaced changes nothing.
pub fn mark_joined(conn: &Connection, link_id: i64, room_id: &str, now: jiff::Timestamp) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE voice_links SET state = 'linked', linked_at = COALESCE(linked_at, ?3)
         WHERE id = ?1 AND room_id = ?2",
        (link_id, room_id, now.to_string()),
    )? > 0)
}

/// Drops a link whose invite never went out.
pub fn forget_unsent(conn: &Connection, link_id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM voice_links WHERE id = ?1 AND room_id IS NULL", [link_id])? > 0)
}

pub fn remove(conn: &Connection, user_id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM voice_links WHERE user_id = ?1", [user_id])? > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = crate::db::open_memory().unwrap();
        c.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", [])
            .unwrap();
        c
    }

    #[test]
    fn a_link_is_ringable_only_once_joined() {
        let c = conn();
        let now = jiff::Timestamp::now();
        let id = begin(&c, 1, "@aki:t", now).unwrap();
        set_room(&c, id, "!r:t").unwrap();
        assert!(ringable(&c, 1).unwrap().is_none(), "invited is not linked");
        assert!(mark_joined(&c, id, "!r:t", now).unwrap());
        let l = ringable(&c, 1).unwrap().unwrap();
        assert_eq!((l.mxid.as_str(), l.room_id.as_deref()), ("@aki:t", Some("!r:t")));
    }

    #[test]
    fn relinking_replaces_the_old_account_and_resets_the_state() {
        let c = conn();
        let now = jiff::Timestamp::now();
        let first = begin(&c, 1, "@aki:t", now).unwrap();
        set_room(&c, first, "!r:t").unwrap();
        mark_joined(&c, first, "!r:t", now).unwrap();
        let second = begin(&c, 1, "@other:t", now).unwrap();
        assert_eq!(first, second, "one row per user, same id");
        let l = get(&c, 1).unwrap().unwrap();
        assert_eq!((l.mxid.as_str(), l.state.as_str(), l.room_id), ("@other:t", "invited", None));
        assert!(!mark_joined(&c, 999, "!r:t", now).unwrap(), "an unknown link changes nothing");
        assert!(remove(&c, 1).unwrap());
        assert!(get(&c, 1).unwrap().is_none());
    }

    #[test]
    fn a_join_counts_only_for_the_room_the_link_invited_to() {
        let c = conn();
        let now = jiff::Timestamp::now();
        let id = begin(&c, 1, "@aki:t", now).unwrap();
        assert!(!mark_joined(&c, id, "!old:t", now).unwrap(), "no room yet");
        assert!(set_room(&c, id, "!new:t").unwrap());
        assert!(!mark_joined(&c, id, "!old:t", now).unwrap(), "a previous account's room");
        assert!(ringable(&c, 1).unwrap().is_none());
        assert!(mark_joined(&c, id, "!new:t", now).unwrap());
        assert!(ringable(&c, 1).unwrap().is_some());
    }

    #[test]
    fn only_a_link_whose_invite_never_went_out_is_forgotten() {
        let c = conn();
        let now = jiff::Timestamp::now();
        let id = begin(&c, 1, "@aki:t", now).unwrap();
        assert!(forget_unsent(&c, id).unwrap());
        assert!(get(&c, 1).unwrap().is_none());
        let id = begin(&c, 1, "@aki:t", now).unwrap();
        set_room(&c, id, "!r:t").unwrap();
        assert!(!forget_unsent(&c, id).unwrap());
        assert!(get(&c, 1).unwrap().is_some());
        assert!(remove(&c, 1).unwrap());
        assert!(!set_room(&c, id, "!r:t").unwrap(), "a removed link takes no room");
    }
}
