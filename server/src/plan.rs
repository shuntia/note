use crate::templates::Template;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use thiserror::Error;

/// Distinguishes a slide that violates the event's window (a client/model
/// mistake) from infrastructure failures.
#[derive(Debug, Error)]
pub enum ShiftError {
    #[error("cumulative slide of {offset} min exceeds the ±{window} min window")]
    OutOfWindow { offset: i64, window: i64 },
    #[error("event already {status}")]
    Decided { status: String },
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Where a dropped event went, when the agent named its replacement.
#[derive(Debug, Serialize)]
pub struct MovedTo {
    pub event_id: i64,
    pub date: String,
    pub wall_time: String,
    pub kind: String,
}

/// The task a block holds, for a client that draws the block and offers the
/// task's own actions on it.
#[derive(Debug, Serialize)]
pub struct TaskRef {
    pub id: i64,
    pub title: String,
    pub state: String,
}

#[derive(Debug, Serialize)]
pub struct PlanEvent {
    pub id: i64,
    pub kind: String,
    pub wall_time: String,
    pub end_wall_time: Option<String>,
    pub entry: String,
    pub status: String,
    pub flexibility: String,
    pub slide_window_min: i64,
    pub channel: String,
    pub alert: bool,
    pub origin: String,
    /// What a trigger is meant to follow up on; empty on every other event.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moved_to: Option<MovedTo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskRef>,
}

fn weekday_key(date: jiff::civil::Date) -> &'static str {
    match date.weekday() {
        jiff::civil::Weekday::Monday => "mon",
        jiff::civil::Weekday::Tuesday => "tue",
        jiff::civil::Weekday::Wednesday => "wed",
        jiff::civil::Weekday::Thursday => "thu",
        jiff::civil::Weekday::Friday => "fri",
        jiff::civil::Weekday::Saturday => "sat",
        jiff::civil::Weekday::Sunday => "sun",
    }
}

/// Creates the plan for `user_id`/`date` from `template`'s events matching that
/// weekday. Returns the existing plan id without inserting events again if a
/// plan for that (user, date) already exists.
pub fn generate(conn: &Connection, user_id: i64, template: &Template, date: jiff::civil::Date) -> Result<i64> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM plans WHERE user_id = ?1 AND date = ?2",
            (user_id, date.to_string()), |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    // Plan creation and its events insert as one unit: a failure partway through
    // (e.g. an invalid flexibility value from a hand-edited template) must roll
    // back the plan row too, or the idempotency check above would forever return
    // a truncated plan on retry. A caller already inside a transaction — tool
    // dispatch — provides that atomicity itself, and SQLite has no nesting; such
    // a caller must abort its own transaction on error, or the truncated-plan
    // hazard returns with nothing left here to prevent it.
    let tx = conn.is_autocommit().then(|| conn.unchecked_transaction()).transpose()?;
    conn.execute(
        "INSERT INTO plans (user_id, date, created_at) VALUES (?1, ?2, ?3)",
        (user_id, date.to_string(), jiff::Timestamp::now().to_string()),
    )?;
    let plan_id = conn.last_insert_rowid();
    let day = weekday_key(date);
    let today: Vec<&crate::templates::TemplateEvent> =
        template.events.iter().filter(|e| e.days.iter().any(|d| d == day)).collect();
    for (ev, time) in around_calendar(conn, user_id, date, &today)? {
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, end_wall_time,
                                 flexibility, slide_window_min, channel, alert, span_min)
             VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            (
                plan_id, &ev.kind, &time, ev.is_block().then(|| ev.end()), ev.flexibility(),
                ev.slide_window_min(), &ev.channel, ev.alert(), ev.span_min().max(1),
            ),
        )?;
    }
    if let Some(tx) = tx {
        tx.commit()?;
    }
    Ok(plan_id)
}

/// Where today's template entries start once the user's fixed commitments are
/// accounted for: a routine that would begin inside one moves to its end if it
/// can slide, is left out of the day if it can be dropped, and otherwise
/// stands where the template put it. A block keeps its own shape.
fn around_calendar<'a>(
    conn: &Connection,
    user_id: i64,
    date: jiff::civil::Date,
    events: &[&'a crate::templates::TemplateEvent],
) -> Result<Vec<(&'a crate::templates::TemplateEvent, String)>> {
    let busy = crate::calendar::occurrences(conn, user_id, date)?;
    let mut out = Vec::with_capacity(events.len());
    for ev in events {
        let inside = busy.iter().find(|o| {
            o.kind == "fixed" && !ev.is_block() && o.start <= ev.time && ev.time < o.end
        });
        let Some(o) = inside else {
            out.push((*ev, ev.time.clone()));
            continue;
        };
        let window = format!("inside {} {}-{}", o.title, o.start, o.end);
        match ev.flexibility() {
            "slide" => {
                crate::log::record(conn, Some(user_id), "plan_adjusted", &format!(
                    "{date} {} moved {} -> {} ({window})", ev.kind, ev.time, o.end,
                ))?;
                out.push((*ev, o.end.clone()));
            }
            "drop" => {
                crate::log::record(conn, Some(user_id), "plan_adjusted", &format!(
                    "{date} {} at {} skipped ({window})", ev.kind, ev.time,
                ))?;
            }
            _ => out.push((*ev, ev.time.clone())),
        }
    }
    Ok(out)
}

/// The day's plan id, generating it from the user's template when the day has
/// none. A template that cannot be read contributes no events rather than
/// failing: the caller has something to place now, and the nightly run fills
/// the rest of the day in later.
pub fn ensure(
    conn: &Connection,
    config_dir: &std::path::Path,
    username: &str,
    user_id: i64,
    date: jiff::civil::Date,
) -> Result<i64> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM plans WHERE user_id = ?1 AND date = ?2",
            (user_id, date.to_string()),
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let template = crate::config::UserConfig::load(config_dir, username)
        .ok()
        .and_then(|c| Template::load(config_dir, username, &c.template).ok())
        .unwrap_or(Template { events: Vec::new() });
    generate(conn, user_id, &template, date)
}

/// Whether that day has a plan at all, without reading its events.
pub fn exists(conn: &Connection, user_id: i64, date: jiff::civil::Date) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM plans WHERE user_id = ?1 AND date = ?2",
            (user_id, date.to_string()),
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

pub fn events_for(conn: &Connection, user_id: i64, date: jiff::civil::Date) -> Result<Vec<PlanEvent>> {
    let mut stmt = conn.prepare(
        "SELECT e.id, e.kind, e.wall_time, e.end_wall_time, e.status, e.flexibility,
                e.slide_window_min, e.channel, e.alert,
                m.id, mp.date, m.wall_time, m.kind, e.span_min,
                e.origin, e.decided_at, t.id, t.title, t.state, e.prompt
         FROM events e JOIN plans p ON p.id = e.plan_id
         LEFT JOIN events m ON m.id = e.moved_to_event_id
         LEFT JOIN plans mp ON mp.id = m.plan_id
         LEFT JOIN event_tasks et ON et.event_id = e.id
         LEFT JOIN tasks t ON t.id = et.task_id
         WHERE p.user_id = ?1 AND p.date = ?2 ORDER BY e.wall_time",
    )?;
    let rows = stmt.query_map((user_id, date.to_string()), |r| {
        let end_wall_time: Option<String> = r.get(3)?;
        let wall_time: String = r.get(2)?;
        let span_min: i64 = r.get(13)?;
        let entry = if end_wall_time.is_some() { "block" } else { "routine" };
        Ok(PlanEvent {
            id: r.get(0)?,
            kind: r.get(1)?,
            end_wall_time: Some(
                end_wall_time.unwrap_or_else(|| crate::templates::wall_add(&wall_time, span_min)),
            ),
            wall_time,
            entry: entry.into(),
            status: r.get(4)?,
            flexibility: r.get(5)?,
            slide_window_min: r.get(6)?,
            channel: r.get(7)?,
            alert: r.get(8)?,
            origin: r.get(14)?,
            prompt: r.get(19)?,
            decided_at: r.get(15)?,
            moved_to: r.get::<_, Option<i64>>(9)?.map(|event_id| {
                Ok::<_, rusqlite::Error>(MovedTo {
                    event_id,
                    date: r.get(10)?,
                    wall_time: r.get(11)?,
                    kind: r.get(12)?,
                })
            }).transpose()?,
            task: r.get::<_, Option<i64>>(16)?.map(|id| {
                Ok::<_, rusqlite::Error>(TaskRef { id, title: r.get(17)?, state: r.get(18)? })
            }).transpose()?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

struct OwnedEvent {
    wall_time: String,
    flexibility: String,
    orig_wall_time: String,
    slide_window_min: i64,
    status: String,
    is_block: bool,
}

/// Resolves an event only when its plan belongs to `user_id`, so callers cannot
/// distinguish someone else's event from a missing one.
fn owned_event(conn: &Connection, user_id: i64, event_id: i64) -> rusqlite::Result<Option<OwnedEvent>> {
    conn.query_row(
        "SELECT e.wall_time, e.flexibility, e.orig_wall_time, e.slide_window_min, e.status,
                e.end_wall_time IS NOT NULL
         FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE e.id = ?1 AND p.user_id = ?2",
        (event_id, user_id),
        |r| {
            Ok(OwnedEvent {
                wall_time: r.get(0)?,
                flexibility: r.get(1)?,
                orig_wall_time: r.get(2)?,
                slide_window_min: r.get(3)?,
                status: r.get(4)?,
                is_block: r.get(5)?,
            })
        },
    )
    .optional()
}

fn parse_minutes(wall: &str) -> anyhow::Result<i64> {
    let (h, m) = wall.split_once(':').ok_or_else(|| anyhow::anyhow!("bad wall_time: {wall}"))?;
    Ok(h.parse::<i64>()? * 60 + m.parse::<i64>()?)
}

/// Moves the event's wall time by `minutes`, clamped inside the day and — when
/// the event carries a positive `slide_window_min` — bounded so the cumulative
/// offset from `orig_wall_time` stays within the window. A `done` or `dropped`
/// event is a settled user decision and is refused. Only a `snoozed` event
/// returns to `pending`; a `fired` one keeps its status. `None` when the event
/// is not the user's, its flexibility is `fixed`, or it is a block — a block
/// carries an end as well as a start, and moves only through `reshape`.
pub fn shift(conn: &Connection, user_id: i64, event_id: i64, minutes: i64) -> Result<Option<()>, ShiftError> {
    let Some(ev) = owned_event(conn, user_id, event_id)? else {
        return Ok(None);
    };
    if ev.flexibility == "fixed" || ev.is_block {
        return Ok(None);
    }
    if ev.status == "done" || ev.status == "dropped" {
        return Err(ShiftError::Decided { status: ev.status });
    }
    let total = parse_minutes(&ev.wall_time)?.saturating_add(minutes).clamp(0, 23 * 60 + 59);
    if ev.slide_window_min > 0 {
        let offset = total - parse_minutes(&ev.orig_wall_time)?;
        if offset.abs() > ev.slide_window_min {
            return Err(ShiftError::OutOfWindow { offset, window: ev.slide_window_min });
        }
    }
    conn.execute(
        "UPDATE events SET wall_time = ?1,
             status = CASE WHEN status = 'snoozed' THEN 'pending' ELSE status END
         WHERE id = ?2",
        (format!("{:02}:{:02}", total / 60, total % 60), event_id),
    )?;
    Ok(Some(()))
}

/// Postpones delivery: any owned, undecided event (pending/snoozed/fired) can
/// be snoozed regardless of flexibility, and the slide window does not apply —
/// snooze is "not now", not a schedule change. A `done` or `dropped` event is a
/// settled user decision and is refused, exactly as in `shift`. A block has no
/// delivery to postpone, so it is `None`.
pub fn snooze(
    conn: &Connection,
    user_id: i64,
    event_id: i64,
    minutes: i64,
) -> Result<Option<()>, ShiftError> {
    if !(1..=24 * 60).contains(&minutes) {
        return Err(ShiftError::Other(anyhow::anyhow!(
            "snooze minutes must be in 1..=1440, got {minutes}"
        )));
    }
    let Some(ev) = owned_event(conn, user_id, event_id)? else {
        return Ok(None);
    };
    if ev.is_block {
        return Ok(None);
    }
    let (wall, status) = (ev.wall_time, ev.status);
    if status == "done" || status == "dropped" {
        return Err(ShiftError::Decided { status });
    }
    let total = (parse_minutes(&wall)? + minutes).clamp(0, 23 * 60 + 59);
    conn.execute(
        "UPDATE events SET wall_time = ?1, status = 'snoozed', decided_at = ?3 WHERE id = ?2",
        (format!("{:02}:{:02}", total / 60, total % 60), event_id, jiff::Timestamp::now().to_string()),
    )?;
    Ok(Some(()))
}

/// Start and end of an owned block, or `None` when the event is not the user's
/// or is a routine.
pub fn block_shape(
    conn: &Connection,
    user_id: i64,
    event_id: i64,
) -> Result<Option<(String, String, String)>> {
    Ok(conn
        .query_row(
            "SELECT e.wall_time, e.end_wall_time, e.status FROM events e
             JOIN plans p ON p.id = e.plan_id
             WHERE e.id = ?1 AND p.user_id = ?2 AND e.end_wall_time IS NOT NULL",
            (event_id, user_id),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?)
}

/// Moves and resizes a block. A block carries no slide window — its shape is the
/// agent's to arrange — but a `done` or `dropped` block is a settled user
/// decision and is refused, exactly as in `shift`. `orig_wall_time` stays where
/// the template put it. `None` when the event is not the user's or is a routine.
pub fn reshape(
    conn: &Connection,
    user_id: i64,
    event_id: i64,
    start: &str,
    end: &str,
) -> Result<Option<()>, ShiftError> {
    let Some((_, _, status)) = block_shape(conn, user_id, event_id)? else {
        return Ok(None);
    };
    if status == "done" || status == "dropped" {
        return Err(ShiftError::Decided { status });
    }
    conn.execute(
        "UPDATE events SET wall_time = ?1, end_wall_time = ?2 WHERE id = ?3",
        (start, end, event_id),
    )?;
    Ok(Some(()))
}

/// Records where a dropped event went. The target is resolved under the same
/// ownership predicate as every other plan write, so it can only ever be one of
/// the user's own events.
pub fn set_moved_to(
    conn: &Connection,
    user_id: i64,
    event_id: i64,
    target_id: i64,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE events SET moved_to_event_id = ?1
         WHERE id = ?2 AND plan_id IN (SELECT id FROM plans WHERE user_id = ?3)",
        (target_id, event_id, user_id),
    )?;
    Ok(n > 0)
}

/// Flexibility and current status of an owned event: the two fields the agent
/// drop gate weighs before touching a user decision.
pub fn event_gate(conn: &Connection, user_id: i64, event_id: i64) -> Result<Option<(String, String)>> {
    Ok(conn
        .query_row(
            "SELECT e.flexibility, e.status FROM events e
             JOIN plans p ON p.id = e.plan_id
             WHERE e.id = ?1 AND p.user_id = ?2",
            (event_id, user_id),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// Records a user decision on an event. Only `done` and `dropped` are user
/// decisions; the lifecycle statuses belong to the scheduler.
pub fn set_status(conn: &Connection, user_id: i64, event_id: i64, status: &str) -> Result<Option<()>> {
    if status != "done" && status != "dropped" {
        anyhow::bail!("invalid status: {status}");
    }
    if owned_event(conn, user_id, event_id)?.is_none() {
        return Ok(None);
    }
    conn.execute(
        "UPDATE events SET status = ?1, decided_at = ?3 WHERE id = ?2",
        (status, event_id, jiff::Timestamp::now().to_string()),
    )?;
    Ok(Some(()))
}

/// A block never pings, so silencing one is a client mistake rather than a
/// no-op the caller should ignore.
#[derive(Debug, Error)]
pub enum AlertRefused {
    #[error("a block never pings")]
    Block,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Turns the bell on or off for this day's instance only; the template keeps
/// its own setting for every other day.
pub fn set_alert(
    conn: &Connection,
    user_id: i64,
    event_id: i64,
    alert: bool,
) -> Result<Option<()>, AlertRefused> {
    let Some(ev) = owned_event(conn, user_id, event_id).map_err(anyhow::Error::from)? else {
        return Ok(None);
    };
    if ev.is_block {
        return Err(AlertRefused::Block);
    }
    conn.execute("UPDATE events SET alert = ?1 WHERE id = ?2", (alert, event_id))
        .map_err(anyhow::Error::from)?;
    Ok(Some(()))
}

/// Sends an undecided entry to the day after its own plan date, creating that
/// plan from `template` if needed, and marks the original dropped with a pointer
/// to where it landed. When tomorrow's plan already holds an undecided instance
/// of the same shape at the same planned time — the template recurs — that
/// instance is the landing place; otherwise a copy is inserted. A routine lands
/// at `orig_wall_time`, so a snoozed one arrives at its planned time; a block
/// keeps the shape it has now, and takes its task with it.
pub fn move_to_tomorrow(
    conn: &Connection,
    user_id: i64,
    event_id: i64,
    template: &Template,
) -> Result<Option<(i64, jiff::civil::Date)>, ShiftError> {
    let Some(ev) = owned_event(conn, user_id, event_id)? else {
        return Ok(None);
    };
    if ev.status == "done" || ev.status == "dropped" {
        return Err(ShiftError::Decided { status: ev.status });
    }
    let date: String = conn.query_row(
        "SELECT p.date FROM events e JOIN plans p ON p.id = e.plan_id WHERE e.id = ?1",
        [event_id],
        |r| r.get(0),
    )?;
    let tomorrow = date
        .parse::<jiff::civil::Date>()
        .map_err(anyhow::Error::from)?
        .tomorrow()
        .map_err(anyhow::Error::from)?;
    let tx = conn.unchecked_transaction()?;
    let plan_id = generate(conn, user_id, template, tomorrow)?;
    let existing: Option<i64> = conn
        .query_row(
            "SELECT t.id FROM events t JOIN events s ON s.id = ?2
             WHERE t.plan_id = ?1 AND t.kind = s.kind AND t.orig_wall_time = s.orig_wall_time
               AND (t.end_wall_time IS NULL) = (s.end_wall_time IS NULL)
               AND t.status IN ('pending', 'snoozed', 'fired')
             ORDER BY t.id LIMIT 1",
            (plan_id, event_id),
            |r| r.get(0),
        )
        .optional()?;
    let new_id = match existing {
        Some(id) => id,
        None => {
            conn.execute(
                "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, end_wall_time,
                                     flexibility, slide_window_min, channel, alert, span_min,
                                     message, origin)
                 SELECT ?1,
                        kind,
                        CASE WHEN end_wall_time IS NULL THEN orig_wall_time ELSE wall_time END,
                        orig_wall_time, end_wall_time,
                        flexibility, slide_window_min, channel, alert, span_min, message, origin
                 FROM events WHERE id = ?2",
                (plan_id, event_id),
            )?;
            let id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO event_tasks (event_id, task_id)
                 SELECT ?1, task_id FROM event_tasks WHERE event_id = ?2",
                (id, event_id),
            )?;
            id
        }
    };
    conn.execute(
        "UPDATE events SET status = 'dropped', moved_to_event_id = ?1, decided_at = ?3
         WHERE id = ?2",
        (new_id, event_id, jiff::Timestamp::now().to_string()),
    )?;
    tx.commit()?;
    Ok(Some((new_id, tomorrow)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::{Template, TemplateEvent};

    fn tmpl() -> Template {
        Template {
            events: vec![
                TemplateEvent {
                    kind: "checkin_call".into(), time: "09:00".into(),
                    days: vec!["mon".into(), "tue".into(), "wed".into(), "thu".into(), "fri".into()],
                    flexibility: Some("slide".into()), slide_window_min: Some(60), channel: "push".into(), ..Default::default()
                },
                TemplateEvent {
                    kind: "nudge".into(), time: "14:00".into(),
                    days: vec!["sat".into()],
                    flexibility: Some("drop".into()), slide_window_min: Some(0), channel: "push".into(), ..Default::default()
                },
            ],
        }
    }

    /// School covers 08:15-15:30 every weekday, and `kind` is a routine at
    /// 10:00 on the Monday the tests plan.
    fn day_against_school(kind: &str, flexibility: &str) -> (Connection, i64, jiff::civil::Date) {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        crate::calendar::create(&conn, uid, crate::calendar::Fields {
            title: "school".into(), kind: "fixed".into(),
            start_time: "08:15".into(), end_time: "15:30".into(),
            days: Some(crate::calendar::day_mask(&["mon", "tue", "wed", "thu", "fri"]).unwrap()),
            ..Default::default()
        }).unwrap();
        let t = Template { events: vec![TemplateEvent {
            kind: kind.into(), time: "10:00".into(), days: vec!["mon".into()],
            flexibility: Some(flexibility.into()), slide_window_min: Some(30),
            channel: "push".into(), ..Default::default()
        }]};
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, uid, &t, date).unwrap();
        (conn, uid, date)
    }

    fn adjustments(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT detail FROM event_log WHERE kind='plan_adjusted' ORDER BY id")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get(0)).unwrap();
        rows.collect::<rusqlite::Result<_>>().unwrap()
    }

    #[test]
    fn a_slideable_routine_inside_a_commitment_starts_when_it_ends() {
        let (conn, uid, date) = day_against_school("nudge", "slide");
        let evs = events_for(&conn, uid, date).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].wall_time, "15:30");
        assert_eq!(evs[0].end_wall_time.as_deref(), Some("15:45"), "the span follows the start");
        let orig: String =
            conn.query_row("SELECT orig_wall_time FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(orig, "15:30", "the day's plan, not a slide off the template time");
        assert_eq!(adjustments(&conn).len(), 1);
        assert!(adjustments(&conn)[0].contains("moved 10:00 -> 15:30 (inside school 08:15-15:30)"));
    }

    #[test]
    fn a_droppable_routine_inside_a_commitment_is_left_out_of_the_day() {
        let (conn, uid, date) = day_against_school("nudge", "drop");
        assert!(events_for(&conn, uid, date).unwrap().is_empty());
        assert!(adjustments(&conn)[0].contains("at 10:00 skipped (inside school 08:15-15:30)"));
    }

    #[test]
    fn a_fixed_routine_inside_a_commitment_stands() {
        let (conn, uid, date) = day_against_school("nudge", "fixed");
        assert_eq!(events_for(&conn, uid, date).unwrap()[0].wall_time, "10:00");
        assert!(adjustments(&conn).is_empty());
    }

    #[test]
    fn a_routine_outside_every_commitment_is_left_alone() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        crate::calendar::create(&conn, uid, crate::calendar::Fields {
            title: "school".into(), kind: "fixed".into(),
            start_time: "10:00".into(), end_time: "15:30".into(),
            days: Some(crate::calendar::day_mask(&["mon"]).unwrap()), ..Default::default()
        }).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, uid, &tmpl(), date).unwrap();
        assert_eq!(events_for(&conn, uid, date).unwrap()[0].wall_time, "09:00");
        assert!(adjustments(&conn).is_empty());
    }

    #[test]
    fn a_block_keeps_its_shape_whatever_the_calendar_says() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        crate::calendar::create(&conn, uid, crate::calendar::Fields {
            title: "school".into(), kind: "fixed".into(),
            start_time: "08:15".into(), end_time: "15:30".into(),
            days: Some(crate::calendar::day_mask(&["mon"]).unwrap()), ..Default::default()
        }).unwrap();
        let t = Template { events: vec![TemplateEvent {
            kind: "Work time".into(), time: "09:30".into(), days: vec!["mon".into()],
            entry: crate::templates::Entry::Block, end_time: Some("12:30".into()),
            channel: "push".into(), ..Default::default()
        }]};
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, uid, &t, date).unwrap();
        let evs = events_for(&conn, uid, date).unwrap();
        assert_eq!((evs[0].wall_time.as_str(), evs[0].end_wall_time.as_deref()),
                   ("09:30", Some("12:30")));
        assert!(adjustments(&conn).is_empty());
    }

    #[test]
    fn generates_weekday_events_only() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        // 2026-08-31 is a Monday
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, uid, &tmpl(), date).unwrap();
        let evs = events_for(&conn, uid, date).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].kind, "checkin_call");
        assert_eq!(evs[0].wall_time, "09:00");
    }

    #[test]
    fn generate_stores_the_block_range_and_bell_state() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let t = Template {
            events: vec![
                TemplateEvent {
                    kind: "Work time".into(), time: "09:30".into(), days: vec!["mon".into()],
                    entry: crate::templates::Entry::Block, end_time: Some("12:30".into()),
                    channel: "push".into(), ..Default::default()
                },
                TemplateEvent {
                    kind: "meds".into(), time: "08:00".into(), days: vec!["mon".into()],
                    alert: Some(false), channel: "push".into(), ..Default::default()
                },
            ],
        };
        generate(&conn, uid, &t, date).unwrap();
        let evs = events_for(&conn, uid, date).unwrap();
        assert_eq!(evs[0].kind, "meds");
        assert_eq!(evs[0].entry, "routine");
        assert_eq!(evs[0].end_wall_time.as_deref(), Some("08:15"));
        assert!(!evs[0].alert);
        assert_eq!(evs[1].kind, "Work time");
        assert_eq!(evs[1].entry, "block");
        assert_eq!(evs[1].end_wall_time.as_deref(), Some("12:30"));
        assert!(!evs[1].alert);
        assert_eq!(evs[1].flexibility, "slide");
    }

    #[test]
    fn a_block_cannot_take_a_bell() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let t = Template {
            events: vec![
                TemplateEvent {
                    kind: "meds".into(), time: "08:00".into(), days: vec!["mon".into()],
                    channel: "push".into(), ..Default::default()
                },
                TemplateEvent {
                    kind: "Work time".into(), time: "09:30".into(), days: vec!["mon".into()],
                    entry: crate::templates::Entry::Block, end_time: Some("12:30".into()),
                    channel: "push".into(), ..Default::default()
                },
            ],
        };
        generate(&conn, uid, &t, date).unwrap();
        assert!(events_for(&conn, uid, date).unwrap()[0].alert);
        assert!(matches!(set_alert(&conn, uid, 2, true), Err(AlertRefused::Block)));
        set_alert(&conn, uid, 1, false).unwrap().unwrap();
        assert!(!events_for(&conn, uid, date).unwrap()[0].alert);
    }

    #[test]
    fn a_decided_event_never_moves_and_a_block_takes_its_shape_along() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let t = Template {
            events: vec![
                TemplateEvent {
                    kind: "meds".into(), time: "08:00".into(), days: vec!["mon".into()],
                    channel: "push".into(), ..Default::default()
                },
                TemplateEvent {
                    kind: "Work time".into(), time: "09:30".into(), days: vec!["mon".into()],
                    entry: crate::templates::Entry::Block, end_time: Some("12:30".into()),
                    channel: "push".into(), ..Default::default()
                },
            ],
        };
        generate(&conn, uid, &t, date).unwrap();
        assert!(move_to_tomorrow(&conn, uid, 99, &t).unwrap().is_none());
        reshape(&conn, uid, 2, "10:00", "13:00").unwrap().unwrap();
        let (block_id, tomorrow) = move_to_tomorrow(&conn, uid, 2, &t).unwrap().unwrap();
        let moved: (String, Option<String>) = conn
            .query_row("SELECT wall_time, end_wall_time FROM events WHERE id = ?1", [block_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(moved, ("10:00".into(), Some("13:00".into())));
        assert_eq!(tomorrow.to_string(), "2026-09-01");
        set_status(&conn, uid, 1, "done").unwrap().unwrap();
        assert!(matches!(
            move_to_tomorrow(&conn, uid, 1, &t),
            Err(ShiftError::Decided { .. })
        ));
    }

    #[test]
    fn a_moved_event_keeps_its_message() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let mut t = tmpl();
        t.events[0].days = vec!["mon".into()];
        generate(&conn, uid, &t, date).unwrap();
        conn.execute("UPDATE events SET message = ?1 WHERE id = 1", ["water the plants"]).unwrap();
        let (new_id, tomorrow) = move_to_tomorrow(&conn, uid, 1, &t).unwrap().unwrap();
        assert_eq!(tomorrow.to_string(), "2026-09-01");
        let message: String = conn
            .query_row("SELECT message FROM events WHERE id = ?1", [new_id], |r| r.get(0))
            .unwrap();
        assert_eq!(message, "water the plants");
    }

    #[test]
    fn a_moved_routine_merges_into_the_instance_tomorrow_already_has() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let t = Template {
            events: vec![TemplateEvent {
                kind: "meds".into(), time: "08:00".into(),
                days: vec!["mon".into(), "tue".into()],
                channel: "push".into(), ..Default::default()
            }],
        };
        generate(&conn, uid, &t, date).unwrap();
        let (new_id, tomorrow) = move_to_tomorrow(&conn, uid, 1, &t).unwrap().unwrap();
        let evs = events_for(&conn, uid, tomorrow).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].id, new_id);
        assert_eq!(evs[0].wall_time, "08:00");
        assert_eq!(evs[0].status, "pending");
    }

    #[test]
    fn a_moved_routine_tomorrow_does_not_repeat_is_copied() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let t = Template {
            events: vec![TemplateEvent {
                kind: "meds".into(), time: "08:00".into(), days: vec!["mon".into()],
                channel: "push".into(), ..Default::default()
            }],
        };
        generate(&conn, uid, &t, date).unwrap();
        let (new_id, tomorrow) = move_to_tomorrow(&conn, uid, 1, &t).unwrap().unwrap();
        assert_ne!(new_id, 1);
        let evs = events_for(&conn, uid, tomorrow).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].id, new_id);
        assert_eq!(evs[0].kind, "meds");
        assert_eq!(evs[0].wall_time, "08:00");
    }

    #[test]
    fn regenerating_same_day_is_noop() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let p1 = generate(&conn, uid, &tmpl(), date).unwrap();
        let p2 = generate(&conn, uid, &tmpl(), date).unwrap();
        assert_eq!(p1, p2);
        assert_eq!(events_for(&conn, uid, date).unwrap().len(), 1);
    }

    #[test]
    fn failed_event_insert_leaves_no_plan_row() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let mut bad = tmpl();
        bad.events[0].flexibility = Some("soft".into()); // not in the events.flexibility CHECK
        assert!(generate(&conn, uid, &bad, date).is_err());
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM plans WHERE user_id = ?1", [uid], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        // A retry with a valid template must succeed, not be blocked by a
        // leftover row from the failed attempt.
        let plan_id = generate(&conn, uid, &tmpl(), date).unwrap();
        assert!(plan_id > 0);
    }

    /// Tool dispatch calls `generate` inside its own transaction, where SQLite
    /// forbids a nested one: the plan must still be written, and must still
    /// vanish when the caller aborts.
    #[test]
    fn generate_defers_to_a_caller_transaction() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let plans = || -> i64 {
            conn.query_row("SELECT COUNT(*) FROM plans", [], |r| r.get(0)).unwrap()
        };

        let tx = conn.unchecked_transaction().unwrap();
        generate(&conn, uid, &tmpl(), date).unwrap();
        drop(tx);
        assert_eq!(plans(), 0);

        let tx = conn.unchecked_transaction().unwrap();
        generate(&conn, uid, &tmpl(), date).unwrap();
        tx.commit().unwrap();
        assert_eq!(plans(), 1);
        assert_eq!(events_for(&conn, uid, date).unwrap().len(), 1);
    }

    #[test]
    fn shift_moves_wall_time_and_respects_fixed() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let mut t = tmpl();
        t.events[0].flexibility = Some("slide".into());
        generate(&conn, uid, &t, date).unwrap();
        let ev = &events_for(&conn, uid, date).unwrap()[0];
        assert!(shift(&conn, uid, ev.id, 45).unwrap().is_some());
        let ev = &events_for(&conn, uid, date).unwrap()[0];
        assert_eq!(ev.wall_time, "09:45");

        conn.execute("UPDATE events SET flexibility='fixed'", []).unwrap();
        assert!(shift(&conn, uid, ev.id, 15).unwrap().is_none());
    }

    #[test]
    fn shift_clamps_to_the_day_and_clears_snoozed() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let mut t = tmpl();
        t.events[0].flexibility = Some("slide".into());
        t.events[0].slide_window_min = Some(0);
        generate(&conn, uid, &t, date).unwrap();
        let ev_id = events_for(&conn, uid, date).unwrap()[0].id;
        conn.execute("UPDATE events SET status='snoozed' WHERE id = ?1", [ev_id]).unwrap();

        assert!(shift(&conn, uid, ev_id, i64::MAX).unwrap().is_some());
        let ev = &events_for(&conn, uid, date).unwrap()[0];
        assert_eq!(ev.wall_time, "23:59");
        assert_eq!(ev.status, "pending");

        assert!(shift(&conn, uid, ev_id, i64::MIN).unwrap().is_some());
        assert_eq!(events_for(&conn, uid, date).unwrap()[0].wall_time, "00:00");
    }

    #[test]
    fn shift_keeps_a_fired_event_fired() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let mut t = tmpl();
        t.events[0].flexibility = Some("slide".into());
        t.events[0].slide_window_min = Some(0);
        generate(&conn, uid, &t, date).unwrap();
        let ev_id = events_for(&conn, uid, date).unwrap()[0].id;
        conn.execute("UPDATE events SET status = 'fired' WHERE id = ?1", [ev_id]).unwrap();

        assert!(shift(&conn, uid, ev_id, 30).unwrap().is_some());
        let ev = &events_for(&conn, uid, date).unwrap()[0];
        assert_eq!(ev.status, "fired");
        assert_eq!(ev.wall_time, "09:30");
    }

    #[test]
    fn shift_rejects_done_and_dropped_events() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let mut t = tmpl();
        t.events[0].flexibility = Some("slide".into());
        t.events[0].slide_window_min = Some(0);
        generate(&conn, uid, &t, date).unwrap();
        let ev_id = events_for(&conn, uid, date).unwrap()[0].id;

        for decided in ["done", "dropped"] {
            conn.execute("UPDATE events SET status = ?1 WHERE id = ?2", (decided, ev_id)).unwrap();
            let err = shift(&conn, uid, ev_id, 30).unwrap_err();
            assert!(matches!(err, ShiftError::Decided { .. }), "got {err:?}");
            let ev = &events_for(&conn, uid, date).unwrap()[0];
            assert_eq!(ev.status, decided);
            assert_eq!(ev.wall_time, "09:00");
        }
    }

    #[test]
    fn shift_by_non_owner_is_none() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let other = crate::auth::create_user(&conn, "b", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let mut t = tmpl();
        t.events[0].flexibility = Some("slide".into());
        generate(&conn, uid, &t, date).unwrap();
        let ev_id = events_for(&conn, uid, date).unwrap()[0].id;
        assert!(shift(&conn, other, ev_id, 30).unwrap().is_none());
        assert_eq!(events_for(&conn, uid, date).unwrap()[0].wall_time, "09:00");
    }

    #[test]
    fn shift_beyond_window_is_rejected_and_writes_nothing() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, uid, &tmpl(), date).unwrap();
        let ev_id = events_for(&conn, uid, date).unwrap()[0].id;
        // window is ±60: +45 then +45 puts cumulative offset at 90
        assert!(shift(&conn, uid, ev_id, 45).unwrap().is_some());
        let err = shift(&conn, uid, ev_id, 45).unwrap_err();
        assert!(matches!(err, ShiftError::OutOfWindow { offset: 90, window: 60 }), "got {err:?}");
        assert_eq!(events_for(&conn, uid, date).unwrap()[0].wall_time, "09:45");
        // sliding back inside the window still works
        assert!(shift(&conn, uid, ev_id, -45).unwrap().is_some());
        assert_eq!(events_for(&conn, uid, date).unwrap()[0].wall_time, "09:00");
    }

    #[test]
    fn a_routine_end_follows_its_start_when_snoozed() {
        let conn = crate::db::open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')", [])
            .unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, 1, &tmpl(), date).unwrap();
        snooze(&conn, 1, 1, 20).unwrap().unwrap();
        let evs = events_for(&conn, 1, date).unwrap();
        assert_eq!(evs[0].wall_time, "09:20");
        assert_eq!(evs[0].end_wall_time.as_deref(), Some("09:35"));
    }

    #[test]
    fn snooze_sets_status_and_pushes_time() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, uid, &tmpl(), date).unwrap();
        let ev_id = events_for(&conn, uid, date).unwrap()[0].id;
        // snooze works on a fired event and is not window-bound
        conn.execute("UPDATE events SET status='fired' WHERE id=?1", [ev_id]).unwrap();
        assert!(snooze(&conn, uid, ev_id, 90).unwrap().is_some());
        let ev = &events_for(&conn, uid, date).unwrap()[0];
        assert_eq!(ev.status, "snoozed");
        assert_eq!(ev.wall_time, "10:30");
        // decided events cannot be snoozed
        conn.execute("UPDATE events SET status='done' WHERE id=?1", [ev_id]).unwrap();
        let err = snooze(&conn, uid, ev_id, 10).unwrap_err();
        assert!(matches!(err, ShiftError::Decided { .. }), "got {err:?}");
        // range and ownership
        conn.execute("UPDATE events SET status='pending' WHERE id=?1", [ev_id]).unwrap();
        assert!(snooze(&conn, uid, ev_id, 0).is_err());
        let other = crate::auth::create_user(&conn, "b", "p", false).unwrap();
        assert!(snooze(&conn, other, ev_id, 10).unwrap().is_none());
    }

    #[test]
    fn event_gate_is_owner_scoped() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, uid, &tmpl(), date).unwrap();
        let ev_id = events_for(&conn, uid, date).unwrap()[0].id;
        assert_eq!(
            event_gate(&conn, uid, ev_id).unwrap(),
            Some(("slide".into(), "pending".into()))
        );
        let other = crate::auth::create_user(&conn, "b", "p", false).unwrap();
        assert!(event_gate(&conn, other, ev_id).unwrap().is_none());
    }

    #[test]
    fn set_status_accepts_only_done_and_dropped() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, uid, &tmpl(), date).unwrap();
        let ev_id = events_for(&conn, uid, date).unwrap()[0].id;
        assert!(set_status(&conn, uid, ev_id, "done").unwrap().is_some());
        assert!(set_status(&conn, uid, ev_id, "fired").is_err());
        let other = crate::auth::create_user(&conn, "b", "p", false).unwrap();
        assert!(set_status(&conn, other, ev_id, "done").unwrap().is_none());
    }
}
