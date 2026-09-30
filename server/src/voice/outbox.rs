use note_voice_proto::{CallBody, Outbox};
use rusqlite::Connection;
use std::io;
use std::sync::{Arc, Mutex};

/// Note's outbound call frames, kept in `voice_frames` until acknowledged.
pub struct SqliteOutbox {
    db: Arc<Mutex<Connection>>,
}

impl SqliteOutbox {
    pub fn new(db: Arc<Mutex<Connection>>) -> Self {
        Self { db }
    }
}

fn io_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

impl Outbox for SqliteOutbox {
    fn append(&mut self, call_id: &str, body: &CallBody) -> io::Result<u64> {
        let conn = crate::db_guard(&self.db);
        let tx = conn.unchecked_transaction().map_err(io_err)?;
        let seq: i64 = tx
            .query_row(
                "UPDATE voice_calls SET sent_seq = sent_seq + 1 WHERE id = ?1 RETURNING sent_seq",
                [call_id],
                |r| r.get(0),
            )
            .map_err(io_err)?;
        tx.execute(
            "INSERT INTO voice_frames (call_id, seq, body) VALUES (?1, ?2, ?3)",
            (call_id, seq, serde_json::to_string(body)?),
        )
        .map_err(io_err)?;
        tx.commit().map_err(io_err)?;
        Ok(seq as u64)
    }

    fn unacked(&self, call_id: &str, after: u64) -> io::Result<Vec<(u64, CallBody)>> {
        let conn = crate::db_guard(&self.db);
        let mut stmt = conn
            .prepare("SELECT seq, body FROM voice_frames WHERE call_id = ?1 AND seq > ?2 ORDER BY seq")
            .map_err(io_err)?;
        let rows = stmt
            .query_map((call_id, after as i64), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(io_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, body) = row.map_err(io_err)?;
            out.push((seq as u64, serde_json::from_str(&body)?));
        }
        Ok(out)
    }

    fn ack(&mut self, call_id: &str, upto: u64) -> io::Result<()> {
        crate::db_guard(&self.db)
            .execute("DELETE FROM voice_frames WHERE call_id = ?1 AND seq <= ?2", (call_id, upto as i64))
            .map_err(io_err)?;
        Ok(())
    }

    fn pending_calls(&self) -> io::Result<Vec<String>> {
        let conn = crate::db_guard(&self.db);
        let mut stmt = conn
            .prepare("SELECT DISTINCT call_id FROM voice_frames ORDER BY call_id")
            .map_err(io_err)?;
        let ids = stmt.query_map([], |r| r.get(0)).map_err(io_err)?;
        ids.collect::<Result<Vec<String>, _>>().map_err(io_err)
    }

    fn forget(&mut self, call_id: &str) -> io::Result<()> {
        crate::db_guard(&self.db)
            .execute("DELETE FROM voice_frames WHERE call_id = ?1", [call_id])
            .map_err(io_err)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use note_voice_proto::{CallBody, Outbox};

    fn db() -> Arc<Mutex<Connection>> {
        let conn = crate::db::open_memory().unwrap();
        conn.execute_batch(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member');
             INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at)
                 VALUES ('c1', 1, 'outbound', 'starting', 'x', 'x');",
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
    }

    #[test]
    fn append_ack_and_replay() {
        let mut o = SqliteOutbox::new(db());
        assert_eq!(o.append("c1", &CallBody::HangUp).unwrap(), 1);
        assert_eq!(o.append("c1", &CallBody::HangUp).unwrap(), 2);
        o.ack("c1", 1).unwrap();
        assert_eq!(o.unacked("c1", 0).unwrap(), vec![(2, CallBody::HangUp)]);
        assert_eq!(o.pending_calls().unwrap(), vec!["c1".to_string()]);
        o.ack("c1", 2).unwrap();
        assert!(o.pending_calls().unwrap().is_empty());
        assert_eq!(o.append("c1", &CallBody::HangUp).unwrap(), 3, "sent_seq keeps counting");
    }

    #[test]
    fn append_needs_the_call_row() {
        let mut o = SqliteOutbox::new(db());
        assert!(o.append("nope", &CallBody::HangUp).is_err());
    }
}
