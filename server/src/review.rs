use crate::agent::SessionDeps;
use crate::tools::SessionKind;
use crate::model_text as mt;
use crate::text::Lang;
use anyhow::Result;
use rusqlite::Connection;
use std::fmt::Write as _;

const MAX_DIGEST_BYTES: usize = 32 * 1024;
const MAX_ROWS: i64 = 60;
const MAX_SUMMARY_CHARS: usize = 400;
/// Newest episodic facts scanned for ones the week created.
const EPISODIC_SCAN: usize = 40;
/// How far back the long view reaches, the reviewed week included.
const HORIZON_WEEKS: i64 = 8;
const MAX_AREAS: usize = 12;
const MAX_GOALS: i64 = 20;

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

/// The half-open stretch of instants from `start` to `end`, in the user's own
/// zone, so rows stamped in UTC can be compared as the strings they are stored as.
struct Window {
    start: jiff::civil::Date,
    end: jiff::civil::Date,
    from: String,
    to: String,
}

impl Window {
    fn between(start: jiff::civil::Date, end: jiff::civil::Date, tz: &jiff::tz::TimeZone) -> Result<Self> {
        let at = |d: jiff::civil::Date| -> Result<String> {
            let dt = d.to_datetime(jiff::civil::Time::midnight());
            Ok(tz.to_ambiguous_zoned(dt).compatible()?.timestamp().to_string())
        };
        Ok(Self { start, end, from: at(start)?, to: at(end)? })
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

fn local_date(stamp: &str, tz: &jiff::tz::TimeZone) -> Option<jiff::civil::Date> {
    stamp.parse::<jiff::Timestamp>().ok().map(|ts| ts.to_zoned(tz.clone()).date())
}

fn day_label(stamp: &str, tz: &jiff::tz::TimeZone, l: Lang) -> String {
    local_date(stamp, tz).map_or_else(|| "?".into(), |d| mt::weekday(l, d.weekday()).to_string())
}

fn section(l: Lang, title: mt::ReviewSection, lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    format!("## {}\n{}\n\n", mt::review_section(l, title), lines.join("\n"))
}

fn tasks(conn: &Connection, user_id: i64, w: &Window, tz: &jiff::tz::TimeZone, l: Lang) -> Result<String> {
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
            Ok(format!("- {} ({state}, {})", clip(&title), day_label(&at, tz, l)))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(section(l, mt::ReviewSection::Tasks, &lines))
}

fn sessions(conn: &Connection, user_id: i64, w: &Window, tz: &jiff::tz::TimeZone, l: Lang) -> Result<String> {
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
            let mut line = format!("- {} ({})", clip(&title), day_label(&started, tz, l));
            if let Some(p) = planned {
                line.push_str(&mt::planned_min(l, p));
            }
            if let Some(min) = elapsed_min(&started, ended.as_deref(), paused_ms) {
                line.push_str(&mt::ran_min(l, min));
            }
            match outcome.as_deref() {
                Some(o) => { let _ = write!(line, ", {o}"); },
                None => line.push_str(mt::never_ended(l)),
            }
            if overrun.is_some() {
                line.push_str(mt::asked_about_overrun(l));
            }
            Ok(line)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(section(l, mt::ReviewSection::Sessions, &lines))
}

fn elapsed_min(started: &str, ended: Option<&str>, paused_ms: i64) -> Option<i64> {
    let from: jiff::Timestamp = started.parse().ok()?;
    let to: jiff::Timestamp = ended?.parse().ok()?;
    let ms = to.as_millisecond() - from.as_millisecond() - paused_ms;
    Some((ms / 60_000).max(0))
}

fn triggers(conn: &Connection, user_id: i64, w: &Window, l: Lang) -> Result<String> {
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
        l,
        mt::ReviewSection::Triggers,
        &[mt::triggers_line(l, &by_status.join(", "), spoken("trigger_said"), spoken("trigger_quiet"))],
    ))
}

fn nudges(conn: &Connection, user_id: i64, w: &Window, l: Lang) -> Result<String> {
    let lines = crate::follow_up::review_lines(conn, user_id, &w.from, &w.to, l)?;
    Ok(section(l, mt::ReviewSection::Nudges, &lines))
}

fn conversations(
    conn: &Connection,
    user_id: i64,
    w: &Window,
    tz: &jiff::tz::TimeZone,
    l: Lang,
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
            Ok(format!("- {} ({}): {}", clip(&title), day_label(&at, tz, l), clip(&summary)))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(section(l, mt::ReviewSection::Conversations, &lines))
}

fn days_remembered(
    conn: &Connection,
    data_dir: &std::path::Path,
    username: &str,
    w: &Window,
    l: Lang,
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
    Ok(section(l, mt::ReviewSection::Memory, &lines))
}

/// Every goal still open and every one settled inside the long view, soonest
/// deadline first, with how far its tasks have got.
fn goals(conn: &Connection, user_id: i64, long: &Window, tz: &jiff::tz::TimeZone, l: Lang) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT g.title, g.state, g.due_at,
                (SELECT COUNT(*) FROM tasks t WHERE t.goal_id = g.id AND t.state != 'dropped'),
                (SELECT COUNT(*) FROM tasks t WHERE t.goal_id = g.id AND t.state = 'done')
         FROM goals g
         WHERE g.user_id = ?1 AND (g.state = 'open' OR g.updated_at >= ?2)
         ORDER BY g.due_at IS NULL, g.due_at LIMIT ?3",
    )?;
    let lines = stmt
        .query_map((user_id, &long.from, MAX_GOALS), |r| {
            let title: String = r.get(0)?;
            let state: String = r.get(1)?;
            let due: Option<String> = r.get(2)?;
            let due = due.as_deref().and_then(|d| local_date(d, tz)).map(|d| d.to_string());
            Ok(mt::goal_line(l, &clip(&title), &state, due.as_deref(), r.get(4)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(section(l, mt::ReviewSection::Goals, &lines))
}

/// What a goal or category is called in the long view; work under neither
/// shares one unnamed line.
const AREA_OF: &str = "COALESCE(g.title, NULLIF(COALESCE(pt.category, t.category), ''), '')";

/// Each week of the long view in a line, and the time it held by goal or
/// category across all of it.
fn long_view(conn: &Connection, user_id: i64, long: &Window, tz: &jiff::tz::TimeZone, l: Lang) -> Result<String> {
    #[derive(Default)]
    struct Tally {
        done: i64,
        dropped: i64,
        minutes: i64,
    }
    let week_of = |stamp: &str| local_date(stamp, tz).map(monday_of);
    let mut weeks: std::collections::BTreeMap<jiff::civil::Date, Tally> = std::collections::BTreeMap::new();
    let mut areas: std::collections::BTreeMap<String, Tally> = std::collections::BTreeMap::new();

    let mut stmt = conn.prepare(&format!(
        "SELECT t.state, COALESCE(t.completed_at, t.updated_at), {AREA_OF}
         FROM tasks t LEFT JOIN tasks pt ON pt.id = t.parent_id
              LEFT JOIN goals g ON g.id = t.goal_id
         WHERE t.user_id = ?1 AND t.parent_id IS NULL
           AND ((t.state = 'done' AND t.completed_at >= ?2 AND t.completed_at < ?3)
                OR (t.state = 'dropped' AND t.updated_at >= ?2 AND t.updated_at < ?3))"
    ))?;
    let rows = stmt.query_map((user_id, &long.from, &long.to), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
    })?;
    for row in rows {
        let (state, at, area) = row?;
        let Some(week) = week_of(&at) else { continue };
        let tally = weeks.entry(week).or_default();
        if state == "done" {
            tally.done += 1;
            areas.entry(area).or_default().done += 1;
        } else {
            tally.dropped += 1;
        }
    }

    let mut stmt = conn.prepare(&format!(
        "SELECT ws.started_at, ws.ended_at, ws.paused_ms, {AREA_OF}
         FROM work_sessions ws LEFT JOIN tasks t ON t.id = ws.task_id
              LEFT JOIN tasks pt ON pt.id = t.parent_id
              LEFT JOIN goals g ON g.id = COALESCE(pt.goal_id, t.goal_id)
         WHERE ws.user_id = ?1 AND ws.started_at >= ?2 AND ws.started_at < ?3"
    ))?;
    let rows = stmt.query_map((user_id, &long.from, &long.to), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?))
    })?;
    for row in rows {
        let (started, ended, paused_ms, area) = row?;
        let (Some(week), Some(minutes)) = (week_of(&started), elapsed_min(&started, ended.as_deref(), paused_ms)) else {
            continue;
        };
        weeks.entry(week).or_default().minutes += minutes;
        areas.entry(area).or_default().minutes += minutes;
    }

    let week_lines: Vec<String> = weeks
        .iter()
        .map(|(week, t)| mt::week_line(l, *week, t.done, t.dropped, t.minutes))
        .collect();
    let mut by_time: Vec<(String, Tally)> = areas.into_iter().collect();
    by_time.sort_by_key(|(_, t)| std::cmp::Reverse((t.minutes, t.done)));
    let area_lines: Vec<String> = by_time
        .iter()
        .take(MAX_AREAS)
        .map(|(area, t)| {
            let name = if area.is_empty() { mt::unfiled(l).to_string() } else { clip(area) };
            mt::area_line(l, &name, t.minutes, t.done)
        })
        .collect();
    Ok(section(l, mt::ReviewSection::Weeks, &week_lines) + &section(l, mt::ReviewSection::Areas, &area_lines))
}

/// What the review session reads: the goals and the last eight weeks for the
/// long view, then the week just ended in detail — what finished and what was
/// let go, the sessions behind it, the nudges, the conversations and the
/// episodic entries already kept.
pub fn digest(
    conn: &Connection,
    data_dir: &std::path::Path,
    user_id: i64,
    username: &str,
    tz: &jiff::tz::TimeZone,
    l: Lang,
    week_start: jiff::civil::Date,
) -> Result<String> {
    let end = week_start.checked_add(jiff::Span::new().days(7))?;
    let w = Window::between(week_start, end, tz)?;
    let long = Window::between(week_start.checked_sub(jiff::Span::new().days(7 * (HORIZON_WEEKS - 1)))?, end, tz)?;
    let parts = [
        goals(conn, user_id, &long, tz, l)?,
        long_view(conn, user_id, &long, tz, l)?,
        tasks(conn, user_id, &w, tz, l)?,
        sessions(conn, user_id, &w, tz, l)?,
        triggers(conn, user_id, &w, l)?,
        nudges(conn, user_id, &w, l)?,
        conversations(conn, user_id, &w, tz, l)?,
        days_remembered(conn, data_dir, username, &w, l)?,
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

/// The Monday nightly rewrites the standing strategy every session reads; the
/// strategy's own date is the idempotency marker, so a week is read once
/// however many times the sweep comes round.
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
    if crate::context::read_strategy(deps.config_dir, username).is_some_and(|(written, _)| written == week) {
        return Ok(());
    }
    let l = Lang::for_user(deps.config_dir, username);
    let digest = {
        let conn = crate::db_guard(deps.db);
        digest(&conn, deps.data_dir, user_id, username, tz, l, week_start)?
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
        caller: deps.caller,
    };
    let sunday = week_start.checked_add(jiff::Span::new().days(6))?;
    let opening = mt::review_opening(l, &week, sunday, &digest);
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
        .ok_or_else(|| anyhow::anyhow!("the session wrote no strategy"))?;
    crate::context::write_strategy(deps.config_dir, username, &text, week_start)?;
    Ok(())
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
            caller: None,
        }
    }

    fn a_week_of_work(conn: &Connection) {
        conn.execute(
            "INSERT INTO goals (id, user_id, title, due_at, state, created_at, updated_at)
             VALUES (4, 1, 'apply to colleges', '2026-11-01T07:00:00Z', 'open', 'x', 'x')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, user_id, title, state, goal_id, created_at, updated_at, completed_at)
             VALUES (1, 1, 'the essay', 'done', 4, '2026-09-14T00:00:00Z', '2026-09-16T04:00:00Z',
                     '2026-09-16T04:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, user_id, title, state, goal_id, created_at, updated_at)
             VALUES (2, 1, 'the recital', 'dropped', NULL, '2026-09-14T00:00:00Z', '2026-09-17T04:00:00Z')",
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
            "INSERT INTO tasks (id, user_id, title, state, category, created_at, updated_at, completed_at)
             VALUES (5, 1, 'problem set', 'done', 'AP Micro', '2026-08-30T00:00:00Z', '2026-09-02T04:00:00Z',
                     '2026-09-02T04:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO work_sessions (user_id, task_id, title, planned_min, started_at, ended_at, outcome,
                                        overrun_asked_at, phase_started_at)
             VALUES (1, 1, 'essay draft', 50, '2026-09-16T01:00:00Z', '2026-09-16T02:00:00Z', 'done',
                     '2026-09-16T01:50:00Z', '2026-09-16T01:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO work_sessions (user_id, task_id, title, started_at, ended_at, outcome, phase_started_at)
             VALUES (1, 5, 'problem set', '2026-09-02T01:00:00Z', '2026-09-02T01:30:00Z', 'done',
                     '2026-09-02T01:00:00Z')",
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
        conn.execute(
            "INSERT INTO outreach (user_id, event_id, origin, via, at, window_until, wait_min, level, outcome)
             VALUES (1, 1, 'agent', 'message', '2026-09-16T02:00:00Z', '2026-09-16T02:15:00Z', 15, 0, 'acted')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn the_digest_holds_the_goals_the_long_view_and_the_week_that_ended() {
        let (db, tmp) = env();
        let conn = db.lock().unwrap();
        a_week_of_work(&conn);
        let id = crate::memory::add_until(&conn, tmp.path(), "aki", &crate::memory::Fact { category: "episodic", summary: "2026-09-16 · the essay: Aki sent the draft", body: "Aki drafted for an hour and sent it.", until: Some("2026-12-15") }, None)
        .unwrap();
        let raw = crate::memory::read_raw(tmp.path(), "aki", &id).unwrap().unwrap();
        let dated: Vec<&str> = raw
            .lines()
            .map(|l| if l.starts_with("created: ") { "created: 2026-09-16T05:00:00Z" } else { l })
            .collect();
        crate::memory::write_raw(&conn, tmp.path(), "aki", &id, &format!("{}\n", dated.join("\n")))
            .unwrap();

        let out =
            digest(&conn, tmp.path(), 1, "aki", &tokyo(), Lang::En, date("2026-09-14")).unwrap();
        assert!(out.contains("- apply to colleges (open, due 2026-11-01): 1 of 1 tasks done"), "{out}");
        assert!(out.contains("- week of 2026-08-31: 1 done, 0 dropped, 30 min in work sessions"), "{out}");
        assert!(out.contains("- week of 2026-09-14: 1 done, 1 dropped, 60 min in work sessions"), "{out}");
        assert!(out.contains("- apply to colleges: 60 min in work sessions, 1 done"), "{out}");
        assert!(out.contains("- AP Micro: 30 min in work sessions, 1 done"), "{out}");
        assert!(out.contains("- the essay (done, Wed)"), "{out}");
        assert!(out.contains("- the recital (dropped, Thu)"), "{out}");
        assert!(!out.contains("next week"), "the week after is not this week's: {out}");
        assert!(out.contains("planned 50 min, ran 60 min, done, asked about overrun"), "{out}");
        assert!(out.contains("1 done — said 1, stayed quiet 0"), "{out}");
        assert!(out.contains("## How the nudges landed\n- messages that waited for an answer: 1 — 1 acted"), "{out}");
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
        let call = |id: &str, name: &str, args: &str| ToolCall { id: id.into(), name: name.into(), args: args.into() };
        MockLLM::scripted(vec![ChatResponse {
            text: String::new(),
            tool_calls: vec![
                call(
                    "1",
                    "memory_write",
                    r#"{"op":"add","category":"episodic","summary":"Week of 2026-09-14: the essay landed","body":"one long draft and a dropped recital"}"#,
                ),
                call("2", "review_write", r#"{"text":"Colleges: essay sent; mornings hold the deep work."}"#),
            ],
        }])
    }

    #[test]
    fn a_monday_rewrites_the_strategy_once_in_one_model_call_and_remembers_the_week() {
        let (db, tmp) = env();
        {
            let conn = db.lock().unwrap();
            a_week_of_work(&conn);
        }
        let llm = scripted_review();
        let now = at("2026-09-20T18:00:00Z");
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", &tokyo(), date("2026-09-21"), now).unwrap();
        assert_eq!(llm.seen().len(), 1);

        let (week, strategy) = crate::context::read_strategy(tmp.path(), "aki").unwrap();
        assert_eq!(week, "2026-09-14");
        assert_eq!(strategy, "Colleges: essay sent; mornings hold the deep work.");
        let conn = db.lock().unwrap();
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
        assert_eq!(llm.seen().len(), 1, "the same week is never read twice");
    }

    #[test]
    fn an_empty_stretch_costs_no_session() {
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
        assert!(crate::context::read_strategy(tmp.path(), "aki").is_none());
    }
}
