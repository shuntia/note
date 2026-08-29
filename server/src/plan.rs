use crate::templates::Template;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct PlanEvent {
    pub id: i64,
    pub kind: String,
    pub wall_time: String,
    pub status: String,
    pub flexibility: String,
    pub slide_window_min: i64,
    pub channel: String,
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
    // a truncated plan on retry.
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO plans (user_id, date, created_at) VALUES (?1, ?2, ?3)",
        (user_id, date.to_string(), jiff::Timestamp::now().to_string()),
    )?;
    let plan_id = tx.last_insert_rowid();
    let day = weekday_key(date);
    for ev in template.events.iter().filter(|e| e.days.iter().any(|d| d == day)) {
        tx.execute(
            "INSERT INTO events (plan_id, kind, wall_time, flexibility, slide_window_min, channel)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (plan_id, &ev.kind, &ev.time, &ev.flexibility, ev.slide_window_min, &ev.channel),
        )?;
    }
    tx.commit()?;
    Ok(plan_id)
}

pub fn events_for(conn: &Connection, user_id: i64, date: jiff::civil::Date) -> Result<Vec<PlanEvent>> {
    let mut stmt = conn.prepare(
        "SELECT e.id, e.kind, e.wall_time, e.status, e.flexibility, e.slide_window_min, e.channel
         FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND p.date = ?2 ORDER BY e.wall_time",
    )?;
    let rows = stmt.query_map((user_id, date.to_string()), |r| {
        Ok(PlanEvent {
            id: r.get(0)?, kind: r.get(1)?, wall_time: r.get(2)?, status: r.get(3)?,
            flexibility: r.get(4)?, slide_window_min: r.get(5)?, channel: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Returns `(wall_time, flexibility)` only when the event's plan belongs to
/// `user_id`, so callers cannot distinguish someone else's event from a
/// missing one.
fn owned_event(conn: &Connection, user_id: i64, event_id: i64) -> Result<Option<(String, String)>> {
    Ok(conn
        .query_row(
            "SELECT e.wall_time, e.flexibility FROM events e
             JOIN plans p ON p.id = e.plan_id
             WHERE e.id = ?1 AND p.user_id = ?2",
            (event_id, user_id),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// Moves the event's wall time by `minutes`, clamped inside the day. Only a
/// `snoozed` event returns to `pending`; a decided one (`done`/`dropped`) or an
/// already `fired` one keeps its status so a shift cannot resurrect it. `None`
/// when the event is not the user's or its flexibility is `fixed`.
pub fn shift(conn: &Connection, user_id: i64, event_id: i64, minutes: i64) -> Result<Option<()>> {
    let Some((wall, flex)) = owned_event(conn, user_id, event_id)? else {
        return Ok(None);
    };
    if flex == "fixed" {
        return Ok(None);
    }
    let (h, m) = wall.split_once(':').ok_or_else(|| anyhow::anyhow!("bad wall_time: {wall}"))?;
    let now = h.parse::<i64>()? * 60 + m.parse::<i64>()?;
    let total = now.saturating_add(minutes).clamp(0, 23 * 60 + 59);
    conn.execute(
        "UPDATE events SET wall_time = ?1,
             status = CASE WHEN status = 'snoozed' THEN 'pending' ELSE status END
         WHERE id = ?2",
        (format!("{:02}:{:02}", total / 60, total % 60), event_id),
    )?;
    Ok(Some(()))
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
    conn.execute("UPDATE events SET status = ?1 WHERE id = ?2", (status, event_id))?;
    Ok(Some(()))
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
                    flexibility: "slide".into(), slide_window_min: 60, channel: "voice".into(),
                },
                TemplateEvent {
                    kind: "nudge".into(), time: "14:00".into(),
                    days: vec!["sat".into()],
                    flexibility: "drop".into(), slide_window_min: 0, channel: "push".into(),
                },
            ],
        }
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
        bad.events[0].flexibility = "soft".into(); // not in the events.flexibility CHECK
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

    #[test]
    fn shift_moves_wall_time_and_respects_fixed() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let mut t = tmpl();
        t.events[0].flexibility = "slide".into();
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
        t.events[0].flexibility = "slide".into();
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
    fn shift_does_not_resurrect_a_decided_event() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let mut t = tmpl();
        t.events[0].flexibility = "slide".into();
        generate(&conn, uid, &t, date).unwrap();
        let ev_id = events_for(&conn, uid, date).unwrap()[0].id;

        for decided in ["done", "dropped", "fired"] {
            conn.execute("UPDATE events SET status = ?1 WHERE id = ?2", (decided, ev_id)).unwrap();
            assert!(shift(&conn, uid, ev_id, 30).unwrap().is_some());
            let ev = &events_for(&conn, uid, date).unwrap()[0];
            assert_eq!(ev.status, decided);
        }
        assert_eq!(events_for(&conn, uid, date).unwrap()[0].wall_time, "10:30");
    }

    #[test]
    fn shift_by_non_owner_is_none() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let other = crate::auth::create_user(&conn, "b", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let mut t = tmpl();
        t.events[0].flexibility = "slide".into();
        generate(&conn, uid, &t, date).unwrap();
        let ev_id = events_for(&conn, uid, date).unwrap()[0].id;
        assert!(shift(&conn, other, ev_id, 30).unwrap().is_none());
        assert_eq!(events_for(&conn, uid, date).unwrap()[0].wall_time, "09:00");
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
