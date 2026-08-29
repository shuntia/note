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
    conn.execute(
        "INSERT INTO plans (user_id, date, created_at) VALUES (?1, ?2, ?3)",
        (user_id, date.to_string(), jiff::Timestamp::now().to_string()),
    )?;
    let plan_id = conn.last_insert_rowid();
    let day = weekday_key(date);
    for ev in template.events.iter().filter(|e| e.days.iter().any(|d| d == day)) {
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, flexibility, slide_window_min, channel)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (plan_id, &ev.kind, &ev.time, &ev.flexibility, ev.slide_window_min, &ev.channel),
        )?;
    }
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
}
