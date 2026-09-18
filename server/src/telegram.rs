use anyhow::Result;
use argon2::password_hash::rand_core::{OsRng, RngCore};
use rusqlite::{Connection, OptionalExtension};

/// Unambiguous when read off a screen and typed into a phone: no O/0, no I/1.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const CODE_LEN: usize = 6;
pub const CODE_TTL_MINS: i64 = 10;

/// The account a chat speaks for.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub user_id: i64,
    pub username: String,
    pub chat_id: i64,
}

pub fn new_code() -> String {
    let mut bytes = [0u8; CODE_LEN];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| CODE_ALPHABET[*b as usize % CODE_ALPHABET.len()] as char).collect()
}

/// One live code per user: issuing another drops the one before it, and every
/// code that has run out goes with it.
pub fn issue_code(conn: &Connection, user_id: i64, now: jiff::Timestamp) -> Result<String> {
    conn.execute("DELETE FROM telegram_link_codes WHERE user_id = ?1", [user_id])?;
    conn.execute("DELETE FROM telegram_link_codes WHERE expires_at <= ?1", [now.to_string()])?;
    let code = new_code();
    let expires = now + jiff::Span::new().minutes(CODE_TTL_MINS);
    conn.execute(
        "INSERT INTO telegram_link_codes (code, user_id, expires_at) VALUES (?1, ?2, ?3)",
        (&code, user_id, expires.to_string()),
    )?;
    Ok(code)
}

/// Spends a code on a chat, replacing whatever that account was linked to.
/// A code that has expired, or that no one issued, links nothing.
pub fn redeem(
    conn: &Connection,
    code: &str,
    chat_id: i64,
    handle: &str,
    now: jiff::Timestamp,
) -> Result<Option<i64>> {
    let user_id: Option<i64> = conn
        .query_row(
            "SELECT user_id FROM telegram_link_codes WHERE code = ?1 AND expires_at > ?2",
            (code, now.to_string()),
            |r| r.get(0),
        )
        .optional()?;
    let Some(user_id) = user_id else { return Ok(None) };
    conn.execute("DELETE FROM telegram_link_codes WHERE code = ?1", [code])?;
    conn.execute("DELETE FROM telegram_links WHERE user_id = ?1 OR chat_id = ?2", (user_id, chat_id))?;
    conn.execute(
        "INSERT INTO telegram_links (user_id, chat_id, handle, linked_at)
         VALUES (?1, ?2, ?3, ?4)",
        (user_id, chat_id, handle, now.to_string()),
    )?;
    Ok(Some(user_id))
}

pub fn unlink(conn: &Connection, user_id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM telegram_links WHERE user_id = ?1", [user_id])? > 0)
}

pub fn link_for_chat(conn: &Connection, chat_id: i64) -> Result<Option<Link>> {
    Ok(conn
        .query_row(
            "SELECT l.user_id, u.username FROM telegram_links l
             JOIN users u ON u.id = l.user_id WHERE l.chat_id = ?1",
            [chat_id],
            |r| Ok(Link { user_id: r.get(0)?, username: r.get(1)?, chat_id }),
        )
        .optional()?)
}

pub fn chat_for_user(conn: &Connection, user_id: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row("SELECT chat_id FROM telegram_links WHERE user_id = ?1", [user_id], |r| r.get(0))
        .optional()?)
}

pub fn cursor(conn: &Connection) -> Result<i64> {
    Ok(conn
        .query_row("SELECT last_update_id FROM telegram_cursor WHERE id = 1", [], |r| r.get(0))
        .optional()?
        .unwrap_or(0))
}

/// The cursor only moves forward, so a batch answered out of order cannot
/// replay updates the loop has already handled.
pub fn set_cursor(conn: &Connection, last_update_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO telegram_cursor (id, last_update_id) VALUES (1, ?1)
         ON CONFLICT (id) DO UPDATE SET last_update_id = MAX(last_update_id, excluded.last_update_id)",
        [last_update_id],
    )?;
    Ok(())
}

/// The thread a reply from Telegram continues: the user's most recent one that
/// crossed Telegram since `cutoff`. Nothing that recent means a fresh thread.
pub fn thread_for(conn: &Connection, user_id: i64, cutoff: jiff::Timestamp) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM conversations
             WHERE user_id = ?1 AND telegram_at IS NOT NULL AND telegram_at >= ?2
             ORDER BY telegram_at DESC, id DESC LIMIT 1",
            (user_id, cutoff.to_string()),
            |r| r.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Connection {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role)
             VALUES ('aki', 'x', 'member'), ('bo', 'x', 'member')",
            [],
        )
        .unwrap();
        conn
    }

    fn at(rfc: &str) -> jiff::Timestamp {
        rfc.parse().unwrap()
    }

    #[test]
    fn a_code_is_six_unambiguous_characters() {
        for _ in 0..64 {
            let code = new_code();
            assert_eq!(code.chars().count(), CODE_LEN);
            assert!(
                code.bytes().all(|b| CODE_ALPHABET.contains(&b)),
                "unexpected code: {code}"
            );
        }
    }

    #[test]
    fn issuing_again_replaces_the_live_code() {
        let conn = env();
        let now = at("2026-09-17T09:00:00Z");
        let first = issue_code(&conn, 1, now).unwrap();
        let second = issue_code(&conn, 1, now).unwrap();
        assert_ne!(first, second);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM telegram_link_codes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(redeem(&conn, &first, 42, "aki", now).unwrap(), None);
        assert_eq!(redeem(&conn, &second, 42, "aki", now).unwrap(), Some(1));
    }

    #[test]
    fn a_code_dies_after_ten_minutes() {
        let conn = env();
        let now = at("2026-09-17T09:00:00Z");
        let code = issue_code(&conn, 1, now).unwrap();
        assert_eq!(redeem(&conn, &code, 42, "aki", at("2026-09-17T09:10:01Z")).unwrap(), None);
        assert_eq!(redeem(&conn, &code, 42, "aki", at("2026-09-17T09:09:59Z")).unwrap(), Some(1));
    }

    #[test]
    fn redeeming_spends_the_code_and_moves_the_link() {
        let conn = env();
        let now = at("2026-09-17T09:00:00Z");
        let code = issue_code(&conn, 1, now).unwrap();
        assert_eq!(redeem(&conn, &code, 42, "aki_t", now).unwrap(), Some(1));
        assert_eq!(redeem(&conn, &code, 43, "aki_t", now).unwrap(), None, "a code is spent once");
        assert_eq!(chat_for_user(&conn, 1).unwrap(), Some(42));
        assert_eq!(
            link_for_chat(&conn, 42).unwrap(),
            Some(Link { user_id: 1, username: "aki".into(), chat_id: 42 })
        );

        let again = issue_code(&conn, 1, now).unwrap();
        assert_eq!(redeem(&conn, &again, 43, "aki_t", now).unwrap(), Some(1));
        assert_eq!(chat_for_user(&conn, 1).unwrap(), Some(43), "one account, one chat");
        assert_eq!(link_for_chat(&conn, 42).unwrap(), None);

        let theirs = issue_code(&conn, 2, now).unwrap();
        assert_eq!(redeem(&conn, &theirs, 43, "bo_t", now).unwrap(), Some(2));
        assert_eq!(chat_for_user(&conn, 1).unwrap(), None, "one chat, one account");

        assert!(unlink(&conn, 2).unwrap());
        assert!(!unlink(&conn, 2).unwrap());
        assert_eq!(link_for_chat(&conn, 43).unwrap(), None);
    }

    #[test]
    fn an_unknown_code_links_nothing() {
        let conn = env();
        assert_eq!(redeem(&conn, "ZZZZZZ", 42, "aki", at("2026-09-17T09:00:00Z")).unwrap(), None);
        assert_eq!(link_for_chat(&conn, 42).unwrap(), None);
    }

    #[test]
    fn the_cursor_starts_at_zero_and_only_moves_forward() {
        let conn = env();
        assert_eq!(cursor(&conn).unwrap(), 0);
        set_cursor(&conn, 17).unwrap();
        assert_eq!(cursor(&conn).unwrap(), 17);
        set_cursor(&conn, 9).unwrap();
        assert_eq!(cursor(&conn).unwrap(), 17);
        set_cursor(&conn, 18).unwrap();
        assert_eq!(cursor(&conn).unwrap(), 18);
    }

    #[test]
    fn the_newest_recent_thread_continues_and_a_stale_one_does_not() {
        let conn = env();
        let now = at("2026-09-17T09:00:00Z");
        for (title, stamp) in [
            ("old", Some("2026-09-17T07:00:00Z")),
            ("recent", Some("2026-09-17T08:50:00Z")),
            ("web only", None),
        ] {
            conn.execute(
                "INSERT INTO conversations (user_id, title, created_at, updated_at, telegram_at)
                 VALUES (1, ?1, 'c', 'c', ?2)",
                (title, stamp),
            )
            .unwrap();
        }
        let cutoff = now - jiff::Span::new().minutes(30);
        assert_eq!(thread_for(&conn, 1, cutoff).unwrap(), Some(2));
        assert_eq!(thread_for(&conn, 2, cutoff).unwrap(), None, "another account's thread");

        let cutoff = now - jiff::Span::new().minutes(5);
        assert_eq!(thread_for(&conn, 1, cutoff).unwrap(), None);
    }
}
