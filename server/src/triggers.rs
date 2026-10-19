use crate::model_text as mt;
use crate::text::Lang;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;
use std::fmt::Write as _;

/// The event kind a trigger point carries on the day's plan.
pub const KIND: &str = "trigger";
/// How far past a work session's planned end its own checks may still reach.
pub const OVERRUN_MIN: i64 = 15;
/// The furthest ahead a `+Nmin` offset may reach; a later day is named by date.
pub const MAX_OFFSET_MIN: i64 = 24 * 60;
/// Two pending triggers closer than this are one follow-up laid twice.
pub const NEAR_MIN: i64 = 20;
pub const MAX_PROMPT_BYTES: usize = 1000;
pub const MAX_SAY_BYTES: usize = 1200;
/// How much of the thread a firing trigger reads.
pub const HISTORY_TURNS: usize = 12;
pub const MAX_EXTRA: u32 = 10;
/// How long `say` may wait for an answer before its follow-up fires.
pub const AWAIT_MIN: std::ops::RangeInclusive<u32> = 5..=120;
/// The origin of the event a waiting `say` resumes on; off the day's budget.
pub const AWAIT_ORIGIN: &str = "await_reply";
/// Waits a day holds, so a chain of them ends.
pub const MAX_AWAITS_PER_DAY: i64 = 6;
pub const TIMED_OUT: &str = "Timed out";
pub const RESPONDED: &str = "User responded before timeout";
pub const ACTED: &str = "User acted before timeout";
/// How long a fired trigger whose session never decided waits before it is
/// tried again, and how many tries it gets in all.
pub const RETRY_AFTER_MIN: i64 = 10;
pub const MAX_ATTEMPTS: i64 = 3;
/// How late a trigger may still fire; one later than this no longer applies.
pub const LATE_MIN: i64 = 90;
/// Longer than any wake-up session runs: a trigger fired before this and still
/// undecided is not running.
pub const SESSION_MAX_MIN: i64 = 30;
const MAX_DELTA_BYTES: usize = 4000;
/// Pending triggers the model may hold across all days, so a far horizon
/// cannot fill without end.
pub const MAX_PENDING: i64 = 200;
/// Session-tied triggers a work session may hold, its own checks included,
/// before the model's next one there is charged to the day's budget.
pub const MAX_FREE_PER_SESSION: i64 = 8;

/// Why a trigger was not laid. `CapReached` names the way out, because the
/// model's next move is to ask the user rather than to try again.
#[derive(Debug)]
pub enum Refusal {
    CapReached { date: jiff::civil::Date, allowance: u32, spent: u32 },
    Past { at: String },
    NotFound(String),
    Rejected(String),
    Internal(String),
}

fn internal(e: impl std::fmt::Display) -> Refusal {
    Refusal::Internal(e.to_string())
}

#[derive(Debug)]
pub struct Laid {
    pub event_id: i64,
    pub at: String,
    pub cancel_if: Option<&'static str>,
}

/// A trigger's cancel rule and what it points at: the reason this follow-up
/// would no longer be worth sending.
#[derive(Clone, Copy, Debug)]
pub enum Cancel {
    Replied,
    TaskDone(i64),
    EventDecided(i64),
    Active,
}

impl Cancel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Cancel::Replied => "replied",
            Cancel::TaskDone(_) => "task_done",
            Cancel::EventDecided(_) => "event_decided",
            Cancel::Active => "active",
        }
    }

    fn reference(&self) -> Option<i64> {
        match self {
            Cancel::Replied | Cancel::Active => None,
            Cancel::TaskDone(id) | Cancel::EventDecided(id) => Some(*id),
        }
    }
}

#[derive(Debug)]
pub struct WorkSession {
    pub id: i64,
    pub title: String,
    pub planned_min: Option<i64>,
    pub started_at: String,
    pub paused: bool,
}

/// The user's session, if one is open, paused or not. A session check laid
/// while one is open belongs to it: accountability is the point, so those
/// checks stay off the day's budget up to `MAX_FREE_PER_SESSION` and end with
/// the session.
pub fn open_work_session(conn: &Connection, user_id: i64) -> rusqlite::Result<Option<WorkSession>> {
    conn.query_row(
        "SELECT id, title, planned_min, started_at, paused_at IS NOT NULL FROM work_sessions
         WHERE user_id = ?1 AND ended_at IS NULL ORDER BY id DESC LIMIT 1",
        [user_id],
        |r| {
            Ok(WorkSession {
                id: r.get(0)?,
                title: r.get(1)?,
                planned_min: r.get(2)?,
                started_at: r.get(3)?,
                paused: r.get(4)?,
            })
        },
    )
    .optional()
}

/// The user's session while its clock runs; a paused one is not work.
pub fn working(conn: &Connection, user_id: i64) -> rusqlite::Result<Option<WorkSession>> {
    Ok(open_work_session(conn, user_id)?.filter(|s| !s.paused))
}

pub fn timezone(config_dir: &Path, username: &str) -> jiff::tz::TimeZone {
    crate::config::UserConfig::load(config_dir, username)
        .ok()
        .and_then(|c| jiff::tz::TimeZone::get(&c.timezone).ok())
        .unwrap_or(jiff::tz::TimeZone::UTC)
}

/// A fixed-offset zone whose wall clock reads midday however the suite is
/// timed. A test writes it as the user's own zone when its `+Nmin` lays must
/// stay on the day that laid them, budget and plan alike.
#[cfg(test)]
pub(crate) fn midday_zone() -> jiff::tz::TimeZone {
    let hour = i32::from(jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).hour());
    // Etc/GMT+N runs N hours behind UTC, so the sign reads backwards.
    jiff::tz::TimeZone::get(&format!("Etc/GMT{:+}", hour - 12)).unwrap()
}

pub fn allowance_of(config_dir: &Path, username: &str) -> u32 {
    crate::config::UserConfig::load(config_dir, username)
        .map_or(crate::config::DEFAULT_TRIGGERS_PER_DAY, |c| c.triggers_per_day())
}

/// What the user's own extra raised today's allowance to.
pub fn allowance(
    conn: &Connection,
    config_dir: &Path,
    username: &str,
    user_id: i64,
    date: jiff::civil::Date,
) -> rusqlite::Result<u32> {
    allowance_over(conn, allowance_of(config_dir, username), user_id, date)
}

/// `allowance` over a daily allowance already read from the user's config.
pub fn allowance_over(conn: &Connection, per_day: u32, user_id: i64, date: jiff::civil::Date) -> rusqlite::Result<u32> {
    let extra: i64 = conn
        .query_row(
            "SELECT extra FROM trigger_budgets WHERE user_id = ?1 AND date = ?2",
            (user_id, date.to_string()),
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    Ok(per_day.saturating_add(extra.max(0) as u32))
}

/// Triggers the agent laid, or idleness laid, for that day out of its own
/// budget: a check inside a work session is the session's, not the day's, and
/// a dropped one gives its place back.
pub fn spent(conn: &Connection, user_id: i64, date: jiff::civil::Date) -> rusqlite::Result<u32> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND p.date = ?2 AND e.kind = ?3
           AND e.origin IN ('agent', 'idle') AND e.work_session_id IS NULL AND e.status != 'dropped'",
        (user_id, date.to_string(), KIND),
        |r| r.get(0),
    )?;
    Ok(n as u32)
}

/// Raises today's allowance, once the user has agreed to it. Returns the new
/// extra, so the same agreement asked for twice does not silently double.
pub fn add_extra(
    conn: &Connection,
    user_id: i64,
    date: jiff::civil::Date,
    extra: u32,
) -> rusqlite::Result<u32> {
    conn.execute(
        "INSERT INTO trigger_budgets (user_id, date, extra) VALUES (?1, ?2, ?3)
         ON CONFLICT (user_id, date) DO UPDATE SET extra = extra + ?3",
        (user_id, date.to_string(), extra),
    )?;
    conn.query_row(
        "SELECT extra FROM trigger_budgets WHERE user_id = ?1 AND date = ?2",
        (user_id, date.to_string()),
        |r| r.get::<_, i64>(0).map(|n| n as u32),
    )
}

/// Writes the trigger row itself, with no budget or timing opinion: the caller
/// has already decided this one is allowed. `origin` separates a check the agent
/// chose to lay from one the server lays for the ritual it runs.
#[allow(clippy::too_many_arguments)]
pub fn insert(
    conn: &Connection,
    plan_id: i64,
    wall: &str,
    prompt: &str,
    origin: &str,
    cancel: Option<Cancel>,
    conversation_id: Option<i64>,
    work_session_id: Option<i64>,
    now: jiff::Timestamp,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, flexibility,
                             slide_window_min, channel, origin, prompt, cancel_if, cancel_ref,
                             conversation_id, work_session_id, created_at)
         VALUES (?1, ?2, ?3, ?3, 'drop', 0, 'push', ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        (
            plan_id,
            KIND,
            wall,
            origin,
            prompt,
            cancel.map(|c| c.as_str()),
            cancel.and_then(|c| c.reference()),
            conversation_id,
            work_session_id,
            now.to_string(),
        ),
    )?;
    Ok(conn.last_insert_rowid())
}

/// Marks a laid trigger to ring when it fires.
pub fn set_ring(conn: &Connection, event_id: i64) -> rusqlite::Result<usize> {
    conn.execute("UPDATE events SET channel = ?1 WHERE id = ?2", (crate::channels::RING_CHANNEL, event_id))
}

pub struct Lay<'a> {
    pub config_dir: &'a Path,
    pub user_id: i64,
    pub username: &'a str,
    /// Zero-padded `HH:MM` on `date`, or `+Nmin` from now.
    pub at: &'a str,
    pub prompt: &'a str,
    /// The plan day the trigger belongs to: today, or the day a nightly run is
    /// planning.
    pub date: jiff::civil::Date,
    pub cancel: Option<Cancel>,
    pub conversation_id: Option<i64>,
    /// The session this check belongs to, named by the server for a session
    /// that is already closed; it costs the day's budget nothing.
    pub work_session_id: Option<i64>,
    /// A check on how the open work session is going: within the session's
    /// reach it is the session's, off the day's budget and dropped when the
    /// session ends. Anything else stays on the day whatever is open.
    pub session_check: bool,
    /// A check the server lays for a ritual of its own rather than one the
    /// agent chose: it is the day's furniture, so it is exempt from the budget
    /// and never belongs to an open session.
    pub system: bool,
    pub now: jiff::Timestamp,
}

/// Where `at` lands: an offset carries its own date, so one crossing midnight
/// is tomorrow's business rather than a refusal; a wall time belongs to the day
/// the session is laying for.
fn resolve(at: &str, lay: &Lay, local: &jiff::Zoned) -> Result<(jiff::civil::Date, i64), Refusal> {
    let loose = || Refusal::Rejected(format!("at must be zero-padded HH:MM or +Nmin, got {at:?}"));
    if let Some(rest) = at.strip_prefix('+') {
        let n: i64 = rest.trim_end_matches("min").parse().map_err(|_| loose())?;
        if !(0..=MAX_OFFSET_MIN).contains(&n) {
            return Err(Refusal::Rejected(format!(
                "an offset must be 0 to {MAX_OFFSET_MIN} minutes, got {at:?}"
            )));
        }
        let there = (lay.now + jiff::Span::new().minutes(n))
            .to_zoned(local.time_zone().clone());
        return Ok((there.date(), i64::from(there.hour()) * 60 + i64::from(there.minute())));
    }
    if !crate::templates::valid_time(at) {
        return Err(loose());
    }
    Ok((lay.date, crate::templates::wall_minutes(at)))
}

/// The instant `lay.at` names, read the way `lay` will place it.
pub fn aimed_at(lay: &Lay) -> Result<jiff::Timestamp, Refusal> {
    let tz = timezone(lay.config_dir, lay.username);
    let local = lay.now.to_zoned(tz.clone());
    let (date, target) = resolve(lay.at, lay, &local)?;
    let wall = format!("{:02}:{:02}", target / 60, target % 60);
    crate::zone::instant(&tz, date, &wall).ok_or_else(|| Refusal::Internal(format!("{date} {wall} names no instant")))
}

/// A pending trigger already laid for about the same moment.
#[derive(Debug)]
pub struct Near {
    pub event_id: i64,
    pub date: String,
    pub at: String,
    pub prompt: String,
}

/// The pending trigger of this user nearest `target`, when one sits within
/// `NEAR_MIN` minutes of it.
pub fn near(
    conn: &Connection,
    user_id: i64,
    tz: &jiff::tz::TimeZone,
    target: jiff::Timestamp,
) -> rusqlite::Result<Option<Near>> {
    let day = target.to_zoned(tz.clone()).date();
    let around = |d: i64| day.checked_add(jiff::Span::new().days(d)).unwrap_or(day).to_string();
    let mut stmt = conn.prepare(
        "SELECT e.id, p.date, e.wall_time, e.prompt FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND e.kind = ?2 AND e.status IN ('pending', 'snoozed')
           AND p.date BETWEEN ?3 AND ?4",
    )?;
    let rows = stmt.query_map((user_id, KIND, around(-1), around(1)), |r| {
        Ok(Near { event_id: r.get(0)?, date: r.get(1)?, at: r.get(2)?, prompt: r.get(3)? })
    })?;
    let mut best: Option<(i64, Near)> = None;
    for row in rows {
        let row = row?;
        let Some(at) = row.date.parse().ok().and_then(|d| crate::zone::instant(tz, d, &row.at)) else {
            continue;
        };
        let apart = (at - target).get_seconds().abs() / 60;
        if apart <= NEAR_MIN && best.as_ref().is_none_or(|(b, _)| apart < *b) {
            best = Some((apart, row));
        }
    }
    Ok(best.map(|(_, n)| n))
}

/// Lays one trigger point, refusing on the day's budget and on a time already
/// gone. A session check laid for today within an open work session's reach
/// belongs to that session and never counts, until the session holds
/// `MAX_FREE_PER_SESSION`; one beyond either is the day's.
pub fn lay(conn: &Connection, lay: &Lay) -> Result<Laid, Refusal> {
    let prompt = lay.prompt.trim();
    if prompt.is_empty() || prompt.len() > MAX_PROMPT_BYTES {
        return Err(Refusal::Rejected(format!(
            "prompt must be 1 to {MAX_PROMPT_BYTES} bytes"
        )));
    }
    let tz = timezone(lay.config_dir, lay.username);
    let local = lay.now.to_zoned(tz);
    let (date, target) = resolve(lay.at, lay, &local)?;
    let wall = format!("{:02}:{:02}", target / 60, target % 60);

    let lead = lead_minutes(date, target, &local);
    if lead < 0 {
        return Err(Refusal::Past { at: wall });
    }
    let open = if lay.session_check { open_work_session(conn, lay.user_id).map_err(internal)? } else { None };
    if lay.session_check && open.is_none() {
        return Err(Refusal::Rejected("no work session is open; lay it without session_check".into()));
    }
    let session = match open.filter(|s| {
        date == local.date() && overrun_lead(s, lay.now).is_none_or(|limit| lead <= limit)
    }) {
        Some(s) if session_held(conn, s.id).map_err(internal)? < MAX_FREE_PER_SESSION => Some(s),
        _ => None,
    };
    if lay.work_session_id.is_none() && !lay.system && session.is_none() {
        if pending_laid(conn, lay.user_id, local.date()).map_err(internal)? >= MAX_PENDING {
            return Err(Refusal::Rejected(format!(
                "{MAX_PENDING} triggers already wait; drop some (schedule_drop) or lay this one \
                 with replace before adding more"
            )));
        }
        let allowance = allowance(conn, lay.config_dir, lay.username, lay.user_id, date)
            .map_err(internal)?;
        let spent = spent(conn, lay.user_id, date).map_err(internal)?;
        if spent >= allowance {
            return Err(Refusal::CapReached { date, allowance, spent });
        }
    }
    if let Some(id) = lay.conversation_id {
        if !crate::talk::owned(conn, lay.user_id, id).map_err(internal)? {
            return Err(Refusal::NotFound(format!("no conversation {id} for this user")));
        }
    }
    if let Some(Cancel::TaskDone(id)) = lay.cancel {
        if crate::tasks::get(conn, lay.user_id, id).map_err(internal)?.is_none() {
            return Err(Refusal::NotFound(format!("no task {id} for this user")));
        }
    }
    if let Some(Cancel::EventDecided(id)) = lay.cancel {
        if crate::plan::event_gate(conn, lay.user_id, id).map_err(internal)?.is_none() {
            return Err(Refusal::NotFound(format!("no event {id} for this user")));
        }
    }

    let plan_id = crate::plan::holding(conn, lay.config_dir, lay.username, lay.user_id, date, local.date())
        .map_err(internal)?;
    let event_id = insert(
        conn,
        plan_id,
        &wall,
        prompt,
        if lay.system { "template" } else { "agent" },
        lay.cancel,
        lay.conversation_id,
        if lay.system { None } else { lay.work_session_id.or_else(|| session.as_ref().map(|s| s.id)) },
        lay.now,
    )
    .map_err(internal)?;
    crate::log::record(
        conn,
        Some(lay.user_id),
        "trigger_laid",
        &format!("event {event_id} at {date} {wall}: {prompt}"),
    )
    .map_err(internal)?;
    Ok(Laid { event_id, at: wall, cancel_if: lay.cancel.map(|c| c.as_str()) })
}

/// The model's own triggers still waiting from `today` on.
fn pending_laid(conn: &Connection, user_id: i64, today: jiff::civil::Date) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND p.date >= ?2 AND e.kind = ?3 AND e.origin = 'agent'
           AND e.work_session_id IS NULL AND e.status IN ('pending', 'snoozed')",
        (user_id, today.to_string(), KIND),
        |r| r.get(0),
    )
}

/// Every trigger ever tied to this work session, whatever became of it.
fn session_held(conn: &Connection, session_id: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM events WHERE kind = ?1 AND work_session_id = ?2",
        (KIND, session_id),
        |r| r.get(0),
    )
}

/// A pending trigger as the model is shown it.
#[derive(Debug)]
pub struct Waiting {
    pub event_id: i64,
    pub date: String,
    pub at: String,
    pub prompt: String,
}

/// Up to `limit` pending triggers dated `from` through `to`, soonest first, and
/// how many more there are past those in that span.
pub fn waiting(
    conn: &Connection,
    user_id: i64,
    from: jiff::civil::Date,
    to: Option<jiff::civil::Date>,
    limit: usize,
) -> rusqlite::Result<(Vec<Waiting>, usize)> {
    let to = to.map_or_else(|| "9999-12-31".to_string(), |d| d.to_string());
    let mut stmt = conn.prepare_cached(
        "SELECT e.id, p.date, e.wall_time, COALESCE(e.prompt, '') FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND e.kind = ?2 AND e.status IN ('pending', 'snoozed')
           AND p.date BETWEEN ?3 AND ?4
         ORDER BY p.date, e.wall_time, e.id",
    )?;
    let mut shown = Vec::new();
    let mut more = 0;
    let rows = stmt.query_map((user_id, KIND, from.to_string(), to), |r| {
        Ok(Waiting { event_id: r.get(0)?, date: r.get(1)?, at: r.get(2)?, prompt: r.get(3)? })
    })?;
    for row in rows {
        let row = row?;
        if shown.len() < limit {
            shown.push(row);
        } else {
            more += 1;
        }
    }
    Ok((shown, more))
}

/// How many upcoming and how many settled triggers the log shows, and how far
/// back the settled ones reach.
const LOG_UPCOMING: usize = 50;
const LOG_PAST: usize = 100;
const LOG_DAYS: i64 = 7;

/// `(id, date, at, status, prompt, origin)`.
type LogRow = (i64, String, String, String, String, String);

/// The user's trigger log for the chat's read-only thread, oldest first: the
/// settled ones of the last week, then those still waiting. Each carries what
/// Note meant to look at and, once settled, how it ended.
pub fn log(conn: &Connection, user_id: i64, today: jiff::civil::Date) -> rusqlite::Result<Vec<serde_json::Value>> {
    let since = today.checked_sub(jiff::Span::new().days(LOG_DAYS)).unwrap_or(today);
    let rows = |sql: &str, limit: usize| -> rusqlite::Result<Vec<LogRow>> {
        let mut stmt = conn.prepare(sql)?;
        let out = stmt
            .query_map((user_id, KIND, since.to_string(), limit as i64), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, Option<String>>(4)?.unwrap_or_default(), r.get(5)?))
            })?
            .collect();
        out
    };
    let mut past = rows(
        "SELECT e.id, p.date, e.wall_time, e.status, e.prompt, e.origin FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND e.kind = ?2 AND e.status NOT IN ('pending', 'snoozed') AND p.date >= ?3
         ORDER BY p.date DESC, e.wall_time DESC, e.id DESC LIMIT ?4",
        LOG_PAST,
    )?;
    past.reverse();
    let upcoming = rows(
        "SELECT e.id, p.date, e.wall_time, e.status, e.prompt, e.origin FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND e.kind = ?2 AND e.status IN ('pending', 'snoozed') AND p.date >= ?3
         ORDER BY p.date, e.wall_time, e.id LIMIT ?4",
        LOG_UPCOMING,
    )?;
    let endings = endings(conn, user_id, since)?;
    Ok(past
        .into_iter()
        .chain(upcoming)
        .map(|(id, date, at, status, prompt, origin)| {
            let (outcome, text) = endings.get(&id).cloned().map_or((None, None), |(o, t)| (Some(o), t));
            serde_json::json!({
                "event_id": id, "date": date, "at": at, "status": status, "prompt": prompt,
                "origin": origin, "outcome": outcome, "text": text,
            })
        })
        .collect())
}

/// How each trigger since `since` ended, from the log rows its firing wrote:
/// `said`, `rang`, `quiet`, `cancelled` or `replaced`, with the words or reason.
fn endings(
    conn: &Connection,
    user_id: i64,
    since: jiff::civil::Date,
) -> rusqlite::Result<std::collections::HashMap<i64, (&'static str, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT kind, detail FROM event_log WHERE user_id = ?1 AND ts >= ?2
           AND kind IN ('trigger_said', 'trigger_rang', 'trigger_quiet', 'trigger_cancelled', 'trigger_replaced')
         ORDER BY id",
    )?;
    let mut out: std::collections::HashMap<i64, (&'static str, Option<String>)> = std::collections::HashMap::new();
    let rows = stmt.query_map((user_id, since.to_string()), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (kind, detail) = row?;
        let Some(rest) = detail.strip_prefix("event ") else { continue };
        let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        let Ok(id) = rest[..digits].parse::<i64>() else { continue };
        let tail = rest[digits..].trim_start_matches(':').trim();
        let tail = tail.trim_start_matches('(').trim_end_matches(')');
        let outcome = match kind.as_str() {
            "trigger_said" => "said",
            "trigger_rang" => "rang",
            "trigger_quiet" => "quiet",
            "trigger_cancelled" => "cancelled",
            _ => "replaced",
        };
        if outcome == "rang" {
            if let Some((_, said)) = out.get(&id) {
                out.insert(id, ("rang", said.clone()));
            }
            continue;
        }
        out.insert(id, (outcome, (!tail.is_empty()).then(|| tail.to_string())));
    }
    Ok(out)
}

/// Whether a trigger is the close-the-day check, in whichever language it was laid.
fn is_close_day(prompt: &str) -> bool {
    [Lang::En, Lang::Ja].into_iter().any(|l| prompt == mt::close_day_prompt(l))
}

/// Puts the day's close-the-day check on `date`, replacing the one an earlier
/// run left there so a date never holds two. Returns the event, or nothing when
/// the user has turned the ritual off or the time has already gone by.
pub fn lay_close_day(
    conn: &Connection,
    config_dir: &Path,
    username: &str,
    user_id: i64,
    date: jiff::civil::Date,
    now: jiff::Timestamp,
) -> Result<Option<i64>> {
    let at = crate::config::UserConfig::load(config_dir, username)?.close_day_time().to_string();
    conn.execute(
        "DELETE FROM events WHERE kind = ?1 AND origin = 'template'
           AND plan_id IN (SELECT id FROM plans WHERE user_id = ?2 AND date = ?3)",
        (KIND, user_id, date.to_string()),
    )?;
    if at.is_empty() {
        return Ok(None);
    }
    let laid = lay(
        conn,
        &Lay {
            config_dir,
            user_id,
            username,
            at: &at,
            prompt: mt::close_day_prompt(Lang::for_user(config_dir, username)),
            date,
            cancel: None,
            conversation_id: None,
            work_session_id: None,
            session_check: false,
            system: true,
            now,
        },
    );
    match laid {
        Ok(l) => Ok(Some(l.event_id)),
        Err(Refusal::Past { .. }) => Ok(None),
        Err(e) => Err(anyhow::anyhow!("{e:?}")),
    }
}

/// How many minutes from now the target wall time on `date` is; negative once
/// it has gone by.
pub(crate) fn lead_minutes(date: jiff::civil::Date, target: i64, local: &jiff::Zoned) -> i64 {
    let days = i64::from((date - local.date()).get_days());
    let now_min = i64::from(local.hour()) * 60 + i64::from(local.minute());
    days * 24 * 60 + target - now_min
}

/// How far ahead of now a work session's own checks may still reach, when it
/// named a length.
fn overrun_lead(session: &WorkSession, now: jiff::Timestamp) -> Option<i64> {
    let planned = session.planned_min?;
    let started: jiff::Timestamp = session.started_at.parse().ok()?;
    let end = started + jiff::Span::new().minutes(planned + OVERRUN_MIN);
    (end - now).total(jiff::Unit::Minute).ok().map(|m| m as i64)
}

/// A trigger row as the runner reads it at fire time.
pub struct Firing {
    pub event_id: i64,
    pub prompt: String,
    pub cancel_if: Option<String>,
    pub cancel_ref: Option<i64>,
    pub conversation_id: Option<i64>,
    pub work_session_id: Option<i64>,
    pub created_at: Option<String>,
    pub wall_time: String,
    pub origin: String,
}

pub fn read(conn: &Connection, event_id: i64) -> rusqlite::Result<Option<Firing>> {
    conn.query_row(
        "SELECT id, prompt, cancel_if, cancel_ref, conversation_id, work_session_id,
                created_at, wall_time, origin
         FROM events WHERE id = ?1",
        [event_id],
        |r| {
            Ok(Firing {
                event_id: r.get(0)?,
                prompt: r.get(1)?,
                cancel_if: r.get(2)?,
                cancel_ref: r.get(3)?,
                conversation_id: r.get(4)?,
                work_session_id: r.get(5)?,
                created_at: r.get(6)?,
                wall_time: r.get(7)?,
                origin: r.get(8)?,
            })
        },
    )
    .optional()
}

/// Whether the reason this trigger was laid has already taken care of itself:
/// the user replied in the thread, finished or dropped the task, settled the
/// event it was waiting on, or came back after going quiet.
pub fn cancelled(conn: &Connection, user_id: i64, ev: &Firing) -> rusqlite::Result<bool> {
    let Some(rule) = ev.cancel_if.as_deref() else {
        return Ok(false);
    };
    match rule {
        "replied" => {
            let (Some(id), Some(since)) = (ev.conversation_id, ev.created_at.as_deref()) else {
                return Ok(false);
            };
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM talk_messages
                 WHERE conversation_id = ?1 AND role = 'user' AND created_at > ?2",
                (id, since),
                |r| r.get(0),
            )?;
            Ok(n > 0)
        }
        "task_done" => {
            let Some(task_id) = ev.cancel_ref else { return Ok(false) };
            let state: Option<String> = conn
                .query_row(
                    "SELECT state FROM tasks WHERE id = ?1 AND user_id = ?2",
                    (task_id, user_id),
                    |r| r.get(0),
                )
                .optional()?;
            Ok(matches!(state.as_deref(), Some("done" | "dropped")))
        }
        "event_decided" => {
            let Some(event_id) = ev.cancel_ref else { return Ok(false) };
            let status = crate::plan::event_gate(conn, user_id, event_id)
                .ok()
                .flatten()
                .map(|(_, status)| status);
            Ok(matches!(status.as_deref(), Some("done" | "dropped")))
        }
        "active" => {
            let Some(since) =
                ev.created_at.as_deref().and_then(|t| t.parse::<jiff::Timestamp>().ok())
            else {
                return Ok(false);
            };
            Ok(crate::presence::last_active(conn, user_id)?
                .is_some_and(|at| at.as_second() >= since.as_second()))
        }
        _ => Ok(false),
    }
}

/// Settles a trigger whose reason has passed, before anything is sent.
pub fn cancel(conn: &Connection, user_id: i64, ev: &Firing, now: jiff::Timestamp) -> Result<()> {
    conn.execute(
        "UPDATE events SET status = 'dropped', decided_at = ?1 WHERE id = ?2",
        (now.to_string(), ev.event_id),
    )?;
    crate::log::record(
        conn,
        Some(user_id),
        "trigger_cancelled",
        &format!("event {} ({})", ev.event_id, ev.cancel_if.as_deref().unwrap_or("no rule")),
    )?;
    Ok(())
}

/// What the session is looking at, under the prompt it was laid with: when it
/// was laid, when it was meant for, the work session it belongs to or else the
/// one open, and how long since the user last said anything.
pub fn situation(
    conn: &Connection,
    user_id: i64,
    ev: &Firing,
    tz: &jiff::tz::TimeZone,
    l: Lang,
) -> String {
    let clock = |ts: &str| -> String {
        ts.parse::<jiff::Timestamp>().map_or_else(|_| ts.to_string(), |t| t.to_zoned(tz.clone()).strftime("%H:%M").to_string())
    };
    let mut s = format!("{}\n\n", ev.prompt);
    match ev.created_at.as_deref() {
        Some(at) => { let _ = writeln!(s, "{}", mt::laid_at(l, &clock(at), &ev.wall_time)); },
        None => { let _ = writeln!(s, "{}", mt::meant_for(l, &ev.wall_time)); },
    }
    let session_id = ev
        .work_session_id
        .or_else(|| open_work_session(conn, user_id).ok().flatten().map(|w| w.id));
    if let Some(id) = session_id {
        type Row = (String, Option<i64>, String, String, String, i64, Option<String>);
        let row: Option<Row> = conn
            .query_row(
                "SELECT title, planned_min, started_at, mode, phase, round, paused_at
                 FROM work_sessions WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .optional()
            .ok()
            .flatten();
        if let Some((title, planned, started, mode, phase, round, paused)) = row {
            s.push_str(&mt::work_session(l, &title, &clock(&started)));
            if let Some(min) = planned {
                s.push_str(&mt::planned_min(l, min));
            }
            if mode == "pomodoro" {
                s.push_str(&mt::pomodoro_round(l, &phase, round));
            }
            if let Some(at) = paused {
                s.push_str(&mt::paused_since(l, &clock(&at)));
            }
            let done = steps_done_since(conn, user_id, id, &started).unwrap_or(0);
            let _ = writeln!(s, "{}", mt::steps_done_since(l, done));
        }
    }
    match last_user_message(conn, user_id).ok().flatten() {
        Some(at) => { let _ = writeln!(s, "{}", mt::last_user_message(l, &clock(&at))); },
        None => s.push_str(mt::user_silent(l)),
    }
    s
}

/// Tasks the user finished since this work session's previous check, falling
/// back to the whole session when it has had none.
fn steps_done_since(
    conn: &Connection,
    user_id: i64,
    work_session_id: i64,
    started_at: &str,
) -> rusqlite::Result<i64> {
    let last: Option<String> = conn
        .query_row(
            "SELECT MAX(fired_at) FROM events
             WHERE work_session_id = ?1 AND fired_at IS NOT NULL",
            [work_session_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let since = last.filter(|t| t.as_str() > started_at).unwrap_or_else(|| started_at.to_string());
    conn.query_row(
        "SELECT COUNT(*) FROM tasks
         WHERE user_id = ?1 AND state = 'done' AND completed_at > ?2",
        (user_id, since),
        |r| r.get(0),
    )
}

fn last_user_message(conn: &Connection, user_id: i64) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT MAX(m.created_at) FROM talk_messages m
         JOIN conversations c ON c.id = m.conversation_id
         WHERE c.user_id = ?1 AND m.role = 'user'",
        [user_id],
        |r| r.get(0),
    )
    .optional()
    .map(Option::flatten)
}

/// The thread a firing trigger reads before it decides, and the note that says
/// what the window no longer reaches.
fn thread_context(
    conn: &Connection,
    conversation_id: Option<i64>,
    l: Lang,
) -> (Vec<crate::providers::Message>, Option<String>) {
    let Some(id) = conversation_id else {
        return (Vec::new(), None);
    };
    let history = crate::talk::history(conn, id, HISTORY_TURNS).unwrap_or_default();
    let note = crate::talk::summary(conn, id)
        .ok()
        .flatten()
        .map(|(summary, _)| mt::summary_thread_note(l, &summary));
    (history, note)
}

/// Where a trigger's words land: the thread it was laid against, else the day's
/// check-in thread. The text is written there whether or not a channel takes it.
fn record_said(
    conn: &Connection,
    fired: &crate::runner::FiredEvent,
    lang: crate::text::Lang,
    conversation_id: Option<i64>,
    text: &str,
    now: jiff::Timestamp,
) -> Result<i64> {
    match conversation_id {
        Some(id) => {
            crate::talk::append_text(conn, id, "assistant", text, now)?;
            crate::talk::touch(conn, id, now)?;
            Ok(id)
        }
        None => crate::talk::checkin_thread(conn, fired.user_id, &fired.date, &fired.wall_time, text, lang, now),
    }
}

const MAX_CHANGE_CHARS: usize = 300;

/// The activity row for one step a wake-up took, so the user can see what Note
/// did on its own; `false` when the step changed nothing.
pub fn log_change(conn: &Connection, user_id: i64, event_id: i64, name: &str, args: &str, is_error: bool) -> Result<bool> {
    if is_error || !crate::tools::writes(name) {
        return Ok(false);
    }
    let args: String = args.chars().take(MAX_CHANGE_CHARS).collect();
    crate::log::record(conn, Some(user_id), "wake_change", &format!("event {event_id}: {name} {args}"))?;
    Ok(true)
}

/// Ends `event_id` as `status`, taking the session it parked with it.
fn close(conn: &Connection, event_id: i64, status: &str, now: jiff::Timestamp) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE events SET status = ?1, decided_at = ?2 WHERE id = ?3",
        (status, now.to_string(), event_id),
    )?;
    conn.execute("DELETE FROM paused_sessions WHERE event_id = ?1", [event_id])?;
    Ok(())
}

/// Drops a trigger that came due more than `LATE_MIN` ago, unfired.
pub fn drop_stale(
    conn: &Connection,
    user_id: i64,
    event_id: i64,
    due: jiff::Timestamp,
    now: jiff::Timestamp,
) -> Result<()> {
    close(conn, event_id, "dropped", now)?;
    let late = now.duration_since(due).as_mins();
    crate::log::record(conn, Some(user_id), "trigger_stale", &format!("event {event_id}: {late} min late, dropped"))?;
    Ok(())
}

/// Takes back every trigger left fired for at least `after` whose user has no
/// wake-up `running`: a session that failed, never decided, or died with the
/// process. Each goes back to wait for the next sweep until `MAX_ATTEMPTS`
/// tries are spent, and is dropped then.
pub fn reclaim<S: std::hash::BuildHasher>(
    conn: &Connection,
    now: jiff::Timestamp,
    after: jiff::SignedDuration,
    running: &std::collections::HashSet<i64, S>,
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "SELECT e.id, p.user_id, e.attempts FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE e.kind = ?1 AND e.status = 'fired' AND (e.fired_at IS NULL OR e.fired_at <= ?2)",
    )?;
    let stuck: Vec<(i64, i64, i64)> = stmt
        .query_map((KIND, (now - after).to_string()), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for (id, user_id, attempts) in stuck.into_iter().filter(|(_, u, _)| !running.contains(u)) {
        if attempts + 1 >= MAX_ATTEMPTS {
            close(conn, id, "dropped", now)?;
            crate::log::record(conn, Some(user_id), "trigger_given_up", &format!("event {id}: undecided after {MAX_ATTEMPTS} tries"))?;
        } else {
            conn.execute(
                "UPDATE events SET status = 'pending', fired_at = NULL, attempts = attempts + 1 WHERE id = ?1",
                [id],
            )?;
            crate::log::record(conn, Some(user_id), "trigger_retry", &format!("event {id}: try {}", attempts + 2))?;
        }
    }
    Ok(())
}

/// Parks a session whose `say` waits `min` minutes for an answer: an event in
/// `thread` resumes it at the end of the wait, or as soon as the user writes or
/// moves (`wait_ended`). `None` once the day holds `MAX_AWAITS_PER_DAY`.
#[allow(clippy::too_many_arguments)]
pub fn pause(
    conn: &Connection,
    config_dir: &std::path::Path,
    fired: &crate::runner::FiredEvent,
    thread: i64,
    said: &str,
    min: u32,
    paused: &crate::agent::Paused,
    now: jiff::Timestamp,
) -> Result<Option<i64>> {
    let local = (now + jiff::SignedDuration::from_mins(i64::from(min))).to_zoned(timezone(config_dir, &fired.username));
    let date = local.date();
    let held: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND p.date = ?2 AND e.kind = ?3 AND e.origin = ?4",
        (fired.user_id, date.to_string(), KIND, AWAIT_ORIGIN),
        |r| r.get(0),
    )?;
    if held >= MAX_AWAITS_PER_DAY {
        return Ok(None);
    }
    let plan_id = crate::plan::ensure(conn, config_dir, &fired.username, fired.user_id, date)?;
    let prompt = mt::await_prompt(Lang::for_user(config_dir, &fired.username), said);
    let wall = local.strftime("%H:%M").to_string();
    let tx = conn.unchecked_transaction()?;
    let id = insert(&tx, plan_id, &wall, &prompt, AWAIT_ORIGIN, None, Some(thread), None, now)?;
    tx.execute("INSERT INTO paused_sessions (event_id, session) VALUES (?1, ?2)", (id, serde_json::to_string(paused)?))?;
    if fired.channel == crate::channels::RING_CHANNEL {
        set_ring(&tx, id)?;
    }
    tx.commit()?;
    Ok(Some(id))
}

/// A parked session and the context as it stood once it parked, so the resumed
/// session is told only what moved since.
struct Parked {
    paused: crate::agent::Paused,
    context: String,
}

/// The session `event_id` resumes, if it parked one. It stays parked until the
/// resumed session decides, so a resume that fails is tried again.
fn parked(conn: &Connection, event_id: i64) -> Result<Option<Parked>> {
    let raw: Option<(String, String)> = conn
        .query_row("SELECT session, context FROM paused_sessions WHERE event_id = ?1", [event_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    raw.map(|(session, context)| Ok(Parked { paused: serde_json::from_str(&session)?, context }))
        .transpose()
}

/// What the user wrote anywhere after `since`, oldest first.
fn written_since(conn: &Connection, user_id: i64, since: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT m.content FROM talk_messages m JOIN conversations c ON c.id = m.conversation_id
         WHERE c.user_id = ?1 AND m.role = 'user' AND m.created_at > ?2 ORDER BY m.created_at, m.id",
    )?;
    let rows = stmt.query_map((user_id, since), |r| r.get(0))?.collect();
    rows
}

/// What ended a waiting `say` before its time ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitEnd {
    Reply,
    TaskDone,
    WorkStarted,
    WorkEnded,
    TaskChanged,
}

impl WaitEnd {
    pub fn as_str(self) -> &'static str {
        match self {
            WaitEnd::Reply => "reply",
            WaitEnd::TaskDone => "task_done",
            WaitEnd::WorkStarted => "work_started",
            WaitEnd::WorkEnded => "work_ended",
            WaitEnd::TaskChanged => "task_changed",
        }
    }
}

/// Whether a waiting `say` should resume early, and why: the user wrote,
/// finished a task, started or ended a work session, or changed a task since
/// it parked. `None` for an event that is not a wait still undecided.
pub fn wait_ended(conn: &Connection, user_id: i64, event_id: i64) -> rusqlite::Result<Option<WaitEnd>> {
    let since: Option<String> = conn
        .query_row(
            "SELECT created_at FROM events WHERE id = ?1 AND origin = ?2 AND status IN ('pending', 'snoozed', 'fired')",
            (event_id, AWAIT_ORIGIN),
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let Some(since) = since else { return Ok(None) };
    if last_user_message(conn, user_id)?.is_some_and(|at| at > since) {
        return Ok(Some(WaitEnd::Reply));
    }
    let any = |sql: &str| conn.query_row(sql, (user_id, &since), |r| r.get::<_, bool>(0));
    let checks = [
        (WaitEnd::TaskDone, "SELECT EXISTS (SELECT 1 FROM tasks WHERE user_id = ?1 AND state = 'done' AND completed_at > ?2)"),
        (WaitEnd::WorkStarted, "SELECT EXISTS (SELECT 1 FROM work_sessions WHERE user_id = ?1 AND started_at > ?2)"),
        (WaitEnd::WorkEnded, "SELECT EXISTS (SELECT 1 FROM work_sessions WHERE user_id = ?1 AND ended_at > ?2)"),
        (WaitEnd::TaskChanged, "SELECT EXISTS (SELECT 1 FROM tasks WHERE user_id = ?1 AND updated_at > ?2)"),
    ];
    for (end, sql) in checks {
        if any(sql)? {
            return Ok(Some(end));
        }
    }
    Ok(None)
}

/// The waiting `say`'s result once the session resumes: what ended the wait,
/// what the user wrote, and what moved in the context since it began.
fn wait_result(ended: Option<WaitEnd>, replies: &[String], changed: &str) -> String {
    let result = match ended {
        None => TIMED_OUT,
        Some(WaitEnd::Reply) => RESPONDED,
        Some(_) => ACTED,
    };
    let mut out = serde_json::json!({ "result": result, "ended_by": ended.map_or("timeout", WaitEnd::as_str) });
    if !replies.is_empty() {
        out["replies"] = replies.into();
    }
    if !changed.is_empty() {
        out["changed"] = changed.into();
    }
    out.to_string()
}

/// The context a wake-up session reads, rendered as of `now`.
fn picture(state: &crate::AppState, user_id: i64, username: &str, now: jiff::Timestamp) -> Option<String> {
    let ucfg = crate::config::UserConfig::load(&state.config_dir, username).ok()?;
    let sources = crate::context::Sources::load(&state.config_dir, username, ucfg);
    let conn = state.db();
    crate::context::assemble_from(&conn, &sources, &state.data_dir, user_id, username, now).ok()
}

/// The messages a parked session resumes from, its wait answered with what
/// ended it, the user's words and what moved in the context; and those words.
fn resumed(
    state: &crate::AppState,
    fired: &crate::runner::FiredEvent,
    ev: &Firing,
    parked: Parked,
    now: jiff::Timestamp,
) -> (Vec<crate::providers::Message>, Vec<String>) {
    let (ended, replies) = {
        let conn = state.db();
        let ended = wait_ended(&conn, fired.user_id, ev.event_id).ok().flatten();
        let replies = written_since(&conn, fired.user_id, ev.created_at.as_deref().unwrap_or_default()).unwrap_or_default();
        (ended, replies)
    };
    let changed = picture(state, fired.user_id, &fired.username, now)
        .map(|fresh| {
            let (was, is) = (not_about(&parked.context, ev.event_id), not_about(&fresh, ev.event_id));
            crate::context::delta(&was, &is, MAX_DELTA_BYTES)
        })
        .unwrap_or_default();
    (parked.paused.answered(&wait_result(ended, &replies, &changed)), replies)
}

/// `context` without the lines about the wait's own event, whose firing is
/// no news to the session it resumes.
fn not_about(context: &str, event_id: i64) -> String {
    let marks = [format!("event_id {event_id})"), format!("event {event_id} "), format!("event {event_id}:")];
    context.lines().filter(|l| !marks.iter().any(|m| l.contains(m.as_str()))).collect::<Vec<_>>().join("\n")
}

/// The tool a trigger session ended on and its result: its last terminal step,
/// else its closing text, taken as `say`.
fn decision(out: &crate::agent::SessionOutcome) -> Option<(String, serde_json::Value)> {
    let ended = out.steps.iter().rev().find(|s| {
        !s.is_error && crate::tools::ends(crate::tools::SessionKind::Trigger, &s.name, &s.result)
    });
    if let Some(step) = ended {
        return Some((step.name.clone(), serde_json::from_str(&step.result).unwrap_or_default()));
    }
    let text = out.reply.trim();
    (!text.is_empty()).then(|| {
        let said = crate::talk::clip(text, MAX_SAY_BYTES);
        ("say".to_string(), serde_json::json!({ "said": said, "notes": [] }))
    })
}

/// The prompts of the triggers folded into this firing, in the order laid.
fn folded_prompts(conn: &Connection, folded: &[i64]) -> Vec<String> {
    folded
        .iter()
        .filter_map(|id| read(conn, *id).ok().flatten())
        .map(|ev| ev.prompt)
        .collect()
}

/// A fired trigger, off the runner's lock: a short session reads the situation
/// and either says something or stays quiet, for it and every trigger folded
/// into it. A session that never gets that far leaves them fired for `reclaim`
/// to try again; its user hears only why, at most once a window.
pub fn fire(state: &crate::AppState, fired: &crate::runner::FiredEvent) {
    let all: Vec<i64> = std::iter::once(fired.event_id).chain(fired.folded.iter().copied()).collect();
    let Ok(permit) = state.talk_gate.try_enter(fired.user_id) else {
        let conn = state.db();
        for id in &all {
            let _ = conn.execute(
                "UPDATE events SET status = 'pending', fired_at = NULL
                 WHERE id = ?1 AND status = 'fired'",
                [id],
            );
        }
        return;
    };
    let _permit = permit;
    let now = jiff::Timestamp::now();
    let settle_all = |status: &str| {
        let conn = state.db();
        for id in &all {
            let _ = close(&conn, *id, status, now);
        }
    };
    if crate::api::background_capped(state, fired.user_id, "wake-up") {
        crate::outage::tell_pending(state, fired.user_id, &fired.username, now);
        for id in std::iter::once(fired.event_id).chain(fired.folded.iter().copied()) {
            fire_capped(state, &crate::runner::FiredEvent { event_id: id, folded: Vec::new(), ..fired.clone() }, now);
        }
        return;
    }
    let tz = timezone(&state.config_dir, &fired.username);
    let l = Lang::for_user(&state.config_dir, &fired.username);
    let (ev, opening, history, thread_note, parked, also) = {
        let conn = state.db();
        let Ok(Some(ev)) = read(&conn, fired.event_id) else {
            return;
        };
        let parked = if ev.origin == AWAIT_ORIGIN {
            parked(&conn, ev.event_id).unwrap_or_else(|e| {
                let detail = format!("event {}: the paused session is unreadable: {e:#}", ev.event_id);
                let _ = crate::log::record(&conn, Some(fired.user_id), "trigger_error", &detail);
                None
            })
        } else {
            None
        };
        let mut opening = situation(&conn, fired.user_id, &ev, &tz, l);
        let also = folded_prompts(&conn, &fired.folded);
        if !also.is_empty() {
            opening.push_str(&mt::also_due(l, &also));
        }
        if fired.channel == crate::channels::RING_CHANNEL {
            opening.push_str(mt::laid_to_ring(l));
        }
        if ev.origin == crate::idle::ORIGIN {
            opening.push_str(&crate::idle::context(&conn, &state.data_dir, fired.user_id, &fired.username, &tz, l, now).unwrap_or_default());
        }
        let (history, note) = thread_context(&conn, ev.conversation_id, l);
        (ev, opening, history, note, parked, also)
    };
    let resumed = parked.map(|p| resumed(state, fired, &ev, p, now));
    let deps = crate::agent::SessionDeps {
        db: &state.db,
        config_dir: &state.config_dir,
        data_dir: &state.data_dir,
        llm: state.llm.as_ref(),
        embeddings: state.embeddings.as_deref(),
        search: state.search.as_deref(),
        task_scope: None,
        inbox_source: None,
        memory_source: None,
        token_id: None,
        thread_note,
        share: None,
        caller: Some(state),
    };
    let call_args = std::cell::RefCell::new(String::new());
    let changed = std::cell::Cell::new(false);
    let log_writes = |event: crate::agent::AgentEvent| match event {
        crate::agent::AgentEvent::ToolCall { args, .. } => *call_args.borrow_mut() = args.to_string(),
        crate::agent::AgentEvent::ToolResult { name, is_error, .. } => {
            let conn = state.db();
            if log_change(&conn, fired.user_id, ev.event_id, name, &call_args.borrow(), is_error).unwrap_or(false) {
                changed.set(true);
            }
        }
        _ => {}
    };
    let outcome = match &resumed {
        Some((messages, replies)) => {
            crate::agent::resume_session(&deps, fired.user_id, &fired.username, now, messages, replies, &log_writes)
        }
        None => crate::agent::run_session_watched(
            &deps,
            fired.user_id,
            &fired.username,
            crate::tools::SessionKind::Trigger,
            now,
            &history,
            &opening,
            &log_writes,
        ),
    };
    if changed.get() {
        state.hub.broadcast_changed(fired.user_id);
    }
    let failed = |detail: String| {
        let conn = state.db();
        let _ = crate::log::record_throttled(
            &conn,
            Some(fired.user_id),
            "trigger_error",
            &detail,
            now,
            crate::log::ERROR_LOG_WINDOW_MINS,
        );
    };
    let out = match outcome {
        Ok(out) => out,
        Err(e) => {
            crate::failure::tell(state, fired.user_id, &fired.username, &crate::failure::Failure::of(&e), now);
            return failed(format!("event {}: {e:#}", ev.event_id));
        }
    };
    let Some((name, result)) = decision(&out) else {
        return failed(format!("event {}: the session never decided", ev.event_id));
    };
    if name == "stay_quiet" {
        settle_all("done");
        let conn = state.db();
        let _ = crate::log::record(
            &conn,
            Some(fired.user_id),
            "trigger_quiet",
            &format!(
                "event {}: {}",
                ev.event_id,
                result["reason"].as_str().unwrap_or("no reason given")
            ),
        );
        drop(conn);
        state.hub.broadcast_changed(fired.user_id);
        return;
    }
    let Some(text) = result["said"].as_str().map(str::to_string) else {
        return failed(format!("event {}: the session said nothing readable", ev.event_id));
    };
    let call = (name == "call_user").then(|| crate::tools::call_ops::Confirmed::from_decided(&result)).flatten();
    let lang = crate::text::Lang::for_user(&state.config_dir, &fired.username);
    let waits = result["await_min"].as_u64().zip(out.paused.as_ref());
    let parked_at = jiff::Timestamp::now();
    let mut parks = None;
    settle_all("done");
    let conversation_id = {
        let conn = state.db();
        let landed = record_said(
            &conn,
            fired,
            lang,
            ev.conversation_id,
            &text,
            now,
        );
        if let (Ok(thread), Some((min, paused))) = (&landed, waits) {
            let min = u32::try_from(min).unwrap_or(*AWAIT_MIN.end());
            match pause(&conn, &state.config_dir, fired, *thread, &text, min, paused, parked_at) {
                Ok(Some(id)) => {
                    parks = Some(id);
                    let _ = crate::log::record(&conn, Some(fired.user_id), "trigger_laid", &format!("event {id}: waits {min} min for an answer"));
                }
                Ok(None) => {
                    let _ = crate::log::record(&conn, Some(fired.user_id), "trigger_quiet", &format!("event {}: the day's waits are spent", ev.event_id));
                }
                Err(e) => {
                    let detail = format!("event {}: pausing: {e:#}", ev.event_id);
                    let _ = crate::log::record_throttled(&conn, Some(fired.user_id), "trigger_error", &detail, now, crate::log::ERROR_LOG_WINDOW_MINS);
                }
            }
        }
        let named: Vec<String> = serde_json::from_value(result["notes"].clone()).unwrap_or_default();
        let _ = crate::notes::nudged(&conn, &state.data_dir, &fired.username, &named, now);
        let _ = crate::log::record(
            &conn,
            Some(fired.user_id),
            "trigger_said",
            &format!("event {}: {text}", ev.event_id),
        );
        landed.ok()
    };
    if let Some(id) = parks {
        let snapshot = picture(state, fired.user_id, &fired.username, parked_at).unwrap_or_default();
        let _ = state.db().execute("UPDATE paused_sessions SET context = ?1 WHERE event_id = ?2", (snapshot, id));
    }
    state.hub.broadcast_changed(fired.user_id);
    let own = ev.origin == crate::idle::ORIGIN || ev.origin == crate::wake::ORIGIN;
    let close_day = is_close_day(&ev.prompt) || also.iter().any(|p| is_close_day(p));
    let mut msg = said_message(fired, &ev, close_day, text, conversation_id, lang);
    msg.checkin = !own;
    if own && !close_day {
        msg.actions.clear();
    }
    if let Some(call) = call {
        msg.reason = Some(call.reason.clone());
        msg.brief = Some(call.brief.clone());
        let outcome = crate::channels::ring(state, fired.user_id, &fired.username, &msg);
        let conn = state.db();
        let _ = crate::log::record(
            &conn,
            Some(fired.user_id),
            "trigger_rang",
            &call.log_line(&format!("event {}", ev.event_id), outcome.as_str()),
        );
    } else if fired.channel == crate::channels::RING_CHANNEL {
        msg.reason = Some(ev.prompt.clone());
        let outcome = crate::channels::calling::ring_if_free(state, fired.user_id, &fired.username, &msg);
        if !matches!(outcome, crate::channels::RingOutcome::Held(_)) {
            let conn = state.db();
            let detail = format!("event {}: {}: laid to ring", ev.event_id, outcome.as_str());
            let _ = crate::log::record(&conn, Some(fired.user_id), "trigger_rang", &detail);
        }
    } else {
        crate::channels::deliver_via(&state.db, &state.channels, fired.user_id, &fired.username, &msg);
    }
}

/// What a trigger's words go out as: the close of the day offers to carry
/// what is left, any other check one tap to ask again later.
fn said_message(
    fired: &crate::runner::FiredEvent,
    ev: &Firing,
    close_day: bool,
    body: String,
    conversation_id: Option<i64>,
    lang: Lang,
) -> crate::channels::OutboundMessage {
    let actions = if close_day {
        vec![crate::channels::Action {
            label: crate::text::action_carry_to_tomorrow(lang),
            data: format!("carry:{}", fired.date),
        }]
    } else {
        crate::channels::trigger_actions(ev.event_id, lang)
    };
    crate::channels::OutboundMessage {
        title: "Note".into(),
        reason: None,
        brief: None,
        body,
        urgency: crate::channels::Urgency::Normal,
        checkin: true,
        event_id: Some(ev.event_id),
        conversation_id,
        actions,
    }
}

/// A wake-up past the day's cap on AI sessions, which cannot think: one the
/// user laid or the server keeps goes out as a plain reminder of what it was
/// laid for, and one Note laid for its own looking (idleness, laying the day,
/// a wait for an answer) settles without a word, its parked session with it.
fn fire_capped(state: &crate::AppState, fired: &crate::runner::FiredEvent, now: jiff::Timestamp) {
    let lang = Lang::for_user(&state.config_dir, &fired.username);
    let conn = state.db();
    let Ok(Some(ev)) = read(&conn, fired.event_id) else {
        return;
    };
    let silent = [crate::idle::ORIGIN, crate::wake::ORIGIN, AWAIT_ORIGIN].contains(&ev.origin.as_str());
    if silent {
        let _ = conn.execute("DELETE FROM paused_sessions WHERE event_id = ?1", [ev.event_id]);
        let _ = conn.execute(
            "UPDATE events SET status = 'dropped', decided_at = ?1 WHERE id = ?2",
            (now.to_string(), ev.event_id),
        );
        let _ = crate::log::record(&conn, Some(fired.user_id), "trigger_quiet", &format!("event {}: the day's AI sessions are spent", ev.event_id));
        return;
    }
    let text = if is_close_day(&ev.prompt) {
        crate::text::capped_close_day(lang)
    } else {
        crate::text::capped_reminder(lang, &ev.prompt)
    };
    let landed = record_said(&conn, fired, lang, ev.conversation_id, &text, now).ok();
    let _ = close(&conn, ev.event_id, "done", now);
    let _ = crate::log::record(&conn, Some(fired.user_id), "trigger_said", &format!("event {}: {text}", ev.event_id));
    drop(conn);
    state.hub.broadcast_changed(fired.user_id);
    let msg = said_message(fired, &ev, is_close_day(&ev.prompt), text, landed, lang);
    crate::channels::deliver_via(&state.db, &state.channels, fired.user_id, &fired.username, &msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> (Connection, tempfile::TempDir, i64) {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("defaults/user.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\ntriggers_per_day = 4\n")
            .unwrap();
        (conn, tmp, uid)
    }

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    fn date() -> jiff::civil::Date {
        "2026-09-17".parse().unwrap()
    }

    fn lay_at<'a>(tmp: &'a tempfile::TempDir, uid: i64, when: &'a str) -> Lay<'a> {
        Lay {
            config_dir: tmp.path(),
            user_id: uid,
            username: "aki",
            at: when,
            prompt: "how did the essay go?",
            date: date(),
            cancel: None,
            conversation_id: None,
            work_session_id: None,
            session_check: false,
            system: false,
            now: at("2026-09-17T09:00:00Z"),
        }
    }

    fn check_at<'a>(tmp: &'a tempfile::TempDir, uid: i64, when: &'a str) -> Lay<'a> {
        Lay { session_check: true, ..lay_at(tmp, uid, when) }
    }

    #[test]
    fn a_system_trigger_stands_outside_the_budget() {
        let (conn, tmp, uid) = env();
        for hour in 10..14 {
            lay(&conn, &lay_at(&tmp, uid, &format!("{hour}:00"))).unwrap();
        }
        assert_eq!(spent(&conn, uid, date()).unwrap(), 4);
        assert!(matches!(
            lay(&conn, &lay_at(&tmp, uid, "15:00")),
            Err(Refusal::CapReached { .. })
        ));

        let system = Lay { system: true, ..lay_at(&tmp, uid, "09:05") };
        let laid = lay(&conn, &system).unwrap();
        let origin: String = conn
            .query_row("SELECT origin FROM events WHERE id = ?1", [laid.event_id], |r| r.get(0))
            .unwrap();
        assert_eq!(origin, "template");
        assert_eq!(spent(&conn, uid, date()).unwrap(), 4, "it costs the day nothing");
    }

    #[test]
    fn a_system_trigger_outlives_the_session_that_happens_to_be_open() {
        let (conn, tmp, uid) = env();
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, planned_min, started_at)
             VALUES (?1, 'essay', 60, '2026-09-17T09:00:00Z')",
            [uid],
        )
        .unwrap();
        let laid = lay(&conn, &Lay { system: true, ..lay_at(&tmp, uid, "21:30") }).unwrap();
        let session: Option<i64> = conn
            .query_row("SELECT work_session_id FROM events WHERE id = ?1", [laid.event_id], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(session.is_none());
    }

    #[test]
    fn the_close_of_the_day_replaces_the_one_already_there() {
        let (conn, tmp, uid) = env();
        let now = at("2026-09-17T09:00:00Z");
        lay_close_day(&conn, tmp.path(), "aki", uid, date(), now).unwrap().unwrap();
        let event_id = lay_close_day(&conn, tmp.path(), "aki", uid, date(), now).unwrap().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, wall_time FROM events WHERE kind = ?1 AND origin = 'template'")
            .unwrap();
        let rows: Vec<(i64, String)> = stmt
            .query_map([KIND], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows, vec![(event_id, crate::config::DEFAULT_CLOSE_DAY_TIME.to_string())]);
    }

    #[test]
    fn a_trigger_lands_on_the_day_and_counts_against_the_allowance() {
        let (conn, tmp, uid) = env();
        assert_eq!(allowance(&conn, tmp.path(), "aki", uid, date()).unwrap(), 4);
        assert_eq!(spent(&conn, uid, date()).unwrap(), 0);

        let laid = lay(&conn, &lay_at(&tmp, uid, "11:00")).unwrap();
        assert_eq!(laid.at, "11:00");
        assert!(laid.cancel_if.is_none());
        assert_eq!(spent(&conn, uid, date()).unwrap(), 1);

        let (kind, flex, alert, origin, prompt): (String, String, i64, String, String) = conn
            .query_row(
                "SELECT kind, flexibility, alert, origin, prompt FROM events WHERE id = ?1",
                [laid.event_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!((kind.as_str(), flex.as_str(), alert, origin.as_str()), (KIND, "drop", 1, "agent"));
        assert_eq!(prompt, "how did the essay go?");

        crate::plan::set_status(&conn, uid, laid.event_id, "dropped").unwrap().unwrap();
        assert_eq!(spent(&conn, uid, date()).unwrap(), 0, "a dropped trigger gives its place back");
    }

    #[test]
    fn a_relative_time_resolves_from_now_and_reaches_no_further_than_a_day() {
        let (conn, tmp, uid) = env();
        let laid = lay(&conn, &lay_at(&tmp, uid, "+45min")).unwrap();
        assert_eq!(laid.at, "09:45");
        // an offset over midnight belongs to the day it lands on
        let laid = lay(&conn, &lay_at(&tmp, uid, "+1000min")).unwrap();
        assert_eq!(laid.at, "01:40");
        let date: String = conn
            .query_row(
                "SELECT p.date FROM plans p JOIN events e ON e.plan_id = p.id WHERE e.id = ?1",
                [laid.event_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(date, "2026-09-18");

        for silly in ["+9223372036854775807min", "+-30min", "+2000min", "+min"] {
            assert!(
                matches!(lay(&conn, &lay_at(&tmp, uid, silly)).unwrap_err(), Refusal::Rejected(_)),
                "{silly} was accepted"
            );
        }
    }

    #[test]
    fn near_finds_the_closest_pending_trigger_within_twenty_minutes() {
        let (conn, tmp, uid) = env();
        let eleven = lay(&conn, &lay_at(&tmp, uid, "11:00")).unwrap().event_id;
        let noon = lay(&conn, &lay_at(&tmp, uid, "11:40")).unwrap().event_id;
        let utc = jiff::tz::TimeZone::UTC;
        let found = |ts: &str| near(&conn, uid, &utc, at(ts)).unwrap().map(|n| n.event_id);
        assert_eq!(found("2026-09-17T11:15:00Z"), Some(eleven));
        assert_eq!(found("2026-09-17T11:25:00Z"), Some(noon));
        assert_eq!(found("2026-09-17T12:01:00Z"), None);
        crate::plan::set_status(&conn, uid, eleven, "dropped").unwrap().unwrap();
        assert_eq!(found("2026-09-17T11:05:00Z"), None, "a dropped one is gone");
        let laid = Lay { date: date().tomorrow().unwrap(), ..lay_at(&tmp, uid, "00:10") };
        assert_eq!(aimed_at(&laid).unwrap(), at("2026-09-18T00:10:00Z"));
    }

    /// A check aimed at a later day while a work session runs belongs to that
    /// day, not to the session.
    #[test]
    fn a_trigger_for_a_later_day_is_not_the_open_sessions() {
        let (conn, tmp, uid) = env();
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, planned_min, started_at)
             VALUES (?1, 'essay', 60, '2026-09-17T08:55:00Z')",
            [uid],
        )
        .unwrap();
        let tomorrow = date().tomorrow().unwrap();
        let laid = lay(&conn, &Lay { date: tomorrow, ..check_at(&tmp, uid, "11:00") }).unwrap();
        let session: Option<i64> = conn
            .query_row("SELECT work_session_id FROM events WHERE id = ?1", [laid.event_id], |r| r.get(0))
            .unwrap();
        assert_eq!(session, None);
        assert_eq!(spent(&conn, uid, tomorrow).unwrap(), 1);
        assert_eq!(spent(&conn, uid, date()).unwrap(), 0);
    }

    #[test]
    fn the_budget_refuses_and_an_extra_reopens_it() {
        let (conn, tmp, uid) = env();
        for hour in 11..15 {
            lay(&conn, &lay_at(&tmp, uid, &format!("{hour}:00"))).unwrap();
        }
        let e = lay(&conn, &lay_at(&tmp, uid, "16:00")).unwrap_err();
        assert!(matches!(e, Refusal::CapReached { allowance: 4, spent: 4, .. }), "got {e:?}");

        assert_eq!(add_extra(&conn, uid, date(), 2).unwrap(), 2);
        assert_eq!(allowance(&conn, tmp.path(), "aki", uid, date()).unwrap(), 6);
        lay(&conn, &lay_at(&tmp, uid, "16:00")).unwrap();
        assert_eq!(add_extra(&conn, uid, date(), 1).unwrap(), 3);
    }

    #[test]
    fn a_time_gone_by_is_refused_and_one_close_is_laid() {
        let (conn, tmp, uid) = env();
        let e = lay(&conn, &lay_at(&tmp, uid, "08:00")).unwrap_err();
        assert!(matches!(e, Refusal::Past { .. }), "got {e:?}");
        lay(&conn, &lay_at(&tmp, uid, "09:01")).unwrap();
        assert_eq!(spent(&conn, uid, date()).unwrap(), 1);
    }

    #[test]
    fn a_foreign_reference_is_not_found_and_lays_nothing() {
        let (conn, tmp, uid) = env();
        let other = crate::auth::create_user(&conn, "bo", "p", false).unwrap();
        let theirs = crate::talk::create(&conn, other, "theirs", at("2026-09-17T08:00:00Z")).unwrap();
        let mut args = lay_at(&tmp, uid, "11:00");
        args.conversation_id = Some(theirs);
        assert!(matches!(lay(&conn, &args).unwrap_err(), Refusal::NotFound(_)));

        let mut args = lay_at(&tmp, uid, "11:00");
        args.cancel = Some(Cancel::TaskDone(404));
        assert!(matches!(lay(&conn, &args).unwrap_err(), Refusal::NotFound(_)));

        let mut args = lay_at(&tmp, uid, "11:00");
        args.cancel = Some(Cancel::EventDecided(404));
        assert!(matches!(lay(&conn, &args).unwrap_err(), Refusal::NotFound(_)));
        assert_eq!(spent(&conn, uid, date()).unwrap(), 0);
    }

    #[test]
    fn a_work_session_check_never_counts_until_past_the_planned_end() {
        let (conn, tmp, uid) = env();
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, planned_min, started_at)
             VALUES (?1, 'read the chapter', 60, '2026-09-17T08:55:00Z')",
            [uid],
        )
        .unwrap();
        // 08:55 + 60 + 15 = 10:10 is the last minute its own checks may reach
        lay(&conn, &check_at(&tmp, uid, "10:10")).unwrap();
        lay(&conn, &check_at(&tmp, uid, "09:30")).unwrap();
        assert_eq!(spent(&conn, uid, date()).unwrap(), 0, "the session's checks are its own");
        lay(&conn, &check_at(&tmp, uid, "10:11")).unwrap();
        assert_eq!(spent(&conn, uid, date()).unwrap(), 1, "past its reach the day pays");
        let held: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE work_session_id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(held, 2);
    }

    #[test]
    fn a_loop_of_immediate_lays_in_a_session_stops_being_free_at_the_cap() {
        let (conn, tmp, uid) = env();
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, started_at)
             VALUES (?1, 'read the chapter', '2026-09-17T08:55:00Z')",
            [uid],
        )
        .unwrap();
        for _ in 0..MAX_FREE_PER_SESSION {
            let laid = lay(&conn, &check_at(&tmp, uid, "+0min")).unwrap();
            conn.execute("UPDATE events SET status = 'done' WHERE id = ?1", [laid.event_id]).unwrap();
        }
        assert_eq!(spent(&conn, uid, date()).unwrap(), 0);
        for n in 1..=4 {
            lay(&conn, &check_at(&tmp, uid, "+0min")).unwrap();
            assert_eq!(spent(&conn, uid, date()).unwrap(), n);
        }
        assert!(matches!(
            lay(&conn, &check_at(&tmp, uid, "+0min")).unwrap_err(),
            Refusal::CapReached { .. }
        ));
    }

    #[test]
    fn a_pause_parks_the_session_until_the_day_holds_enough() {
        let (conn, tmp, uid) = env();
        let checkin = crate::talk::create(&conn, uid, "check-in", at("2026-09-17T08:00:00Z")).unwrap();
        let fired = crate::runner::FiredEvent {
            event_id: 0, user_id: uid, username: "aki".into(), kind: KIND.into(),
            wall_time: "09:00".into(), date: "2026-09-17".into(), channel: "push".into(), ..Default::default()
        };
        let paused = crate::agent::Paused { messages: vec![], call_id: "c1".into(), skipped: vec!["c2".into()] };
        let now = at("2026-09-17T09:00:00Z");
        let id = pause(&conn, tmp.path(), &fired, checkin, "start the essay", 15, &paused, now).unwrap().unwrap();
        let ev = read(&conn, id).unwrap().unwrap();
        assert_eq!((ev.wall_time.as_str(), ev.conversation_id, ev.origin.as_str()), ("09:15", Some(checkin), AWAIT_ORIGIN));
        assert_eq!(spent(&conn, uid, date()).unwrap(), 0, "a wait is off the budget");

        assert_eq!(wait_ended(&conn, uid, id).unwrap(), None);
        let other = crate::talk::create(&conn, uid, "chat", now).unwrap();
        crate::talk::append_text(&conn, other, "user", "on it", at("2026-09-17T09:05:00Z")).unwrap();
        assert_eq!(wait_ended(&conn, uid, id).unwrap(), Some(WaitEnd::Reply), "an answer in any thread ends the wait");
        assert_eq!(written_since(&conn, uid, ev.created_at.as_deref().unwrap()).unwrap(), ["on it"]);

        let back = parked(&conn, id).unwrap().unwrap();
        assert!(parked(&conn, id).unwrap().is_some(), "it stays parked until the resumed session decides");
        let messages = back.paused.answered(&wait_result(None, &[], ""));
        assert!(matches!(&messages[0], crate::providers::Message::ToolResult { call_id, content, is_error: false }
            if call_id == "c1" && content.contains("Timed out")));
        assert!(matches!(&messages[1], crate::providers::Message::ToolResult { call_id, is_error: true, .. } if call_id == "c2"));
        close(&conn, id, "done", now).unwrap();
        assert!(parked(&conn, id).unwrap().is_none(), "a session resumes once");

        for _ in 1..MAX_AWAITS_PER_DAY {
            pause(&conn, tmp.path(), &fired, checkin, "x", 15, &paused, now).unwrap().unwrap();
        }
        assert_eq!(pause(&conn, tmp.path(), &fired, checkin, "x", 15, &paused, now).unwrap(), None);
    }

    fn waiting_for(conn: &Connection, tmp: &tempfile::TempDir, uid: i64) -> i64 {
        let checkin = crate::talk::create(conn, uid, "check-in", at("2026-09-17T08:00:00Z")).unwrap();
        let fired = crate::runner::FiredEvent { user_id: uid, username: "aki".into(), kind: KIND.into(), ..Default::default() };
        let paused = crate::agent::Paused { messages: vec![], call_id: "c1".into(), skipped: vec![] };
        pause(conn, tmp.path(), &fired, checkin, "start the essay", 15, &paused, at("2026-09-17T09:00:00Z")).unwrap().unwrap()
    }

    #[test]
    fn the_users_own_move_ends_a_wait_and_says_which() {
        let (conn, tmp, uid) = env();
        let task = crate::tasks::create(&conn, uid, crate::tasks::NewTask { title: "essay".into(), ..Default::default() },
            "manual", crate::tasks::Actor::User).unwrap().id;
        conn.execute("UPDATE tasks SET created_at = '2026-09-17T08:00:00Z', updated_at = '2026-09-17T08:00:00Z'", []).unwrap();
        let id = waiting_for(&conn, &tmp, uid);
        assert_eq!(wait_ended(&conn, uid, id).unwrap(), None, "what the session itself did before parking is not news");

        conn.execute("UPDATE tasks SET updated_at = '2026-09-17T09:03:00Z' WHERE id = ?1", [task]).unwrap();
        assert_eq!(wait_ended(&conn, uid, id).unwrap(), Some(WaitEnd::TaskChanged));
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, started_at) VALUES (?1, 'essay', '2026-09-17T09:04:00Z')",
            [uid],
        )
        .unwrap();
        assert_eq!(wait_ended(&conn, uid, id).unwrap(), Some(WaitEnd::WorkStarted));
        conn.execute("UPDATE tasks SET state = 'done', completed_at = '2026-09-17T09:05:00Z' WHERE id = ?1", [task]).unwrap();
        assert_eq!(wait_ended(&conn, uid, id).unwrap(), Some(WaitEnd::TaskDone), "a finished task outranks the rest");

        let result: serde_json::Value =
            serde_json::from_str(&wait_result(Some(WaitEnd::TaskDone), &[], "# Tasks\n+ - essay (done)\n")).unwrap();
        assert_eq!(
            result,
            serde_json::json!({ "result": ACTED, "ended_by": "task_done", "changed": "# Tasks\n+ - essay (done)\n" })
        );
    }

    fn fired_trigger(conn: &Connection, tmp: &tempfile::TempDir, uid: i64, fired_at: &str) -> i64 {
        let plan_id = crate::plan::ensure(conn, tmp.path(), "aki", uid, date()).unwrap();
        let id = insert(conn, plan_id, "09:00", "see how it went", "agent", None, None, None, at("2026-09-17T08:00:00Z")).unwrap();
        conn.execute("UPDATE events SET status = 'fired', fired_at = ?1 WHERE id = ?2", (fired_at, id)).unwrap();
        id
    }

    fn status_of(conn: &Connection, id: i64) -> (String, i64) {
        conn.query_row("SELECT status, attempts FROM events WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
    }

    #[test]
    fn an_undecided_trigger_is_tried_again_until_its_tries_run_out() {
        let (conn, tmp, uid) = env();
        let id = fired_trigger(&conn, &tmp, uid, "2026-09-17T09:00:00Z");
        let after = jiff::SignedDuration::from_mins(RETRY_AFTER_MIN);
        let none = std::collections::HashSet::new();

        reclaim(&conn, at("2026-09-17T09:05:00Z"), after, &none).unwrap();
        assert_eq!(status_of(&conn, id), ("fired".into(), 0), "too soon: it may still be running");
        reclaim(&conn, at("2026-09-17T09:30:00Z"), after, &[uid].into()).unwrap();
        assert_eq!(status_of(&conn, id), ("fired".into(), 0), "its user's wake-up is still running");

        for tries in 1..MAX_ATTEMPTS {
            reclaim(&conn, at("2026-09-17T09:30:00Z"), after, &none).unwrap();
            assert_eq!(status_of(&conn, id), ("pending".into(), tries));
            conn.execute("UPDATE events SET status = 'fired', fired_at = '2026-09-17T09:10:00Z' WHERE id = ?1", [id]).unwrap();
        }
        reclaim(&conn, at("2026-09-17T09:30:00Z"), after, &none).unwrap();
        assert_eq!(status_of(&conn, id).0, "dropped");
        let given_up: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind = 'trigger_given_up'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(given_up, 1);
    }

    #[test]
    fn at_boot_every_fired_trigger_is_taken_back_and_a_parked_session_kept() {
        let (conn, tmp, uid) = env();
        let id = waiting_for(&conn, &tmp, uid);
        conn.execute("UPDATE events SET status = 'fired', fired_at = '2026-09-17T09:15:00Z' WHERE id = ?1", [id]).unwrap();
        reclaim(&conn, at("2026-09-17T09:15:00Z"), jiff::SignedDuration::ZERO, &std::collections::HashSet::new()).unwrap();
        assert_eq!(status_of(&conn, id), ("pending".into(), 1));
        assert!(parked(&conn, id).unwrap().is_some(), "the resume is tried again, not lost");
    }

    #[test]
    fn a_reply_after_the_trigger_was_laid_cancels_it() {
        let (conn, tmp, uid) = env();
        let thread = crate::talk::create(&conn, uid, "chat", at("2026-09-17T08:00:00Z")).unwrap();
        crate::talk::append_text(&conn, thread, "user", "before", at("2026-09-17T08:30:00Z"))
            .unwrap();
        let mut args = lay_at(&tmp, uid, "11:00");
        args.cancel = Some(Cancel::Replied);
        args.conversation_id = Some(thread);
        let laid = lay(&conn, &args).unwrap();
        let ev = read(&conn, laid.event_id).unwrap().unwrap();
        assert!(!cancelled(&conn, uid, &ev).unwrap(), "an older message is not a reply");

        crate::talk::append_text(&conn, thread, "user", "after", at("2026-09-17T09:30:00Z"))
            .unwrap();
        assert!(cancelled(&conn, uid, &ev).unwrap());
        cancel(&conn, uid, &ev, at("2026-09-17T11:00:00Z")).unwrap();
        let status: String = conn
            .query_row("SELECT status FROM events WHERE id = ?1", [laid.event_id], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "dropped");
    }

    #[test]
    fn a_finished_task_or_settled_event_cancels_the_wait() {
        let (conn, tmp, uid) = env();
        let task = crate::tasks::create(
            &conn,
            uid,
            crate::tasks::NewTask { title: "essay".into(), ..Default::default() },
            "manual",
            crate::tasks::Actor::User,
        )
        .unwrap()
        .id;
        let mut args = lay_at(&tmp, uid, "11:00");
        args.cancel = Some(Cancel::TaskDone(task));
        let laid = lay(&conn, &args).unwrap();
        let ev = read(&conn, laid.event_id).unwrap().unwrap();
        assert!(!cancelled(&conn, uid, &ev).unwrap());
        conn.execute("UPDATE tasks SET state = 'done' WHERE id = ?1", [task]).unwrap();
        assert!(cancelled(&conn, uid, &ev).unwrap());

        let mut args = lay_at(&tmp, uid, "12:00");
        args.cancel = Some(Cancel::EventDecided(laid.event_id));
        let second = lay(&conn, &args).unwrap();
        let ev = read(&conn, second.event_id).unwrap().unwrap();
        assert!(!cancelled(&conn, uid, &ev).unwrap());
        crate::plan::set_status(&conn, uid, laid.event_id, "done").unwrap().unwrap();
        assert!(cancelled(&conn, uid, &ev).unwrap());
    }

    #[test]
    fn the_situation_names_the_work_session_and_the_last_word_from_the_user() {
        let (conn, tmp, uid) = env();
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, planned_min, started_at)
             VALUES (?1, 'read the chapter', 60, '2026-09-17T08:55:00Z')",
            [uid],
        )
        .unwrap();
        let thread = crate::talk::create(&conn, uid, "chat", at("2026-09-17T08:00:00Z")).unwrap();
        crate::talk::append_text(&conn, thread, "user", "on it", at("2026-09-17T08:40:00Z"))
            .unwrap();
        let laid = lay(&conn, &check_at(&tmp, uid, "09:30")).unwrap();
        let ev = read(&conn, laid.event_id).unwrap().unwrap();
        let text = situation(&conn, uid, &ev, &jiff::tz::TimeZone::UTC, Lang::En);
        assert!(text.starts_with("how did the essay go?"), "{text}");
        assert!(text.contains("Laid at 09:00, meant for 09:30."), "{text}");
        assert!(text.contains("\"read the chapter\", started 08:55, planned 60 min"), "{text}");
        assert!(text.contains("0 steps done since the last check"), "{text}");
        assert!(text.contains("Last message from the user: 08:40."), "{text}");
    }

    #[test]
    fn any_wakeup_sees_the_open_session_and_its_pause() {
        let (conn, tmp, uid) = env();
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, started_at, paused_at)
             VALUES (?1, 'read the chapter', '2026-09-17T08:00:00Z', '2026-09-17T08:20:00Z')",
            [uid],
        )
        .unwrap();
        let laid = lay(&conn, &lay_at(&tmp, uid, "09:30")).unwrap();
        let ev = read(&conn, laid.event_id).unwrap().unwrap();
        assert_eq!(ev.work_session_id, None);
        let text = situation(&conn, uid, &ev, &jiff::tz::TimeZone::UTC, Lang::En);
        assert!(text.contains("\"read the chapter\", started 08:00, paused since 08:20"), "{text}");
    }

    #[test]
    fn a_session_check_needs_an_open_session() {
        let (conn, tmp, uid) = env();
        let e = lay(&conn, &check_at(&tmp, uid, "09:30")).unwrap_err();
        assert!(matches!(&e, Refusal::Rejected(m) if m.contains("no work session")), "{e:?}");
    }

    #[test]
    fn the_model_holds_at_most_max_pending_triggers_across_days() {
        let (conn, tmp, uid) = env();
        let far: jiff::civil::Date = "2027-03-01".parse().unwrap();
        let plan_id = crate::plan::ensure(&conn, tmp.path(), "aki", uid, far).unwrap();
        for _ in 0..MAX_PENDING {
            insert(&conn, plan_id, "09:00", "later", "agent", None, None, None, at("2026-09-17T09:00:00Z"))
                .unwrap();
        }
        let e = lay(&conn, &lay_at(&tmp, uid, "11:00")).unwrap_err();
        assert!(matches!(&e, Refusal::Rejected(m) if m.contains("drop some")), "{e:?}");
        let mut system = lay_at(&tmp, uid, "11:00");
        system.system = true;
        lay(&conn, &system).unwrap();
        conn.execute("UPDATE events SET status = 'dropped' WHERE id = 1", []).unwrap();
        lay(&conn, &lay_at(&tmp, uid, "11:00")).unwrap();
    }

    #[test]
    fn an_idle_trigger_spends_the_days_budget() {
        let (conn, tmp, uid) = env();
        let plan_id = crate::plan::ensure(&conn, tmp.path(), "aki", uid, date()).unwrap();
        insert(&conn, plan_id, "09:00", "idle", "idle", Some(Cancel::Active), None, None,
            at("2026-09-17T09:00:00Z")).unwrap();
        assert_eq!(spent(&conn, uid, date()).unwrap(), 1);
    }

    #[test]
    fn activity_after_an_idle_trigger_was_laid_cancels_it() {
        let (conn, tmp, uid) = env();
        let plan_id = crate::plan::ensure(&conn, tmp.path(), "aki", uid, date()).unwrap();
        let id = insert(&conn, plan_id, "09:00", "idle", "idle", Some(Cancel::Active), None, None,
            at("2026-09-17T09:00:00.400Z")).unwrap();
        let ev = read(&conn, id).unwrap().unwrap();
        assert_eq!(ev.origin, "idle");
        assert_eq!(ev.cancel_if.as_deref(), Some("active"));

        conn.execute("UPDATE users SET last_active_at = '2026-09-17T08:30:00Z' WHERE id = ?1", [uid])
            .unwrap();
        assert!(!cancelled(&conn, uid, &ev).unwrap(), "the quiet before it is what laid it");

        crate::presence::touch(&conn, uid, at("2026-09-17T09:00:00.900Z")).unwrap();
        assert!(cancelled(&conn, uid, &ev).unwrap(), "a stamp in the same second still counts");
    }

    #[test]
    fn a_check_past_the_sessions_end_is_laid_for_the_day() {
        let (conn, tmp, uid) = env();
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, planned_min, started_at)
             VALUES (?1, 'read the chapter', 60, '2026-09-17T08:55:00Z')",
            [uid],
        )
        .unwrap();
        let session_of = |id: i64| -> Option<i64> {
            conn.query_row("SELECT work_session_id FROM events WHERE id = ?1", [id], |r| r.get(0)).unwrap()
        };
        let later = lay(&conn, &check_at(&tmp, uid, "15:00")).unwrap();
        assert_eq!(session_of(later.event_id), None);
        assert_eq!(spent(&conn, uid, date()).unwrap(), 1, "past the session it spends the day's budget");
        let inside = lay(&conn, &check_at(&tmp, uid, "09:30")).unwrap();
        assert_eq!(session_of(inside.event_id), Some(1), "inside the session it is the session's");
    }

    #[test]
    fn a_trigger_months_ahead_is_laid_on_its_own_day() {
        let (conn, tmp, uid) = env();
        let far: jiff::civil::Date = "2027-03-02".parse().unwrap();
        let laid = lay(&conn, &Lay { date: far, ..lay_at(&tmp, uid, "09:15") }).unwrap();
        let day: String = conn
            .query_row(
                "SELECT p.date FROM plans p JOIN events e ON e.plan_id = p.id WHERE e.id = ?1",
                [laid.event_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(day, "2027-03-02");
        assert_eq!(spent(&conn, uid, far).unwrap(), 1);
    }

    fn outcome(reply: &str, steps: Vec<crate::agent::SessionStep>) -> crate::agent::SessionOutcome {
        crate::agent::SessionOutcome {
            reply: reply.into(),
            turns: 1,
            tool_calls: steps.len(),
            steps,
            reasoning: String::new(),
            thought_ms: 0,
            paused: None,
        }
    }

    fn step(name: &str, result: &str) -> crate::agent::SessionStep {
        crate::agent::SessionStep { name: name.into(), args: "{}".into(), result: result.into(), is_error: false, thinking: None }
    }

    #[test]
    fn a_session_ending_in_plain_text_says_it() {
        let (name, result) = decision(&outcome("  Back on the essay?  ", vec![step("task_list", "{}")])).unwrap();
        assert_eq!((name.as_str(), result["said"].as_str()), ("say", Some("Back on the essay?")));
        let (_, long) = decision(&outcome(&"あ".repeat(1000), Vec::new())).unwrap();
        assert!(long["said"].as_str().unwrap().len() <= MAX_SAY_BYTES);
        let (name, _) = decision(&outcome("x", vec![step("stay_quiet", r#"{"quiet":true,"reason":"r"}"#)])).unwrap();
        assert_eq!(name, "stay_quiet", "a terminal step outranks the text");
        assert!(decision(&outcome("  ", vec![step("task_list", "{}")])).is_none());
    }

    #[test]
    fn every_change_a_wake_up_made_is_logged() {
        let (conn, _tmp, uid) = env();
        let step = |name: &str, is_error: bool| crate::agent::SessionStep {
            name: name.into(),
            args: r#"{"task_id":3}"#.into(),
            result: "{}".into(),
            is_error,
            thinking: None,
        };
        let steps = [step("task_read", false), step("order_move", false), step("task_update", true), step("stay_quiet", false)];
        let logged = steps
            .iter()
            .filter(|s| log_change(&conn, uid, 9, &s.name, &s.args, s.is_error).unwrap())
            .count();
        assert_eq!(logged, 1);
        let detail: String = conn
            .query_row("SELECT detail FROM event_log WHERE kind = 'wake_change'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(detail, r#"event 9: order_move {"task_id":3}"#);
    }
}
