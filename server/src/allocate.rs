use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

pub const GAP_MIN: u16 = 5;
pub const DEFAULT_BLOCK_MIN: u16 = 25;
pub const MAX_AUTO_BLOCKS: usize = 8;
const MAX_KIND_CHARS: usize = 60;
/// A block laid into the day the user is living in never starts sooner than
/// this, so an allocation is never already late.
const LEAD_MIN: i64 = 10;
const END_OF_DAY_MIN: u16 = 24 * 60;

/// A half-open range of minutes from local midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start: u16,
    pub end: u16,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: i64,
    pub minutes: u16,
    pub is_now: bool,
    pub due: Option<jiff::civil::Date>,
    pub created: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub task_id: i64,
    pub start: u16,
    pub end: u16,
}

/// One placement as it landed on the plan.
#[derive(Debug, Clone, Serialize)]
pub struct Laid {
    pub event_id: i64,
    pub task_id: i64,
    pub start: String,
    pub end: String,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub placed: Vec<Laid>,
    pub cleared: usize,
}

fn wall(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

fn window(start: &str, end: &str) -> Window {
    Window {
        start: crate::templates::wall_minutes(start).clamp(0, END_OF_DAY_MIN as i64) as u16,
        end: crate::templates::wall_minutes(end).clamp(0, END_OF_DAY_MIN as i64) as u16,
    }
}

/// The free time a day actually offers: its `free` occurrences with the hard
/// commitments cut out of them.
pub fn free_windows(occurrences: &[crate::calendar::Occurrence]) -> Vec<Window> {
    let free: Vec<Window> = occurrences
        .iter()
        .filter(|o| o.kind == "free")
        .map(|o| window(&o.start, &o.end))
        .collect();
    let fixed: Vec<Window> = occurrences
        .iter()
        .filter(|o| o.kind == "fixed")
        .map(|o| window(&o.start, &o.end))
        .collect();
    usable(&free, &fixed)
}

impl Window {
    pub fn start_wall(&self) -> String {
        wall(self.start)
    }
    pub fn end_wall(&self) -> String {
        wall(self.end)
    }
}

/// What is left of the free windows once every busy range is cut out of them.
fn usable(free: &[Window], busy: &[Window]) -> Vec<Window> {
    let mut out: Vec<Window> = free.iter().copied().filter(|w| w.start < w.end).collect();
    for b in busy.iter().filter(|b| b.start < b.end) {
        let mut next = Vec::with_capacity(out.len() + 1);
        for w in out {
            if b.end <= w.start || b.start >= w.end {
                next.push(w);
                continue;
            }
            if w.start < b.start {
                next.push(Window { start: w.start, end: b.start });
            }
            if b.end < w.end {
                next.push(Window { start: b.end, end: w.end });
            }
        }
        out = next;
    }
    out.sort_by_key(|w| (w.start, w.end));
    out
}

/// First-fit in priority order: `is_now`, then earliest due (overdue first, no
/// due date last), then oldest created. A task that fits nowhere is skipped and
/// the next one is tried. `GAP_MIN` separates placements.
pub fn pack(free: &[Window], busy: &[Window], tasks: &[Candidate], cap: usize) -> Vec<Placement> {
    let mut slots = usable(free, busy);
    let mut order: Vec<&Candidate> = tasks.iter().collect();
    order.sort_by(|a, b| {
        b.is_now
            .cmp(&a.is_now)
            .then(a.due.is_none().cmp(&b.due.is_none()))
            .then(a.due.cmp(&b.due))
            .then(a.created.cmp(&b.created))
            .then(a.id.cmp(&b.id))
    });
    let mut out = Vec::new();
    for c in order {
        if out.len() >= cap {
            break;
        }
        let minutes = c.minutes.max(1);
        let Some(i) = slots.iter().position(|s| s.end - s.start >= minutes) else { continue };
        let start = slots[i].start;
        out.push(Placement { task_id: c.id, start, end: start + minutes });
        let taken = (start + minutes).saturating_add(GAP_MIN);
        if taken >= slots[i].end {
            slots.remove(i);
        } else {
            slots[i].start = taken;
        }
    }
    out
}

fn candidates(
    conn: &Connection,
    user_id: i64,
    date: jiff::civil::Date,
) -> rusqlite::Result<Vec<Candidate>> {
    let held = crate::tools::plan_ops::planned_on(conn, user_id, date)?;
    let mut stmt = conn.prepare(
        "SELECT id, duration_min, is_now, due_at, created_at FROM tasks
         WHERE user_id = ?1 AND parent_id IS NULL AND state IN ('open','in_progress')
         ORDER BY id",
    )?;
    let rows = stmt.query_map([user_id], |r| {
        let minutes: Option<i64> = r.get(1)?;
        let due: Option<String> = r.get(3)?;
        Ok(Candidate {
            id: r.get(0)?,
            minutes: minutes.map_or(DEFAULT_BLOCK_MIN, |m| m.clamp(1, u16::MAX as i64) as u16),
            is_now: r.get(2)?,
            due: due.and_then(|d| d.parse().ok()),
            created: r.get(4)?,
        })
    })?;
    Ok(rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|c| !held.contains_key(&c.id))
        .collect())
}

/// Removes this day's `origin = 'auto'` blocks that are still pending, then
/// packs the day's free time with the user's open tasks. When `date` is the day
/// the user is living in, slots before `now` plus a short lead are busy.
pub fn run(
    conn: &Connection,
    user_id: i64,
    tz: &jiff::tz::TimeZone,
    date: jiff::civil::Date,
    now: jiff::Timestamp,
) -> Result<Outcome> {
    let plan_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM plans WHERE user_id = ?1 AND date = ?2",
            (user_id, date.to_string()),
            |r| r.get(0),
        )
        .optional()?;
    let Some(plan_id) = plan_id else {
        anyhow::bail!("no plan for {date}");
    };

    let mut stmt = conn.prepare(
        "SELECT e.id FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND p.date = ?2 AND e.origin = 'auto' AND e.status = 'pending'",
    )?;
    let stale: Vec<i64> = stmt
        .query_map((user_id, date.to_string()), |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for id in &stale {
        conn.execute("DELETE FROM event_tasks WHERE event_id = ?1", [id])?;
        conn.execute("DELETE FROM events WHERE id = ?1", [id])?;
    }

    let occurrences = crate::calendar::occurrences(conn, user_id, date)?;
    let free = free_windows(&occurrences);
    let mut busy: Vec<Window> = Vec::new();
    busy.extend(
        crate::tools::plan_ops::occupied(conn, user_id, date)?
            .into_iter()
            .map(|(_, from, to)| Window {
                start: from.clamp(0, END_OF_DAY_MIN as i64) as u16,
                end: to.clamp(0, END_OF_DAY_MIN as i64) as u16,
            }),
    );
    let local = now.to_zoned(tz.clone());
    if local.date() == date {
        let cut = i64::from(local.hour()) * 60 + i64::from(local.minute()) + LEAD_MIN;
        busy.push(Window { start: 0, end: cut.clamp(0, END_OF_DAY_MIN as i64) as u16 });
    }

    let tasks = candidates(conn, user_id, date)?;
    let placed = pack(&free, &busy, &tasks, MAX_AUTO_BLOCKS);

    let mut laid = Vec::with_capacity(placed.len());
    for p in placed {
        let title: String = conn.query_row(
            "SELECT title FROM tasks WHERE id = ?1",
            [p.task_id],
            |r| r.get(0),
        )?;
        let kind: String = title.chars().take(MAX_KIND_CHARS).collect();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, end_wall_time,
                                 flexibility, slide_window_min, channel, alert, span_min, origin)
             VALUES (?1, ?2, ?3, ?3, ?4, 'drop', 0, 'push', 0, ?5, 'auto')",
            (plan_id, &kind, wall(p.start), wall(p.end), i64::from(p.end - p.start)),
        )?;
        let event_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO event_tasks (event_id, task_id) VALUES (?1, ?2)",
            (event_id, p.task_id),
        )?;
        laid.push(Laid {
            event_id,
            task_id: p.task_id,
            start: wall(p.start),
            end: wall(p.end),
        });
    }
    if !laid.is_empty() || !stale.is_empty() {
        crate::log::record(
            conn,
            Some(user_id),
            "plan_allocated",
            &format!("{date}: {} placed, {} cleared", laid.len(), stale.len()),
        )?;
    }
    Ok(Outcome { placed: laid, cleared: stale.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(start: &str, end: &str) -> Window {
        window(start, end)
    }

    fn task(id: i64, minutes: u16) -> Candidate {
        Candidate { id, minutes, is_now: false, due: None, created: "2026-01-01T00:00:00Z".into() }
    }

    #[test]
    fn a_task_lands_at_the_front_of_the_first_window_that_holds_it() {
        let free = [w("16:00", "18:30")];
        let out = pack(&free, &[], &[task(1, 60)], MAX_AUTO_BLOCKS);
        assert_eq!(out, vec![Placement { task_id: 1, start: 16 * 60, end: 17 * 60 }]);
    }

    #[test]
    fn placements_are_separated_by_the_gap() {
        let free = [w("16:00", "18:30")];
        let out = pack(&free, &[], &[task(1, 30), task(2, 30)], MAX_AUTO_BLOCKS);
        assert_eq!(
            out,
            vec![
                Placement { task_id: 1, start: 16 * 60, end: 16 * 60 + 30 },
                Placement { task_id: 2, start: 16 * 60 + 35, end: 17 * 60 + 5 },
            ]
        );
    }

    #[test]
    fn now_comes_first_then_the_nearest_deadline_then_the_oldest() {
        let free = [w("09:00", "12:00")];
        let dated = |id: i64, due: &str, created: &str| Candidate {
            id,
            minutes: 30,
            is_now: false,
            due: Some(due.parse().unwrap()),
            created: created.into(),
        };
        let tasks = vec![
            Candidate { created: "2026-02-01T00:00:00Z".into(), ..task(1, 30) },
            dated(2, "2026-09-20", "2026-03-01T00:00:00Z"),
            dated(3, "2026-09-10", "2026-03-01T00:00:00Z"),
            Candidate { is_now: true, ..task(4, 30) },
            Candidate { created: "2026-01-01T00:00:00Z".into(), ..task(5, 30) },
        ];
        let order: Vec<i64> =
            pack(&free, &[], &tasks, MAX_AUTO_BLOCKS).iter().map(|p| p.task_id).collect();
        assert_eq!(order, vec![4, 3, 2, 5, 1]);
    }

    #[test]
    fn a_task_that_fits_nowhere_is_skipped_and_the_next_one_is_tried() {
        let free = [w("16:00", "16:40")];
        let out = pack(&free, &[], &[task(1, 90), task(2, 30)], MAX_AUTO_BLOCKS);
        assert_eq!(out, vec![Placement { task_id: 2, start: 16 * 60, end: 16 * 60 + 30 }]);
    }

    #[test]
    fn busy_ranges_are_cut_out_of_the_free_time() {
        let free = [w("09:00", "17:00")];
        let busy = [w("10:00", "16:00")];
        let out = pack(&free, &busy, &[task(1, 90), task(2, 30)], MAX_AUTO_BLOCKS);
        assert_eq!(
            out,
            vec![Placement { task_id: 2, start: 9 * 60, end: 9 * 60 + 30 }],
            "the busy range leaves two hour-long remnants, and 90 minutes fits neither"
        );
    }

    #[test]
    fn the_day_takes_at_most_the_cap() {
        let free = [w("08:00", "20:00")];
        let tasks: Vec<Candidate> = (1..=12).map(|id| task(id, 25)).collect();
        assert_eq!(pack(&free, &[], &tasks, MAX_AUTO_BLOCKS).len(), MAX_AUTO_BLOCKS);
    }

    fn env() -> (Connection, i64, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "pw", false).unwrap();
        (conn, uid, tempfile::tempdir().unwrap())
    }

    fn new_task(conn: &Connection, uid: i64, title: &str, minutes: Option<u32>) -> i64 {
        crate::tasks::create(
            conn,
            uid,
            crate::tasks::NewTask { title: title.into(), duration_min: minutes, ..Default::default() },
            "manual",
            crate::tasks::Actor::User,
        )
        .unwrap()
        .id
    }

    fn day(conn: &Connection, uid: i64) -> jiff::civil::Date {
        let date: jiff::civil::Date = "2026-09-21".parse().unwrap();
        crate::calendar::create(
            conn,
            uid,
            crate::calendar::Fields {
                title: "open afternoon".into(),
                kind: "free".into(),
                start_time: "16:00".into(),
                end_time: "18:30".into(),
                on_date: Some(date.to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        crate::plan::generate(conn, uid, &crate::templates::Template { events: Vec::new() }, date)
            .unwrap();
        date
    }

    fn at(when: &str) -> jiff::Timestamp {
        when.parse().unwrap()
    }

    #[test]
    fn a_run_fills_the_free_window_and_leaves_the_day_alone() {
        let (conn, uid, _tmp) = env();
        let date = day(&conn, uid);
        new_task(&conn, uid, "read the chapter", Some(60));
        new_task(&conn, uid, "email the office", None);
        let out = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-20T09:00:00Z")).unwrap();
        assert_eq!(out.cleared, 0);
        let shape: Vec<(String, String)> =
            out.placed.iter().map(|p| (p.start.clone(), p.end.clone())).collect();
        assert_eq!(
            shape,
            vec![("16:00".into(), "17:00".into()), ("17:05".into(), "17:30".into())]
        );
        let origin: String = conn
            .query_row("SELECT origin FROM events WHERE id = ?1", [out.placed[0].event_id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(origin, "auto");
        let (alert, flexibility): (bool, String) = conn
            .query_row(
                "SELECT alert, flexibility FROM events WHERE id = ?1",
                [out.placed[0].event_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(!alert);
        assert_eq!(flexibility, "drop");
    }

    #[test]
    fn a_second_run_replaces_the_pending_blocks_and_keeps_the_settled_ones() {
        let (conn, uid, _tmp) = env();
        let date = day(&conn, uid);
        new_task(&conn, uid, "read the chapter", Some(60));
        new_task(&conn, uid, "email the office", None);
        let first = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-20T09:00:00Z")).unwrap();
        let kept = first.placed[0].event_id;
        crate::plan::set_status(&conn, uid, kept, "done").unwrap();

        let again = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-20T09:00:00Z")).unwrap();
        assert_eq!(again.cleared, 1, "only the pending block goes");
        let alive: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE id = ?1", [kept], |r| r.get(0))
            .unwrap();
        assert_eq!(alive, 1);
        assert_eq!(again.placed.len(), 1, "the done block still holds its task and its time");
        assert_eq!(again.placed[0].start, "17:00");
    }

    #[test]
    fn today_is_filled_only_from_the_next_free_minute_on() {
        let (conn, uid, _tmp) = env();
        let date = day(&conn, uid);
        new_task(&conn, uid, "read the chapter", Some(30));
        let out = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-21T16:20:00Z")).unwrap();
        assert_eq!(out.placed[0].start, "16:30");
    }

    #[test]
    fn a_day_with_no_free_time_places_nothing_and_logs_nothing() {
        let (conn, uid, _tmp) = env();
        let date: jiff::civil::Date = "2026-09-22".parse().unwrap();
        crate::plan::generate(&conn, uid, &crate::templates::Template { events: Vec::new() }, date)
            .unwrap();
        new_task(&conn, uid, "read the chapter", Some(30));
        let out = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-20T09:00:00Z")).unwrap();
        assert!(out.placed.is_empty());
        let logged: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind = 'plan_allocated'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(logged, 0);
    }
}
