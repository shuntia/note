use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

/// One thing that already happened on a day, in the order it happened.
#[derive(Debug, Clone, Serialize)]
pub struct HistoryRow {
    pub at: String,
    pub time: String,
    pub kind: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<i64>,
}

fn row(at: &str, tz: &jiff::tz::TimeZone, kind: &str, label: String) -> Option<HistoryRow> {
    let stamp: jiff::Timestamp = at.parse().ok()?;
    let local = stamp.to_zoned(tz.clone());
    Some(HistoryRow {
        at: at.to_string(),
        time: format!("{:02}:{:02}", local.hour(), local.minute()),
        kind: kind.to_string(),
        label,
        event_id: None,
        task_id: None,
        conversation_id: None,
    })
}

fn on_date(at: &str, tz: &jiff::tz::TimeZone, date: jiff::civil::Date) -> bool {
    at.parse::<jiff::Timestamp>()
        .is_ok_and(|t| t.to_zoned(tz.clone()).date() == date)
}

/// The half-open span of instants that local date covers.
fn bounds(
    tz: &jiff::tz::TimeZone,
    date: jiff::civil::Date,
) -> Result<(jiff::Timestamp, jiff::Timestamp)> {
    let start = tz.to_ambiguous_zoned(date.to_datetime(jiff::civil::Time::midnight())).compatible()?;
    let end = start.checked_add(jiff::Span::new().days(1))?;
    Ok((start.timestamp(), end.timestamp()))
}

struct Decided {
    id: i64,
    kind: String,
    status: String,
    decided_at: Option<String>,
    fired_at: Option<String>,
    wall_time: String,
    end_wall_time: Option<String>,
    span_min: i64,
    moved: Option<(String, String, String)>,
}

/// What the day has already settled: every decision taken on its plan, the
/// tasks finished inside it, and the check-in once it has been answered. An
/// event that fired and is still waiting for a decision past its own end is not
/// history — it is still owed.
pub fn history(
    conn: &Connection,
    user_id: i64,
    tz: &jiff::tz::TimeZone,
    date: jiff::civil::Date,
    now: jiff::Timestamp,
) -> Result<Vec<HistoryRow>> {
    let mut out = Vec::new();

    let mut stmt = conn.prepare(
        "SELECT e.id, e.kind, e.status, e.decided_at, e.fired_at, e.wall_time, e.end_wall_time,
                e.span_min, m.kind, mp.date, m.wall_time
         FROM events e JOIN plans p ON p.id = e.plan_id
         LEFT JOIN events m ON m.id = e.moved_to_event_id
         LEFT JOIN plans mp ON mp.id = m.plan_id
         WHERE p.user_id = ?1 AND p.date = ?2",
    )?;
    let events: Vec<Decided> = stmt
        .query_map((user_id, date.to_string()), |r| {
            let moved_kind: Option<String> = r.get(8)?;
            Ok(Decided {
                id: r.get(0)?,
                kind: r.get(1)?,
                status: r.get(2)?,
                decided_at: r.get(3)?,
                fired_at: r.get(4)?,
                wall_time: r.get(5)?,
                end_wall_time: r.get(6)?,
                span_min: r.get(7)?,
                moved: moved_kind
                    .map(|k| Ok::<_, rusqlite::Error>((k, r.get(9)?, r.get(10)?)))
                    .transpose()?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let local_now = now.to_zoned(tz.clone());
    let now_min = i64::from(local_now.hour()) * 60 + i64::from(local_now.minute());
    for e in events {
        // A trigger point is Note's own moment, not a thing the user did: what
        // it said is in the thread, and one it called off left no mark at all.
        if e.kind == crate::triggers::KIND {
            continue;
        }
        match &e.decided_at {
            Some(at) if on_date(at, tz, date) => {
                let (kind, label) = match (e.status.as_str(), &e.moved) {
                    ("done", _) => ("event_done", e.kind.clone()),
                    ("dropped", Some((k, d, w))) => {
                        ("event_moved", format!("{} → {k} {d} {w}", e.kind))
                    }
                    ("dropped", None) => ("event_dropped", e.kind.clone()),
                    ("snoozed", _) => ("event_snoozed", e.kind.clone()),
                    _ => continue,
                };
                if let Some(mut r) = row(at, tz, kind, label) {
                    r.event_id = Some(e.id);
                    out.push(r);
                }
            }
            _ => {
                let Some(at) = e.fired_at.as_ref().filter(|at| on_date(at, tz, date)) else {
                    continue;
                };
                if e.status != "fired" {
                    continue;
                }
                let end = e.end_wall_time.as_deref().map_or_else(
                    || crate::templates::wall_minutes(&e.wall_time) + e.span_min,
                    crate::templates::wall_minutes,
                );
                if local_now.date() > date || (local_now.date() == date && end <= now_min) {
                    continue;
                }
                if let Some(mut r) = row(at, tz, "event_fired", e.kind.clone()) {
                    r.event_id = Some(e.id);
                    out.push(r);
                }
            }
        }
    }

    let (from, to) = bounds(tz, date)?;
    let mut stmt = conn.prepare(
        "SELECT id, title, completed_at FROM tasks
         WHERE user_id = ?1 AND state = 'done'
           AND completed_at >= ?2 AND completed_at < ?3",
    )?;
    let tasks: Vec<(i64, String, String)> = stmt
        .query_map((user_id, from.to_string(), to.to_string()), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for (id, title, at) in tasks {
        if let Some(mut r) = row(&at, tz, "task_done", title) {
            r.task_id = Some(id);
            out.push(r);
        }
    }

    let mut stmt = conn.prepare(
        "SELECT c.id, MIN(m.created_at) FROM conversations c
         JOIN talk_messages m ON m.conversation_id = c.id AND m.role = 'user'
         WHERE c.user_id = ?1 AND c.checkin_date = ?2
         GROUP BY c.id",
    )?;
    let answered: Vec<(i64, String)> = stmt
        .query_map((user_id, date.to_string()), |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for (id, at) in answered {
        if let Some(mut r) = row(&at, tz, "checkin", "Check-in answered".into()) {
            r.conversation_id = Some(id);
            out.push(r);
        }
    }

    out.sort_by(|a, b| a.at.cmp(&b.at));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> (Connection, i64, jiff::civil::Date) {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "pw", false).unwrap();
        let date: jiff::civil::Date = "2026-09-21".parse().unwrap();
        crate::plan::generate(&conn, uid, &crate::templates::Template { events: Vec::new() }, date)
            .unwrap();
        (conn, uid, date)
    }

    fn event(conn: &Connection, wall: &str, end: Option<&str>) -> i64 {
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, end_wall_time, span_min)
             VALUES (1, 'stretch', ?1, ?1, ?2, 15)",
            (wall, end),
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn decided_at(conn: &Connection, event_id: i64, at: &str) {
        conn.execute("UPDATE events SET decided_at = ?1 WHERE id = ?2", (at, event_id)).unwrap();
    }

    fn rows(conn: &Connection, uid: i64, date: jiff::civil::Date, now: &str) -> Vec<HistoryRow> {
        history(conn, uid, &jiff::tz::TimeZone::UTC, date, now.parse().unwrap()).unwrap()
    }

    #[test]
    fn a_decision_is_history_and_carries_its_local_time() {
        let (conn, uid, date) = env();
        let id = event(&conn, "09:00", None);
        crate::plan::set_status(&conn, uid, id, "done").unwrap();
        decided_at(&conn, id, "2026-09-21T09:10:00Z");
        let out = rows(&conn, uid, date, "2026-09-21T20:00:00Z");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].time, "09:10");
        assert_eq!(out[0].kind, "event_done");
        assert_eq!(out[0].event_id, Some(id));
        assert_eq!(out[0].label, "stretch");
    }

    #[test]
    fn a_fired_event_leaves_history_once_its_end_has_passed() {
        let (conn, uid, date) = env();
        let id = event(&conn, "09:00", None);
        conn.execute(
            "UPDATE events SET status = 'fired', fired_at = '2026-09-21T09:00:00Z' WHERE id = ?1",
            [id],
        )
        .unwrap();
        let during = rows(&conn, uid, date, "2026-09-21T09:05:00Z");
        assert_eq!(during.len(), 1);
        assert_eq!(during[0].kind, "event_fired");
        assert_eq!(during[0].time, "09:00");
        assert!(
            rows(&conn, uid, date, "2026-09-21T11:00:00Z").is_empty(),
            "an undecided event past its end is still owed, not history"
        );
    }

    #[test]
    fn a_trigger_point_leaves_no_mark_on_the_day_whichever_way_it_ended() {
        let (conn, uid, date) = env();
        for (status, at) in [("done", "09:10"), ("dropped", "10:10")] {
            conn.execute(
                "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, span_min,
                                     status, decided_at, prompt)
                 VALUES (1, 'trigger', ?1, ?1, 15, ?2, ?3, 'ask about the essay')",
                (at, status, format!("2026-09-21T{at}:00Z")),
            )
            .unwrap();
        }
        let id = event(&conn, "11:00", None);
        crate::plan::set_status(&conn, uid, id, "done").unwrap();
        decided_at(&conn, id, "2026-09-21T11:10:00Z");

        let out = rows(&conn, uid, date, "2026-09-21T20:00:00Z");
        assert_eq!(out.len(), 1, "only the user's own day is history");
        assert_eq!(out[0].label, "stretch");
    }

    #[test]
    fn a_move_names_where_it_went() {
        let (conn, uid, date) = env();
        let id = event(&conn, "09:00", None);
        crate::plan::move_to_tomorrow(
            &conn,
            uid,
            id,
            &crate::templates::Template { events: Vec::new() },
        )
        .unwrap();
        decided_at(&conn, id, "2026-09-21T09:10:00Z");
        let out = rows(&conn, uid, date, "2026-09-21T20:00:00Z");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "event_moved");
        assert!(out[0].label.contains("2026-09-22"), "{}", out[0].label);
    }

    #[test]
    fn finished_tasks_and_an_answered_checkin_join_the_day_in_order() {
        let (conn, uid, date) = env();
        let id = event(&conn, "08:00", None);
        crate::plan::set_status(&conn, uid, id, "done").unwrap();
        decided_at(&conn, id, "2026-09-21T08:30:00Z");
        let task = crate::tasks::create(
            &conn,
            uid,
            crate::tasks::NewTask { title: "read the chapter".into(), ..Default::default() },
            "manual",
            crate::tasks::Actor::User,
        )
        .unwrap()
        .id;
        conn.execute(
            "UPDATE tasks SET state = 'done', completed_at = '2026-09-21T10:00:00Z' WHERE id = ?1",
            [task],
        )
        .unwrap();
        let conv = crate::talk::checkin_thread(
            &conn,
            uid,
            &date.to_string(),
            "09:00",
            "How is it going?",
            "2026-09-21T09:00:00Z".parse().unwrap(),
        )
        .unwrap();
        let out = rows(&conn, uid, date, "2026-09-21T20:00:00Z");
        assert_eq!(
            out.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(),
            vec!["event_done", "task_done"],
            "an unanswered check-in is not history"
        );

        crate::talk::append_text(&conn, conv, "user", "fine", "2026-09-21T09:20:00Z".parse().unwrap())
            .unwrap();
        let out = rows(&conn, uid, date, "2026-09-21T20:00:00Z");
        assert_eq!(
            out.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(),
            vec!["event_done", "checkin", "task_done"]
        );
        assert_eq!(out[1].conversation_id, Some(conv));
        assert_eq!(out[2].task_id, Some(task));
    }
}
