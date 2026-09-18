use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;

/// How long after a session starts its first progress check lands, when the
/// session named no length.
pub const DEFAULT_FIRST_CHECK_MIN: i64 = 25;
pub const MAX_TITLE_BYTES: usize = 200;
pub const MAX_PLANNED_MIN: i64 = 12 * 60;
pub const MAX_NOTES_BYTES: usize = 4000;
pub const MAX_STEP_COUNT: i64 = 50;
/// How many work rounds a pomodoro session runs before it stops on its own.
pub const MAX_ROUNDS: i64 = 16;

const COLS: &str = "id, task_id, event_id, title, planned_min, started_at, paused_at, \
                    paused_ms, mode, work_min, break_min, phase, phase_started_at, \
                    phase_paused_ms, round, step_index, step_count, step_name, notes, \
                    conversation_id";

const COLS_W: &str = "w.id, w.task_id, w.event_id, w.title, w.planned_min, w.started_at, \
                      w.paused_at, w.paused_ms, w.mode, w.work_min, w.break_min, w.phase, \
                      w.phase_started_at, w.phase_paused_ms, w.round, w.step_index, \
                      w.step_count, w.step_name, w.notes, w.conversation_id";

#[derive(Debug, Serialize)]
pub struct Session {
    pub id: i64,
    pub task_id: Option<i64>,
    pub event_id: Option<i64>,
    pub title: String,
    pub planned_min: Option<i64>,
    pub started_at: String,
    pub paused_at: Option<String>,
    pub paused_ms: i64,
    pub mode: String,
    pub work_min: Option<i64>,
    pub break_min: Option<i64>,
    pub phase: String,
    pub phase_started_at: String,
    pub phase_paused_ms: i64,
    pub round: i64,
    pub step_index: Option<i64>,
    pub step_count: Option<i64>,
    pub step_name: Option<String>,
    pub notes: String,
    pub conversation_id: Option<i64>,
}

fn row_to_session(r: &rusqlite::Row) -> rusqlite::Result<Session> {
    let started_at: String = r.get(5)?;
    let phase_started_at: Option<String> = r.get(12)?;
    Ok(Session {
        id: r.get(0)?,
        task_id: r.get(1)?,
        event_id: r.get(2)?,
        title: r.get(3)?,
        planned_min: r.get(4)?,
        paused_at: r.get(6)?,
        paused_ms: r.get(7)?,
        mode: r.get(8)?,
        work_min: r.get(9)?,
        break_min: r.get(10)?,
        phase: r.get(11)?,
        phase_started_at: phase_started_at.unwrap_or_else(|| started_at.clone()),
        started_at,
        phase_paused_ms: r.get(13)?,
        round: r.get(14)?,
        step_index: r.get(15)?,
        step_count: r.get(16)?,
        step_name: r.get(17)?,
        notes: r.get(18)?,
        conversation_id: r.get(19)?,
    })
}

fn since(at: &str, now: jiff::Timestamp) -> i64 {
    at.parse::<jiff::Timestamp>()
        .ok()
        .and_then(|t| (now - t).total(jiff::Unit::Millisecond).ok())
        .map_or(0, |ms| ms as i64)
        .max(0)
}

impl Session {
    /// How long the current pause has run, zero while the session is going.
    fn paused_for_ms(&self, now: jiff::Timestamp) -> i64 {
        self.paused_at.as_deref().map_or(0, |at| since(at, now))
    }

    pub fn elapsed_ms(&self, now: jiff::Timestamp) -> i64 {
        (since(&self.started_at, now) - self.paused_ms - self.paused_for_ms(now)).max(0)
    }

    pub fn phase_elapsed_ms(&self, now: jiff::Timestamp) -> i64 {
        (since(&self.phase_started_at, now) - self.phase_paused_ms - self.paused_for_ms(now)).max(0)
    }

    /// How long this phase is meant to run; `None` outside pomodoro mode, where
    /// nothing flips on its own.
    fn phase_len_ms(&self) -> Option<i64> {
        if self.mode != "pomodoro" {
            return None;
        }
        let min = if self.phase == "break" { self.break_min } else { self.work_min }?;
        Some(min * 60_000)
    }
}

#[derive(Debug, Default)]
pub struct NewSession {
    pub task_id: Option<i64>,
    pub event_id: Option<i64>,
    pub title: String,
    pub planned_min: Option<i64>,
    pub step_index: Option<i64>,
    pub step_count: Option<i64>,
    pub step_name: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// The session the user is in, if any.
pub fn open(conn: &Connection, user_id: i64) -> rusqlite::Result<Option<Session>> {
    conn.query_row(
        &format!(
            "SELECT {COLS} FROM work_sessions
             WHERE user_id = ?1 AND ended_at IS NULL ORDER BY id DESC LIMIT 1"
        ),
        [user_id],
        row_to_session,
    )
    .optional()
}

/// The user's open session, only when it is the one the client named.
fn open_named(conn: &Connection, user_id: i64, id: i64) -> rusqlite::Result<Option<Session>> {
    Ok(open(conn, user_id)?.filter(|s| s.id == id))
}

/// Opens a session — stopping whatever was still running — and lays its first
/// progress check itself, so accountability does not wait on the agent's next
/// turn. A task or event named by someone else is refused.
pub fn start(
    conn: &Connection,
    config_dir: &Path,
    user_id: i64,
    username: &str,
    new: NewSession,
    now: jiff::Timestamp,
) -> Result<Session, StartError> {
    let title = new.title.trim();
    if title.is_empty() || title.len() > MAX_TITLE_BYTES {
        return Err(StartError::Invalid(format!(
            "title must be 1 to {MAX_TITLE_BYTES} bytes"
        )));
    }
    if let Some(min) = new.planned_min {
        if !(1..=MAX_PLANNED_MIN).contains(&min) {
            return Err(StartError::Invalid(format!(
                "planned_min must be 1 to {MAX_PLANNED_MIN}"
            )));
        }
    }
    let notes = new.notes.unwrap_or_default();
    if notes.len() > MAX_NOTES_BYTES {
        return Err(StartError::Invalid(format!("notes must be at most {MAX_NOTES_BYTES} bytes")));
    }
    if let Some(count) = new.step_count {
        if !(1..=MAX_STEP_COUNT).contains(&count) {
            return Err(StartError::Invalid(format!("step_count must be 1 to {MAX_STEP_COUNT}")));
        }
    }
    if new.step_index.is_some_and(|i| i < 0) {
        return Err(StartError::Invalid("step_index must not be negative".into()));
    }
    if let Some(id) = new.task_id {
        if crate::tasks::get(conn, user_id, id)?.is_none() {
            return Err(StartError::Invalid(format!("no task {id}")));
        }
    }
    if let Some(id) = new.event_id {
        if crate::plan::event_gate(conn, user_id, id).map_err(StartError::Other)?.is_none() {
            return Err(StartError::Invalid(format!("no event {id}")));
        }
    }
    let cfg = crate::config::UserConfig::load(config_dir, username).ok();
    let pomodoro = cfg.as_ref().is_some_and(|c| c.pomodoro_enabled());
    let work_min = cfg.as_ref().map_or(crate::config::DEFAULT_POMODORO_WORK_MIN, |c| {
        c.pomodoro_work_min()
    }) as i64;
    let break_min = cfg.as_ref().map_or(crate::config::DEFAULT_POMODORO_BREAK_MIN, |c| {
        c.pomodoro_break_min()
    }) as i64;
    let tx = conn.unchecked_transaction()?;
    end(conn, config_dir, user_id, username, None, "stopped", now).map_err(StartError::Other)?;
    let conversation_id =
        crate::talk::create(conn, user_id, &format!("Session: {title}"), now)
            .map_err(StartError::Other)?;
    conn.execute(
        "INSERT INTO work_sessions
            (user_id, task_id, event_id, title, planned_min, started_at, mode, work_min,
             break_min, phase, phase_started_at, round, step_index, step_count, step_name,
             notes, conversation_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'work', ?6, 1, ?10, ?11, ?12, ?13, ?14)",
        rusqlite::params![
            user_id,
            new.task_id,
            new.event_id,
            title,
            new.planned_min,
            now.to_string(),
            if pomodoro { "pomodoro" } else { "single" },
            work_min,
            break_min,
            new.step_index,
            new.step_count,
            new.step_name,
            notes,
            conversation_id,
        ],
    )?;
    let id = conn.last_insert_rowid();
    if !pomodoro {
        lay_opening_checks(conn, config_dir, user_id, username, id, title, new.planned_min, now)
            .map_err(StartError::Other)?;
    }
    crate::log::record(conn, Some(user_id), "work_session_started", &format!("session {id} {title:?}"))
        .map_err(StartError::Other)?;
    tx.commit()?;
    Ok(conn.query_row(&format!("SELECT {COLS} FROM work_sessions WHERE id = ?1"), [id], row_to_session)?)
}

/// Halfway through, or 25 minutes in, whichever comes first.
fn first_check_minutes(planned_min: Option<i64>) -> i64 {
    planned_min
        .map_or(DEFAULT_FIRST_CHECK_MIN, |min| (min / 2).min(DEFAULT_FIRST_CHECK_MIN))
        .max(1)
}

/// The checks a session opens with: the midpoint, and — where the session named
/// a length — one at the planned end.
#[allow(clippy::too_many_arguments)]
fn lay_opening_checks(
    conn: &Connection,
    config_dir: &Path,
    user_id: i64,
    username: &str,
    session_id: i64,
    title: &str,
    planned_min: Option<i64>,
    now: jiff::Timestamp,
) -> Result<()> {
    lay_session_check(
        conn,
        config_dir,
        user_id,
        username,
        session_id,
        first_check_minutes(planned_min),
        &format!("Progress check on {title}."),
        now,
    )?;
    if let Some(min) = planned_min {
        lay_session_check(
            conn,
            config_dir,
            user_id,
            username,
            session_id,
            min,
            &format!(
                "The session on {title} has run its planned {min} minutes: ask in one line \
                 whether it is done or they want to keep going."
            ),
            now,
        )?;
    }
    Ok(())
}

/// One check of the session's own, `after` minutes from now: tied to the
/// session, so it never counts against the day's budget and dies with it.
#[allow(clippy::too_many_arguments)]
fn lay_session_check(
    conn: &Connection,
    config_dir: &Path,
    user_id: i64,
    username: &str,
    session_id: i64,
    after: i64,
    prompt: &str,
    now: jiff::Timestamp,
) -> Result<()> {
    let tz = crate::triggers::timezone(config_dir, username);
    let at = (now + jiff::Span::new().minutes(after)).to_zoned(tz);
    let plan_id = crate::plan::ensure(conn, config_dir, username, user_id, at.date())?;
    let wall = format!("{:02}:{:02}", at.hour(), at.minute());
    let event_id =
        crate::triggers::insert(conn, plan_id, &wall, prompt, "agent", None, None, Some(session_id), now)?;
    crate::log::record(
        conn,
        Some(user_id),
        "trigger_laid",
        &format!("event {event_id} at {} {wall}: {prompt}", at.date()),
    )
}

/// Closes the user's open session and drops the checks it had waiting; ending
/// nothing is success, so a client that lost track can always say stop.
/// `id` restricts the close to one session, for a client naming the one it
/// started. The farewell is laid once the session is closed, so it reads the
/// whole of it.
#[allow(clippy::too_many_arguments)]
pub fn end(
    conn: &Connection,
    config_dir: &Path,
    user_id: i64,
    username: &str,
    id: Option<i64>,
    outcome: &str,
    now: jiff::Timestamp,
) -> Result<Option<i64>> {
    anyhow::ensure!(outcome == "done" || outcome == "stopped", "invalid outcome: {outcome}");
    let Some(session) = open(conn, user_id)? else {
        return Ok(None);
    };
    if id.is_some_and(|wanted| wanted != session.id) {
        return Ok(None);
    }
    let tx = conn.is_autocommit().then(|| conn.unchecked_transaction()).transpose()?;
    conn.execute(
        "UPDATE work_sessions SET ended_at = ?1, outcome = ?2 WHERE id = ?3",
        (now.to_string(), outcome, session.id),
    )?;
    let dropped = conn.execute(
        "UPDATE events SET status = 'dropped', decided_at = ?1
         WHERE work_session_id = ?2 AND status IN ('pending','snoozed')",
        (now.to_string(), session.id),
    )?;
    if dropped > 0 {
        crate::log::record(
            conn,
            Some(user_id),
            "trigger_cancelled",
            &format!("{dropped} check{} left with work session {}",
                if dropped == 1 { "" } else { "s" }, session.id),
        )?;
    }
    lay_farewell(conn, config_dir, user_id, username, &session, outcome, now)?;
    crate::log::record(
        conn,
        Some(user_id),
        "work_session_ended",
        &format!("session {} {outcome}", session.id),
    )?;
    if let Some(tx) = tx {
        tx.commit()?;
    }
    Ok(Some(session.id))
}

/// The one check that outlives the session, laid into its own thread. A refusal
/// is recorded rather than raised: the session is over either way.
fn lay_farewell(
    conn: &Connection,
    config_dir: &Path,
    user_id: i64,
    username: &str,
    session: &Session,
    outcome: &str,
    now: jiff::Timestamp,
) -> Result<()> {
    let elapsed = session.elapsed_ms(now) / 60_000;
    let prompt = format!(
        "The session on {} just ended ({outcome}, {elapsed} min, round {}): ask how it went in \
         one line, name what is left, and offer the next step.",
        session.title, session.round
    );
    let laid = crate::triggers::lay(
        conn,
        &crate::triggers::Lay {
            config_dir,
            user_id,
            username,
            at: "+0min",
            prompt: &prompt,
            date: now.to_zoned(crate::triggers::timezone(config_dir, username)).date(),
            cancel: None,
            conversation_id: session.conversation_id,
            work_session_id: Some(session.id),
            system: false,
            now,
        },
    );
    if let Err(refusal) = laid {
        crate::log::record(
            conn,
            Some(user_id),
            "trigger_error",
            &format!("session {}: the farewell was refused ({refusal:?})", session.id),
        )?;
    }
    Ok(())
}

/// Holds the clock where it stands; pausing a session already paused changes
/// nothing.
pub fn pause(
    conn: &Connection,
    user_id: i64,
    id: i64,
    now: jiff::Timestamp,
) -> rusqlite::Result<Option<Session>> {
    let Some(session) = open_named(conn, user_id, id)? else { return Ok(None) };
    if session.paused_at.is_none() {
        conn.execute(
            "UPDATE work_sessions SET paused_at = ?1 WHERE id = ?2",
            (now.to_string(), id),
        )?;
    }
    open_named(conn, user_id, id)
}

/// Starts the clock again, adding the pause to both the session's total and the
/// round's; resuming a session that is running changes nothing.
pub fn resume(
    conn: &Connection,
    user_id: i64,
    id: i64,
    now: jiff::Timestamp,
) -> rusqlite::Result<Option<Session>> {
    let Some(session) = open_named(conn, user_id, id)? else { return Ok(None) };
    if let Some(at) = session.paused_at.as_deref() {
        let held = since(at, now);
        conn.execute(
            "UPDATE work_sessions
             SET paused_at = NULL, paused_ms = paused_ms + ?1, phase_paused_ms = phase_paused_ms + ?1
             WHERE id = ?2",
            (held, id),
        )?;
    }
    open_named(conn, user_id, id)
}

/// Where in the task's steps the session has reached.
pub fn set_step(
    conn: &Connection,
    user_id: i64,
    id: i64,
    step_index: i64,
    step_name: &str,
) -> rusqlite::Result<Option<Session>> {
    if open_named(conn, user_id, id)?.is_none() {
        return Ok(None);
    }
    conn.execute(
        "UPDATE work_sessions SET step_index = ?1, step_name = ?2 WHERE id = ?3",
        (step_index, step_name, id),
    )?;
    open_named(conn, user_id, id)
}

/// Cuts a break short and starts the next round now; outside a break there is
/// nothing to cut.
pub fn skip_break(
    conn: &Connection,
    user_id: i64,
    id: i64,
    now: jiff::Timestamp,
) -> rusqlite::Result<Option<Session>> {
    let Some(session) = open_named(conn, user_id, id)? else { return Ok(None) };
    if session.phase == "break" {
        flip(conn, &session, "work", session.round + 1, now)?;
    }
    open_named(conn, user_id, id)
}

fn flip(
    conn: &Connection,
    session: &Session,
    phase: &str,
    round: i64,
    now: jiff::Timestamp,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE work_sessions
         SET phase = ?1, round = ?2, phase_started_at = ?3, phase_paused_ms = 0
         WHERE id = ?4",
        (phase, round, now.to_string(), session.id),
    )?;
    Ok(())
}

/// A phase that has run its length, and what the user is told about it. The
/// message is delivered off the runner's lock.
#[derive(Debug)]
pub struct Flip {
    pub user_id: i64,
    pub username: String,
    pub message: Option<crate::channels::OutboundMessage>,
}

/// Turns every open pomodoro session that has reached the end of its phase.
/// A paused session never flips, and the round after the last one ends the
/// session instead.
pub fn tick(conn: &Connection, config_dir: &Path, now: jiff::Timestamp) -> Result<Vec<Flip>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS_W}, w.user_id, u.username FROM work_sessions w
         JOIN users u ON u.id = w.user_id
         WHERE w.ended_at IS NULL AND w.mode = 'pomodoro' AND w.paused_at IS NULL"
    ))?;
    let due: Vec<(Session, i64, String)> = stmt
        .query_map([], |r| Ok((row_to_session(r)?, r.get(20)?, r.get(21)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(s, _, _)| s.phase_len_ms().is_some_and(|len| s.phase_elapsed_ms(now) >= len))
        .collect();
    drop(stmt);

    let mut flips = Vec::new();
    for (session, user_id, username) in due {
        let message = if session.phase == "work" {
            if session.round >= MAX_ROUNDS {
                end(conn, config_dir, user_id, &username, Some(session.id), "stopped", now)?;
                crate::log::record(
                    conn,
                    Some(user_id),
                    "session_ended",
                    &format!("session {} ran its {MAX_ROUNDS} rounds", session.id),
                )?;
                None
            } else {
                flip(conn, &session, "break", session.round, now)?;
                crate::log::record(
                    conn,
                    Some(user_id),
                    "session_break",
                    &format!("session {} round {}", session.id, session.round),
                )?;
                Some(crate::channels::OutboundMessage {
                    title: "Break".into(),
                    body: format!(
                        "{} min. Round {} of {} done.",
                        session.break_min.unwrap_or_default(),
                        session.round,
                        session.title
                    ),
                    urgency: crate::channels::Urgency::Normal,
                    event_id: None,
                    conversation_id: session.conversation_id,
                })
            }
        } else {
            let round = session.round + 1;
            flip(conn, &session, "work", round, now)?;
            crate::log::record(
                conn,
                Some(user_id),
                "session_round",
                &format!("session {} round {round}", session.id),
            )?;
            Some(crate::channels::OutboundMessage {
                title: format!("Round {round}"),
                body: format!("Back to {}.", session.title),
                urgency: crate::channels::Urgency::Normal,
                event_id: None,
                conversation_id: session.conversation_id,
            })
        };
        flips.push(Flip { user_id, username, message });
    }
    flips.extend(overrun(conn, config_dir, now)?);
    Ok(flips)
}

/// How far past its planned length a session may run before Note asks after it,
/// and how far before Note ends it.
pub const OVERRUN_ASK_PCT: i64 = 150;
pub const OVERRUN_END_PCT: i64 = 300;

/// Looks after every open session that has run past its plan: at
/// `OVERRUN_ASK_PCT` it asks once, in plain words; at `OVERRUN_END_PCT` it ends
/// the session and says so. Each line lands in the session's thread and goes out
/// through the ladder. A paused session is not running.
pub fn overrun(conn: &Connection, config_dir: &Path, now: jiff::Timestamp) -> Result<Vec<Flip>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS_W}, w.user_id, u.username, w.overrun_asked_at FROM work_sessions w
         JOIN users u ON u.id = w.user_id
         WHERE w.ended_at IS NULL AND w.paused_at IS NULL AND w.planned_min IS NOT NULL"
    ))?;
    let open: Vec<(Session, i64, String, Option<String>)> = stmt
        .query_map([], |r| Ok((row_to_session(r)?, r.get(20)?, r.get(21)?, r.get(22)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    let mut asks = Vec::new();
    for (session, user_id, username, asked) in open {
        let planned = session.planned_min.unwrap_or_default();
        let elapsed_ms = session.elapsed_ms(now);
        let past = |pct: i64| elapsed_ms >= planned * 60_000 * pct / 100;
        let elapsed = elapsed_ms / 60_000;
        let (title, body, kind) = if past(OVERRUN_END_PCT) {
            end(conn, config_dir, user_id, &username, Some(session.id), "stopped", now)?;
            (
                "Force-terminating",
                format!(
                    "{} ran {elapsed} min against {planned} planned, so it ends here. \
                     Start it again when you are ready.",
                    session.title
                ),
                "session_force_ended",
            )
        } else if asked.is_none() && past(OVERRUN_ASK_PCT) {
            let extra = ((planned / 2).max(5) + 4) / 5 * 5;
            conn.execute(
                "UPDATE work_sessions SET overrun_asked_at = ?1 WHERE id = ?2",
                (now.to_string(), session.id),
            )?;
            (
                "Are you OK?",
                format!(
                    "{} has run {elapsed} min against {planned} planned. How is it going? \
                     Take a break, or give it {extra} more minutes.",
                    session.title
                ),
                "session_overrun",
            )
        } else {
            continue;
        };
        if let Some(thread) = session.conversation_id {
            crate::talk::append_assistant(conn, thread, &format!("{title}: {body}"), "", 0, now)?;
        }
        crate::log::record(
            conn,
            Some(user_id),
            kind,
            &format!("session {} at {elapsed} of {planned} min", session.id),
        )?;
        asks.push(Flip {
            user_id,
            username,
            message: Some(crate::channels::OutboundMessage {
                title: title.into(),
                body,
                urgency: crate::channels::Urgency::High,
                event_id: None,
                conversation_id: session.conversation_id,
            }),
        });
    }
    Ok(asks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(extra: &str) -> (Connection, tempfile::TempDir, i64) {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("defaults/user.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            p,
            format!("display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n{extra}"),
        )
        .unwrap();
        (conn, tmp, uid)
    }

    fn env() -> (Connection, tempfile::TempDir, i64) {
        env_with("")
    }

    /// 25 minutes of work, 5 of break, as the defaults have it.
    fn pomodoro_env() -> (Connection, tempfile::TempDir, i64) {
        env_with("pomodoro_enabled = true\n")
    }

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    fn start_one(
        conn: &Connection,
        tmp: &tempfile::TempDir,
        uid: i64,
        planned_min: Option<i64>,
    ) -> Session {
        start(
            conn,
            tmp.path(),
            uid,
            "aki",
            NewSession { title: "read the chapter".into(), planned_min, ..Default::default() },
            at("2026-09-17T09:00:00Z"),
        )
        .unwrap()
    }

    fn end_one(
        conn: &Connection,
        tmp: &tempfile::TempDir,
        uid: i64,
        id: Option<i64>,
        outcome: &str,
        now: &str,
    ) -> Option<i64> {
        end(conn, tmp.path(), uid, "aki", id, outcome, at(now)).unwrap()
    }

    fn checks(conn: &Connection) -> Vec<(i64, String, String, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT work_session_id, wall_time, status, prompt FROM events
                 WHERE kind = 'trigger' ORDER BY id",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn reload(conn: &Connection, uid: i64) -> Session {
        open(conn, uid).unwrap().unwrap()
    }

    #[test]
    fn the_first_check_lands_halfway_in_or_at_twenty_five_minutes() {
        let (conn, tmp, uid) = env();
        start_one(&conn, &tmp, uid, Some(30));
        assert_eq!(checks(&conn)[0].1, "09:15");

        let (conn, tmp, uid) = env();
        start_one(&conn, &tmp, uid, Some(120));
        assert_eq!(checks(&conn)[0].1, "09:25");

        let (conn, tmp, uid) = env();
        start_one(&conn, &tmp, uid, None);
        let first = &checks(&conn)[0];
        assert_eq!((first.0, first.1.as_str()), (1, "09:25"));
        assert_eq!(first.3, "Progress check on read the chapter.");
    }

    #[test]
    fn a_session_that_named_a_length_is_asked_about_it_at_the_end() {
        let (conn, tmp, uid) = env();
        start_one(&conn, &tmp, uid, Some(50));
        let rows = checks(&conn);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].1, "09:50");
        assert!(rows[1].3.starts_with("The session on read the chapter has run its planned 50"),
                "{}", rows[1].3);

        let (conn, tmp, uid) = env();
        start_one(&conn, &tmp, uid, None);
        assert_eq!(checks(&conn).len(), 1, "with no length there is no planned end to ask about");

        let (conn, tmp, uid) = pomodoro_env();
        start_one(&conn, &tmp, uid, Some(50));
        assert!(checks(&conn).is_empty(), "a pomodoro session keeps its own time in rounds");
    }

    #[test]
    fn a_session_opens_a_thread_of_its_own() {
        let (conn, tmp, uid) = env();
        let session = start_one(&conn, &tmp, uid, Some(30));
        let id = session.conversation_id.expect("a session has a thread");
        let (title, via): (String, String) = conn
            .query_row("SELECT title, via FROM conversations WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((title.as_str(), via.as_str()), ("Session: read the chapter", "web"));
    }

    #[test]
    fn a_new_session_stops_the_one_still_running() {
        let (conn, tmp, uid) = env();
        let first = start_one(&conn, &tmp, uid, Some(30));
        let second = start_one(&conn, &tmp, uid, Some(30));
        assert_ne!(first.id, second.id);
        assert_eq!(open(&conn, uid).unwrap().unwrap().id, second.id);
        let outcome: String = conn
            .query_row("SELECT outcome FROM work_sessions WHERE id = ?1", [first.id], |r| r.get(0))
            .unwrap();
        assert_eq!(outcome, "stopped");

        let rows = checks(&conn);
        let left: Vec<(i64, &str, &str)> =
            rows.iter().map(|r| (r.0, r.2.as_str(), r.3.as_str())).collect();
        assert!(
            left.iter().filter(|(s, st, _)| *s == first.id && *st == "dropped").count() == 2,
            "the stopped session took both its checks with it: {left:?}"
        );
        let farewell = left
            .iter()
            .find(|(s, _, p)| *s == first.id && p.contains("just ended"))
            .expect("the session it stopped says goodbye");
        assert_eq!(farewell.1, "pending");
        assert_eq!(
            left.iter().filter(|(s, st, _)| *s == second.id && *st == "pending").count(),
            2
        );
    }

    #[test]
    fn the_farewell_names_the_outcome_and_waits_in_the_sessions_own_thread() {
        let (conn, tmp, uid) = env();
        let session = start_one(&conn, &tmp, uid, Some(60));
        assert_eq!(end_one(&conn, &tmp, uid, Some(session.id), "done", "2026-09-17T09:35:00Z"),
                   Some(session.id));
        let (prompt, thread, status): (String, Option<i64>, String) = conn
            .query_row(
                "SELECT prompt, conversation_id, status FROM events
                 WHERE work_session_id = ?1 AND prompt LIKE '%just ended%'",
                [session.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            prompt,
            "The session on read the chapter just ended (done, 35 min, round 1): ask how it \
             went in one line, name what is left, and offer the next step."
        );
        assert_eq!(thread, session.conversation_id);
        assert_eq!(status, "pending", "it is laid for now, not for ten minutes from now");
    }

    #[test]
    fn ending_a_session_drops_its_waiting_checks_and_leaves_the_rest_alone() {
        let (conn, tmp, uid) = env();
        let session = start_one(&conn, &tmp, uid, Some(60));
        crate::triggers::lay(
            &conn,
            &crate::triggers::Lay {
                config_dir: tmp.path(),
                user_id: uid,
                username: "aki",
                at: "09:40",
                prompt: "still going?",
                date: "2026-09-17".parse().unwrap(),
                cancel: None,
                conversation_id: None,
                work_session_id: None,
                system: false,
                now: at("2026-09-17T09:00:00Z"),
            },
        )
        .unwrap();
        conn.execute("UPDATE events SET status = 'fired' WHERE wall_time = '09:25'", []).unwrap();

        assert_eq!(end_one(&conn, &tmp, uid, Some(session.id), "done", "2026-09-17T09:35:00Z"),
                   Some(session.id));
        let rows = checks(&conn);
        assert_eq!(rows[0].2, "fired", "a check already sent stays as it went");
        assert!(rows[1..3].iter().all(|r| r.2 == "dropped"));
        assert!(open(&conn, uid).unwrap().is_none());
        assert_eq!(end_one(&conn, &tmp, uid, None, "done", "2026-09-17T09:36:00Z"), None);
    }

    #[test]
    fn ending_by_a_stale_id_leaves_the_running_session_alone() {
        let (conn, tmp, uid) = env();
        let session = start_one(&conn, &tmp, uid, Some(60));
        assert_eq!(
            end_one(&conn, &tmp, uid, Some(session.id + 99), "done", "2026-09-17T09:35:00Z"),
            None
        );
        assert_eq!(open(&conn, uid).unwrap().unwrap().id, session.id);
    }

    #[test]
    fn a_session_refuses_a_blank_title_a_silly_length_and_a_foreign_reference() {
        let (conn, tmp, uid) = env();
        let bad = |new: NewSession| {
            start(&conn, tmp.path(), uid, "aki", new, at("2026-09-17T09:00:00Z")).unwrap_err()
        };
        assert!(matches!(bad(NewSession { title: "  ".into(), ..Default::default() }), StartError::Invalid(_)));
        assert!(matches!(
            bad(NewSession { title: "x".into(), planned_min: Some(0), ..Default::default() }),
            StartError::Invalid(_)
        ));
        assert!(matches!(
            bad(NewSession { title: "x".into(), task_id: Some(404), ..Default::default() }),
            StartError::Invalid(_)
        ));
        assert!(matches!(
            bad(NewSession { title: "x".into(), event_id: Some(404), ..Default::default() }),
            StartError::Invalid(_)
        ));
        assert!(matches!(
            bad(NewSession { title: "x".into(), step_count: Some(0), ..Default::default() }),
            StartError::Invalid(_)
        ));
        assert!(open(&conn, uid).unwrap().is_none());
        let events: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(events, 0);
    }

    #[test]
    fn the_clock_stops_while_the_session_is_paused() {
        let (conn, tmp, uid) = pomodoro_env();
        let session = start_one(&conn, &tmp, uid, Some(60));
        assert_eq!(session.elapsed_ms(at("2026-09-17T09:10:00Z")), 600_000);

        let paused = pause(&conn, uid, session.id, at("2026-09-17T09:10:00Z")).unwrap().unwrap();
        assert_eq!(paused.paused_at.as_deref(), Some("2026-09-17T09:10:00Z"));
        assert_eq!(paused.elapsed_ms(at("2026-09-17T09:12:00Z")), 600_000, "a pause holds the clock");
        assert_eq!(paused.phase_elapsed_ms(at("2026-09-17T09:12:00Z")), 600_000);

        let again = pause(&conn, uid, session.id, at("2026-09-17T09:12:00Z")).unwrap().unwrap();
        assert_eq!(again.paused_at, paused.paused_at, "pausing twice changes nothing");

        let running = resume(&conn, uid, session.id, at("2026-09-17T09:12:00Z")).unwrap().unwrap();
        assert_eq!((running.paused_ms, running.phase_paused_ms), (120_000, 120_000));
        assert!(running.paused_at.is_none());
        assert_eq!(running.elapsed_ms(at("2026-09-17T09:13:00Z")), 660_000);
        let untouched = resume(&conn, uid, session.id, at("2026-09-17T09:14:00Z")).unwrap().unwrap();
        assert_eq!(untouched.paused_ms, 120_000, "resuming a running session changes nothing");

        assert!(pause(&conn, uid, session.id + 9, at("2026-09-17T09:15:00Z")).unwrap().is_none());
    }

    #[test]
    fn a_round_runs_its_length_whatever_the_session_spent_paused() {
        let (conn, tmp, uid) = pomodoro_env();
        let session = start_one(&conn, &tmp, uid, None);
        assert_eq!((session.mode.as_str(), session.work_min, session.break_min),
                   ("pomodoro", Some(25), Some(5)));

        pause(&conn, uid, session.id, at("2026-09-17T09:10:00Z")).unwrap();
        resume(&conn, uid, session.id, at("2026-09-17T09:20:00Z")).unwrap();
        assert!(tick(&conn, tmp.path(), at("2026-09-17T09:30:00Z")).unwrap().is_empty(),
                "ten of those minutes were a pause");

        let flips = tick(&conn, tmp.path(), at("2026-09-17T09:35:00Z")).unwrap();
        assert_eq!(flips.len(), 1);
        let msg = flips[0].message.as_ref().unwrap();
        assert_eq!(msg.title, "Break");
        assert_eq!(msg.body, "5 min. Round 1 of read the chapter done.");
        assert_eq!(msg.conversation_id, session.conversation_id);
        let session = reload(&conn, uid);
        assert_eq!((session.phase.as_str(), session.round, session.phase_paused_ms),
                   ("break", 1, 0));
        assert_eq!(session.phase_elapsed_ms(at("2026-09-17T09:35:00Z")), 0);

        let flips = tick(&conn, tmp.path(), at("2026-09-17T09:40:00Z")).unwrap();
        let msg = flips[0].message.as_ref().unwrap();
        assert_eq!(msg.title, "Round 2");
        assert_eq!(msg.body, "Back to read the chapter.");
        let session = reload(&conn, uid);
        assert_eq!((session.phase.as_str(), session.round), ("work", 2));
    }

    #[test]
    fn a_paused_session_and_a_session_running_straight_through_never_flip() {
        let (conn, tmp, uid) = pomodoro_env();
        let session = start_one(&conn, &tmp, uid, None);
        pause(&conn, uid, session.id, at("2026-09-17T09:05:00Z")).unwrap();
        assert!(tick(&conn, tmp.path(), at("2026-09-17T10:00:00Z")).unwrap().is_empty());

        let (conn, tmp, uid) = env();
        start_one(&conn, &tmp, uid, None);
        assert!(tick(&conn, tmp.path(), at("2026-09-17T10:00:00Z")).unwrap().is_empty());
    }

    #[test]
    fn a_break_cut_short_starts_the_next_round_now() {
        let (conn, tmp, uid) = pomodoro_env();
        let session = start_one(&conn, &tmp, uid, None);
        tick(&conn, tmp.path(), at("2026-09-17T09:25:00Z")).unwrap();

        let back = skip_break(&conn, uid, session.id, at("2026-09-17T09:27:00Z")).unwrap().unwrap();
        assert_eq!((back.phase.as_str(), back.round), ("work", 2));
        assert_eq!(back.phase_started_at, "2026-09-17T09:27:00Z");

        let unchanged =
            skip_break(&conn, uid, session.id, at("2026-09-17T09:28:00Z")).unwrap().unwrap();
        assert_eq!((unchanged.phase.as_str(), unchanged.round), ("work", 2),
                   "outside a break there is nothing to cut");
        assert!(skip_break(&conn, uid, session.id + 9, at("2026-09-17T09:29:00Z")).unwrap().is_none());
    }

    #[test]
    fn the_last_round_ends_the_session_rather_than_starting_another_break() {
        let (conn, tmp, uid) = pomodoro_env();
        let session = start_one(&conn, &tmp, uid, None);
        conn.execute(
            "UPDATE work_sessions SET round = ?1, phase_started_at = '2026-09-17T09:00:00Z'
             WHERE id = ?2",
            (MAX_ROUNDS, session.id),
        )
        .unwrap();

        let flips = tick(&conn, tmp.path(), at("2026-09-17T09:25:00Z")).unwrap();
        assert_eq!(flips.len(), 1);
        assert!(flips[0].message.is_none());
        assert!(open(&conn, uid).unwrap().is_none());
        let outcome: String = conn
            .query_row("SELECT outcome FROM work_sessions WHERE id = ?1", [session.id], |r| r.get(0))
            .unwrap();
        assert_eq!(outcome, "stopped");
        let farewell: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE work_session_id = ?1 AND prompt LIKE '%just ended%'",
                [session.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(farewell, 1);
    }

    #[test]
    fn the_step_the_session_is_on_is_the_servers_to_hold() {
        let (conn, tmp, uid) = env();
        let session = start(
            &conn,
            tmp.path(),
            uid,
            "aki",
            NewSession {
                title: "read the chapter".into(),
                step_index: Some(0),
                step_count: Some(3),
                step_name: Some("the first page".into()),
                notes: Some("quietly".into()),
                ..Default::default()
            },
            at("2026-09-17T09:00:00Z"),
        )
        .unwrap();
        assert_eq!((session.step_index, session.step_count), (Some(0), Some(3)));
        assert_eq!(session.notes, "quietly");

        let moved = set_step(&conn, uid, session.id, 1, "the second page").unwrap().unwrap();
        assert_eq!((moved.step_index, moved.step_name.as_deref()), (Some(1), Some("the second page")));
        assert!(set_step(&conn, uid, session.id + 9, 2, "nowhere").unwrap().is_none());
    }

    #[test]
    fn a_session_far_past_its_plan_is_asked_after_once() {
        let (conn, tmp, uid) = env();
        let session = start_one(&conn, &tmp, uid, Some(20));
        assert!(overrun(&conn, tmp.path(), at("2026-09-17T09:29:00Z")).unwrap().is_empty(), "under 1.5x");
        let asks = overrun(&conn, tmp.path(), at("2026-09-17T09:31:00Z")).unwrap();
        assert_eq!(asks.len(), 1);
        let msg = asks[0].message.as_ref().unwrap();
        assert_eq!(msg.title, "Are you OK?");
        assert!(msg.body.contains("31 min against 20 planned"), "{}", msg.body);
        assert!(msg.body.contains("give it 10 more minutes"), "{}", msg.body);
        assert_eq!(msg.conversation_id, session.conversation_id);
        let thread: String = conn
            .query_row(
                "SELECT content FROM talk_messages WHERE conversation_id = ?1 ORDER BY id DESC LIMIT 1",
                [session.conversation_id.unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(thread.starts_with("Are you OK?"));
        assert!(overrun(&conn, tmp.path(), at("2026-09-17T09:45:00Z")).unwrap().is_empty(), "asked once");

        let ended = overrun(&conn, tmp.path(), at("2026-09-17T10:00:00Z")).unwrap();
        assert_eq!(ended.len(), 1, "3x the plan ends it");
        let msg = ended[0].message.as_ref().unwrap();
        assert_eq!(msg.title, "Force-terminating");
        assert!(msg.body.contains("60 min against 20 planned"), "{}", msg.body);
        assert!(open(&conn, uid).unwrap().is_none(), "the session is over");
        assert!(
            checks(&conn).iter().any(|(_, _, status, prompt)| status == "pending" && prompt.contains("just ended")),
            "the farewell is laid"
        );
        assert!(overrun(&conn, tmp.path(), at("2026-09-17T11:00:00Z")).unwrap().is_empty());
    }

    #[test]
    fn a_paused_or_unplanned_session_is_never_asked_after() {
        let (conn, tmp, uid) = env();
        start_one(&conn, &tmp, uid, None);
        assert!(overrun(&conn, tmp.path(), at("2026-09-17T12:00:00Z")).unwrap().is_empty());
        end_one(&conn, &tmp, uid, None, "stopped", "2026-09-17T12:00:00Z");
        let s = start_one(&conn, &tmp, uid, Some(10));
        pause(&conn, uid, s.id, at("2026-09-17T09:05:00Z")).unwrap();
        assert!(overrun(&conn, tmp.path(), at("2026-09-17T09:30:00Z")).unwrap().is_empty());
    }
}
