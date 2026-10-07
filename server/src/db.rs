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
    // v20
    "
    CREATE TABLE voice_calls (
        id INTEGER PRIMARY KEY,
        token TEXT NOT NULL UNIQUE,
        user_id INTEGER NOT NULL REFERENCES users(id),
        event_id INTEGER REFERENCES events(id),
        message TEXT NOT NULL,
        status TEXT NOT NULL DEFAULT 'placed'
            CHECK (status IN ('placed','ringing','answered','completed','no_answer','busy','failed')),
        digit TEXT,
        call_sid TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE INDEX idx_voice_calls_user ON voice_calls(user_id, created_at DESC);
    ",
    // v21
    "
    ALTER TABLE conversations ADD COLUMN summary TEXT;
    ALTER TABLE conversations ADD COLUMN summarized_at TEXT;
    ALTER TABLE conversations ADD COLUMN summary_through INTEGER;
    CREATE TABLE harvests (
        user_id INTEGER NOT NULL REFERENCES users(id),
        date TEXT NOT NULL,
        facts_written INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        PRIMARY KEY (user_id, date)
    );
    ",
    // v22
    "
    CREATE TABLE calendar_entries_v2 (
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
        CHECK (end_time > start_time),
        CHECK ((days = 0) = (on_date IS NOT NULL))
    );
    INSERT INTO calendar_entries_v2 SELECT * FROM calendar_entries;
    CREATE TABLE calendar_exceptions_v2 (
        entry_id INTEGER NOT NULL REFERENCES calendar_entries_v2(id) ON DELETE CASCADE,
        date TEXT NOT NULL,
        PRIMARY KEY (entry_id, date)
    );
    INSERT INTO calendar_exceptions_v2 SELECT * FROM calendar_exceptions;
    DROP TABLE calendar_exceptions;
    DROP TABLE calendar_entries;
    ALTER TABLE calendar_entries_v2 RENAME TO calendar_entries;
    ALTER TABLE calendar_exceptions_v2 RENAME TO calendar_exceptions;
    CREATE INDEX idx_calendar_entries_user ON calendar_entries(user_id, start_time);
    ",
    // v23
    "
    ALTER TABLE events ADD COLUMN origin TEXT NOT NULL DEFAULT 'template'
        CHECK (origin IN ('template','agent','auto','user'));
    ALTER TABLE events ADD COLUMN decided_at TEXT;
    ",
    // v24
    "
    ALTER TABLE tasks ADD COLUMN completed_at TEXT;
    UPDATE tasks SET completed_at = updated_at WHERE state = 'done';
    ",
    // v25
    "
    DROP TABLE IF EXISTS voice_calls;
    ",
    // v26
    "
    CREATE TABLE telegram_links (
        user_id INTEGER PRIMARY KEY REFERENCES users(id),
        chat_id INTEGER NOT NULL UNIQUE,
        handle TEXT NOT NULL DEFAULT '',
        linked_at TEXT NOT NULL
    );
    CREATE TABLE telegram_link_codes (
        code TEXT PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        expires_at TEXT NOT NULL
    );
    CREATE TABLE telegram_cursor (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        last_update_id INTEGER NOT NULL
    );
    ALTER TABLE conversations ADD COLUMN via TEXT NOT NULL DEFAULT 'web'
        CHECK (via IN ('web','telegram'));
    ALTER TABLE conversations ADD COLUMN telegram_at TEXT;
    CREATE INDEX idx_conversations_telegram ON conversations(user_id, telegram_at DESC);
    ",
    // v27
    "
    CREATE TABLE work_sessions (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        task_id INTEGER REFERENCES tasks(id),
        event_id INTEGER REFERENCES events(id),
        title TEXT NOT NULL,
        planned_min INTEGER,
        started_at TEXT NOT NULL,
        ended_at TEXT,
        outcome TEXT CHECK (outcome IS NULL OR outcome IN ('done','stopped'))
    );
    CREATE INDEX idx_work_sessions_open ON work_sessions(user_id) WHERE ended_at IS NULL;
    CREATE TABLE trigger_budgets (
        user_id INTEGER NOT NULL REFERENCES users(id),
        date TEXT NOT NULL,
        extra INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (user_id, date)
    );
    ALTER TABLE events ADD COLUMN prompt TEXT NOT NULL DEFAULT '';
    ALTER TABLE events ADD COLUMN cancel_if TEXT
        CHECK (cancel_if IS NULL OR cancel_if IN ('replied','task_done','event_decided'));
    ALTER TABLE events ADD COLUMN cancel_ref INTEGER;
    ALTER TABLE events ADD COLUMN conversation_id INTEGER REFERENCES conversations(id);
    ALTER TABLE events ADD COLUMN work_session_id INTEGER REFERENCES work_sessions(id);
    ALTER TABLE events ADD COLUMN created_at TEXT;
    ",
    // v28
    "
    ALTER TABLE tasks ADD COLUMN notify TEXT NOT NULL DEFAULT 'notify'
        CHECK (notify IN ('none','chat','notify'));

    ALTER TABLE work_sessions ADD COLUMN mode TEXT NOT NULL DEFAULT 'single'
        CHECK (mode IN ('single','pomodoro'));
    ALTER TABLE work_sessions ADD COLUMN work_min INTEGER;
    ALTER TABLE work_sessions ADD COLUMN break_min INTEGER;
    ALTER TABLE work_sessions ADD COLUMN phase TEXT NOT NULL DEFAULT 'work'
        CHECK (phase IN ('work','break'));
    ALTER TABLE work_sessions ADD COLUMN phase_started_at TEXT;
    ALTER TABLE work_sessions ADD COLUMN phase_paused_ms INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE work_sessions ADD COLUMN round INTEGER NOT NULL DEFAULT 1;
    ALTER TABLE work_sessions ADD COLUMN paused_at TEXT;
    ALTER TABLE work_sessions ADD COLUMN paused_ms INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE work_sessions ADD COLUMN step_index INTEGER;
    ALTER TABLE work_sessions ADD COLUMN step_count INTEGER;
    ALTER TABLE work_sessions ADD COLUMN step_name TEXT;
    ALTER TABLE work_sessions ADD COLUMN notes TEXT NOT NULL DEFAULT '';
    ALTER TABLE work_sessions ADD COLUMN conversation_id INTEGER REFERENCES conversations(id);
    UPDATE work_sessions SET phase_started_at = started_at WHERE phase_started_at IS NULL;
    ",
    // v29
    "
    ALTER TABLE work_sessions ADD COLUMN overrun_asked_at TEXT;
    ",
    // v30
    "
    ALTER TABLE calendar_entries ADD COLUMN external_id TEXT;
    CREATE UNIQUE INDEX idx_calendar_external
        ON calendar_entries(user_id, external_id) WHERE external_id IS NOT NULL;
    ",
    // v31
    "
    ALTER TABLE tasks ADD COLUMN actual_min INTEGER;
    CREATE TABLE learning (
        user_id INTEGER NOT NULL REFERENCES users(id),
        key TEXT NOT NULL,
        value REAL NOT NULL,
        sample INTEGER NOT NULL,
        computed_at TEXT NOT NULL,
        PRIMARY KEY (user_id, key)
    );
    ",
    // v32
    "
    CREATE TABLE reviews (
        user_id INTEGER NOT NULL REFERENCES users(id),
        week_start TEXT NOT NULL,
        content TEXT NOT NULL,
        created_at TEXT NOT NULL,
        PRIMARY KEY (user_id, week_start)
    );
    ",
    // v33
    "
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
    ",
    // v34
    "
    ALTER TABLE tasks ADD COLUMN progress INTEGER NOT NULL DEFAULT 0
        CHECK (progress BETWEEN 0 AND 100);
    ",
    // v35
    "
    UPDATE tasks SET progress = COALESCE((
        SELECT CAST(ROUND(SUM(w * p) * 1.0 / SUM(w)) AS INTEGER) FROM (
            SELECT COALESCE(s.duration_min, (
                    SELECT AVG(a.duration_min) FROM tasks a
                    WHERE a.parent_id = s.parent_id AND a.state != 'dropped'
                ), 1) AS w,
                CASE WHEN s.state = 'done' THEN 100 ELSE s.progress END AS p
            FROM tasks s WHERE s.parent_id = tasks.id AND s.state != 'dropped'
        )
    ), progress)
    WHERE state != 'done' AND parent_id IS NULL;
    ",
    // v36
    "
    ALTER TABLE conversations ADD COLUMN title_kind TEXT NOT NULL DEFAULT 'draft'
        CHECK (title_kind IN ('draft','generated','user'));
    ",
    // v37
    "
    ALTER TABLE tasks ADD COLUMN category TEXT NOT NULL DEFAULT ''
        CHECK (category = '' OR parent_id IS NULL);
    UPDATE tasks SET category = substr(title, 1, instr(title, ' — ') - 1)
    WHERE parent_id IS NULL AND category = '' AND instr(title, ' — ') > 1;
    ",
    // v38
    "
    CREATE TABLE goals (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        title TEXT NOT NULL,
        description TEXT NOT NULL DEFAULT '',
        due_at TEXT,
        state TEXT NOT NULL DEFAULT 'open'
            CHECK (state IN ('open','done','dropped')),
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE INDEX idx_goals_user ON goals(user_id, id);
    ALTER TABLE tasks ADD COLUMN goal_id INTEGER REFERENCES goals(id)
        CHECK (goal_id IS NULL OR parent_id IS NULL);
    CREATE INDEX idx_tasks_goal ON tasks(goal_id);
    ",
    // v39
    "
    ALTER TABLE tasks ADD COLUMN urgency TEXT NOT NULL DEFAULT 'normal'
        CHECK (urgency IN ('low','normal','high'));
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
        updated_at TEXT NOT NULL,
        UNIQUE(share_id, visitor_key)
    );
    CREATE TABLE share_messages (
        id INTEGER PRIMARY KEY,
        thread_id INTEGER NOT NULL REFERENCES share_threads(id) ON DELETE CASCADE,
        role TEXT NOT NULL CHECK (role IN ('user','assistant','note')),
        content TEXT NOT NULL,
        created_at TEXT NOT NULL
    );
    CREATE INDEX idx_share_messages_thread ON share_messages(thread_id, id);
    ",
    // v40
    "
    ALTER TABLE work_sessions ADD COLUMN end_notified_at TEXT;
    ",
    // v41
    "
    CREATE TABLE share_threads_v2 (
        id INTEGER PRIMARY KEY,
        share_id INTEGER NOT NULL REFERENCES shares(id) ON DELETE CASCADE,
        visitor_key TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    INSERT INTO share_threads_v2 SELECT * FROM share_threads;
    CREATE TABLE share_messages_v2 (
        id INTEGER PRIMARY KEY,
        thread_id INTEGER NOT NULL REFERENCES share_threads_v2(id) ON DELETE CASCADE,
        role TEXT NOT NULL CHECK (role IN ('user','assistant','note')),
        content TEXT NOT NULL,
        created_at TEXT NOT NULL
    );
    INSERT INTO share_messages_v2 SELECT * FROM share_messages;
    DROP TABLE share_messages;
    DROP TABLE share_threads;
    ALTER TABLE share_threads_v2 RENAME TO share_threads;
    ALTER TABLE share_messages_v2 RENAME TO share_messages;
    CREATE INDEX idx_share_threads_share ON share_threads(share_id, visitor_key);
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
    ALTER TABLE users ADD COLUMN seen_lat REAL;
    ALTER TABLE users ADD COLUMN seen_lon REAL;
    ",
    // v42
    "
    CREATE TABLE voice_links (
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
        fell_through_at TEXT
    );
    CREATE INDEX idx_voice_calls_open ON voice_calls(state) WHERE state != 'ended';
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
    ",
    // v43
    "
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
    ",
    // v44
    "
    CREATE TABLE notes (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        text TEXT NOT NULL CHECK (length(text) BETWEEN 1 AND 200),
        pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
        created_at TEXT NOT NULL,
        done_at TEXT,
        last_nudged_at TEXT
    );
    CREATE INDEX idx_notes_user ON notes(user_id, done_at);
    ",
    // v45
    "
    ALTER TABLE users ADD COLUMN last_active_at TEXT;
    ALTER TABLE events ADD COLUMN origin_next TEXT NOT NULL DEFAULT 'template'
        CHECK (origin_next IN ('template','agent','auto','user','idle'));
    UPDATE events SET origin_next = origin;
    ALTER TABLE events DROP COLUMN origin;
    ALTER TABLE events RENAME COLUMN origin_next TO origin;
    ALTER TABLE events ADD COLUMN cancel_if_next TEXT
        CHECK (cancel_if_next IS NULL
               OR cancel_if_next IN ('replied','task_done','event_decided','active'));
    UPDATE events SET cancel_if_next = cancel_if;
    ALTER TABLE events DROP COLUMN cancel_if;
    ALTER TABLE events RENAME COLUMN cancel_if_next TO cancel_if;
    ",
    // v46
    "
    CREATE TABLE server_settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    ",
    // v47
    "
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
    ALTER TABLE voice_calls ADD COLUMN conversation_id INTEGER REFERENCES conversations(id) ON DELETE SET NULL;
    ALTER TABLE voice_calls ADD COLUMN last_reply INTEGER NOT NULL DEFAULT 1;
    ALTER TABLE voice_calls ADD COLUMN bowed_out INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE conversations ADD COLUMN via_next TEXT NOT NULL DEFAULT 'web'
        CHECK (via_next IN ('web','telegram','voice'));
    UPDATE conversations SET via_next = via;
    ALTER TABLE conversations DROP COLUMN via;
    ALTER TABLE conversations RENAME COLUMN via_next TO via;
    ",
    // v48
    "
    ALTER TABLE voice_calls ADD COLUMN inbound_key TEXT;
    CREATE UNIQUE INDEX idx_voice_calls_inbound ON voice_calls(inbound_key) WHERE inbound_key IS NOT NULL;
    ",
    // v49
    "
    DROP TABLE telegram_links;
    DROP TABLE telegram_link_codes;
    DROP TABLE telegram_cursor;
    DROP INDEX idx_conversations_telegram;
    ALTER TABLE conversations DROP COLUMN telegram_at;
    ALTER TABLE conversations ADD COLUMN via_next TEXT NOT NULL DEFAULT 'web'
        CHECK (via_next IN ('web','matrix','voice'));
    UPDATE conversations SET via_next = CASE via WHEN 'telegram' THEN 'web' ELSE via END;
    ALTER TABLE conversations DROP COLUMN via;
    ALTER TABLE conversations RENAME COLUMN via_next TO via;
    ALTER TABLE conversations ADD COLUMN matrix_at TEXT;
    CREATE INDEX idx_conversations_matrix ON conversations(user_id, matrix_at DESC);
    ALTER TABLE voice_links RENAME TO matrix_links;
    CREATE TABLE matrix_cursor (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        next_batch TEXT NOT NULL
    );
    ",
    // v50
    "
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
    ALTER TABLE users ADD COLUMN onboarding INTEGER NOT NULL DEFAULT 0 CHECK (onboarding IN (0, 1));
    ",
    // v51
    "
    CREATE TABLE memory_index_v51 (
        user TEXT NOT NULL,
        id TEXT NOT NULL,
        category TEXT NOT NULL CHECK (category IN ('semantic','episodic','procedural','note')),
        summary TEXT NOT NULL,
        archived INTEGER NOT NULL DEFAULT 0,
        path TEXT NOT NULL,
        until TEXT,
        PRIMARY KEY (user, id)
    );
    INSERT INTO memory_index_v51 (rowid, user, id, category, summary, archived, path, until)
        SELECT rowid, user, id, category, summary, archived, path, until FROM memory_index;
    DROP TABLE memory_index;
    ALTER TABLE memory_index_v51 RENAME TO memory_index;
    ",
];

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
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    apply_migrations(conn, MIGRATIONS)
}

/// Each step and its `user_version` bump commit atomically, so a failure
/// partway through a step leaves the database exactly at the previous version.
fn apply_migrations(conn: &Connection, migrations: &[&str]) -> Result<()> {
    let version = usize::try_from(conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))?)?;
    if version > migrations.len() {
        anyhow::bail!(
            "database schema version {version} is newer than this binary supports ({})",
            migrations.len()
        );
    }
    for (i, sql) in migrations.iter().enumerate().skip(version) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i64::try_from(i + 1)?)?;
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
    fn v22_rebuilds_the_calendar_tables_and_keeps_every_row() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        apply_migrations(&conn, &MIGRATIONS[..21]).unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calendar_entries
             (id, user_id, title, kind, quiet, start_time, end_time, days, on_date,
              created_at, updated_at)
             VALUES (7, 1, 'school', 'fixed', 1, '08:15', '15:30', 31, NULL, 't1', 't2')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calendar_exceptions (entry_id, date) VALUES (7, '2026-09-16')",
            [],
        )
        .unwrap();
        apply_migrations(&conn, MIGRATIONS).unwrap();

        let (id, title, kind, start, created): (i64, String, String, String, String) = conn
            .query_row("SELECT id, title, kind, start_time, created_at FROM calendar_entries", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap();
        assert_eq!((id, title.as_str(), kind.as_str(), start.as_str(), created.as_str()),
                   (7, "school", "fixed", "08:15", "t1"));
        let skipped: (i64, String) = conn
            .query_row("SELECT entry_id, date FROM calendar_exceptions", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(skipped, (7, "2026-09-16".to_string()));

        conn.execute(
            "INSERT INTO calendar_entries
             (user_id, title, kind, quiet, start_time, end_time, days, on_date, created_at, updated_at)
             VALUES (1, 'open afternoon', 'free', 0, '16:00', '18:30', 0, '2026-09-18', 't', 't')",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO calendar_entries
                 (user_id, title, kind, quiet, start_time, end_time, days, on_date, created_at, updated_at)
                 VALUES (1, 'x', 'party', 1, '09:00', '10:00', 0, '2026-09-18', 't', 't')",
                [],
            )
            .is_err(),
            "kind is still closed"
        );
        conn.execute("DELETE FROM calendar_entries WHERE id = 7", []).unwrap();
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM calendar_exceptions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "exceptions still follow their entry");
    }

    #[test]
    fn v23_adds_event_origin_and_decision_time() {
        let conn = open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO plans (user_id, date, created_at) VALUES (1, '2026-09-17', 'x')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time) VALUES (1, 'nudge', '09:15')",
            [],
        )
        .unwrap();
        let (origin, decided): (String, Option<String>) = conn
            .query_row("SELECT origin, decided_at FROM events WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(origin, "template");
        assert!(decided.is_none());
        assert!(
            conn.execute("UPDATE events SET origin = 'somewhere' WHERE id = 1", []).is_err(),
            "origin is closed"
        );
    }

    #[test]
    fn v24_backfills_completion_time_for_finished_tasks() {
        let conn = Connection::open_in_memory().unwrap();
        apply_migrations(&conn, &MIGRATIONS[..23]).unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (user_id, title, state, created_at, updated_at)
             VALUES (1, 'shipped', 'done', 'c', '2026-09-16T12:00:00Z'),
                    (1, 'open one', 'open', 'c', '2026-09-16T12:00:00Z')",
            [],
        )
        .unwrap();
        apply_migrations(&conn, MIGRATIONS).unwrap();
        let done: Option<String> = conn
            .query_row("SELECT completed_at FROM tasks WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(done.as_deref(), Some("2026-09-16T12:00:00Z"));
        let open: Option<String> = conn
            .query_row("SELECT completed_at FROM tasks WHERE id = 2", [], |r| r.get(0))
            .unwrap();
        assert!(open.is_none());
    }

    #[test]
    fn v25_leaves_no_voice_calls_table() {
        let conn = Connection::open_in_memory().unwrap();
        apply_migrations(&conn, &MIGRATIONS[..25]).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 25);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE name = 'voice_calls'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn v27_adds_the_trigger_columns_the_work_sessions_and_the_budgets() {
        let conn = open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO plans (user_id, date, created_at) VALUES (1, '2026-09-17', 'x')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time) VALUES (1, 'trigger', '10:00')",
            [],
        )
        .unwrap();
        let (prompt, rule, created): (String, Option<String>, Option<String>) = conn
            .query_row("SELECT prompt, cancel_if, created_at FROM events WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(prompt, "");
        assert!(rule.is_none() && created.is_none());
        assert!(
            conn.execute("UPDATE events SET cancel_if = 'whenever' WHERE id = 1", []).is_err(),
            "the cancel rule is a closed set"
        );

        conn.execute(
            "INSERT INTO work_sessions (user_id, title, planned_min, started_at)
             VALUES (1, 'read the chapter', 50, 'now')",
            [],
        )
        .unwrap();
        assert!(
            conn.execute("UPDATE work_sessions SET outcome = 'abandoned' WHERE id = 1", [])
                .is_err(),
            "the outcome is a closed set"
        );
        conn.execute("UPDATE events SET work_session_id = 1 WHERE id = 1", []).unwrap();

        conn.execute("INSERT INTO trigger_budgets (user_id, date, extra) VALUES (1, '2026-09-17', 2)", [])
            .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO trigger_budgets (user_id, date, extra) VALUES (1, '2026-09-17', 3)",
                [],
            )
            .is_err(),
            "one day carries one extra"
        );
    }

    #[test]
    fn v28_gives_a_task_its_notice_and_a_session_its_clock() {
        let conn = Connection::open_in_memory().unwrap();
        apply_migrations(&conn, &MIGRATIONS[..27]).unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, started_at)
             VALUES (1, 'the chapter', '2026-09-18T09:00:00Z')",
            [],
        )
        .unwrap();
        apply_migrations(&conn, MIGRATIONS).unwrap();

        let (mode, phase, started, round, paused): (String, String, String, i64, i64) = conn
            .query_row(
                "SELECT mode, phase, phase_started_at, round, paused_ms FROM work_sessions
                 WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!((mode.as_str(), phase.as_str()), ("single", "work"));
        assert_eq!(started, "2026-09-18T09:00:00Z", "a session already running keeps its clock");
        assert_eq!((round, paused), (1, 0));
        assert!(
            conn.execute("UPDATE work_sessions SET mode = 'tomato' WHERE id = 1", []).is_err(),
            "the mode is a closed set"
        );

        conn.execute(
            "INSERT INTO tasks (user_id, title, created_at, updated_at) VALUES (1, 't', 'x', 'x')",
            [],
        )
        .unwrap();
        let notify: String =
            conn.query_row("SELECT notify FROM tasks WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(notify, "notify");
        assert!(
            conn.execute("UPDATE tasks SET notify = 'shout' WHERE id = 1", []).is_err(),
            "notify is a closed set"
        );
    }

    #[test]
    fn v30_gives_a_calendar_entry_a_name_from_outside() {
        let conn = open_memory().unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        let entry = |user: i64, title: &str, external: &str| {
            conn.execute(
                "INSERT INTO calendar_entries
                 (user_id, title, kind, quiet, start_time, end_time, days, on_date, external_id,
                  created_at, updated_at)
                 VALUES (?1, ?2, 'busy', 1, '11:00', '12:00', 0, '2026-09-19', ?3, 't', 't')",
                rusqlite::params![user, title, (!external.is_empty()).then_some(external)],
            )
        };
        entry(1, "standup", "").unwrap();
        let external: Option<String> = conn
            .query_row("SELECT external_id FROM calendar_entries WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert!(external.is_none(), "an entry the user made carries no outside name");

        entry(1, "Club meeting", "gcal:c_8f3a:t:busy").unwrap();
        assert!(
            entry(1, "again", "gcal:c_8f3a:t:busy").is_err(),
            "one external id belongs to one entry per user"
        );
        entry(1, "another", "").expect("entries without an external id do not collide");
        crate::auth::create_user(&conn, "bo", "pw", false).unwrap();
        entry(2, "theirs", "gcal:c_8f3a:t:busy")
            .expect("the same external id in another account is a different entry");
    }

    #[test]
    fn v31_counts_real_minutes_and_holds_what_was_learned() {
        let conn = open_memory().unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (user_id, title, created_at, updated_at) VALUES (1, 't', 'x', 'x')",
            [],
        )
        .unwrap();
        let actual: Option<i64> =
            conn.query_row("SELECT actual_min FROM tasks WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert!(actual.is_none(), "a task nobody has worked on counts nothing yet");

        conn.execute(
            "INSERT INTO learning (user_id, key, value, sample, computed_at)
             VALUES (1, 'plan_factor', 1.4, 9, '2026-09-18T03:00:00Z')",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO learning (user_id, key, value, sample, computed_at)
                 VALUES (1, 'plan_factor', 1.1, 4, '2026-09-19T03:00:00Z')",
                [],
            )
            .is_err(),
            "one user holds one value per key"
        );
    }

    #[test]
    fn v34_starts_a_task_at_no_progress_and_holds_it_in_range() {
        let conn = open_memory().unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (user_id, title, created_at, updated_at) VALUES (1, 't', 'x', 'x')",
            [],
        )
        .unwrap();
        let progress: i64 =
            conn.query_row("SELECT progress FROM tasks WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(progress, 0, "a task nobody has touched is nowhere yet");

        conn.execute("UPDATE tasks SET progress = 100 WHERE id = 1", []).unwrap();
        for out in [-1, 101] {
            assert!(
                conn.execute("UPDATE tasks SET progress = ?1 WHERE id = 1", [out]).is_err(),
                "progress {out} is outside 0..=100"
            );
        }
    }

    #[test]
    fn v35_derives_a_parents_progress_from_its_steps() {
        let conn = Connection::open_in_memory().unwrap();
        apply_migrations(&conn, &MIGRATIONS[..34]).unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO tasks (id, user_id, title, created_at, updated_at)
                 VALUES (1, 1, 'parent', 'x', 'x');
             INSERT INTO tasks (id, user_id, title, created_at, updated_at, parent_id, state)
                 VALUES (2, 1, 'a', 'x', 'x', 1, 'done'), (3, 1, 'b', 'x', 'x', 1, 'open'),
                        (4, 1, 'c', 'x', 'x', 1, 'open'), (5, 1, 'd', 'x', 'x', 1, 'dropped');
             INSERT INTO tasks (id, user_id, title, created_at, updated_at, progress)
                 VALUES (6, 1, 'alone', 'x', 'x', 30);",
        )
        .unwrap();
        apply_migrations(&conn, MIGRATIONS).unwrap();
        let progress = |id: i64| -> i64 {
            conn.query_row("SELECT progress FROM tasks WHERE id = ?1", [id], |r| r.get(0)).unwrap()
        };
        assert_eq!(progress(1), 33, "one of three live steps done");
        assert_eq!(progress(6), 30, "a task without steps keeps its own");
    }

    #[test]
    fn v36_opens_every_conversation_as_a_draft_title() {
        let conn = Connection::open_in_memory().unwrap();
        apply_migrations(&conn, &MIGRATIONS[..35]).unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (id, user_id, title, created_at, updated_at)
             VALUES (1, 1, 'call mom', 'x', 'x')",
            [],
        )
        .unwrap();
        apply_migrations(&conn, MIGRATIONS).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        let kind: String = conn
            .query_row("SELECT title_kind FROM conversations WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kind, "draft");
        assert!(
            conn.execute("UPDATE conversations SET title_kind = 'guessed' WHERE id = 1", [])
                .is_err(),
            "the check rejects a kind outside the three"
        );
        for good in ["generated", "user", "draft"] {
            conn.execute("UPDATE conversations SET title_kind = ?1 WHERE id = 1", [good]).unwrap();
        }
    }

    #[test]
    fn v37_takes_a_category_from_the_title_and_keeps_it_off_steps() {
        let conn = Connection::open_in_memory().unwrap();
        apply_migrations(&conn, &MIGRATIONS[..36]).unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO tasks (id, user_id, title, created_at, updated_at)
                 VALUES (1, 1, 'Biology — chapter 4', 'x', 'x'),
                        (2, 1, 'call the dentist', 'x', 'x'),
                        (3, 1, ' — nothing before it', 'x', 'x');
             INSERT INTO tasks (id, user_id, title, parent_id, created_at, updated_at)
                 VALUES (4, 1, 'History — read it', 1, 'x', 'x');",
        )
        .unwrap();
        apply_migrations(&conn, MIGRATIONS).unwrap();
        let category = |id: i64| -> String {
            conn.query_row("SELECT category FROM tasks WHERE id = ?1", [id], |r| r.get(0)).unwrap()
        };
        assert_eq!(category(1), "Biology");
        assert_eq!(category(2), "", "a title without the separator carries none");
        assert_eq!(category(3), "", "nothing stands before the separator");
        assert_eq!(category(4), "", "a step takes its parent's on read");
        assert!(
            conn.execute("UPDATE tasks SET category = 'History' WHERE id = 4", []).is_err(),
            "a step carries no category of its own"
        );
    }

    #[test]
    fn v38_opens_the_goals_table_and_hangs_top_level_tasks_from_it() {
        let conn = open_memory().unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO goals (user_id, title, created_at, updated_at)
             VALUES (1, 'get into the programme', 't', 't')",
            [],
        )
        .unwrap();
        let (state, due): (String, Option<String>) = conn
            .query_row("SELECT state, due_at FROM goals WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(state, "open");
        assert!(due.is_none());
        assert!(
            conn.execute("UPDATE goals SET state = 'someday' WHERE id = 1", []).is_err(),
            "the state is a closed set"
        );

        conn.execute_batch(
            "INSERT INTO tasks (id, user_id, title, created_at, updated_at)
                 VALUES (1, 1, 'write the essay', 'x', 'x');
             INSERT INTO tasks (id, user_id, title, parent_id, created_at, updated_at)
                 VALUES (2, 1, 'draft it', 1, 'x', 'x');",
        )
        .unwrap();
        conn.execute("UPDATE tasks SET goal_id = 1 WHERE id = 1", []).unwrap();
        assert!(
            conn.execute("UPDATE tasks SET goal_id = 1 WHERE id = 2", []).is_err(),
            "a step hangs from its task, not from a goal"
        );
        assert!(
            conn.execute("UPDATE tasks SET goal_id = 99 WHERE id = 1", []).is_err(),
            "a task cannot name a goal that is not there"
        );
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

    #[test]
    fn v41_keeps_every_share_message_and_lets_a_visitor_hold_many_threads() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        apply_migrations(&conn, &MIGRATIONS[..40]).unwrap();
        conn.execute_batch(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member');
             INSERT INTO shares (id, user_id, name, token, scope, expires_at, created_at, updated_at)
                 VALUES (1, 1, 'Mom', 'share_x', '{}', 'z', 't', 't');
             INSERT INTO share_threads (id, share_id, visitor_key, created_at, updated_at) VALUES (7, 1, 'v1', 't', 't');
             INSERT INTO share_messages (thread_id, role, content, created_at) VALUES (7, 'user', 'hi', 't');
             INSERT INTO share_messages (thread_id, role, content, created_at) VALUES (7, 'assistant', 'hello', 't');",
        )
        .unwrap();
        apply_migrations(&conn, MIGRATIONS).unwrap();
        let kept: i64 = conn
            .query_row("SELECT COUNT(*) FROM share_messages WHERE thread_id = 7", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 2);
        conn.execute("INSERT INTO share_threads (share_id, visitor_key, created_at, updated_at) VALUES (1, 'v1', 't', 't')", [])
            .unwrap();
        conn.execute("DELETE FROM shares WHERE id = 1", []).unwrap();
        let left: i64 = conn.query_row("SELECT COUNT(*) FROM share_messages", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0, "revoking a link still takes its threads with it");
    }

    #[test]
    fn v42_adds_the_voice_tables() {
        let conn = open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO matrix_links (user_id, mxid, state, created_at) VALUES (1, '@a:t', 'invited', 'x')",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO matrix_links (user_id, mxid, state, created_at) VALUES (1, '@b:t', 'invited', 'x')",
                [],
            )
            .is_err(),
            "one link per user"
        );
        conn.execute(
            "INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at, fell_through_at)
             VALUES ('c1', 1, 'outbound', 'starting', 'x', 'x', NULL)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO voice_frames (call_id, seq, body) VALUES ('c1', 1, '{}')", []).unwrap();
        conn.execute(
            "INSERT INTO voice_ops (call_id, op_key, result, created_at) VALUES ('c1', '1:0', '{}', 'x')",
            [],
        )
        .unwrap();
        assert!(conn
            .execute("UPDATE voice_calls SET state = 'dialing' WHERE id = 'c1'", [])
            .is_err());
        conn.execute("DELETE FROM voice_calls WHERE id = 'c1'", []).unwrap();
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM voice_frames", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "frames go with their call");
        conn.execute("DELETE FROM matrix_links", []).unwrap();
        conn.execute(
            "INSERT INTO matrix_links (user_id, mxid, state, created_at) VALUES (1, '@a:t', 'invited', 'x')",
            [],
        )
        .unwrap();
        let id: i64 = conn.query_row("SELECT id FROM matrix_links", [], |r| r.get(0)).unwrap();
        assert_eq!(id, 2, "a relink never reuses an unlinked id");
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
    fn notes_hold_one_line_of_text_and_a_pinned_flag() {
        let conn = open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')", [])
            .unwrap();
        let insert = |text: &str, pinned: i64| {
            conn.execute(
                "INSERT INTO notes (user_id, text, pinned, created_at)
                 VALUES (1, ?1, ?2, '2026-09-30T12:00:00Z')",
                (text, pinned),
            )
        };
        insert("milk", 0).unwrap();
        insert(&"あ".repeat(200), 1).unwrap();
        assert!(insert("", 0).is_err());
        assert!(insert(&"あ".repeat(201), 0).is_err());
        assert!(insert("milk", 2).is_err());
        let (pinned, done, nudged): (i64, Option<String>, Option<String>) = conn
            .query_row("SELECT pinned, done_at, last_nudged_at FROM notes WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!((pinned, done, nudged), (0, None, None));
    }
    #[test]
    fn the_idle_migration_opens_origin_and_cancel_if_and_keeps_every_row() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        apply_migrations(&conn, &MIGRATIONS[..MIGRATIONS.len() - 1]).unwrap();
        conn.execute_batch(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member');
             INSERT INTO plans (user_id, date, created_at) VALUES (1, '2026-09-30', 'x');
             INSERT INTO events (plan_id, kind, wall_time, origin, cancel_if)
                 VALUES (1, 'trigger', '10:00', 'agent', 'replied');
             INSERT INTO event_tasks (event_id, task_id)
                 SELECT 1, id FROM tasks WHERE 0;",
        )
        .unwrap();
        apply_migrations(&conn, MIGRATIONS).unwrap();

        let kept: (String, Option<String>) = conn
            .query_row("SELECT origin, cancel_if FROM events WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(kept, ("agent".to_string(), Some("replied".to_string())));
        conn.execute("UPDATE events SET origin = 'idle', cancel_if = 'active' WHERE id = 1", [])
            .unwrap();
        assert!(
            conn.execute("UPDATE events SET origin = 'somewhere' WHERE id = 1", []).is_err(),
            "origin is still a closed set"
        );
        assert!(
            conn.execute("UPDATE events SET cancel_if = 'whenever' WHERE id = 1", []).is_err(),
            "the cancel rule is still a closed set"
        );
        let fresh: String = {
            conn.execute("INSERT INTO events (plan_id, kind, wall_time) VALUES (1, 'nudge', '11:00')", [])
                .unwrap();
            conn.query_row("SELECT origin FROM events WHERE id = 2", [], |r| r.get(0)).unwrap()
        };
        assert_eq!(fresh, "template");
        let seen: Option<String> = conn
            .query_row("SELECT last_active_at FROM users WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert!(seen.is_none());
    }
    #[test]
    fn v47_adds_voice_jobs_and_opens_via_to_voice() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        apply_migrations(&conn, &MIGRATIONS[..46]).unwrap();
        conn.execute_batch(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member');
             INSERT INTO conversations (user_id, title, created_at, updated_at, via)
                 VALUES (1, 'chat', 'now', 'now', 'telegram');",
        )
        .unwrap();
        apply_migrations(&conn, &MIGRATIONS[..47]).unwrap();
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 47);
        let via: String =
            conn.query_row("SELECT via FROM conversations WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(via, "telegram");
        conn.execute("UPDATE conversations SET via = 'voice' WHERE id = 1", []).unwrap();
        assert!(
            conn.execute("UPDATE conversations SET via = 'sms' WHERE id = 1", []).is_err(),
            "via is still a closed set"
        );
        conn.execute(
            "INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at, conversation_id)
             VALUES ('c1', 1, 'outbound', 'answered', 'x', 'x', 1)",
            [],
        )
        .unwrap();
        let job = |n: i64, state: &str| {
            conn.execute(
                "INSERT INTO voice_jobs (call_id, job, reply, call_index, tool, args, state, started_at)
                 VALUES ('c1', ?1, 1, 0, 'web_search', '{}', ?2, 'x')",
                (n, state),
            )
        };
        conn.execute(
            "INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at, conversation_id)
             VALUES ('c2', 1, 'outbound', 'ended', 'x', 'x', 1)",
            [],
        )
        .unwrap();
        job(1, "running").unwrap();
        assert!(job(1, "done").is_err(), "a job number is one row per call");
        assert!(job(2, "lost").is_err(), "the state is a closed set");
        conn.execute("DELETE FROM voice_calls WHERE id = 'c1'", []).unwrap();
        let left: i64 = conn.query_row("SELECT COUNT(*) FROM voice_jobs", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0, "jobs go with their call");
        conn.execute("DELETE FROM conversations WHERE id = 1", []).unwrap();
        let thread: Option<i64> = conn
            .query_row("SELECT conversation_id FROM voice_calls WHERE id = 'c2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(thread, None, "a deleted thread leaves its call standing");
        let last: i64 =
            conn.query_row("SELECT last_reply FROM voice_calls WHERE id = 'c2'", [], |r| r.get(0)).unwrap();
        assert_eq!(last, 1, "reply 1 is the opening");
    }
    #[test]
    fn v48_keys_inbound_calls_once_each() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        apply_migrations(&conn, &MIGRATIONS[..47]).unwrap();
        conn.execute_batch(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member');
             INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at)
                 VALUES ('c1', 1, 'outbound', 'ended', 'x', 'x');",
        )
        .unwrap();
        apply_migrations(&conn, &MIGRATIONS[..48]).unwrap();
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 48);
        let key: Option<String> =
            conn.query_row("SELECT inbound_key FROM voice_calls WHERE id = 'c1'", [], |r| r.get(0)).unwrap();
        assert_eq!(key, None);
        let call = |id: &str, key: Option<&str>| {
            conn.execute(
                "INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at, inbound_key)
                 VALUES (?1, 1, 'inbound', 'starting', 'x', 'x', ?2)",
                (id, key),
            )
        };
        call("c2", Some("$ev1")).unwrap();
        assert!(call("c3", Some("$ev1")).is_err(), "a call attempt opens one call");
        call("c4", Some("$ev2")).unwrap();
        call("c5", None).unwrap();
        call("c6", None).unwrap();
    }

    #[test]
    fn v49_trades_telegram_for_matrix() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        apply_migrations(&conn, &MIGRATIONS[..48]).unwrap();
        conn.execute_batch(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member');
             INSERT INTO conversations (user_id, title, created_at, updated_at, via, telegram_at)
                 VALUES (1, 'tg', 'now', 'now', 'telegram', 'now'),
                        (1, 'call', 'now', 'now', 'voice', NULL);
             INSERT INTO voice_links (user_id, mxid, room_id, state, created_at)
                 VALUES (1, '@a:t', '!dm:t', 'linked', 'x');",
        )
        .unwrap();
        apply_migrations(&conn, &MIGRATIONS[..49]).unwrap();
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 49);
        let vias: Vec<String> = conn
            .prepare("SELECT via FROM conversations ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(vias, ["web", "voice"]);
        conn.execute("UPDATE conversations SET via = 'matrix', matrix_at = 'now' WHERE id = 1", []).unwrap();
        assert!(conn.execute("UPDATE conversations SET via = 'telegram' WHERE id = 1", []).is_err());
        let room: String =
            conn.query_row("SELECT room_id FROM matrix_links WHERE user_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(room, "!dm:t");
        conn.execute("INSERT INTO matrix_cursor (id, next_batch) VALUES (1, 's1')", []).unwrap();
        for gone in ["telegram_links", "telegram_link_codes", "telegram_cursor", "voice_links"] {
            let n: i64 = conn
                .query_row("SELECT COUNT(*) FROM sqlite_master WHERE name = ?1", [gone], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{gone} is gone");
        }
    }

    #[test]
    fn v50_adds_invites_and_leaves_existing_accounts_out_of_onboarding() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        apply_migrations(&conn, &MIGRATIONS[..49]).unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('a','h','admin')", []).unwrap();
        apply_migrations(&conn, MIGRATIONS).unwrap();
        let onboarding: bool = conn.query_row("SELECT onboarding FROM users WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert!(!onboarding);
        conn.execute(
            "INSERT INTO invites (token_hash, role, created_by, created_at, expires_at) VALUES ('x', 'member', 1, 'now', 0)",
            [],
        )
        .unwrap();
        assert!(conn
            .execute("INSERT INTO invites (token_hash, role, created_at, expires_at) VALUES ('y', 'owner', 'now', 0)", [])
            .is_err());
    }

    #[test]
    fn v51_lets_the_memory_index_hold_notes_and_keeps_its_rows() {
        let conn = Connection::open_in_memory().unwrap();
        apply_migrations(&conn, &MIGRATIONS[..50]).unwrap();
        conn.execute(
            "INSERT INTO memory_index (user, id, category, summary, archived, path, until)
             VALUES ('a', 'x', 'semantic', 's', 1, 'p', '2026-10-08')",
            [],
        )
        .unwrap();
        apply_migrations(&conn, &MIGRATIONS[..51]).unwrap();
        let kept: (String, String, String, i64, String, Option<String>) = conn
            .query_row(
                "SELECT user, category, summary, archived, path, until FROM memory_index WHERE id = 'x'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert_eq!(kept, ("a".into(), "semantic".into(), "s".into(), 1, "p".into(), Some("2026-10-08".into())));
        conn.execute(
            "INSERT INTO memory_index (user, id, category, summary, path) VALUES ('a', 'n', 'note', 's', 'p')",
            [],
        )
        .unwrap();
        assert!(conn
            .execute(
                "INSERT INTO memory_index (user, id, category, summary, path) VALUES ('a', 'y', 'archive', 's', 'p')",
                [],
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO memory_index (user, id, category, summary, path) VALUES ('a', 'x', 'semantic', 's', 'p')",
                [],
            )
            .is_err());
    }
}
