use crate::model_text as mt;
use crate::text::Lang;
use rusqlite::{Connection, OptionalExtension};
use std::fmt::Write as _;

/// How long a nudge that does not wait has for an answer before it counts as ignored.
pub const WINDOW_MIN: i64 = 30;
/// How far back the record a wake-up reads, and the levels it carries, reach.
pub const LOOKBACK_HOURS: i64 = 48;
const MAX_TASKS: usize = 5;
const MAX_ENTRIES: usize = 4;
const MAX_SUMMARY_BYTES: usize = 1200;
const MAX_TASKS_PER_NUDGE: usize = 10;
const MAX_TITLE_CHARS: usize = 40;
/// How far from a call's outreach its ring is looked for.
const RING_BEFORE_SECS: i64 = 60;
const RING_AFTER_SECS: i64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Message,
    Call,
}

impl Via {
    pub fn as_str(self) -> &'static str {
        match self {
            Via::Message => "message",
            Via::Call => "call",
        }
    }

    fn parse(s: &str) -> Self {
        if s == "call" { Via::Call } else { Via::Message }
    }
}

/// How an outreach landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landed {
    Answered,
    Acted,
    Ignored,
    Missed,
    Declined,
}

impl Landed {
    pub fn as_str(self) -> &'static str {
        match self {
            Landed::Answered => "answered",
            Landed::Acted => "acted",
            Landed::Ignored => "ignored",
            Landed::Missed => "missed",
            Landed::Declined => "declined",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "answered" => Landed::Answered,
            "acted" => Landed::Acted,
            "ignored" => Landed::Ignored,
            "missed" => Landed::Missed,
            "declined" => Landed::Declined,
            _ => return None,
        })
    }

    /// Whether it adds to the pressure: nothing came back.
    pub fn unanswered(self) -> bool {
        matches!(self, Landed::Ignored | Landed::Missed | Landed::Declined)
    }

    pub const ALL: [Landed; 5] = [Landed::Answered, Landed::Acted, Landed::Ignored, Landed::Missed, Landed::Declined];
}

/// How a call the user was rung for ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEnd {
    Answered,
    Missed,
    Declined,
}

impl CallEnd {
    fn landed(self) -> Landed {
        match self {
            CallEnd::Answered => Landed::Answered,
            CallEnd::Missed => Landed::Missed,
            CallEnd::Declined => Landed::Declined,
        }
    }
}

/// One nudge or call a wake-up sent.
pub struct Sent<'a> {
    pub user_id: i64,
    pub event_id: i64,
    /// The wait its `say` parked on, which settles it when it ends.
    pub wait_event_id: Option<i64>,
    pub origin: &'a str,
    pub via: Via,
    pub tasks: &'a [i64],
    pub wait_min: Option<u32>,
    pub at: jiff::Timestamp,
}

fn minutes(n: i64) -> jiff::SignedDuration {
    jiff::SignedDuration::from_mins(n)
}

/// Records `sent` against those of its tasks the user owns, with the level it
/// went out at, and returns its id.
pub fn record(conn: &Connection, sent: &Sent) -> rusqlite::Result<i64> {
    let mut tasks = Vec::new();
    for &id in sent.tasks {
        let owned: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM tasks WHERE id = ?1 AND user_id = ?2)",
            (id, sent.user_id),
            |r| r.get(0),
        )?;
        if owned && !tasks.contains(&id) && tasks.len() < MAX_TASKS_PER_NUDGE {
            tasks.push(id);
        }
    }
    let level = if tasks.is_empty() {
        level(conn, sent.user_id, None, sent.at)?
    } else {
        let mut top = 0;
        for &t in &tasks {
            top = top.max(level(conn, sent.user_id, Some(t), sent.at)?);
        }
        top
    };
    let window = sent.wait_min.map_or(WINDOW_MIN, i64::from);
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO outreach (user_id, event_id, wait_event_id, origin, via, at, window_until, wait_min, level)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            sent.user_id,
            sent.event_id,
            sent.wait_event_id,
            sent.origin,
            sent.via.as_str(),
            sent.at.to_string(),
            (sent.at + minutes(window)).to_string(),
            sent.wait_min,
            i64::try_from(level).unwrap_or(i64::MAX),
        ],
    )?;
    let id = tx.last_insert_rowid();
    for t in tasks {
        tx.execute("INSERT INTO outreach_tasks (outreach_id, task_id) VALUES (?1, ?2)", (id, t))?;
    }
    tx.commit()?;
    Ok(id)
}

fn set(conn: &Connection, id: i64, landed: Landed, now: jiff::Timestamp) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE outreach SET outcome = ?1, outcome_at = ?2 WHERE id = ?3",
        (landed.as_str(), now.to_string(), id),
    )
    .map(|_| ())
}

/// Settles the outreach that parked on `wait_event_id` with what ended the
/// wait: a reply answers it, any other move of the user's acts on it, and
/// running out ignores it.
pub fn wait_over(
    conn: &Connection,
    wait_event_id: i64,
    ended: Option<crate::triggers::WaitEnd>,
    now: jiff::Timestamp,
) -> rusqlite::Result<()> {
    let landed = match ended {
        None => Landed::Ignored,
        Some(crate::triggers::WaitEnd::Reply) => Landed::Answered,
        Some(_) => Landed::Acted,
    };
    conn.execute(
        "UPDATE outreach SET outcome = ?1, outcome_at = ?2 WHERE wait_event_id = ?3",
        (landed.as_str(), now.to_string(), wait_event_id),
    )
    .map(|_| ())
}

/// Settles a call outreach with how the call itself ended, over whatever its
/// window would have made of it.
pub fn call_ended(conn: &Connection, outreach_id: i64, end: CallEnd, now: jiff::Timestamp) -> rusqlite::Result<()> {
    set(conn, outreach_id, end.landed(), now)
}

fn tasks_of(conn: &Connection, outreach_id: i64) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached("SELECT task_id FROM outreach_tasks WHERE outreach_id = ?1 ORDER BY task_id")?;
    let rows = stmt.query_map([outreach_id], |r| r.get(0))?.collect();
    rows
}

/// Whether the user moved between `from` and `until`: on one of `tasks` (done,
/// changed, or a work session started on it or a step of it), or with none
/// named, by finishing any task or starting any work session.
fn acted(conn: &Connection, user_id: i64, tasks: &[i64], from: &str, until: &str) -> rusqlite::Result<bool> {
    if tasks.is_empty() {
        return conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM tasks WHERE user_id = ?1 AND state = 'done'
                            AND completed_at > ?2 AND completed_at <= ?3)
                 OR EXISTS (SELECT 1 FROM work_sessions WHERE user_id = ?1
                            AND started_at > ?2 AND started_at <= ?3)",
            (user_id, from, until),
            |r| r.get(0),
        );
    }
    for &task in tasks {
        let moved: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM tasks WHERE user_id = ?1 AND (id = ?2 OR parent_id = ?2)
                            AND ((completed_at > ?3 AND completed_at <= ?4) OR (updated_at > ?3 AND updated_at <= ?4)))
                 OR EXISTS (SELECT 1 FROM work_sessions w WHERE w.user_id = ?1
                            AND w.started_at > ?3 AND w.started_at <= ?4
                            AND (w.task_id = ?2 OR w.task_id IN (SELECT id FROM tasks WHERE parent_id = ?2)))",
            (user_id, task, from, until),
            |r| r.get(0),
        )?;
        if moved {
            return Ok(true);
        }
    }
    Ok(false)
}

fn answered(conn: &Connection, user_id: i64, from: &str, until: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM talk_messages m JOIN conversations c ON c.id = m.conversation_id
                        WHERE c.user_id = ?1 AND m.role = 'user' AND m.created_at > ?2 AND m.created_at <= ?3)",
        (user_id, from, until),
        |r| r.get(0),
    )
}

/// How the rings placed around `at` ended, once one has: picked up anywhere,
/// else declined, else missed. `None` while one still rings or none says.
fn ring_ended(conn: &Connection, user_id: i64, at: jiff::Timestamp) -> rusqlite::Result<Option<CallEnd>> {
    let mut stmt = conn.prepare_cached(
        "SELECT state, COALESCE(outcome, '') FROM voice_calls
         WHERE user_id = ?1 AND direction = 'outbound' AND created_at >= ?2 AND created_at <= ?3",
    )?;
    let from = (at - jiff::SignedDuration::from_secs(RING_BEFORE_SECS)).to_string();
    let to = (at + jiff::SignedDuration::from_secs(RING_AFTER_SECS)).to_string();
    let rings: Vec<(String, String)> =
        stmt.query_map((user_id, from, to), |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    if rings.iter().any(|(state, outcome)| state == "answered" || outcome == "answered") {
        return Ok(Some(CallEnd::Answered));
    }
    if rings.iter().any(|(state, _)| state != "ended") {
        return Ok(None);
    }
    Ok(if rings.iter().any(|(_, o)| o == "declined") {
        Some(CallEnd::Declined)
    } else if rings.iter().any(|(_, o)| o == "missed") {
        Some(CallEnd::Missed)
    } else {
        None
    })
}

/// Settles every open outreach of the user's that has landed by `now`: a
/// call by how its ring ended, then any by the user acting or writing inside
/// its window, and one whose window ran out with neither as ignored.
pub fn settle(conn: &Connection, user_id: i64, now: jiff::Timestamp) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, via, at, window_until FROM outreach WHERE user_id = ?1 AND outcome IS NULL ORDER BY id",
    )?;
    let open: Vec<(i64, String, String, String)> = stmt
        .query_map([user_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for (id, via, at, until) in open {
        if Via::parse(&via) == Via::Call {
            if let Some(end) = at.parse().ok().map(|t| ring_ended(conn, user_id, t)).transpose()?.flatten() {
                set(conn, id, end.landed(), now)?;
                continue;
            }
        }
        let tasks = tasks_of(conn, id)?;
        let landed = if acted(conn, user_id, &tasks, &at, &until)? {
            Some(Landed::Acted)
        } else if answered(conn, user_id, &at, &until)? {
            Some(Landed::Answered)
        } else if now.to_string() >= until {
            Some(Landed::Ignored)
        } else {
            None
        };
        if let Some(landed) = landed {
            set(conn, id, landed, now)?;
        }
    }
    Ok(())
}

/// When the user last moved on `task` itself: finished it, or started a work
/// session on it or one of its steps.
fn last_action(conn: &Connection, user_id: i64, task: i64) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT MAX(at) FROM (
             SELECT completed_at AS at FROM tasks WHERE user_id = ?1 AND (id = ?2 OR parent_id = ?2)
             UNION ALL
             SELECT w.started_at FROM work_sessions w WHERE w.user_id = ?1
                 AND (w.task_id = ?2 OR w.task_id IN (SELECT id FROM tasks WHERE parent_id = ?2)))",
        (user_id, task),
        |r| r.get(0),
    )
    .optional()
    .map(Option::flatten)
}

struct Entry {
    at: String,
    via: Via,
    landed: Option<Landed>,
}

/// The outreach about `task` (or about none, for `None`) inside the lookback,
/// newest first, and when the user last acted on the task.
fn entries(
    conn: &Connection,
    user_id: i64,
    task: Option<i64>,
    now: jiff::Timestamp,
) -> rusqlite::Result<(Vec<Entry>, Option<String>)> {
    let lookback = (now - jiff::SignedDuration::from_hours(LOOKBACK_HOURS)).to_string();
    let acted = match task {
        Some(t) => last_action(conn, user_id, t)?,
        None => None,
    };
    let read = |r: &rusqlite::Row| -> rusqlite::Result<Entry> {
        let via: String = r.get(1)?;
        let landed: Option<String> = r.get(2)?;
        Ok(Entry { at: r.get(0)?, via: Via::parse(&via), landed: landed.as_deref().and_then(Landed::parse) })
    };
    let rows = match task {
        Some(t) => conn
            .prepare_cached(
                "SELECT o.at, o.via, o.outcome FROM outreach o JOIN outreach_tasks ot ON ot.outreach_id = o.id
                 WHERE o.user_id = ?1 AND ot.task_id = ?2 AND o.at > ?3 ORDER BY o.at DESC, o.id DESC LIMIT 50",
            )?
            .query_map((user_id, t, &lookback), read)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        None => conn
            .prepare_cached(
                "SELECT o.at, o.via, o.outcome FROM outreach o
                 WHERE o.user_id = ?1 AND o.at > ?2
                   AND NOT EXISTS (SELECT 1 FROM outreach_tasks ot WHERE ot.outreach_id = o.id)
                 ORDER BY o.at DESC, o.id DESC LIMIT 50",
            )?
            .query_map((user_id, &lookback), read)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    };
    Ok((rows, acted))
}

/// Unanswered outreach in a row, newest back, since the user last answered,
/// or acted on the task at `acted`; one still inside its window neither
/// counts nor breaks the run.
fn level_from(entries: &[Entry], acted: Option<&str>) -> usize {
    entries
        .iter()
        .take_while(|e| acted.is_none_or(|a| e.at.as_str() > a))
        .filter_map(|e| e.landed)
        .take_while(|l| l.unanswered())
        .count()
}

/// The escalation level on `task`, or on outreach about no task.
pub fn level(conn: &Connection, user_id: i64, task: Option<i64>, now: jiff::Timestamp) -> rusqlite::Result<usize> {
    let (entries, acted) = entries(conn, user_id, task, now)?;
    Ok(level_from(&entries, acted.as_deref()))
}

fn clock(at: &str, tz: &jiff::tz::TimeZone, today: jiff::civil::Date, l: Lang) -> String {
    let Ok(t) = at.parse::<jiff::Timestamp>() else { return at.to_string() };
    let z = t.to_zoned(tz.clone());
    let hm = z.strftime("%H:%M").to_string();
    if z.date() == today { hm } else { format!("{} {hm}", mt::weekday(l, z.weekday())) }
}

/// What a wake-up reads of its recent outreach: per open task, and for
/// outreach about none, the last few and how each landed, its level, and when
/// the user last acted on it if since. Settles what has landed first; empty
/// when the lookback holds no outreach about an open task or about none.
pub fn summary(
    conn: &Connection,
    user_id: i64,
    tz: &jiff::tz::TimeZone,
    l: Lang,
    now: jiff::Timestamp,
) -> rusqlite::Result<String> {
    settle(conn, user_id, now)?;
    let lookback = (now - jiff::SignedDuration::from_hours(LOOKBACK_HOURS)).to_string();
    let mut stmt = conn.prepare_cached(
        "SELECT ot.task_id, MAX(o.at) FROM outreach o LEFT JOIN outreach_tasks ot ON ot.outreach_id = o.id
         LEFT JOIN tasks t ON t.id = ot.task_id
         WHERE o.user_id = ?1 AND o.at > ?2
           AND (ot.task_id IS NULL OR (t.user_id = ?1 AND t.state NOT IN ('done', 'dropped')))
         GROUP BY ot.task_id ORDER BY 2 DESC LIMIT ?3",
    )?;
    let subjects: Vec<Option<i64>> = stmt
        .query_map((user_id, &lookback, i64::try_from(MAX_TASKS).unwrap_or(i64::MAX)), |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let today = now.to_zoned(tz.clone()).date();
    let mut lines = Vec::new();
    for subject in subjects {
        let (entries, acted) = entries(conn, user_id, subject, now)?;
        let name = match subject {
            Some(id) => {
                let title: String = conn.query_row("SELECT title FROM tasks WHERE id = ?1", [id], |r| r.get(0))?;
                mt::nudge_task(l, id, &title.chars().take(MAX_TITLE_CHARS).collect::<String>())
            }
            None => mt::nudge_no_task(l).to_string(),
        };
        let shown: Vec<String> = entries
            .iter()
            .take(MAX_ENTRIES)
            .rev()
            .map(|e| format!("{} {} {}", clock(&e.at, tz, today, l), mt::nudge_via(l, e.via), mt::nudge_landed(l, e.landed)))
            .collect();
        if shown.is_empty() {
            continue;
        }
        let level = level_from(&entries, acted.as_deref());
        let acted_since = acted.filter(|a| entries.first().is_some_and(|e| *a > e.at)).map(|a| clock(&a, tz, today, l));
        lines.push(mt::nudge_line(l, &name, &shown.join(" · "), level, acted_since.as_deref()));
    }
    if lines.is_empty() {
        return Ok(String::new());
    }
    let mut out = format!("\n{}\n", mt::nudges_header(l));
    for line in lines {
        if out.len() + line.len() + 1 > MAX_SUMMARY_BYTES {
            break;
        }
        let _ = writeln!(out, "{line}");
    }
    Ok(out)
}

/// The tasks a firing is about: those its events are tied to, the task a
/// check's cancel rule watches, its work session's, and for a wait, the
/// outreach it parked.
pub fn tasks_of_events(conn: &Connection, events: &[i64]) -> rusqlite::Result<Vec<i64>> {
    let mut out: Vec<i64> = Vec::new();
    for &id in events {
        let mut stmt = conn.prepare_cached(
            "SELECT task_id FROM event_tasks WHERE event_id = ?1
             UNION SELECT cancel_ref FROM events WHERE id = ?1 AND cancel_if = 'task_done' AND cancel_ref IS NOT NULL
             UNION SELECT w.task_id FROM events e JOIN work_sessions w ON w.id = e.work_session_id
                   WHERE e.id = ?1 AND w.task_id IS NOT NULL
             UNION SELECT ot.task_id FROM outreach o JOIN outreach_tasks ot ON ot.outreach_id = o.id
                   WHERE o.wait_event_id = ?1",
        )?;
        for t in stmt.query_map([id], |r| r.get::<_, i64>(0))? {
            let t = t?;
            if !out.contains(&t) {
                out.push(t);
            }
        }
    }
    Ok(out)
}

/// The week's outreach for the weekly review: how each kind landed, by how it
/// went out, why it went out, and the level it went out at.
pub fn review_lines(conn: &Connection, user_id: i64, from: &str, to: &str, l: Lang) -> rusqlite::Result<Vec<String>> {
    settle(conn, user_id, jiff::Timestamp::now())?;
    let mut stmt = conn.prepare_cached(
        "SELECT via, wait_min IS NOT NULL, origin = 'idle', level, outcome FROM outreach
         WHERE user_id = ?1 AND at >= ?2 AND at < ?3",
    )?;
    type Row = (String, bool, bool, i64, Option<String>);
    let rows: Vec<Row> = stmt
        .query_map((user_id, from, to), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    type Holds = fn(&Row) -> bool;
    let groups: [(mt::NudgeKind, Holds); 8] = [
        (mt::NudgeKind::MessageWaited, |r| r.0 == "message" && r.1),
        (mt::NudgeKind::Message, |r| r.0 == "message" && !r.1),
        (mt::NudgeKind::Call, |r| r.0 == "call"),
        (mt::NudgeKind::Idle, |r| r.2),
        (mt::NudgeKind::Planned, |r| !r.2),
        (mt::NudgeKind::Level(0), |r| r.3 == 0),
        (mt::NudgeKind::Level(1), |r| r.3 == 1),
        (mt::NudgeKind::Level(2), |r| r.3 >= 2),
    ];
    let mut lines = Vec::new();
    for (kind, holds) in groups {
        let picked: Vec<&Row> = rows.iter().filter(|r| holds(r)).collect();
        if picked.is_empty() {
            continue;
        }
        let tally: Vec<(Landed, usize)> = Landed::ALL
            .iter()
            .map(|&landed| (landed, picked.iter().filter(|r| r.4.as_deref() == Some(landed.as_str())).count()))
            .filter(|(_, n)| *n > 0)
            .collect();
        lines.push(mt::nudge_tally(l, kind, picked.len(), &tally));
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> (Connection, i64) {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aitest", "p", false).unwrap();
        (conn, uid)
    }

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    fn task(conn: &Connection, uid: i64, title: &str) -> i64 {
        conn.execute(
            "INSERT INTO tasks (user_id, title, created_at, updated_at) VALUES (?1, ?2, '2026-10-18T00:00:00Z', '2026-10-18T00:00:00Z')",
            (uid, title),
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn sent(conn: &Connection, uid: i64, via: Via, tasks: &[i64], when: &str) -> i64 {
        record(
            conn,
            &Sent { user_id: uid, event_id: 1, wait_event_id: None, origin: "agent", via, tasks, wait_min: None, at: at(when) },
        )
        .unwrap()
    }

    fn outcome(conn: &Connection, id: i64) -> Option<String> {
        conn.query_row("SELECT outcome FROM outreach WHERE id = ?1", [id], |r| r.get(0)).unwrap()
    }

    fn user_wrote(conn: &Connection, uid: i64, when: &str) {
        let chat = crate::talk::create(conn, uid, "chat", at(when)).unwrap();
        crate::talk::append_text(conn, chat, "user", "on it", at(when)).unwrap();
    }

    #[test]
    fn a_nudge_lands_as_answered_acted_or_ignored_by_what_followed_inside_its_window() {
        let (conn, uid) = env();
        let essay = task(&conn, uid, "essay");
        let ignored = sent(&conn, uid, Via::Message, &[essay], "2026-10-18T14:00:00Z");
        settle(&conn, uid, at("2026-10-18T14:10:00Z")).unwrap();
        assert_eq!(outcome(&conn, ignored), None, "still inside its window");
        settle(&conn, uid, at("2026-10-18T14:31:00Z")).unwrap();
        assert_eq!(outcome(&conn, ignored).as_deref(), Some("ignored"));

        let answered = sent(&conn, uid, Via::Message, &[essay], "2026-10-18T15:00:00Z");
        user_wrote(&conn, uid, "2026-10-18T15:05:00Z");
        settle(&conn, uid, at("2026-10-18T15:06:00Z")).unwrap();
        assert_eq!(outcome(&conn, answered).as_deref(), Some("answered"));

        let acted = sent(&conn, uid, Via::Message, &[essay], "2026-10-18T16:00:00Z");
        conn.execute("UPDATE tasks SET updated_at = '2026-10-18T16:10:00Z' WHERE id = ?1", [essay]).unwrap();
        settle(&conn, uid, at("2026-10-18T16:11:00Z")).unwrap();
        assert_eq!(outcome(&conn, acted).as_deref(), Some("acted"));
    }

    #[test]
    fn unanswered_nudges_raise_the_level_and_acting_on_the_task_resets_it() {
        let (conn, uid) = env();
        let essay = task(&conn, uid, "essay");
        let other = task(&conn, uid, "laundry");
        for t in ["2026-10-18T13:00:00Z", "2026-10-18T14:00:00Z", "2026-10-18T15:00:00Z"] {
            sent(&conn, uid, Via::Message, &[essay], t);
        }
        sent(&conn, uid, Via::Message, &[other], "2026-10-18T15:10:00Z");
        let now = at("2026-10-18T16:00:00Z");
        settle(&conn, uid, now).unwrap();
        assert_eq!(level(&conn, uid, Some(essay), now).unwrap(), 3);
        assert_eq!(level(&conn, uid, Some(other), now).unwrap(), 1, "each task keeps its own count");
        let fourth = sent(&conn, uid, Via::Call, &[essay], "2026-10-18T16:00:00Z");
        let went_out_at: i64 = conn.query_row("SELECT level FROM outreach WHERE id = ?1", [fourth], |r| r.get(0)).unwrap();
        assert_eq!(went_out_at, 3);

        conn.execute(
            "INSERT INTO work_sessions (user_id, task_id, title, started_at) VALUES (?1, ?2, 'essay', '2026-10-18T17:00:00Z')",
            (uid, essay),
        )
        .unwrap();
        let later = at("2026-10-18T17:05:00Z");
        assert_eq!(level(&conn, uid, Some(essay), later).unwrap(), 0);
        let s = summary(&conn, uid, &jiff::tz::TimeZone::UTC, Lang::En, later).unwrap();
        assert!(s.contains("- task 1 essay: 13:00 msg ignored · 14:00 msg ignored · 15:00 msg ignored · 16:00 call ignored — level 0, acted 17:00"), "{s}");
    }

    #[test]
    fn the_summary_shows_the_last_few_per_open_task_and_drops_finished_ones() {
        let (conn, uid) = env();
        let essay = task(&conn, uid, "essay");
        let done = task(&conn, uid, "taxes");
        for t in ["2026-10-18T12:00:00Z", "2026-10-18T13:00:00Z", "2026-10-18T14:00:00Z", "2026-10-18T14:20:00Z", "2026-10-18T15:00:00Z"] {
            sent(&conn, uid, Via::Message, &[essay], t);
        }
        sent(&conn, uid, Via::Message, &[done], "2026-10-18T15:00:00Z");
        conn.execute("UPDATE tasks SET state = 'done', completed_at = '2026-10-18T15:01:00Z' WHERE id = ?1", [done]).unwrap();
        sent(&conn, uid, Via::Message, &[], "2026-10-18T15:10:00Z");
        let s = summary(&conn, uid, &jiff::tz::TimeZone::UTC, Lang::En, at("2026-10-18T15:20:00Z")).unwrap();
        assert!(!s.contains("taxes"), "{s}");
        let essay_line = s.lines().find(|l| l.contains("essay")).unwrap();
        assert_eq!(
            essay_line,
            "- task 1 essay: 13:00 msg ignored · 14:00 msg ignored · 14:20 msg ignored · 15:00 msg waiting — level 4"
        );
        assert!(s.contains("- no task: 15:10 msg waiting — level 0"), "{s}");
        assert!(s.len() <= MAX_SUMMARY_BYTES + 1);
        let none = summary(&conn, uid, &jiff::tz::TimeZone::UTC, Lang::En, at("2026-10-21T15:40:00Z")).unwrap();
        assert!(none.is_empty(), "nothing in the lookback: {none}");
    }

    #[test]
    fn a_call_lands_by_how_its_ring_ended_and_the_hook_overrides_it() {
        let (conn, uid) = env();
        let id = sent(&conn, uid, Via::Call, &[], "2026-10-18T15:00:00Z");
        conn.execute(
            "INSERT INTO voice_calls (id, user_id, direction, state, outcome, ring_by, created_at)
             VALUES ('c1', ?1, 'outbound', 'ended', 'missed', 'x', '2026-10-18T15:00:01Z')",
            [uid],
        )
        .unwrap();
        settle(&conn, uid, at("2026-10-18T15:02:00Z")).unwrap();
        assert_eq!(outcome(&conn, id).as_deref(), Some("missed"));
        call_ended(&conn, id, CallEnd::Declined, at("2026-10-18T15:03:00Z")).unwrap();
        assert_eq!(outcome(&conn, id).as_deref(), Some("declined"));
        assert_eq!(level(&conn, uid, None, at("2026-10-18T15:04:00Z")).unwrap(), 1);
    }

    #[test]
    fn a_wait_settles_the_outreach_it_parked() {
        let (conn, uid) = env();
        let essay = task(&conn, uid, "essay");
        let id = record(
            &conn,
            &Sent { user_id: uid, event_id: 1, wait_event_id: Some(7), origin: "agent", via: Via::Message, tasks: &[essay], wait_min: Some(15), at: at("2026-10-18T15:00:00Z") },
        )
        .unwrap();
        let until: String = conn.query_row("SELECT window_until FROM outreach WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(until, "2026-10-18T15:15:00Z");
        assert_eq!(tasks_of_events(&conn, &[7]).unwrap(), [essay], "the resumed session is about the same task");
        wait_over(&conn, 7, Some(crate::triggers::WaitEnd::TaskDone), at("2026-10-18T15:05:00Z")).unwrap();
        assert_eq!(outcome(&conn, id).as_deref(), Some("acted"));
    }

    #[test]
    fn another_users_task_is_never_recorded() {
        let (conn, uid) = env();
        let theirs = crate::auth::create_user(&conn, "shuntia", "p", false).unwrap();
        let t = task(&conn, theirs, "theirs");
        let id = sent(&conn, uid, Via::Message, &[t], "2026-10-18T15:00:00Z");
        assert!(tasks_of(&conn, id).unwrap().is_empty());
    }

    #[test]
    fn the_review_tallies_how_each_kind_of_nudge_landed() {
        let (conn, uid) = env();
        let a = sent(&conn, uid, Via::Message, &[], "2026-10-13T10:00:00Z");
        set(&conn, a, Landed::Ignored, at("2026-10-13T10:30:00Z")).unwrap();
        let b = sent(&conn, uid, Via::Call, &[], "2026-10-13T11:00:00Z");
        set(&conn, b, Landed::Missed, at("2026-10-13T11:05:00Z")).unwrap();
        let lines = review_lines(&conn, uid, "2026-10-12T00:00:00Z", "2026-10-19T00:00:00Z", Lang::En).unwrap();
        assert!(lines.contains(&"- messages: 1 — 1 ignored".to_string()), "{lines:?}");
        assert!(lines.contains(&"- calls: 1 — 1 missed".to_string()), "{lines:?}");
        assert!(lines.contains(&"- sent at level 1: 1 — 1 missed".to_string()), "{lines:?}");
    }
}
