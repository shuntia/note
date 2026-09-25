use crate::agent::SessionDeps;
use crate::tools::SessionKind;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

/// The plan event the week's letter is delivered on.
pub const EVENT_KIND: &str = "review";
/// How long after the morning debrief "Your week" lands.
const AFTER_DEBRIEF_MIN: i64 = 1;
const MAX_DIGEST_BYTES: usize = 32 * 1024;
const MAX_ROWS: i64 = 60;
const MAX_SUMMARY_CHARS: usize = 400;
/// Newest episodic facts scanned for ones the week created.
const EPISODIC_SCAN: usize = 40;

/// The Monday on or before `date`.
pub fn monday_of(date: jiff::civil::Date) -> jiff::civil::Date {
    let back = jiff::Span::new().days(i64::from(date.weekday().to_monday_zero_offset()));
    date.checked_sub(back).unwrap_or(date)
}

/// The week a nightly run for `date` looks back on: the seven days ending the
/// night before, and only when `date` opens a new one.
pub fn reviewed_week(date: jiff::civil::Date) -> Option<jiff::civil::Date> {
    (date.weekday() == jiff::civil::Weekday::Monday)
        .then(|| date.checked_sub(jiff::Span::new().days(7)).ok())
        .flatten()
}

/// The half-open stretch of instants a week covers, in the user's own zone, so
/// rows stamped in UTC can be compared as the strings they are stored as.
struct Window {
    start: jiff::civil::Date,
    end: jiff::civil::Date,
    from: String,
    to: String,
}

impl Window {
    fn of(week_start: jiff::civil::Date, tz: &jiff::tz::TimeZone) -> Result<Self> {
        let end = week_start.checked_add(jiff::Span::new().days(7))?;
        let at = |d: jiff::civil::Date| -> Result<String> {
            let dt = d.to_datetime(jiff::civil::Time::midnight());
            Ok(tz.to_ambiguous_zoned(dt).compatible()?.timestamp().to_string())
        };
        Ok(Self { start: week_start, end, from: at(week_start)?, to: at(end)? })
    }

    fn holds_date(&self, date: &str) -> bool {
        date >= self.start.to_string().as_str() && date < self.end.to_string().as_str()
    }
}

fn clip(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX_SUMMARY_CHARS {
        return flat;
    }
    flat.chars().take(MAX_SUMMARY_CHARS - 1).chain(std::iter::once('…')).collect()
}

fn day_label(stamp: &str, tz: &jiff::tz::TimeZone) -> String {
    stamp
        .parse::<jiff::Timestamp>()
        .map(|ts| ts.to_zoned(tz.clone()).strftime("%a").to_string())
        .unwrap_or_else(|_| "?".into())
}

fn section(title: &str, lines: Vec<String>) -> String {
    if lines.is_empty() {
        return String::new();
    }
    format!("## {title}\n{}\n\n", lines.join("\n"))
}

fn tasks(conn: &Connection, user_id: i64, w: &Window, tz: &jiff::tz::TimeZone) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT title, state, COALESCE(completed_at, updated_at)
         FROM tasks
         WHERE user_id = ?1 AND parent_id IS NULL
           AND ((state = 'done' AND completed_at >= ?2 AND completed_at < ?3)
                OR (state = 'dropped' AND updated_at >= ?2 AND updated_at < ?3))
         ORDER BY 3 LIMIT ?4",
    )?;
    let lines = stmt
        .query_map((user_id, &w.from, &w.to, MAX_ROWS), |r| {
            let title: String = r.get(0)?;
            let state: String = r.get(1)?;
            let at: String = r.get(2)?;
            Ok(format!("- {} ({state}, {})", clip(&title), day_label(&at, tz)))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(section("Tasks", lines))
}

fn sessions(conn: &Connection, user_id: i64, w: &Window, tz: &jiff::tz::TimeZone) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT title, planned_min, started_at, ended_at, outcome, overrun_asked_at, paused_ms
         FROM work_sessions
         WHERE user_id = ?1 AND started_at >= ?2 AND started_at < ?3
         ORDER BY started_at LIMIT ?4",
    )?;
    let lines = stmt
        .query_map((user_id, &w.from, &w.to, MAX_ROWS), |r| {
            let title: String = r.get(0)?;
            let planned: Option<i64> = r.get(1)?;
            let started: String = r.get(2)?;
            let ended: Option<String> = r.get(3)?;
            let outcome: Option<String> = r.get(4)?;
            let overrun: Option<String> = r.get(5)?;
            let paused_ms: i64 = r.get(6)?;
            let mut line = format!("- {} ({})", clip(&title), day_label(&started, tz));
            if let Some(p) = planned {
                line.push_str(&format!(", planned {p} min"));
            }
            if let Some(min) = elapsed_min(&started, ended.as_deref(), paused_ms) {
                line.push_str(&format!(", ran {min} min"));
            }
            match outcome.as_deref() {
                Some(o) => line.push_str(&format!(", {o}")),
                None => line.push_str(", never ended"),
            }
            if overrun.is_some() {
                line.push_str(", asked about overrun");
            }
            Ok(line)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(section("Sessions", lines))
}

fn elapsed_min(started: &str, ended: Option<&str>, paused_ms: i64) -> Option<i64> {
    let from: jiff::Timestamp = started.parse().ok()?;
    let to: jiff::Timestamp = ended?.parse().ok()?;
    let ms = (to - from).total(jiff::Unit::Millisecond).ok()? as i64 - paused_ms;
    Some((ms / 60_000).max(0))
}

fn triggers(conn: &Connection, user_id: i64, w: &Window) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT e.status, COUNT(*) FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND p.date >= ?2 AND p.date < ?3 AND e.kind = ?4
         GROUP BY e.status ORDER BY e.status",
    )?;
    let by_status = stmt
        .query_map(
            (user_id, w.start.to_string(), w.end.to_string(), crate::triggers::KIND),
            |r| Ok(format!("{} {}", r.get::<_, i64>(1)?, r.get::<_, String>(0)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if by_status.is_empty() {
        return Ok(String::new());
    }
    let spoken = |kind: &str| -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM event_log
             WHERE user_id = ?1 AND kind = ?2 AND ts >= ?3 AND ts < ?4",
            (user_id, kind, &w.from, &w.to),
            |r| r.get(0),
        )
        .unwrap_or(0)
    };
    Ok(section(
        "Trigger points",
        vec![format!(
            "{} — said {}, stayed quiet {}",
            by_status.join(", "),
            spoken("trigger_said"),
            spoken("trigger_quiet"),
        )],
    ))
}

fn nights(conn: &Connection, user_id: i64, w: &Window) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT date, facts_written FROM harvests
         WHERE user_id = ?1 AND date >= ?2 AND date < ?3 ORDER BY date",
    )?;
    let lines = stmt
        .query_map((user_id, w.start.to_string(), w.end.to_string()), |r| {
            Ok(format!("- {}: {} fact(s) kept", r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(section("Nights", lines))
}

fn conversations(
    conn: &Connection,
    user_id: i64,
    w: &Window,
    tz: &jiff::tz::TimeZone,
) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT title, summary, updated_at FROM conversations
         WHERE user_id = ?1 AND summary IS NOT NULL AND summary != ''
           AND updated_at >= ?2 AND updated_at < ?3
         ORDER BY updated_at LIMIT ?4",
    )?;
    let lines = stmt
        .query_map((user_id, &w.from, &w.to, MAX_ROWS), |r| {
            let title: String = r.get(0)?;
            let summary: String = r.get(1)?;
            let at: String = r.get(2)?;
            Ok(format!("- {} ({}): {}", clip(&title), day_label(&at, tz), clip(&summary)))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(section("Conversations", lines))
}

fn days_remembered(
    conn: &Connection,
    data_dir: &std::path::Path,
    username: &str,
    w: &Window,
) -> Result<String> {
    let mut lines = Vec::new();
    for hit in crate::memory::list(conn, username, Some("episodic"), EPISODIC_SCAN)? {
        let Some(f) = crate::memory::read(data_dir, username, &hit.id)? else { continue };
        let created = f.created.get(..10).unwrap_or_default();
        if !w.holds_date(created) {
            continue;
        }
        lines.push(format!("- {}\n{}", f.summary, f.body.trim()));
    }
    lines.reverse();
    Ok(section("The week as memory holds it", lines))
}

/// The week just ended, as the review session reads it: what finished and what
/// was let go, the sessions behind it, the nudges, the nights' reading and the
/// episodic entries the harvest already kept.
pub fn digest(
    conn: &Connection,
    data_dir: &std::path::Path,
    user_id: i64,
    username: &str,
    tz: &jiff::tz::TimeZone,
    week_start: jiff::civil::Date,
) -> Result<String> {
    let w = Window::of(week_start, tz)?;
    let parts = [
        tasks(conn, user_id, &w, tz)?,
        sessions(conn, user_id, &w, tz)?,
        triggers(conn, user_id, &w)?,
        nights(conn, user_id, &w)?,
        conversations(conn, user_id, &w, tz)?,
        days_remembered(conn, data_dir, username, &w)?,
    ];
    let mut out = String::new();
    for part in parts {
        if out.len() + part.len() > MAX_DIGEST_BYTES {
            continue;
        }
        out.push_str(&part);
    }
    Ok(out.trim_end().to_string())
}

/// One week's letter, written on the Monday nightly after the harvest: the
/// `reviews` row is the record and the idempotency marker, so a week is read
/// once however many times the sweep comes round.
pub fn run_for_user(
    deps: &SessionDeps,
    user_id: i64,
    username: &str,
    tz: &jiff::tz::TimeZone,
    date: jiff::civil::Date,
    now: jiff::Timestamp,
) -> Result<()> {
    let Some(week_start) = reviewed_week(date) else { return Ok(()) };
    let week = week_start.to_string();
    let digest = {
        let conn = crate::db_guard(deps.db);
        let done: i64 = conn.query_row(
            "SELECT COUNT(*) FROM reviews WHERE user_id = ?1 AND week_start = ?2",
            (user_id, &week),
            |r| r.get(0),
        )?;
        if done > 0 {
            return Ok(());
        }
        digest(&conn, deps.data_dir, user_id, username, tz, week_start)?
    };
    if digest.is_empty() {
        return Ok(());
    }
    let deps = SessionDeps {
        db: deps.db,
        config_dir: deps.config_dir,
        data_dir: deps.data_dir,
        llm: deps.llm,
        embeddings: deps.embeddings,
        search: deps.search,
        task_scope: None,
        inbox_source: None,
        memory_source: Some(format!("review:{week}")),
        token_id: deps.token_id,
        thread_note: None,
        share: None,
    };
    let sunday = week_start.checked_add(jiff::Span::new().days(6))?;
    let opening = format!("The week of {week}, up to and including {sunday}.\n\n{digest}");
    let out = crate::agent::run_session(
        &deps,
        user_id,
        username,
        SessionKind::Review,
        now,
        &[],
        &opening,
    )?;
    let text = out
        .steps
        .iter()
        .rev()
        .find(|s| s.name == "review_write" && !s.is_error)
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s.result).ok())
        .and_then(|v| v["text"].as_str().map(str::to_string))
        .ok_or_else(|| anyhow::anyhow!("the session wrote no review"))?;
    let conn = crate::db_guard(deps.db);
    conn.execute(
        "INSERT OR IGNORE INTO reviews (user_id, week_start, content, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        (user_id, &week, &text, now.to_string()),
    )?;
    Ok(())
}

/// Puts "Your week" on the day's plan, a minute behind the debrief it stands
/// beside. Nothing is laid on a day with no review to deliver, no debrief to
/// follow, or a review event already there.
pub fn lay_event(conn: &Connection, user_id: i64, date: jiff::civil::Date) -> Result<Option<i64>> {
    let Some(week_start) = reviewed_week(date) else { return Ok(None) };
    let has: i64 = conn.query_row(
        "SELECT COUNT(*) FROM reviews WHERE user_id = ?1 AND week_start = ?2",
        (user_id, week_start.to_string()),
        |r| r.get(0),
    )?;
    if has == 0 {
        return Ok(None);
    }
    let row: Option<(i64, String, String)> = conn
        .query_row(
            "SELECT e.plan_id, e.wall_time, e.channel FROM events e
             JOIN plans p ON p.id = e.plan_id
             WHERE p.user_id = ?1 AND p.date = ?2 AND e.kind = 'debrief'
             ORDER BY e.wall_time LIMIT 1",
            (user_id, date.to_string()),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((plan_id, debrief_at, channel)) = row else { return Ok(None) };
    let already: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE plan_id = ?1 AND kind = ?2",
        (plan_id, EVENT_KIND),
        |r| r.get(0),
    )?;
    if already > 0 {
        return Ok(None);
    }
    let Some(wall) = minutes_after(&debrief_at, AFTER_DEBRIEF_MIN) else { return Ok(None) };
    conn.execute(
        "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, flexibility,
                             slide_window_min, channel, origin, created_at)
         VALUES (?1, ?2, ?3, ?3, 'drop', 0, ?4, 'template', ?5)",
        (plan_id, EVENT_KIND, &wall, &channel, jiff::Timestamp::now().to_string()),
    )?;
    Ok(Some(conn.last_insert_rowid()))
}

fn minutes_after(wall: &str, minutes: i64) -> Option<String> {
    let time: jiff::civil::Time = format!("{wall}:00").parse().ok()?;
    Some(time.wrapping_add(jiff::Span::new().minutes(minutes)).strftime("%H:%M").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{mock::MockLLM, ChatResponse, ToolCall};
    use std::sync::Mutex;

    fn env() -> (Mutex<Connection>, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let write = |rel: &str, c: &str| {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        };
        write(
            "defaults/user.toml",
            "display_name = \"X\"\ntimezone = \"Asia/Tokyo\"\ntemplate = \"default\"\n",
        );
        write("defaults/prompts/review.md", "read the week");
        (Mutex::new(conn), tmp)
    }

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    fn tokyo() -> jiff::tz::TimeZone {
        jiff::tz::TimeZone::get("Asia/Tokyo").unwrap()
    }

    fn date(d: &str) -> jiff::civil::Date {
        d.parse().unwrap()
    }

    fn deps<'a>(
        db: &'a Mutex<Connection>,
        tmp: &'a tempfile::TempDir,
        llm: &'a dyn crate::providers::LLMProvider,
    ) -> SessionDeps<'a> {
        SessionDeps {
            db,
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            llm,
            embeddings: None,
            search: None,
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            token_id: None,
            thread_note: None,
            share: None,
        }
    }

    fn a_week_of_work(conn: &Connection) {
        conn.execute(
            "INSERT INTO tasks (id, user_id, title, state, created_at, updated_at, completed_at)
             VALUES (1, 1, 'the essay', 'done', '2026-09-14T00:00:00Z', '2026-09-16T04:00:00Z',
                     '2026-09-16T04:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, user_id, title, state, created_at, updated_at)
             VALUES (2, 1, 'the recital', 'dropped', '2026-09-14T00:00:00Z', '2026-09-17T04:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, user_id, title, state, created_at, updated_at, completed_at)
             VALUES (3, 1, 'next week', 'done', '2026-09-21T00:00:00Z', '2026-09-22T04:00:00Z',
                     '2026-09-22T04:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, planned_min, started_at, ended_at, outcome,
                                        overrun_asked_at, phase_started_at)
             VALUES (1, 'essay draft', 50, '2026-09-16T01:00:00Z', '2026-09-16T02:00:00Z', 'done',
                     '2026-09-16T01:50:00Z', '2026-09-16T01:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO harvests (user_id, date, facts_written, created_at)
             VALUES (1, '2026-09-16', 3, '2026-09-15T18:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (id, user_id, title, created_at, updated_at, summary)
             VALUES (9, 1, 'mira', '2026-09-15T02:00:00Z', '2026-09-15T03:00:00Z',
                     'Aki and Mira settled the weekend.')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO plans (id, user_id, date, created_at) VALUES (5, 1, '2026-09-16', 'x')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, status)
             VALUES (5, 'trigger', '11:00', '11:00', 'done')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO event_log (ts, user_id, kind, detail)
             VALUES ('2026-09-16T02:00:00Z', 1, 'trigger_said', 'event 1: how is it going')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn the_digest_holds_the_week_that_ended_and_nothing_either_side_of_it() {
        let (db, tmp) = env();
        let conn = db.lock().unwrap();
        a_week_of_work(&conn);
        let id = crate::memory::add_until(
            &conn,
            tmp.path(),
            "aki",
            "episodic",
            "2026-09-16 · the essay: Aki sent the draft",
            "Aki drafted for an hour and sent it.",
            Some("2026-12-15"),
            None,
        )
        .unwrap();
        let raw = crate::memory::read_raw(tmp.path(), "aki", &id).unwrap().unwrap();
        let dated: Vec<&str> = raw
            .lines()
            .map(|l| if l.starts_with("created: ") { "created: 2026-09-16T05:00:00Z" } else { l })
            .collect();
        crate::memory::write_raw(&conn, tmp.path(), "aki", &id, &format!("{}\n", dated.join("\n")))
            .unwrap();

        let out =
            digest(&conn, tmp.path(), 1, "aki", &tokyo(), date("2026-09-14")).unwrap();
        assert!(out.contains("- the essay (done, Wed)"), "{out}");
        assert!(out.contains("- the recital (dropped, Thu)"), "{out}");
        assert!(!out.contains("next week"), "the week after is not this week's: {out}");
        assert!(out.contains("planned 50 min, ran 60 min, done, asked about overrun"), "{out}");
        assert!(out.contains("1 done — said 1, stayed quiet 0"), "{out}");
        assert!(out.contains("- 2026-09-16: 3 fact(s) kept"), "{out}");
        assert!(out.contains("Aki and Mira settled the weekend."), "{out}");
        assert!(out.contains("2026-09-16 · the essay: Aki sent the draft"), "{out}");
        assert!(out.len() <= MAX_DIGEST_BYTES);
    }

    #[test]
    fn only_a_monday_looks_back_and_it_looks_back_exactly_one_week() {
        assert_eq!(reviewed_week(date("2026-09-21")), Some(date("2026-09-14")));
        assert_eq!(reviewed_week(date("2026-09-22")), None);
        assert_eq!(monday_of(date("2026-09-20")), date("2026-09-14"));
        assert_eq!(monday_of(date("2026-09-14")), date("2026-09-14"));

        let (db, tmp) = env();
        db.lock().unwrap().execute(
            "INSERT INTO tasks (user_id, title, state, created_at, updated_at, completed_at)
             VALUES (1, 'the essay', 'done', 'x', '2026-09-16T04:00:00Z', '2026-09-16T04:00:00Z')",
            [],
        )
        .unwrap();
        let llm = MockLLM::scripted(vec![]);
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", &tokyo(), date("2026-09-22"), at("2026-09-21T18:00:00Z"))
            .unwrap();
        assert!(llm.seen().is_empty(), "a Tuesday spends nothing");
    }

    fn scripted_review() -> MockLLM {
        MockLLM::scripted(vec![
            ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "memory_write".into(),
                    args: r#"{"op":"add","category":"episodic","summary":"Week of 2026-09-14: the essay landed","body":"one long draft and a dropped recital"}"#.into(),
                }],
            },
            ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "2".into(),
                    name: "review_write".into(),
                    args: r#"{"text":"You finished the essay and let the recital go."}"#.into(),
                }],
            },
        ])
    }

    #[test]
    fn a_monday_writes_the_week_once_and_remembers_it() {
        let (db, tmp) = env();
        {
            let conn = db.lock().unwrap();
            a_week_of_work(&conn);
        }
        let llm = scripted_review();
        let now = at("2026-09-20T18:00:00Z");
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", &tokyo(), date("2026-09-21"), now).unwrap();

        let conn = db.lock().unwrap();
        let content: String = conn
            .query_row(
                "SELECT content FROM reviews WHERE user_id = 1 AND week_start = '2026-09-14'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(content, "You finished the essay and let the recital go.");
        let episodic: String = conn
            .query_row(
                "SELECT summary FROM memory_index WHERE user = 'aki' AND category = 'episodic'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(episodic, "Week of 2026-09-14: the essay landed");
        let source: String = conn
            .query_row("SELECT source_id FROM memory_sources WHERE user_id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(source, "review:2026-09-14");
        drop(conn);

        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", &tokyo(), date("2026-09-21"), now).unwrap();
        assert_eq!(llm.seen().len(), 2, "the same week is never read twice");
    }

    #[test]
    fn the_week_lands_a_minute_behind_the_debrief_and_only_once() {
        let (db, tmp) = env();
        {
            let conn = db.lock().unwrap();
            a_week_of_work(&conn);
        }
        let llm = scripted_review();
        run_for_user(
            &deps(&db, &tmp, &llm),
            1,
            "aki",
            &tokyo(),
            date("2026-09-21"),
            at("2026-09-20T18:00:00Z"),
        )
        .unwrap();
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO plans (id, user_id, date, created_at) VALUES (7, 1, '2026-09-21', 'x')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, channel)
             VALUES (7, 'debrief', '07:30', '07:30', 'push')",
            [],
        )
        .unwrap();

        assert!(lay_event(&conn, 1, date("2026-09-21")).unwrap().is_some());
        let (wall, flex, origin): (String, String, String) = conn
            .query_row(
                "SELECT wall_time, flexibility, origin FROM events WHERE kind = 'review'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((wall.as_str(), flex.as_str(), origin.as_str()), ("07:31", "drop", "template"));
        assert!(lay_event(&conn, 1, date("2026-09-21")).unwrap().is_none());
    }

    #[test]
    fn a_week_with_no_review_lays_nothing() {
        let (db, tmp) = env();
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO plans (id, user_id, date, created_at) VALUES (7, 1, '2026-09-21', 'x')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, channel)
             VALUES (7, 'debrief', '07:30', '07:30', 'push')",
            [],
        )
        .unwrap();
        assert!(lay_event(&conn, 1, date("2026-09-21")).unwrap().is_none());
        let _ = tmp;
    }

    #[test]
    fn an_empty_week_costs_no_session() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![]);
        run_for_user(
            &deps(&db, &tmp, &llm),
            1,
            "aki",
            &tokyo(),
            date("2026-09-21"),
            at("2026-09-20T18:00:00Z"),
        )
        .unwrap();
        assert!(llm.seen().is_empty());
        let conn = db.lock().unwrap();
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM reviews", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 0);
    }
}
