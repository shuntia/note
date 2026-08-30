use anyhow::Result;
use rusqlite::Connection;

#[derive(Debug, Clone)]
pub struct Subscription {
    pub id: i64,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

pub fn add(conn: &Connection, user_id: i64, endpoint: &str, p256dh: &str, auth: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO push_subscriptions (user_id, endpoint, p256dh, auth, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(endpoint) DO UPDATE SET
           user_id = excluded.user_id, p256dh = excluded.p256dh, auth = excluded.auth",
        (user_id, endpoint, p256dh, auth, jiff::Timestamp::now().to_string()),
    )?;
    Ok(())
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
    fn remove_endpoint_prunes_regardless_of_user() {
        let conn = conn_with_user();
        add(&conn, 1, "https://push.example/a", "pk", "au").unwrap();
        remove_endpoint(&conn, "https://push.example/a").unwrap();
        assert!(list(&conn, 1).unwrap().is_empty());
    }
}
