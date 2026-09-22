use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

/// The event kind a trigger point carries on the day's plan.
pub const KIND: &str = "trigger";
/// How far ahead a trigger must be laid, so it is never a nudge about now.
pub const MIN_LEAD_MIN: i64 = 10;
/// How far past a work session's planned end its own checks may still reach.
pub const OVERRUN_MIN: i64 = 15;
/// The furthest ahead a `+Nmin` offset may reach: a trigger point is about the
/// day it is laid in, not next week.
pub const MAX_OFFSET_MIN: i64 = 24 * 60;
pub const MAX_PROMPT_BYTES: usize = 1000;
pub const MAX_SAY_BYTES: usize = 1200;
/// How much of the thread a firing trigger reads.
pub const HISTORY_TURNS: usize = 12;
pub const MAX_EXTRA: u32 = 10;

/// Why a trigger was not laid. `CapReached` names the way out, because the
/// model's next move is to ask the user rather than to try again.
#[derive(Debug)]
pub enum Refusal {
    CapReached { allowance: u32, spent: u32 },
    TooSoon { minutes: i64 },
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
}

impl Cancel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Cancel::Replied => "replied",
            Cancel::TaskDone(_) => "task_done",
            Cancel::EventDecided(_) => "event_decided",
        }
    }

    fn reference(&self) -> Option<i64> {
        match self {
            Cancel::Replied => None,
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
}

/// The user's session in progress, if one is open. A trigger laid while one
/// runs belongs to it: accountability is the point, so those checks are
/// uncapped and end with the session.
pub fn open_work_session(conn: &Connection, user_id: i64) -> rusqlite::Result<Option<WorkSession>> {
    conn.query_row(
        "SELECT id, title, planned_min, started_at FROM work_sessions
         WHERE user_id = ?1 AND ended_at IS NULL ORDER BY id DESC LIMIT 1",
        [user_id],
        |r| {
            Ok(WorkSession {
                id: r.get(0)?,
                title: r.get(1)?,
                planned_min: r.get(2)?,
                started_at: r.get(3)?,
            })
        },
    )
    .optional()
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

fn allowance_of(config_dir: &Path, username: &str) -> u32 {
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
    let extra: i64 = conn
        .query_row(
            "SELECT extra FROM trigger_budgets WHERE user_id = ?1 AND date = ?2",
            (user_id, date.to_string()),
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    Ok(allowance_of(config_dir, username).saturating_add(extra.max(0) as u32))
}

/// Triggers the agent laid for that day out of its own budget: a check inside a
/// work session is the session's, not the day's, and a dropped one gives its
/// place back.
pub fn spent(conn: &Connection, user_id: i64, date: jiff::civil::Date) -> rusqlite::Result<u32> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND p.date = ?2 AND e.kind = ?3
           AND e.origin = 'agent' AND e.work_session_id IS NULL AND e.status != 'dropped'",
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
    /// that is already closed. It carries the session's exemptions: no lead
    /// time, and no call on the day's budget.
    pub work_session_id: Option<i64>,
    /// A check the server lays for a ritual of its own rather than one the
    /// agent chose: it is the day's furniture, so it is exempt from the budget
    /// and from the lead time, and never belongs to an open session.
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

/// Lays one trigger point, refusing on the day's budget, on a time already gone
/// and on a time too close to be anything but a nudge about now. A trigger laid
/// while a work session runs belongs to that session and never counts.
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

    let session = open_work_session(conn, lay.user_id).map_err(internal)?;
    let lead = lead_minutes(date, target, &local);
    if lead < 0 {
        return Err(Refusal::Past { at: wall });
    }
    if lay.work_session_id.is_none() && !lay.system && lead < MIN_LEAD_MIN {
        return Err(Refusal::TooSoon { minutes: lead });
    }
    match (&lay.work_session_id, &session) {
        (Some(_), _) => {}
        _ if lay.system => {}
        (None, Some(s)) => {
            if let Some(limit) = overrun_lead(s, lay.now) {
                if lead > limit {
                    return Err(Refusal::Rejected(format!(
                        "{wall} is more than {OVERRUN_MIN} min past the end of the work session \
                         on {:?}",
                        s.title
                    )));
                }
            }
        }
        (None, None) => {
            let allowance = allowance(conn, lay.config_dir, lay.username, lay.user_id, date)
                .map_err(internal)?;
            let spent = spent(conn, lay.user_id, date).map_err(internal)?;
            if spent >= allowance {
                return Err(Refusal::CapReached { allowance, spent });
            }
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

    let plan_id = crate::plan::ensure(conn, lay.config_dir, lay.username, lay.user_id, date)
        .map_err(internal)?;
    let event_id = insert(
        conn,
        plan_id,
        &wall,
        prompt,
        if lay.system { "template" } else { "agent" },
        lay.cancel,
        lay.conversation_id,
        match lay.system {
            true => None,
            false => lay.work_session_id.or_else(|| session.as_ref().map(|s| s.id)),
        },
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

/// What the close-the-day check asks for; the tool it names is how the day's
/// leftovers actually move.
pub const CLOSE_DAY_PROMPT: &str = "It is the close of the day. In one or two lines say what is \
     still pending and what got done, then ask whether to carry the rest to tomorrow. If they say \
     yes, call plan_carry.";

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
            prompt: CLOSE_DAY_PROMPT,
            date,
            cancel: None,
            conversation_id: None,
            work_session_id: None,
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
}

pub fn read(conn: &Connection, event_id: i64) -> rusqlite::Result<Option<Firing>> {
    conn.query_row(
        "SELECT id, prompt, cancel_if, cancel_ref, conversation_id, work_session_id,
                created_at, wall_time
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
            })
        },
    )
    .optional()
}

/// Whether the reason this trigger was laid has already taken care of itself:
/// the user replied in the thread, finished or dropped the task, or settled the
/// event it was waiting on.
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
            Ok(matches!(state.as_deref(), Some("done") | Some("dropped")))
        }
        "event_decided" => {
            let Some(event_id) = ev.cancel_ref else { return Ok(false) };
            let status = crate::plan::event_gate(conn, user_id, event_id)
                .ok()
                .flatten()
                .map(|(_, status)| status);
            Ok(matches!(status.as_deref(), Some("done") | Some("dropped")))
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
/// was laid, when it was meant for, the work session it belongs to, and how
/// long since the user last said anything.
pub fn situation(
    conn: &Connection,
    user_id: i64,
    ev: &Firing,
    tz: &jiff::tz::TimeZone,
) -> String {
    let clock = |ts: &str| -> String {
        ts.parse::<jiff::Timestamp>()
            .map(|t| t.to_zoned(tz.clone()).strftime("%H:%M").to_string())
            .unwrap_or_else(|_| ts.to_string())
    };
    let mut s = format!("{}\n\n", ev.prompt);
    match ev.created_at.as_deref() {
        Some(at) => s.push_str(&format!("Laid at {}, meant for {}.\n", clock(at), ev.wall_time)),
        None => s.push_str(&format!("Meant for {}.\n", ev.wall_time)),
    }
    if let Some(id) = ev.work_session_id {
        let row: Option<(String, Option<i64>, String, String, String, i64)> = conn
            .query_row(
                "SELECT title, planned_min, started_at, mode, phase, round
                 FROM work_sessions WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .optional()
            .ok()
            .flatten();
        if let Some((title, planned, started, mode, phase, round)) = row {
            s.push_str(&format!("Work session: {title:?}, started {}", clock(&started)));
            if let Some(min) = planned {
                s.push_str(&format!(", planned {min} min"));
            }
            if mode == "pomodoro" {
                s.push_str(&format!(", in the {phase} of round {round}"));
            }
            let done = steps_done_since(conn, user_id, id, &started).unwrap_or(0);
            s.push_str(&format!(", {done} step{} done since the last check.\n",
                if done == 1 { "" } else { "s" }));
        }
    }
    match last_user_message(conn, user_id).ok().flatten() {
        Some(at) => s.push_str(&format!("Last message from the user: {}.\n", clock(&at))),
        None => s.push_str("The user has not written anything yet.\n"),
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
) -> (Vec<crate::providers::Message>, Option<String>) {
    let Some(id) = conversation_id else {
        return (Vec::new(), None);
    };
    let history = crate::talk::history(conn, id, HISTORY_TURNS).unwrap_or_default();
    let note = crate::talk::summary(conn, id)
        .ok()
        .flatten()
        .map(|(summary, _)| crate::talk::summary_thread_note(&summary));
    (history, note)
}

/// Where a trigger's words land: the thread it was laid against, else the day's
/// check-in thread. The text is written there whether or not a channel takes it.
fn record_said(
    conn: &Connection,
    user_id: i64,
    date: &str,
    at: &str,
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
        None => crate::talk::checkin_thread(conn, user_id, date, at, text, now),
    }
}

fn settle(conn: &Connection, event_id: i64, now: jiff::Timestamp) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE events SET status = 'done', decided_at = ?1 WHERE id = ?2",
        (now.to_string(), event_id),
    )
}

/// A fired trigger, off the runner's lock: a short session reads the situation
/// and either says something or stays quiet. A session that never gets that far
/// leaves the event alone and sends nothing — silence is the safe failure.
pub fn fire(state: &crate::AppState, fired: &crate::runner::FiredEvent) {
    let Ok(permit) = state.talk_gate.try_enter(fired.user_id) else {
        let conn = state.db();
        let _ = conn.execute(
            "UPDATE events SET status = 'pending', fired_at = NULL
             WHERE id = ?1 AND status = 'fired'",
            [fired.event_id],
        );
        return;
    };
    let _permit = permit;
    let now = jiff::Timestamp::now();
    let tz = timezone(&state.config_dir, &fired.username);
    let (ev, opening, history, thread_note) = {
        let conn = state.db();
        let Ok(Some(ev)) = read(&conn, fired.event_id) else {
            return;
        };
        let opening = situation(&conn, fired.user_id, &ev, &tz);
        let (history, note) = thread_context(&conn, ev.conversation_id);
        (ev, opening, history, note)
    };
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
    };
    let outcome = crate::agent::run_session(
        &deps,
        fired.user_id,
        &fired.username,
        crate::tools::SessionKind::Trigger,
        now,
        &history,
        &opening,
    );
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
        Err(e) => return failed(format!("event {}: {e:#}", ev.event_id)),
    };
    let Some(step) = out.steps.iter().rev().find(|s| {
        !s.is_error && crate::tools::is_terminal(crate::tools::SessionKind::Trigger, &s.name)
    }) else {
        return failed(format!("event {}: the session never decided", ev.event_id));
    };
    let result: serde_json::Value = serde_json::from_str(&step.result).unwrap_or_default();
    if step.name == "stay_quiet" {
        let conn = state.db();
        let _ = settle(&conn, ev.event_id, now);
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
    let conversation_id = {
        let conn = state.db();
        let landed = record_said(
            &conn,
            fired.user_id,
            &fired.date,
            &fired.wall_time,
            ev.conversation_id,
            &text,
            now,
        );
        let _ = settle(&conn, ev.event_id, now);
        let _ = crate::log::record(
            &conn,
            Some(fired.user_id),
            "trigger_said",
            &format!("event {}: {text}", ev.event_id),
        );
        landed.ok()
    };
    state.hub.broadcast_changed(fired.user_id);
    crate::channels::deliver_via(
        &state.db,
        &state.channels,
        fired.user_id,
        &fired.username,
        &crate::channels::OutboundMessage {
            title: "Note".into(),
            body: text,
            urgency: crate::channels::Urgency::Normal,
            event_id: Some(ev.event_id),
            conversation_id,
            actions: if ev.prompt == CLOSE_DAY_PROMPT {
                vec![crate::channels::Action {
                    label: "Carry to tomorrow".into(),
                    data: format!("carry:{}", fired.date),
                }]
            } else {
                crate::channels::event_actions(ev.event_id)
            },
        },
    );
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
        std::fs::write(p, "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n")
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
            system: false,
            now: at("2026-09-17T09:00:00Z"),
        }
    }

    #[test]
    fn a_system_trigger_stands_outside_the_budget_and_the_lead_time() {
        let (conn, tmp, uid) = env();
        for hour in 10..14 {
            lay(&conn, &lay_at(&tmp, uid, &format!("{hour}:00"))).unwrap();
        }
        assert_eq!(spent(&conn, uid, date()).unwrap(), 4);
        assert!(matches!(
            lay(&conn, &lay_at(&tmp, uid, "15:00")),
            Err(Refusal::CapReached { .. })
        ));
        assert!(matches!(
            lay(&conn, &lay_at(&tmp, uid, "09:05")),
            Err(Refusal::TooSoon { .. })
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
    fn the_budget_refuses_and_an_extra_reopens_it() {
        let (conn, tmp, uid) = env();
        for hour in 11..15 {
            lay(&conn, &lay_at(&tmp, uid, &format!("{hour}:00"))).unwrap();
        }
        let e = lay(&conn, &lay_at(&tmp, uid, "16:00")).unwrap_err();
        assert!(matches!(e, Refusal::CapReached { allowance: 4, spent: 4 }), "got {e:?}");

        assert_eq!(add_extra(&conn, uid, date(), 2).unwrap(), 2);
        assert_eq!(allowance(&conn, tmp.path(), "aki", uid, date()).unwrap(), 6);
        lay(&conn, &lay_at(&tmp, uid, "16:00")).unwrap();
        assert_eq!(add_extra(&conn, uid, date(), 1).unwrap(), 3);
    }

    #[test]
    fn a_time_gone_by_or_too_close_is_refused() {
        let (conn, tmp, uid) = env();
        let e = lay(&conn, &lay_at(&tmp, uid, "08:00")).unwrap_err();
        assert!(matches!(e, Refusal::Past { .. }), "got {e:?}");
        let e = lay(&conn, &lay_at(&tmp, uid, "09:05")).unwrap_err();
        assert!(matches!(e, Refusal::TooSoon { minutes: 5 }), "got {e:?}");
        lay(&conn, &lay_at(&tmp, uid, "09:10")).unwrap();
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
    fn a_work_session_check_never_counts_and_stops_at_the_planned_end() {
        let (conn, tmp, uid) = env();
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, planned_min, started_at)
             VALUES (?1, 'read the chapter', 60, '2026-09-17T08:55:00Z')",
            [uid],
        )
        .unwrap();
        for hour in 11..16 {
            lay(&conn, &lay_at(&tmp, uid, &format!("{hour}:00"))).unwrap_err();
        }
        // 08:55 + 60 + 15 = 10:10 is the last minute its own checks may reach
        lay(&conn, &lay_at(&tmp, uid, "10:11")).unwrap_err();
        lay(&conn, &lay_at(&tmp, uid, "10:10")).unwrap();
        lay(&conn, &lay_at(&tmp, uid, "09:30")).unwrap();
        assert_eq!(spent(&conn, uid, date()).unwrap(), 0, "the session's checks are its own");
        let held: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE work_session_id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(held, 2);
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
        let laid = lay(&conn, &lay_at(&tmp, uid, "09:30")).unwrap();
        let ev = read(&conn, laid.event_id).unwrap().unwrap();
        let text = situation(&conn, uid, &ev, &jiff::tz::TimeZone::UTC);
        assert!(text.starts_with("how did the essay go?"), "{text}");
        assert!(text.contains("Laid at 09:00, meant for 09:30."), "{text}");
        assert!(text.contains("\"read the chapter\", started 08:55, planned 60 min"), "{text}");
        assert!(text.contains("0 steps done since the last check"), "{text}");
        assert!(text.contains("Last message from the user: 08:40."), "{text}");
    }
}
