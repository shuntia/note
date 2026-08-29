use anyhow::Result;
use rusqlite::Connection;

pub fn record(conn: &Connection, user_id: Option<i64>, kind: &str, detail: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO event_log (ts, user_id, kind, detail) VALUES (?1, ?2, ?3, ?4)",
        (jiff::Timestamp::now().to_string(), user_id, kind, detail),
    )?;
    Ok(())
}
