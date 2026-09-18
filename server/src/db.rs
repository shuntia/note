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
    // v8
    "
    ALTER TABLE events ADD COLUMN end_wall_time TEXT;
    ALTER TABLE events ADD COLUMN alert INTEGER NOT NULL DEFAULT 1
        CHECK (alert IN (0, 1) AND (alert = 0 OR end_wall_time IS NULL));
    ALTER TABLE events ADD COLUMN moved_to_event_id INTEGER REFERENCES events(id);
    ",
    // v9
    "
    ALTER TABLE events ADD COLUMN span_min INTEGER NOT NULL DEFAULT 15
        CHECK (span_min > 0);
    ",
    // v10
    "
    ALTER TABLE users ADD COLUMN disabled INTEGER NOT NULL DEFAULT 0
        CHECK (disabled IN (0, 1));
    CREATE TABLE admin_grants (
        token TEXT PRIMARY KEY,
        session_token TEXT NOT NULL REFERENCES sessions(token) ON DELETE CASCADE,
        user_id INTEGER NOT NULL REFERENCES users(id),
        expires_at INTEGER NOT NULL
    );
    CREATE INDEX idx_admin_grants_session ON admin_grants(session_token);
    CREATE TABLE totp_replay (
        user_id INTEGER PRIMARY KEY REFERENCES users(id),
        last_step INTEGER NOT NULL
    );
    ",
    // v11
    "
    CREATE TABLE api_tokens (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        name TEXT NOT NULL,
        token_hash TEXT NOT NULL UNIQUE,
        created_at TEXT NOT NULL,
        last_used_at TEXT
    );
    CREATE INDEX idx_api_tokens_user ON api_tokens(user_id, id);
    ",
    // v12
    "
    ALTER TABLE users ADD COLUMN category TEXT NOT NULL DEFAULT 'member'
        CHECK (category IN ('member','test'));
    ",
    // v13
    "
    ALTER TABLE users ADD COLUMN totp_secret TEXT;
    ALTER TABLE users ADD COLUMN totp_pending TEXT;
    CREATE TABLE passkeys (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        name TEXT NOT NULL,
        credential TEXT NOT NULL,
        cred_id BLOB NOT NULL UNIQUE,
        created_at TEXT NOT NULL,
        last_used_at TEXT
    );
    CREATE INDEX idx_passkeys_user ON passkeys(user_id, id);
    ",
    // v14
    "
    CREATE TABLE memory_sources (
        user_id INTEGER NOT NULL REFERENCES users(id),
        source_id TEXT NOT NULL,
        memory_id TEXT NOT NULL,
        PRIMARY KEY (user_id, source_id, memory_id)
    );
    ALTER TABLE memory_index ADD COLUMN until TEXT;
    ",
    // v15
    "
    CREATE TABLE calendar_entries (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        title TEXT NOT NULL,
        kind TEXT NOT NULL CHECK (kind IN ('fixed','busy','note')),
        quiet INTEGER NOT NULL DEFAULT 1 CHECK (quiet IN (0, 1)),
        start_time TEXT NOT NULL,
        end_time TEXT NOT NULL,
        days INTEGER NOT NULL DEFAULT 0 CHECK (days BETWEEN 0 AND 127),
        on_date TEXT,
        from_date TEXT,
        until_date TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        CHECK (end_time > start_time),
        CHECK ((days = 0) = (on_date IS NOT NULL))
    );
    CREATE INDEX idx_calendar_entries_user ON calendar_entries(user_id, start_time);
    CREATE TABLE calendar_exceptions (
        entry_id INTEGER NOT NULL REFERENCES calendar_entries(id) ON DELETE CASCADE,
        date TEXT NOT NULL,
        PRIMARY KEY (entry_id, date)
    );
    ",
    // v16
    "
    ALTER TABLE talk_messages ADD COLUMN reasoning TEXT;
    ALTER TABLE talk_messages ADD COLUMN thought_ms INTEGER;
    ",
    // v17
    "
    ALTER TABLE tasks ADD COLUMN due_at TEXT
        CHECK (due_at IS NULL OR parent_id IS NULL);
    ALTER TABLE tasks ADD COLUMN external_id TEXT;
    ALTER TABLE tasks ADD COLUMN url TEXT NOT NULL DEFAULT '';
    CREATE UNIQUE INDEX idx_tasks_external
        ON tasks(user_id, external_id) WHERE external_id IS NOT NULL;
    CREATE TABLE task_tombstones (
        user_id INTEGER NOT NULL REFERENCES users(id),
        external_id TEXT NOT NULL,
        deleted_at TEXT NOT NULL,
        PRIMARY KEY (user_id, external_id)
    );
    ",
    // v18
    "
    ALTER TABLE conversations ADD COLUMN checkin_date TEXT;
    ",
    // v19
    "
    UPDATE events SET flexibility = 'drop'
    WHERE flexibility = 'slide' AND id IN (SELECT event_id FROM event_tasks);
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
    fn v8_adds_the_block_range_and_bell_and_keeps_blocks_silent() {
        let conn = open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')", [])
            .unwrap();
        conn.execute("INSERT INTO plans (user_id, date, created_at) VALUES (1, '2026-08-31', 'x')", [])
            .unwrap();
        conn.execute("INSERT INTO events (plan_id, kind, wall_time) VALUES (1, 'nudge', '09:00')", [])
            .unwrap();
        let (end, alert): (Option<String>, i64) = conn
            .query_row("SELECT end_wall_time, alert FROM events WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(end, None);
        assert_eq!(alert, 1);
        assert!(conn.execute("UPDATE events SET alert = 2 WHERE id = 1", []).is_err());
        assert!(
            conn.execute("UPDATE events SET end_wall_time = '12:30' WHERE id = 1", []).is_err(),
            "a block must not keep its bell"
        );
        conn.execute("UPDATE events SET alert = 0, end_wall_time = '12:30' WHERE id = 1", [])
            .unwrap();
    }

    #[test]
    fn v9_gives_every_event_a_span() {
        let conn = open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')", [])
            .unwrap();
        conn.execute("INSERT INTO plans (user_id, date, created_at) VALUES (1, '2026-09-01', 'x')", [])
            .unwrap();
        conn.execute("INSERT INTO events (plan_id, kind, wall_time) VALUES (1, 'nudge', '09:00')", [])
            .unwrap();
        let span: i64 = conn
            .query_row("SELECT span_min FROM events WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(span, 15);
        assert!(conn.execute("UPDATE events SET span_min = 0 WHERE id = 1", []).is_err());
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

    #[test]
    fn v13_adds_passkeys_and_the_per_user_totp_columns() {
        let conn = open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        let (secret, pending): (Option<String>, Option<String>) = conn
            .query_row("SELECT totp_secret, totp_pending FROM users WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert!(secret.is_none() && pending.is_none());

        conn.execute(
            "INSERT INTO passkeys (user_id, name, credential, cred_id, created_at)
             VALUES (1, 'phone', '{}', X'0102', 'now')",
            [],
        )
        .unwrap();
        let last: Option<String> = conn
            .query_row("SELECT last_used_at FROM passkeys WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert!(last.is_none());
        assert!(
            conn.execute(
                "INSERT INTO passkeys (user_id, name, credential, cred_id, created_at)
                 VALUES (1, 'again', '{}', X'0102', 'now')",
                [],
            )
            .is_err(),
            "one credential id cannot be registered twice"
        );
    }

    #[test]
    fn v14_adds_the_memory_source_map_and_the_until_column() {
        let conn = open_memory().unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_index (user, id, category, summary, path)
             VALUES ('a', 'x', 'semantic', 's', 'p')",
            [],
        )
        .unwrap();
        let until: Option<String> = conn
            .query_row("SELECT until FROM memory_index WHERE id = 'x'", [], |r| r.get(0))
            .unwrap();
        assert!(until.is_none());

        conn.execute(
            "INSERT INTO memory_sources (user_id, source_id, memory_id) VALUES (1, 's1', 'x')",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO memory_sources (user_id, source_id, memory_id) VALUES (1, 's1', 'x')",
                [],
            )
            .is_err(),
            "one memory cannot be recorded twice under the same source"
        );
    }

    #[test]
    fn v15_creates_calendar_entries_and_their_exceptions() {
        let conn = open_memory().unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        let insert = |sql: &str| {
            conn.execute(
                &format!(
                    "INSERT INTO calendar_entries
                     (user_id, title, kind, quiet, start_time, end_time, days, on_date,
                      created_at, updated_at) VALUES {sql}"
                ),
                [],
            )
        };
        insert("(1,'school','fixed',1,'08:15','15:30',31,NULL,'t','t')").unwrap();
        assert!(
            insert("(1,'x','busy',1,'09:00','08:00',31,NULL,'t','t')").is_err(),
            "an entry may not end before it starts"
        );
        assert!(
            insert("(1,'x','party',1,'09:00','10:00',31,NULL,'t','t')").is_err(),
            "kind is closed"
        );
        assert!(
            insert("(1,'x','busy',1,'09:00','10:00',0,NULL,'t','t')").is_err(),
            "a one-off entry carries its date"
        );
        assert!(
            insert("(1,'x','busy',1,'09:00','10:00',31,'2026-09-16','t','t')").is_err(),
            "a recurring entry carries no date"
        );
        assert!(
            insert("(1,'x','busy',1,'09:00','10:00',128,NULL,'t','t')").is_err(),
            "days is a seven-bit mask"
        );

        conn.execute("INSERT INTO calendar_exceptions (entry_id, date) VALUES (1, '2026-09-16')", [])
            .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO calendar_exceptions (entry_id, date) VALUES (1, '2026-09-16')",
                [],
            )
            .is_err(),
            "a date is skipped once"
        );
        conn.execute("DELETE FROM calendar_entries WHERE id = 1", []).unwrap();
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM calendar_exceptions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "exceptions follow their entry");
    }

    #[test]
    fn v16_adds_the_trace_columns_to_talk_messages() {
        let conn = open_memory().unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (user_id, title, created_at, updated_at)
             VALUES (1, 'chat', 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO talk_messages (conversation_id, role, content, created_at)
             VALUES (1, 'assistant', 'hi', 'now')",
            [],
        )
        .unwrap();
        let trace = || -> (Option<String>, Option<i64>) {
            conn.query_row("SELECT reasoning, thought_ms FROM talk_messages WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
        };
        assert_eq!(trace(), (None, None));

        conn.execute(
            "UPDATE talk_messages SET reasoning = 'weighed it up', thought_ms = 1400 WHERE id = 1",
            [],
        )
        .unwrap();
        assert_eq!(trace(), (Some("weighed it up".to_string()), Some(1400)));
    }

    #[test]
    fn v17_adds_the_due_date_the_external_identity_and_the_tombstones() {
        let conn = open_memory().unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (user_id, title, created_at, updated_at)
             VALUES (1, 'essay', 'now', 'now')",
            [],
        )
        .unwrap();
        let (due, ext, url): (Option<String>, Option<String>, String) = conn
            .query_row("SELECT due_at, external_id, url FROM tasks WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert!(due.is_none() && ext.is_none());
        assert_eq!(url, "");

        conn.execute(
            "UPDATE tasks SET due_at = '2026-09-19T14:59:00Z', external_id = 'canvas:1' WHERE id = 1",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (user_id, title, parent_id, created_at, updated_at)
             VALUES (1, 'step', 1, 'now', 'now')",
            [],
        )
        .unwrap();
        assert!(
            conn.execute("UPDATE tasks SET due_at = '2026-09-19T14:59:00Z' WHERE id = 2", [])
                .is_err(),
            "a step carries no due date of its own"
        );

        conn.execute(
            "INSERT INTO tasks (user_id, title, external_id, created_at, updated_at)
             VALUES (1, 'other', NULL, 'now', 'now')",
            [],
        )
        .unwrap();
        assert!(
            conn.execute("UPDATE tasks SET external_id = 'canvas:1' WHERE id = 3", []).is_err(),
            "one external id belongs to one task per user"
        );
        crate::auth::create_user(&conn, "bo", "pw", false).unwrap();
        conn.execute(
            "INSERT INTO tasks (user_id, title, external_id, created_at, updated_at)
             VALUES (2, 'theirs', 'canvas:1', 'now', 'now')",
            [],
        )
        .expect("the same external id in another account is a different task");

        conn.execute(
            "INSERT INTO task_tombstones (user_id, external_id, deleted_at)
             VALUES (1, 'canvas:1', 'now')",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO task_tombstones (user_id, external_id, deleted_at)
                 VALUES (1, 'canvas:1', 'later')",
                [],
            )
            .is_err(),
            "one external id is buried once"
        );
    }

    #[test]
    fn v18_adds_the_checkin_date_to_conversations() {
        let conn = open_memory().unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (user_id, title, created_at, updated_at)
             VALUES (1, 'chat', 'now', 'now')",
            [],
        )
        .unwrap();
        let date = || -> Option<String> {
            conn.query_row("SELECT checkin_date FROM conversations WHERE id = 1", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(date(), None, "a talk thread carries no check-in date");
        conn.execute("UPDATE conversations SET checkin_date = '2026-09-17' WHERE id = 1", [])
            .unwrap();
        assert_eq!(date(), Some("2026-09-17".to_string()));
    }

    #[test]
    fn v11_creates_api_tokens_with_a_unique_hash() {
        let conn = open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO api_tokens (user_id, name, token_hash, created_at)
             VALUES (1, 'cli', 'abc', 'now')",
            [],
        )
        .unwrap();
        assert!(conn
            .execute(
                "INSERT INTO api_tokens (user_id, name, token_hash, created_at)
                 VALUES (1, 'other', 'abc', 'now')",
                [],
            )
            .is_err());
        let last: Option<String> = conn
            .query_row("SELECT last_used_at FROM api_tokens WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert!(last.is_none());
    }
}
