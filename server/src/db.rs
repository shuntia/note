use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

/// The schema a fresh database starts from.
const BASELINE_VERSION: usize = 55;

const BASELINE: &str = "
    CREATE TABLE users (
        id INTEGER PRIMARY KEY,
        username TEXT NOT NULL UNIQUE,
        pass_hash TEXT NOT NULL,
        role TEXT NOT NULL CHECK (role IN ('admin','member')),
        disabled INTEGER NOT NULL DEFAULT 0 CHECK (disabled IN (0, 1)),
        category TEXT NOT NULL DEFAULT 'member' CHECK (category IN ('member','test')),
        totp_secret TEXT,
        totp_pending TEXT,
        seen_lat REAL,
        seen_lon REAL,
        last_active_at TEXT,
        onboarding INTEGER NOT NULL DEFAULT 0 CHECK (onboarding IN (0, 1))
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
        state TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open','in_progress','done','dropped')),
        source TEXT NOT NULL DEFAULT 'manual',
        parent_id INTEGER REFERENCES tasks(id),
        notes TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        duration_min INTEGER CHECK (duration_min IS NULL OR (duration_min > 0 AND duration_min % 5 = 0)),
        duration_source TEXT NOT NULL DEFAULT 'none' CHECK (duration_source IN ('user','agent','none')),
        is_now INTEGER NOT NULL DEFAULT 0 CHECK (is_now IN (0, 1) AND (is_now = 0 OR parent_id IS NULL)),
        due_at TEXT CHECK (due_at IS NULL OR parent_id IS NULL),
        external_id TEXT,
        url TEXT NOT NULL DEFAULT '',
        completed_at TEXT,
        notify TEXT NOT NULL DEFAULT 'notify' CHECK (notify IN ('none','chat','notify')),
        actual_min INTEGER,
        progress INTEGER NOT NULL DEFAULT 0 CHECK (progress BETWEEN 0 AND 100),
        category TEXT NOT NULL DEFAULT '' CHECK (category = '' OR parent_id IS NULL),
        goal_id INTEGER REFERENCES goals(id) CHECK (goal_id IS NULL OR parent_id IS NULL),
        urgency TEXT NOT NULL DEFAULT 'normal' CHECK (urgency IN ('low','normal','high'))
    );
    CREATE INDEX idx_tasks_parent ON tasks(parent_id);
    CREATE INDEX idx_tasks_now ON tasks(user_id, is_now);
    CREATE UNIQUE INDEX idx_tasks_external ON tasks(user_id, external_id) WHERE external_id IS NOT NULL;
    CREATE INDEX idx_tasks_goal ON tasks(goal_id);
    CREATE INDEX idx_tasks_user_parent ON tasks(user_id, parent_id, id);
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
        flexibility TEXT NOT NULL DEFAULT 'fixed' CHECK (flexibility IN ('fixed','slide','drop')),
        slide_window_min INTEGER NOT NULL DEFAULT 0,
        channel TEXT NOT NULL DEFAULT 'push',
        status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','fired','snoozed','dropped','done')),
        fired_at TEXT,
        orig_wall_time TEXT NOT NULL DEFAULT '',
        message TEXT NOT NULL DEFAULT '',
        end_wall_time TEXT,
        alert INTEGER NOT NULL DEFAULT 1 CHECK (alert IN (0, 1) AND (alert = 0 OR end_wall_time IS NULL)),
        moved_to_event_id INTEGER REFERENCES events(id),
        span_min INTEGER NOT NULL DEFAULT 15 CHECK (span_min > 0),
        decided_at TEXT,
        prompt TEXT NOT NULL DEFAULT '',
        cancel_ref INTEGER,
        conversation_id INTEGER REFERENCES conversations(id),
        work_session_id INTEGER REFERENCES work_sessions(id),
        created_at TEXT,
        cancel_if TEXT CHECK (cancel_if IS NULL OR cancel_if IN ('replied','task_done','event_decided','active')),
        origin TEXT NOT NULL DEFAULT 'template' CHECK (origin IN ('template','agent','auto','user','idle','lay_day'))
    );
    CREATE INDEX idx_events_plan ON events(plan_id, wall_time);
    CREATE INDEX idx_events_status ON events(status);
    CREATE TABLE event_tasks (
        event_id INTEGER NOT NULL REFERENCES events(id),
        task_id INTEGER NOT NULL REFERENCES tasks(id),
        PRIMARY KEY (event_id, task_id)
    );
    CREATE INDEX idx_event_tasks_task ON event_tasks(task_id);
    CREATE TABLE event_log (
        id INTEGER PRIMARY KEY,
        ts TEXT NOT NULL,
        user_id INTEGER,
        kind TEXT NOT NULL,
        detail TEXT NOT NULL DEFAULT ''
    );
    CREATE INDEX idx_event_log_kind_user ON event_log(kind, user_id, ts);
    CREATE INDEX idx_event_log_repeat ON event_log(kind, user_id, detail);
    CREATE VIRTUAL TABLE memory_fts USING fts5(user UNINDEXED, id UNINDEXED, summary, body);
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
    CREATE TABLE push_subscriptions (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        endpoint TEXT NOT NULL UNIQUE,
        p256dh TEXT NOT NULL,
        auth TEXT NOT NULL,
        created_at TEXT NOT NULL
    );
    CREATE TABLE conversations (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        title TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        checkin_date TEXT,
        summary TEXT,
        summarized_at TEXT,
        summary_through INTEGER,
        title_kind TEXT NOT NULL DEFAULT 'draft' CHECK (title_kind IN ('draft','generated','user')),
        via TEXT NOT NULL DEFAULT 'web' CHECK (via IN ('web','matrix','voice')),
        matrix_at TEXT
    );
    CREATE INDEX idx_conversations_user ON conversations(user_id, updated_at DESC);
    CREATE INDEX idx_conversations_matrix ON conversations(user_id, matrix_at DESC);
    CREATE TABLE talk_messages (
        id INTEGER PRIMARY KEY,
        conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        role TEXT NOT NULL CHECK (role IN ('user','assistant','tool')),
        content TEXT NOT NULL,
        tool_name TEXT,
        tool_args TEXT,
        is_error INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        reasoning TEXT,
        thought_ms INTEGER
    );
    CREATE INDEX idx_talk_messages_conv ON talk_messages(conversation_id, id);
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
    CREATE TABLE api_tokens (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        name TEXT NOT NULL,
        token_hash TEXT NOT NULL UNIQUE,
        created_at TEXT NOT NULL,
        last_used_at TEXT
    );
    CREATE INDEX idx_api_tokens_user ON api_tokens(user_id, id);
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
    CREATE TABLE memory_sources (
        user_id INTEGER NOT NULL REFERENCES users(id),
        source_id TEXT NOT NULL,
        memory_id TEXT NOT NULL,
        PRIMARY KEY (user_id, source_id, memory_id)
    );
    CREATE TABLE task_tombstones (
        user_id INTEGER NOT NULL REFERENCES users(id),
        external_id TEXT NOT NULL,
        deleted_at TEXT NOT NULL,
        PRIMARY KEY (user_id, external_id)
    );
    CREATE TABLE harvests (
        user_id INTEGER NOT NULL REFERENCES users(id),
        date TEXT NOT NULL,
        facts_written INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        PRIMARY KEY (user_id, date)
    );
    CREATE TABLE calendar_entries (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        title TEXT NOT NULL,
        kind TEXT NOT NULL CHECK (kind IN ('fixed','busy','note','free')),
        quiet INTEGER NOT NULL DEFAULT 1 CHECK (quiet IN (0, 1)),
        start_time TEXT NOT NULL,
        end_time TEXT NOT NULL,
        days INTEGER NOT NULL DEFAULT 0 CHECK (days BETWEEN 0 AND 127),
        on_date TEXT,
        from_date TEXT,
        until_date TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        external_id TEXT,
        CHECK (end_time > start_time),
        CHECK ((days = 0) = (on_date IS NOT NULL))
    );
    CREATE INDEX idx_calendar_entries_user ON calendar_entries(user_id, start_time);
    CREATE UNIQUE INDEX idx_calendar_external ON calendar_entries(user_id, external_id) WHERE external_id IS NOT NULL;
    CREATE TABLE calendar_exceptions (
        entry_id INTEGER NOT NULL REFERENCES calendar_entries(id) ON DELETE CASCADE,
        date TEXT NOT NULL,
        PRIMARY KEY (entry_id, date)
    );
    CREATE TABLE work_sessions (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        task_id INTEGER REFERENCES tasks(id),
        event_id INTEGER REFERENCES events(id),
        title TEXT NOT NULL,
        planned_min INTEGER,
        started_at TEXT NOT NULL,
        ended_at TEXT,
        outcome TEXT CHECK (outcome IS NULL OR outcome IN ('done','stopped')),
        mode TEXT NOT NULL DEFAULT 'single' CHECK (mode IN ('single','pomodoro')),
        work_min INTEGER,
        break_min INTEGER,
        phase TEXT NOT NULL DEFAULT 'work' CHECK (phase IN ('work','break')),
        phase_started_at TEXT,
        phase_paused_ms INTEGER NOT NULL DEFAULT 0,
        round INTEGER NOT NULL DEFAULT 1,
        paused_at TEXT,
        paused_ms INTEGER NOT NULL DEFAULT 0,
        step_index INTEGER,
        step_count INTEGER,
        step_name TEXT,
        notes TEXT NOT NULL DEFAULT '',
        conversation_id INTEGER REFERENCES conversations(id),
        overrun_asked_at TEXT,
        end_notified_at TEXT
    );
    CREATE INDEX idx_work_sessions_open ON work_sessions(user_id) WHERE ended_at IS NULL;
    CREATE TABLE trigger_budgets (
        user_id INTEGER NOT NULL REFERENCES users(id),
        date TEXT NOT NULL,
        extra INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (user_id, date)
    );
    CREATE TABLE learning (
        user_id INTEGER NOT NULL REFERENCES users(id),
        key TEXT NOT NULL,
        value REAL NOT NULL,
        sample INTEGER NOT NULL,
        computed_at TEXT NOT NULL,
        PRIMARY KEY (user_id, key)
    );
    CREATE TABLE reviews (
        user_id INTEGER NOT NULL REFERENCES users(id),
        week_start TEXT NOT NULL,
        content TEXT NOT NULL,
        created_at TEXT NOT NULL,
        PRIMARY KEY (user_id, week_start)
    );
    CREATE TABLE agent_traces (
        id INTEGER PRIMARY KEY,
        ts TEXT NOT NULL,
        user_id INTEGER NOT NULL REFERENCES users(id),
        kind TEXT NOT NULL,
        outcome TEXT NOT NULL,
        turns INTEGER NOT NULL,
        tool_calls INTEGER NOT NULL,
        duration_ms INTEGER NOT NULL,
        error TEXT,
        detail TEXT NOT NULL
    );
    CREATE INDEX idx_agent_traces_user ON agent_traces(user_id, id);
    CREATE TABLE goals (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        title TEXT NOT NULL,
        description TEXT NOT NULL DEFAULT '',
        due_at TEXT,
        state TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open','done','dropped')),
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE INDEX idx_goals_user ON goals(user_id, id);
    CREATE TABLE shares (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        name TEXT NOT NULL,
        brief TEXT NOT NULL DEFAULT '',
        token TEXT NOT NULL UNIQUE,
        scope TEXT NOT NULL,
        expires_at TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        last_used_at TEXT
    );
    CREATE INDEX idx_shares_user ON shares(user_id, id);
    CREATE TABLE share_threads (
        id INTEGER PRIMARY KEY,
        share_id INTEGER NOT NULL REFERENCES shares(id) ON DELETE CASCADE,
        visitor_key TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE INDEX idx_share_threads_share ON share_threads(share_id, visitor_key);
    CREATE TABLE share_messages (
        id INTEGER PRIMARY KEY,
        thread_id INTEGER NOT NULL REFERENCES share_threads(id) ON DELETE CASCADE,
        role TEXT NOT NULL CHECK (role IN ('user','assistant','note')),
        content TEXT NOT NULL,
        created_at TEXT NOT NULL
    );
    CREATE INDEX idx_share_messages_thread ON share_messages(thread_id, id);
    CREATE TABLE share_visits (
        id INTEGER PRIMARY KEY,
        share_id INTEGER NOT NULL REFERENCES shares(id) ON DELETE CASCADE,
        visitor_key TEXT NOT NULL,
        city TEXT,
        country TEXT,
        km REAL,
        at TEXT NOT NULL
    );
    CREATE INDEX idx_share_visits_share ON share_visits(share_id, id);
    CREATE TABLE matrix_links (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        user_id INTEGER NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
        mxid TEXT NOT NULL,
        room_id TEXT,
        state TEXT NOT NULL CHECK (state IN ('invited','linked')),
        created_at TEXT NOT NULL,
        linked_at TEXT
    );
    CREATE TABLE voice_calls (
        id TEXT PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        direction TEXT NOT NULL CHECK (direction IN ('outbound','inbound')),
        message TEXT,
        state TEXT NOT NULL CHECK (state IN ('starting','ringing','answered','ended')),
        outcome TEXT,
        ring_by TEXT NOT NULL,
        sent_seq INTEGER NOT NULL DEFAULT 0,
        applied_seq INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        ended_at TEXT,
        fell_through_at TEXT,
        conversation_id INTEGER REFERENCES conversations(id) ON DELETE SET NULL,
        last_reply INTEGER NOT NULL DEFAULT 1,
        bowed_out INTEGER NOT NULL DEFAULT 0,
        inbound_key TEXT,
        origin TEXT NOT NULL DEFAULT 'matrix' CHECK (origin IN ('matrix','web')),
        thread_id INTEGER REFERENCES conversations(id) ON DELETE SET NULL
    );
    CREATE INDEX idx_voice_calls_open ON voice_calls(state) WHERE state != 'ended';
    CREATE UNIQUE INDEX idx_voice_calls_inbound ON voice_calls(inbound_key) WHERE inbound_key IS NOT NULL;
    CREATE TABLE voice_frames (
        call_id TEXT NOT NULL REFERENCES voice_calls(id) ON DELETE CASCADE,
        seq INTEGER NOT NULL,
        body TEXT NOT NULL,
        PRIMARY KEY (call_id, seq)
    );
    CREATE TABLE voice_ops (
        call_id TEXT NOT NULL REFERENCES voice_calls(id) ON DELETE CASCADE,
        op_key TEXT NOT NULL,
        result TEXT NOT NULL,
        created_at TEXT NOT NULL,
        PRIMARY KEY (call_id, op_key)
    );
    CREATE TABLE inbox_items (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        source_id TEXT NOT NULL,
        kind TEXT NOT NULL CHECK (kind IN ('announcement','material')),
        title TEXT NOT NULL,
        body TEXT NOT NULL,
        received_at TEXT NOT NULL,
        outcome TEXT CHECK (outcome IN ('remembered','nothing','task')),
        reason TEXT,
        decided_at TEXT,
        UNIQUE (user_id, source_id)
    );
    CREATE INDEX idx_inbox_items_recent ON inbox_items(user_id, received_at DESC, id DESC);
    CREATE TABLE server_settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    CREATE TABLE voice_jobs (
        call_id TEXT NOT NULL REFERENCES voice_calls(id) ON DELETE CASCADE,
        job INTEGER NOT NULL,
        reply INTEGER NOT NULL,
        call_index INTEGER NOT NULL,
        tool TEXT NOT NULL,
        args TEXT NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('running','done','error','cancelled','timed_out','interrupted')),
        result TEXT,
        started_at TEXT NOT NULL,
        finished_at TEXT,
        PRIMARY KEY (call_id, job)
    );
    CREATE TABLE matrix_cursor (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        next_batch TEXT NOT NULL
    );
    CREATE TABLE invites (
        id INTEGER PRIMARY KEY,
        token_hash TEXT NOT NULL UNIQUE,
        role TEXT NOT NULL CHECK (role IN ('admin','member')),
        username TEXT,
        created_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
        created_at TEXT NOT NULL,
        expires_at INTEGER NOT NULL,
        used_at TEXT,
        used_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
        revoked_at TEXT
    );
    CREATE TABLE memory_index (
        user TEXT NOT NULL,
        id TEXT NOT NULL,
        category TEXT NOT NULL CHECK (category IN ('semantic','episodic','procedural','note')),
        summary TEXT NOT NULL,
        archived INTEGER NOT NULL DEFAULT 0,
        path TEXT NOT NULL,
        until TEXT,
        PRIMARY KEY (user, id)
    );
    CREATE TABLE run_order (
        user_id INTEGER NOT NULL REFERENCES users(id),
        date TEXT NOT NULL,
        position INTEGER NOT NULL,
        task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        UNIQUE (user_id, date, task_id)
    );
    CREATE INDEX idx_run_order_day ON run_order(user_id, date, position);
";

/// Steps after the baseline, in order: entry `i` takes the schema to
/// `BASELINE_VERSION + i + 1`.
const MIGRATIONS: &[&str] = &["DROP TABLE harvests; DROP TABLE reviews;"];

pub fn server_setting(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    conn.query_row("SELECT value FROM server_settings WHERE key = ?1", [key], |r| r.get(0)).optional()
}

pub fn set_server_setting(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO server_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [key, value],
    )
    .map(|_| ())
}

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
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
    conn.set_prepared_statement_cache_capacity(64);
    apply_migrations(conn, BASELINE, MIGRATIONS)
}

/// A fresh database gets the baseline; one at or past it runs the steps it
/// lacks. The baseline and each step commit atomically with their
/// `user_version` bump, so a failure leaves the database where it was.
fn apply_migrations(conn: &Connection, baseline: &str, migrations: &[&str]) -> Result<()> {
    let version = usize::try_from(conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))?)?;
    let latest = BASELINE_VERSION + migrations.len();
    if version > latest {
        anyhow::bail!("database schema version {version} is newer than this binary supports ({latest})");
    }
    if (1..BASELINE_VERSION).contains(&version) {
        anyhow::bail!(
            "database schema version {version} predates the v{BASELINE_VERSION} baseline; \
             upgrade it with a release that still carries the earlier migrations"
        );
    }
    if version == 0 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(baseline)?;
        tx.pragma_update(None, "user_version", i64::try_from(BASELINE_VERSION)?)?;
        tx.commit()?;
    }
    let applied = version.saturating_sub(BASELINE_VERSION);
    for (i, sql) in migrations.iter().enumerate().skip(applied) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i64::try_from(BASELINE_VERSION + i + 1)?)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_database_writes_ahead_and_syncs_at_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("note.db")).unwrap();
        let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        let sync: i64 = conn.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap();
        assert_eq!((mode.as_str(), sync), ("wal", 1));
    }

    #[test]
    fn a_fresh_database_gets_the_baseline_and_reopens_cleanly() {
        let conn = open_memory().unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, (BASELINE_VERSION + MIGRATIONS.len()) as i64);
        init(&conn).unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','admin')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn a_database_at_the_baseline_runs_only_the_later_steps() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", BASELINE_VERSION as i64).unwrap();
        let steps: &[&str] = &["CREATE TABLE later (id INTEGER);"];
        apply_migrations(&conn, "CREATE TABLE never (id INTEGER);", steps).unwrap();
        let names: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(names, ["later"]);
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, BASELINE_VERSION as i64 + 1);
    }

    #[test]
    fn a_database_older_than_the_baseline_is_refused_untouched() {
        for old in [1, BASELINE_VERSION as i64 - 1] {
            let conn = Connection::open_in_memory().unwrap();
            conn.pragma_update(None, "user_version", old).unwrap();
            let err = apply_migrations(&conn, BASELINE, MIGRATIONS).unwrap_err().to_string();
            assert!(err.contains("predates"), "unexpected error: {err}");
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0)).unwrap();
            assert_eq!(n, 0);
        }
    }

    #[test]
    fn a_failed_baseline_rolls_back_entirely() {
        let conn = Connection::open_in_memory().unwrap();
        // second statement fails; the first must not survive
        let bad = "CREATE TABLE half (id INTEGER); CREATE TABLE half (id INTEGER);";
        assert!(apply_migrations(&conn, bad, &[]).is_err());
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
        let err = apply_migrations(&conn, BASELINE, MIGRATIONS).unwrap_err().to_string();
        assert!(err.contains("99"), "unexpected error: {err}");
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
    fn inbox_items_hold_one_row_per_source_and_check_their_words() {
        let conn = open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')", [])
            .unwrap();
        let insert = |source: &str, kind: &str, outcome: Option<&str>| {
            conn.execute(
                "INSERT INTO inbox_items (user_id, source_id, kind, title, body, received_at, outcome)
                 VALUES (1, ?1, ?2, 't', 'b', '2026-09-30T00:00:00.000000Z', ?3)",
                (source, kind, outcome),
            )
        };
        insert("s1", "announcement", None).unwrap();
        insert("s2", "material", Some("task")).unwrap();
        assert!(insert("s1", "material", None).is_err(), "a source is one row");
        assert!(insert("s3", "gossip", None).is_err());
        assert!(insert("s4", "material", Some("maybe")).is_err());
    }

    #[test]
    fn a_web_call_keeps_the_thread_it_was_opened_from() {
        let conn = open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", []).unwrap();
        conn.execute(
            "INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at)
             VALUES ('m', 1, 'outbound', 'starting', 'x', 'x')",
            [],
        )
        .unwrap();
        let origin: String = conn.query_row("SELECT origin FROM voice_calls WHERE id = 'm'", [], |r| r.get(0)).unwrap();
        assert_eq!(origin, "matrix");
        let thread = crate::talk::create(&conn, 1, "chat", jiff::Timestamp::now()).unwrap();
        conn.execute(
            "INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at, origin, thread_id)
             VALUES ('w', 1, 'inbound', 'starting', 'x', 'x', 'web', ?1)",
            [thread],
        )
        .unwrap();
        assert!(conn.execute("UPDATE voice_calls SET origin = 'phone' WHERE id = 'w'", []).is_err());
        conn.execute("DELETE FROM conversations WHERE id = ?1", [thread]).unwrap();
        let kept: Option<i64> = conn.query_row("SELECT thread_id FROM voice_calls WHERE id = 'w'", [], |r| r.get(0)).unwrap();
        assert_eq!(kept, None);
    }

    fn query_plan(conn: &Connection, sql: &str) -> String {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let unbound = std::iter::repeat_n(rusqlite::types::Null, stmt.parameter_count());
        let rows = stmt
            .query_map(rusqlite::params_from_iter(unbound), |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows.join("\n")
    }

    #[test]
    fn the_hot_reads_use_their_indexes() {
        let conn = open_memory().unwrap();
        let cases = [
            (
                "SELECT e.id FROM events e JOIN plans p ON p.id = e.plan_id
                 WHERE p.user_id = ?1 AND p.date = ?2 ORDER BY e.wall_time",
                "idx_events_plan",
                "SCAN e",
            ),
            (
                "SELECT e.id FROM events e JOIN plans p ON p.id = e.plan_id JOIN users u ON u.id = p.user_id
                 WHERE e.status IN ('pending','snoozed') AND e.alert = 1",
                "idx_events_status",
                "SCAN e",
            ),
            (
                "SELECT e.id FROM events e JOIN plans p ON p.id = e.plan_id JOIN users u ON u.id = p.user_id
                 JOIN event_tasks et ON et.event_id = e.id JOIN tasks t ON t.id = et.task_id
                 WHERE e.status = 'pending' AND e.end_wall_time IS NOT NULL AND t.notify != 'none'",
                "idx_events_status",
                "SCAN",
            ),
            (
                "SELECT event_id, task_id FROM event_tasks
                 WHERE task_id = ?1 OR task_id IN (SELECT id FROM tasks WHERE parent_id = ?1)",
                "idx_event_tasks_task",
                "SCAN",
            ),
            (
                "SELECT t.id FROM tasks t
                 WHERE t.user_id = ?1 AND t.state != 'dropped' AND t.parent_id IS NULL ORDER BY t.id",
                "idx_tasks_user_parent",
                "SCAN",
            ),
            (
                "SELECT t.id FROM tasks t JOIN tasks p ON p.id = t.parent_id
                 WHERE p.user_id = ?1 AND p.parent_id IS NULL AND p.state != 'dropped'
                   AND t.state != 'dropped'",
                "idx_tasks_user_parent",
                "SCAN",
            ),
            (
                "SELECT COUNT(*) FROM event_log WHERE user_id = ?1 AND kind = 'agent_session' AND ts > ?2",
                "idx_event_log_kind_user (kind=? AND user_id=? AND ts>?)",
                "SCAN",
            ),
            (
                "SELECT ts FROM event_log WHERE user_id IS ?1 AND kind = ?2 AND detail = ?3
                 ORDER BY id DESC LIMIT 1",
                "idx_event_log_repeat (kind=? AND user_id=? AND detail=?)",
                "TEMP B-TREE",
            ),
        ];
        for (sql, index, absent) in cases {
            let plan = query_plan(&conn, sql);
            assert!(plan.contains(index), "{sql}\n{plan}");
            assert!(!plan.contains(absent), "{sql}\n{plan}");
        }
        let recent = query_plan(
            &conn,
            "SELECT ts FROM event_log WHERE user_id = ?1 AND kind NOT IN ('x') ORDER BY id DESC LIMIT 10",
        );
        assert!(!recent.contains("TEMP B-TREE"), "{recent}");
    }
}
