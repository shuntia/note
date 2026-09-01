use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

const MIGRATIONS: &[&str] = &[
    // v1
    "
    CREATE TABLE users (
        id INTEGER PRIMARY KEY,
        username TEXT NOT NULL UNIQUE,
        pass_hash TEXT NOT NULL,
        role TEXT NOT NULL CHECK (role IN ('admin','member'))
    );
    CREATE TABLE sessions (
        token TEXT PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        expires_at TEXT NOT NULL
    );
    CREATE TABLE tasks (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        title TEXT NOT NULL,
        description TEXT NOT NULL DEFAULT '',
        state TEXT NOT NULL DEFAULT 'open'
            CHECK (state IN ('open','in_progress','done','dropped')),
        source TEXT NOT NULL DEFAULT 'manual',
        parent_id INTEGER REFERENCES tasks(id),
        notes TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE plans (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        date TEXT NOT NULL,
        created_at TEXT NOT NULL,
        UNIQUE (user_id, date)
    );
    CREATE TABLE events (
        id INTEGER PRIMARY KEY,
        plan_id INTEGER NOT NULL REFERENCES plans(id),
        kind TEXT NOT NULL,
        wall_time TEXT NOT NULL,
        flexibility TEXT NOT NULL DEFAULT 'fixed'
            CHECK (flexibility IN ('fixed','slide','drop')),
        slide_window_min INTEGER NOT NULL DEFAULT 0,
        channel TEXT NOT NULL DEFAULT 'push',
        status TEXT NOT NULL DEFAULT 'pending'
            CHECK (status IN ('pending','fired','snoozed','dropped','done')),
        fired_at TEXT
    );
    CREATE TABLE event_tasks (
        event_id INTEGER NOT NULL REFERENCES events(id),
        task_id INTEGER NOT NULL REFERENCES tasks(id),
        PRIMARY KEY (event_id, task_id)
    );
    CREATE TABLE event_log (
        id INTEGER PRIMARY KEY,
        ts TEXT NOT NULL,
        user_id INTEGER,
        kind TEXT NOT NULL,
        detail TEXT NOT NULL DEFAULT ''
    );
    ",
    // v2
    "
    ALTER TABLE events ADD COLUMN orig_wall_time TEXT NOT NULL DEFAULT '';
    UPDATE events SET orig_wall_time = wall_time WHERE orig_wall_time = '';
    CREATE TABLE memory_index (
        user TEXT NOT NULL,
        id TEXT NOT NULL,
        category TEXT NOT NULL CHECK (category IN ('semantic','episodic','procedural')),
        summary TEXT NOT NULL,
        archived INTEGER NOT NULL DEFAULT 0,
        path TEXT NOT NULL,
        PRIMARY KEY (user, id)
    );
    CREATE VIRTUAL TABLE memory_fts USING fts5(user UNINDEXED, id UNINDEXED, summary, body);
    ",
    // v3
    "
    CREATE TABLE memory_vectors (
        user TEXT NOT NULL,
        id TEXT NOT NULL,
        vector BLOB NOT NULL,
        PRIMARY KEY (user, id)
    );
    CREATE TABLE debriefs (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        date TEXT NOT NULL,
        content TEXT NOT NULL,
        created_at TEXT NOT NULL,
        UNIQUE (user_id, date)
    );
    ",
    // v4
    "
    CREATE TABLE push_subscriptions (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        endpoint TEXT NOT NULL UNIQUE,
        p256dh TEXT NOT NULL,
        auth TEXT NOT NULL,
        created_at TEXT NOT NULL
    );
    ALTER TABLE events ADD COLUMN message TEXT NOT NULL DEFAULT '';
    ",
    // v5
    "
    CREATE TABLE conversations (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        title TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE INDEX idx_conversations_user ON conversations(user_id, updated_at DESC);
    CREATE TABLE talk_messages (
        id INTEGER PRIMARY KEY,
        conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        role TEXT NOT NULL CHECK (role IN ('user','assistant','tool')),
        content TEXT NOT NULL,
        tool_name TEXT,
        tool_args TEXT,
        is_error INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL
    );
    CREATE INDEX idx_talk_messages_conv ON talk_messages(conversation_id, id);
    ",
    // v6
    "
    ALTER TABLE tasks ADD COLUMN duration_min INTEGER
        CHECK (duration_min IS NULL OR (duration_min > 0 AND duration_min % 5 = 0));
    ALTER TABLE tasks ADD COLUMN duration_source TEXT NOT NULL DEFAULT 'none'
        CHECK (duration_source IN ('user','agent','none'));
    CREATE INDEX idx_tasks_parent ON tasks(parent_id);
    ",
    // v7
    "
    ALTER TABLE tasks ADD COLUMN is_now INTEGER NOT NULL DEFAULT 0
        CHECK (is_now IN (0, 1) AND (is_now = 0 OR parent_id IS NULL));
    CREATE INDEX idx_tasks_now ON tasks(user_id, is_now);
    ",
];

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    init(&conn)?;
    Ok(conn)
}

pub fn open_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    init(&conn)?;
    Ok(conn)
}

fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    apply_migrations(conn, MIGRATIONS)
}

/// Each step and its `user_version` bump commit atomically, so a failure
/// partway through a step leaves the database exactly at the previous version.
fn apply_migrations(conn: &Connection, migrations: &[&str]) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version as usize > migrations.len() {
        anyhow::bail!(
            "database schema version {version} is newer than this binary supports ({})",
            migrations.len()
        );
    }
    for (i, sql) in migrations.iter().enumerate().skip(version as usize) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_and_are_idempotent() {
        let conn = open_memory().unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        init(&conn).unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','admin')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn failed_migration_step_rolls_back_entirely() {
        let conn = Connection::open_in_memory().unwrap();
        // second statement fails; the first must not survive
        let bad: &[&str] = &["CREATE TABLE half (id INTEGER); CREATE TABLE half (id INTEGER);"];
        assert!(apply_migrations(&conn, bad).is_err());
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 0);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='half'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn newer_db_version_is_refused() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        let err = apply_migrations(&conn, MIGRATIONS).unwrap_err().to_string();
        assert!(err.contains("99"), "unexpected error: {err}");
    }

    #[test]
    fn v2_backfills_orig_wall_time_from_v1_rows() {
        let conn = Connection::open_in_memory().unwrap();
        apply_migrations(&conn, &MIGRATIONS[..1]).unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO plans (user_id, date, created_at) VALUES (1, '2026-08-31', 'x')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time) VALUES (1, 'nudge', '09:15')",
            [],
        )
        .unwrap();
        apply_migrations(&conn, MIGRATIONS).unwrap();
        let orig: String = conn
            .query_row("SELECT orig_wall_time FROM events WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(orig, "09:15");
    }

    #[test]
    fn v3_creates_vector_and_debrief_tables() {
        let conn = open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')", [],
        ).unwrap();
        conn.execute(
            "INSERT INTO memory_vectors (user, id, vector) VALUES ('a', 'x', X'00000000')", [],
        ).unwrap();
        conn.execute(
            "INSERT INTO debriefs (user_id, date, content, created_at) VALUES (1, '2026-08-31', 'ok', 't')", [],
        ).unwrap();
        // second debrief for the same (user, date) must be refused
        assert!(conn.execute(
            "INSERT INTO debriefs (user_id, date, content, created_at) VALUES (1, '2026-08-31', 'dup', 't')", [],
        ).is_err());
    }

    #[test]
    fn v4_adds_push_subscriptions_and_event_message() {
        let conn = open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO push_subscriptions (user_id, endpoint, p256dh, auth, created_at)
             VALUES (1, 'https://push.example/x', 'k', 'a', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO plans (user_id, date, created_at) VALUES (1, '2026-08-31', 'x')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time) VALUES (1, 'nudge', '09:15')",
            [],
        )
        .unwrap();
        let msg: String = conn
            .query_row("SELECT message FROM events WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(msg, "");
    }

    #[test]
    fn v6_adds_duration_columns_with_five_minute_check() {
        let conn = open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (user_id, title, created_at, updated_at)
             VALUES (1, 't', 'now', 'now')",
            [],
        )
        .unwrap();
        let (dur, src): (Option<i64>, String) = conn
            .query_row("SELECT duration_min, duration_source FROM tasks WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(dur, None);
        assert_eq!(src, "none");
        assert!(conn.execute("UPDATE tasks SET duration_min = 7 WHERE id = 1", []).is_err());
        assert!(conn.execute("UPDATE tasks SET duration_min = 0 WHERE id = 1", []).is_err());
        assert!(conn
            .execute("UPDATE tasks SET duration_source = 'guess' WHERE id = 1", [])
            .is_err());
        conn.execute(
            "UPDATE tasks SET duration_min = 20, duration_source = 'agent' WHERE id = 1",
            [],
        )
        .unwrap();
    }

    #[test]
    fn v7_adds_is_now_and_keeps_it_off_steps() {
        let conn = open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (user_id, title, created_at, updated_at)
             VALUES (1, 'parent', 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (user_id, title, parent_id, created_at, updated_at)
             VALUES (1, 'step', 1, 'now', 'now')",
            [],
        )
        .unwrap();
        let flag: i64 =
            conn.query_row("SELECT is_now FROM tasks WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(flag, 0);
        conn.execute("UPDATE tasks SET is_now = 1 WHERE id = 1", []).unwrap();
        assert!(conn.execute("UPDATE tasks SET is_now = 1 WHERE id = 2", []).is_err());
        assert!(conn.execute("UPDATE tasks SET is_now = 2 WHERE id = 1", []).is_err());
        assert!(conn.execute("UPDATE tasks SET parent_id = 2 WHERE id = 1", []).is_err());
    }

    #[test]
    fn memory_fts_is_available_and_searchable() {
        let conn = open_memory().unwrap();
        conn.execute(
            "INSERT INTO memory_fts (user, id, summary, body) VALUES ('aki', 'x', 'dentist appointment', 'call about the molar')",
            [],
        )
        .unwrap();
        let id: String = conn
            .query_row(
                "SELECT id FROM memory_fts WHERE memory_fts MATCH '\"molar\"' AND user = 'aki'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(id, "x");
    }
}
