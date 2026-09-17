use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

#[derive(Debug, Clone)]
pub struct Subscription {
    pub id: i64,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

/// How many devices one account may register. Each stored subscription is an
/// endpoint the server posts to on every delivery.
pub const MAX_PER_USER: usize = 10;

#[derive(Debug, PartialEq)]
pub enum Added {
    Stored,
    /// The endpoint is registered to another account; re-owning the row would
    /// hand that account's notifications to this one.
    Taken,
    TooMany,
}

pub fn add(
    conn: &Connection,
    user_id: i64,
    endpoint: &str,
    p256dh: &str,
    auth: &str,
) -> Result<Added> {
    let owner: Option<i64> = conn
        .query_row(
            "SELECT user_id FROM push_subscriptions WHERE endpoint = ?1",
            [endpoint],
            |r| r.get(0),
        )
        .optional()?;
    match owner {
        Some(id) if id != user_id => return Ok(Added::Taken),
        None if list(conn, user_id)?.len() >= MAX_PER_USER => return Ok(Added::TooMany),
        _ => {}
    }
    conn.execute(
        "INSERT INTO push_subscriptions (user_id, endpoint, p256dh, auth, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(endpoint) DO UPDATE SET
           p256dh = excluded.p256dh, auth = excluded.auth
         WHERE push_subscriptions.user_id = excluded.user_id",
        (user_id, endpoint, p256dh, auth, jiff::Timestamp::now().to_string()),
    )?;
    Ok(Added::Stored)
}

pub fn remove(conn: &Connection, user_id: i64, endpoint: &str) -> Result<bool> {
    let n = conn.execute(
        "DELETE FROM push_subscriptions WHERE user_id = ?1 AND endpoint = ?2",
        (user_id, endpoint),
    )?;
    Ok(n > 0)
}

/// Prune path for endpoints the push service reports gone (HTTP 404/410).
pub fn remove_endpoint(conn: &Connection, endpoint: &str) -> Result<()> {
    conn.execute("DELETE FROM push_subscriptions WHERE endpoint = ?1", [endpoint])?;
    Ok(())
}

pub fn list(conn: &Connection, user_id: i64) -> Result<Vec<Subscription>> {
    let mut stmt = conn
        .prepare("SELECT id, endpoint, p256dh, auth FROM push_subscriptions WHERE user_id = ?1")?;
    let subs = stmt
        .query_map([user_id], |r| {
            Ok(Subscription {
                id: r.get(0)?,
                endpoint: r.get(1)?,
                p256dh: r.get(2)?,
                auth: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(subs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn_with_user() -> rusqlite::Connection {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn add_list_remove_roundtrip() {
        let conn = conn_with_user();
        add(&conn, 1, "https://push.example/a", "pk", "au").unwrap();
        add(&conn, 1, "https://push.example/b", "pk2", "au2").unwrap();
        let subs = list(&conn, 1).unwrap();
        assert_eq!(subs.len(), 2);
        assert!(remove(&conn, 1, "https://push.example/a").unwrap());
        assert!(!remove(&conn, 1, "https://push.example/a").unwrap());
        assert_eq!(list(&conn, 1).unwrap().len(), 1);
    }

    #[test]
    fn resubscribe_same_endpoint_upserts() {
        let conn = conn_with_user();
        add(&conn, 1, "https://push.example/a", "old", "old").unwrap();
        add(&conn, 1, "https://push.example/a", "new", "new").unwrap();
        let subs = list(&conn, 1).unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].p256dh, "new");
    }

    #[test]
    fn another_user_cannot_take_over_an_endpoint() {
        let conn = conn_with_user();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('bo', 'x', 'member')",
            [],
        )
        .unwrap();
        add(&conn, 1, "https://push.example/a", "pk", "au").unwrap();
        assert_eq!(
            add(&conn, 2, "https://push.example/a", "pk2", "au2").unwrap(),
            Added::Taken
        );
        assert!(list(&conn, 2).unwrap().is_empty());
        let subs = list(&conn, 1).unwrap();
        assert_eq!(subs[0].p256dh, "pk");
    }

    #[test]
    fn a_user_holds_at_most_max_per_user_subscriptions() {
        let conn = conn_with_user();
        for i in 0..MAX_PER_USER {
            assert_eq!(
                add(&conn, 1, &format!("https://push.example/{i}"), "pk", "au").unwrap(),
                Added::Stored
            );
        }
        assert_eq!(
            add(&conn, 1, "https://push.example/one-too-many", "pk", "au").unwrap(),
            Added::TooMany
        );
        // replacing the keys of a stored endpoint is not a new row
        assert_eq!(
            add(&conn, 1, "https://push.example/0", "fresh", "au").unwrap(),
            Added::Stored
        );
        assert_eq!(list(&conn, 1).unwrap().len(), MAX_PER_USER);
    }

    #[test]
    fn remove_endpoint_prunes_regardless_of_user() {
        let conn = conn_with_user();
        add(&conn, 1, "https://push.example/a", "pk", "au").unwrap();
        remove_endpoint(&conn, "https://push.example/a").unwrap();
        assert!(list(&conn, 1).unwrap().is_empty());
    }
}
