use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

pub const GAP_MIN: u16 = 5;
pub const DEFAULT_BLOCK_MIN: u16 = 25;
pub const MAX_AUTO_BLOCKS: usize = 8;
/// The longest one sitting runs, and the shortest piece a longer one is worth
/// being cut into.
pub const MAX_BLOCK_MIN: u16 = 90;
pub const MIN_CHUNK_MIN: u16 = 25;
/// The grain a block is laid on.
const GRAIN: u16 = 5;
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
    /// From `tasks::urgency_rank`; lower is laid first.
    pub urgency_rank: u8,
    pub due: Option<jiff::civil::Date>,
    pub created: String,
    /// Which piece of a split task this is, 1-based, and how many there are.
    pub part: Option<(u16, u16)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub task_id: i64,
    pub start: u16,
    pub end: u16,
    pub part: Option<(u16, u16)>,
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

/// The order work is laid: what the user is on, then urgency, then dated before
/// undated and soonest first, then oldest.
pub fn rank_key(
    is_now: bool,
    urgency_rank: u8,
    due: Option<jiff::civil::Date>,
    created: &str,
    id: i64,
) -> (std::cmp::Reverse<bool>, u8, bool, Option<jiff::civil::Date>, String, i64) {
    (std::cmp::Reverse(is_now), urgency_rank, due.is_none(), due, created.to_owned(), id)
}

/// First-fit in priority order: `is_now`, then urgency rank, then earliest due
/// (overdue first, no due date last), then oldest created. A task that fits
/// nowhere is skipped and the next one is tried. `GAP_MIN` separates placements.
pub fn pack(free: &[Window], busy: &[Window], tasks: &[Candidate], cap: usize) -> Vec<Placement> {
    let mut slots = usable(free, busy);
    let mut order: Vec<&Candidate> = tasks.iter().collect();
    order.sort_by_cached_key(|c| rank_key(c.is_now, c.urgency_rank, c.due, &c.created, c.id));
    let mut out = Vec::new();
    for c in order {
        if out.len() >= cap {
            break;
        }
        let minutes = c.minutes.max(1);
        let Some(i) = slots.iter().position(|s| s.end - s.start >= minutes) else { continue };
        let start = slots[i].start;
        out.push(Placement { task_id: c.id, start, end: start + minutes, part: c.part });
        let taken = (start + minutes).saturating_add(GAP_MIN);
        if taken >= slots[i].end {
            slots.remove(i);
        } else {
            slots[i].start = taken;
        }
    }
    out
}

/// One block too long to sit through, or too long for anything the day still
/// offers, cut into even pieces on the grain: none over `MAX_BLOCK_MIN`, none
/// over `longest`, none under `MIN_CHUNK_MIN`. A block that fits stays whole.
fn split(minutes: u16, longest: u16) -> Vec<u16> {
    let cap = (MAX_BLOCK_MIN.min(longest.max(MIN_CHUNK_MIN)) / GRAIN).max(1);
    let units = minutes.div_ceil(GRAIN);
    if units <= cap {
        return vec![minutes];
    }
    let n = units.div_ceil(cap).min(units / (MIN_CHUNK_MIN / GRAIN)).max(1);
    (0..n).map(|i| (units / n + u16::from(i < units % n)) * GRAIN).collect()
}

/// What a task row says about its length: the estimate it was given, and the
/// progress a projection can be read off.
struct Length {
    duration_min: Option<i64>,
    progress: i64,
    actual_min: Option<i64>,
}

impl Length {
    /// `first` is the first of the three columns in the row.
    fn read(r: &rusqlite::Row, first: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            duration_min: r.get(first)?,
            progress: r.get(first + 1)?,
            actual_min: r.get(first + 2)?,
        })
    }

    /// What is still to come: the projection when the task has one — measured
    /// time, which the learned stretch has no place on — and otherwise the
    /// estimate, or `fallback`, as the nightly says it will really run.
    fn minutes(&self, fallback: Option<i64>, stretch: impl Fn(i64) -> u16) -> u16 {
        let (_, left) = crate::tasks::projection(
            u32::try_from(self.progress).unwrap_or(0),
            self.actual_min.and_then(|m| u32::try_from(m).ok()),
        );
        match left.filter(|m| *m > 0) {
            Some(m) => m.clamp(1, u32::from(u16::MAX)) as u16,
            None => {
                stretch(self.duration_min.or(fallback).unwrap_or(i64::from(DEFAULT_BLOCK_MIN)))
            }
        }
    }
}

fn open_steps(conn: &Connection, parent_id: i64) -> rusqlite::Result<Vec<(i64, Length)>> {
    let mut stmt = conn.prepare(
        "SELECT id, duration_min, progress, actual_min FROM tasks
         WHERE parent_id = ?1 AND state IN ('open','in_progress') ORDER BY id",
    )?;
    let steps = stmt
        .query_map([parent_id], |r| Ok((r.get(0)?, Length::read(r, 1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(steps)
}

struct TaskRow {
    id: i64,
    len: Length,
    is_now: bool,
    state: String,
    urgency: String,
    due_at: Option<String>,
    created: String,
}

/// What the day may still be filled with, already cut the way it will be laid:
/// a task with open steps yields one candidate per step, a long one without
/// yields one per piece, and every duration is stretched by what the nightly
/// learned. `longest` is the largest opening the day has left.
fn candidates(
    conn: &Connection,
    user_id: i64,
    tz: &jiff::tz::TimeZone,
    date: jiff::civil::Date,
    longest: u16,
    now: jiff::Timestamp,
) -> rusqlite::Result<Vec<Candidate>> {
    let held = crate::tools::plan_ops::planned_on(conn, user_id, date)?;
    let factor = crate::learn::plan_factor(conn, user_id)?;
    let stretched = |minutes: i64| {
        crate::learn::stretch(minutes, factor).clamp(1, i64::from(u16::MAX)) as u16
    };
    let mut stmt = conn.prepare(
        "SELECT id, is_now, due_at, created_at, duration_min, progress, actual_min, state, urgency
         FROM tasks
         WHERE user_id = ?1 AND parent_id IS NULL AND state IN ('open','in_progress')
         ORDER BY id",
    )?;
    let rows: Vec<TaskRow> = stmt
        .query_map([user_id], |r| {
            Ok(TaskRow {
                id: r.get(0)?,
                is_now: r.get(1)?,
                due_at: r.get(2)?,
                created: r.get(3)?,
                len: Length::read(r, 4)?,
                state: r.get(7)?,
                urgency: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut out = Vec::new();
    for row in rows {
        let TaskRow { id, len, is_now, state, urgency, due_at, created } = row;
        if held.contains_key(&id) {
            continue;
        }
        let urgency_rank = crate::tasks::urgency_rank(
            &urgency,
            crate::tasks::pressing_at(&state, due_at.as_deref(), now),
        );
        let due = due_at
            .and_then(|d| d.parse::<jiff::Timestamp>().ok())
            .map(|t| t.to_zoned(tz.clone()).date());
        let steps = open_steps(conn, id)?;
        if steps.is_empty() {
            let pieces = split(len.minutes(None, stretched), longest);
            let n = pieces.len() as u16;
            for (i, piece) in pieces.into_iter().enumerate() {
                out.push(Candidate {
                    id,
                    minutes: piece,
                    is_now,
                    urgency_rank,
                    due,
                    created: created.clone(),
                    part: (n > 1).then_some((i as u16 + 1, n)),
                });
            }
            continue;
        }
        let share = len.duration_min.map(|m| (m / steps.len() as i64).max(1));
        for (step_id, step_len) in steps {
            if held.contains_key(&step_id) {
                continue;
            }
            out.push(Candidate {
                id: step_id,
                minutes: step_len.minutes(share, stretched),
                is_now,
                urgency_rank,
                due,
                created: created.clone(),
                part: None,
            });
        }
    }
    Ok(out)
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

    let longest = usable(&free, &busy).iter().map(|w| w.end - w.start).max().unwrap_or(0);
    let tasks = candidates(conn, user_id, tz, date, longest, now)?;
    let placed = pack(&free, &busy, &tasks, MAX_AUTO_BLOCKS);

    let mut laid = Vec::with_capacity(placed.len());
    for p in placed {
        let (title, parent): (String, Option<String>) = conn.query_row(
            "SELECT t.title, up.title FROM tasks t
             LEFT JOIN tasks up ON up.id = t.parent_id
             WHERE t.id = ?1",
            [p.task_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let label = match (parent, p.part) {
            (Some(parent), _) => format!("{parent} · {title}"),
            (None, Some((i, n))) => format!("{title} ({i}/{n})"),
            (None, None) => title,
        };
        let kind: String = label.chars().take(MAX_KIND_CHARS).collect();
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
        Candidate {
            id,
            minutes,
            is_now: false,
            urgency_rank: 2,
            due: None,
            created: "2026-01-01T00:00:00Z".into(),
            part: None,
        }
    }

    fn slot(task_id: i64, start: u16, end: u16) -> Placement {
        Placement { task_id, start, end, part: None }
    }

    #[test]
    fn a_task_lands_at_the_front_of_the_first_window_that_holds_it() {
        let free = [w("16:00", "18:30")];
        let out = pack(&free, &[], &[task(1, 60)], MAX_AUTO_BLOCKS);
        assert_eq!(out, vec![slot(1, 16 * 60, 17 * 60)]);
    }

    #[test]
    fn placements_are_separated_by_the_gap() {
        let free = [w("16:00", "18:30")];
        let out = pack(&free, &[], &[task(1, 30), task(2, 30)], MAX_AUTO_BLOCKS);
        assert_eq!(
            out,
            vec![
                slot(1, 16 * 60, 16 * 60 + 30),
                slot(2, 16 * 60 + 35, 17 * 60 + 5),
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
            urgency_rank: 2,
            due: Some(due.parse().unwrap()),
            created: created.into(),
            part: None,
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
    fn high_urgency_is_placed_before_an_earlier_due_normal_task_and_low_goes_last() {
        let free = [w("09:00", "11:00")];
        let a = Candidate { due: Some("2026-01-01".parse().unwrap()), ..task(1, 20) };
        let b = Candidate { urgency_rank: 0, ..task(2, 20) };
        let c = Candidate { urgency_rank: 3, due: Some("2025-12-01".parse().unwrap()), ..task(3, 20) };
        let order: Vec<i64> =
            pack(&free, &[], &[a, b, c], MAX_AUTO_BLOCKS).iter().map(|p| p.task_id).collect();
        assert_eq!(order, vec![2, 1, 3]);
    }

    #[test]
    fn a_task_that_fits_nowhere_is_skipped_and_the_next_one_is_tried() {
        let free = [w("16:00", "16:40")];
        let out = pack(&free, &[], &[task(1, 90), task(2, 30)], MAX_AUTO_BLOCKS);
        assert_eq!(out, vec![slot(2, 16 * 60, 16 * 60 + 30)]);
    }

    #[test]
    fn busy_ranges_are_cut_out_of_the_free_time() {
        let free = [w("09:00", "17:00")];
        let busy = [w("10:00", "16:00")];
        let out = pack(&free, &busy, &[task(1, 90), task(2, 30)], MAX_AUTO_BLOCKS);
        assert_eq!(
            out,
            vec![slot(2, 9 * 60, 9 * 60 + 30)],
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

    fn new_step(conn: &Connection, uid: i64, parent: i64, title: &str, minutes: Option<u32>) -> i64 {
        crate::tasks::create(
            conn,
            uid,
            crate::tasks::NewTask {
                title: title.into(),
                duration_min: minutes,
                parent_id: Some(parent),
                ..Default::default()
            },
            "manual",
            crate::tasks::Actor::User,
        )
        .unwrap()
        .id
    }

    /// A day whose afternoon is one long opening, for what has to be split.
    fn open_day(conn: &Connection, uid: i64) -> jiff::civil::Date {
        let date: jiff::civil::Date = "2026-09-21".parse().unwrap();
        crate::calendar::create(
            conn,
            uid,
            crate::calendar::Fields {
                title: "the whole day".into(),
                kind: "free".into(),
                start_time: "09:00".into(),
                end_time: "18:00".into(),
                on_date: Some(date.to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        crate::plan::generate(conn, uid, &crate::templates::Template { events: Vec::new() }, date)
            .unwrap();
        date
    }

    fn kinds(conn: &Connection, out: &Outcome) -> Vec<String> {
        out.placed
            .iter()
            .map(|p| {
                conn.query_row("SELECT kind FROM events WHERE id = ?1", [p.event_id], |r| r.get(0))
                    .unwrap()
            })
            .collect()
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
    fn a_task_part_way_through_is_planned_for_what_is_left_of_it() {
        let (conn, uid, _tmp) = env();
        let date = day(&conn, uid);
        let id = new_task(&conn, uid, "read the chapter", Some(120));
        conn.execute(
            "UPDATE tasks SET progress = 75, actual_min = 90 WHERE id = ?1",
            [id],
        )
        .unwrap();
        let out = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-20T09:00:00Z")).unwrap();
        let shape: Vec<(String, String)> =
            out.placed.iter().map(|p| (p.start.clone(), p.end.clone())).collect();
        assert_eq!(shape, vec![("16:00".into(), "16:30".into())], "30 minutes left, not 120");
    }

    #[test]
    fn a_task_nobody_has_started_keeps_its_estimate() {
        let (conn, uid, _tmp) = env();
        let date = day(&conn, uid);
        let id = new_task(&conn, uid, "read the chapter", Some(60));
        conn.execute("UPDATE tasks SET actual_min = 90 WHERE id = ?1", [id]).unwrap();
        let out = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-20T09:00:00Z")).unwrap();
        let shape: Vec<(String, String)> =
            out.placed.iter().map(|p| (p.start.clone(), p.end.clone())).collect();
        assert_eq!(shape, vec![("16:00".into(), "17:00".into())]);
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

    #[test]
    fn a_block_that_fits_the_day_and_the_sitting_stays_whole() {
        assert_eq!(split(60, 300), vec![60]);
        assert_eq!(split(MAX_BLOCK_MIN, 300), vec![MAX_BLOCK_MIN]);
    }

    #[test]
    fn a_long_block_is_cut_into_even_pieces_on_the_grain() {
        assert_eq!(split(180, 300), vec![90, 90]);
        assert_eq!(split(200, 300), vec![70, 65, 65]);
        assert!(split(200, 300).iter().all(|m| m % GRAIN == 0));
    }

    #[test]
    fn the_largest_opening_caps_a_piece() {
        assert_eq!(split(120, 60), vec![60, 60]);
        assert!(split(240, 50).iter().all(|m| *m <= 50));
    }

    #[test]
    fn a_piece_is_never_shorter_than_the_floor() {
        assert!(split(60, 20).iter().all(|m| *m >= MIN_CHUNK_MIN));
        assert_eq!(split(50, 25), vec![25, 25]);
    }

    #[test]
    fn a_task_with_open_steps_is_laid_one_step_at_a_time() {
        let (conn, uid, _tmp) = env();
        let date = day(&conn, uid);
        let essay = new_task(&conn, uid, "the essay", Some(90));
        let outline = new_step(&conn, uid, essay, "outline", None);
        let draft = new_step(&conn, uid, essay, "draft", None);
        let read = new_step(&conn, uid, essay, "read it back", None);
        let out = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-20T09:00:00Z")).unwrap();

        assert_eq!(
            out.placed.iter().map(|p| p.task_id).collect::<Vec<_>>(),
            vec![outline, draft, read],
            "the blocks hold the steps, not the task over them"
        );
        let shape: Vec<(String, String)> =
            out.placed.iter().map(|p| (p.start.clone(), p.end.clone())).collect();
        assert_eq!(
            shape,
            vec![
                ("16:00".into(), "16:30".into()),
                ("16:35".into(), "17:05".into()),
                ("17:10".into(), "17:40".into()),
            ],
            "a step without a duration takes an even share of the task's"
        );
        assert_eq!(kinds(&conn, &out)[0], "the essay · outline");
    }

    #[test]
    fn a_step_names_its_own_duration_and_falls_back_to_the_default() {
        let (conn, uid, _tmp) = env();
        let date = day(&conn, uid);
        let essay = new_task(&conn, uid, "the essay", None);
        new_step(&conn, uid, essay, "outline", Some(45));
        new_step(&conn, uid, essay, "draft", None);
        let out = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-20T09:00:00Z")).unwrap();
        let shape: Vec<(String, String)> =
            out.placed.iter().map(|p| (p.start.clone(), p.end.clone())).collect();
        assert_eq!(
            shape,
            vec![("16:00".into(), "16:45".into()), ("16:50".into(), "17:15".into())]
        );
    }

    #[test]
    fn a_settled_step_keeps_its_slot_and_the_rest_are_laid_around_it() {
        let (conn, uid, _tmp) = env();
        let date = day(&conn, uid);
        let essay = new_task(&conn, uid, "the essay", Some(60));
        new_step(&conn, uid, essay, "outline", None);
        let draft = new_step(&conn, uid, essay, "draft", None);
        let first = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-20T09:00:00Z")).unwrap();
        crate::plan::set_status(&conn, uid, first.placed[0].event_id, "done").unwrap();

        let again = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-20T09:00:00Z")).unwrap();
        assert_eq!(
            again.placed.iter().map(|p| p.task_id).collect::<Vec<_>>(),
            vec![draft],
            "the step that is already on the day is not laid twice"
        );
    }

    #[test]
    fn a_step_that_is_done_is_not_laid_at_all() {
        let (conn, uid, _tmp) = env();
        let date = day(&conn, uid);
        let essay = new_task(&conn, uid, "the essay", Some(60));
        let outline = new_step(&conn, uid, essay, "outline", None);
        let draft = new_step(&conn, uid, essay, "draft", None);
        crate::tasks::update(
            &conn,
            uid,
            outline,
            crate::tasks::TaskPatch { state: Some("done".into()), ..Default::default() },
        )
        .unwrap();
        let out = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-20T09:00:00Z")).unwrap();
        assert_eq!(out.placed.iter().map(|p| p.task_id).collect::<Vec<_>>(), vec![draft]);
    }

    #[test]
    fn a_task_too_long_for_one_sitting_is_laid_in_numbered_pieces() {
        let (conn, uid, _tmp) = env();
        let date = open_day(&conn, uid);
        let essay = new_task(&conn, uid, "essay draft", Some(200));
        let out = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-20T09:00:00Z")).unwrap();
        assert_eq!(
            out.placed.iter().map(|p| p.task_id).collect::<Vec<_>>(),
            vec![essay, essay, essay],
            "every piece is linked to the one task"
        );
        assert_eq!(
            kinds(&conn, &out),
            vec!["essay draft (1/3)", "essay draft (2/3)", "essay draft (3/3)"]
        );
        let shape: Vec<(String, String)> =
            out.placed.iter().map(|p| (p.start.clone(), p.end.clone())).collect();
        assert_eq!(
            shape,
            vec![
                ("09:00".into(), "10:10".into()),
                ("10:15".into(), "11:20".into()),
                ("11:25".into(), "12:30".into()),
            ]
        );
    }

    #[test]
    fn a_task_that_fits_one_sitting_is_laid_under_its_own_name() {
        let (conn, uid, _tmp) = env();
        let date = open_day(&conn, uid);
        new_task(&conn, uid, "essay draft", Some(90));
        let out = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-20T09:00:00Z")).unwrap();
        assert_eq!(kinds(&conn, &out), vec!["essay draft"]);
    }

    #[test]
    fn the_pieces_of_a_split_task_count_against_the_cap() {
        let (conn, uid, _tmp) = env();
        let date = open_day(&conn, uid);
        for _ in 0..5 {
            new_task(&conn, uid, "a long one", Some(120));
        }
        let out = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-20T09:00:00Z")).unwrap();
        assert_eq!(out.placed.len(), MAX_AUTO_BLOCKS, "five tasks, laid in halves, fill the cap");
    }

    #[test]
    fn the_steps_of_a_task_count_against_the_cap() {
        let (conn, uid, _tmp) = env();
        let date = open_day(&conn, uid);
        let essay = new_task(&conn, uid, "the essay", Some(60));
        for i in 0..12 {
            new_step(&conn, uid, essay, &format!("step {i}"), Some(25));
        }
        let out = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-20T09:00:00Z")).unwrap();
        assert_eq!(out.placed.len(), MAX_AUTO_BLOCKS);
    }

    #[test]
    fn what_the_nightly_learned_stretches_every_block() {
        let (conn, uid, _tmp) = env();
        let date = day(&conn, uid);
        new_task(&conn, uid, "read the chapter", Some(30));
        conn.execute(
            "INSERT INTO learning (user_id, key, value, sample, computed_at)
             VALUES (?1, 'plan_factor', 1.4, 9, '2026-09-20T03:00:00Z')",
            [uid],
        )
        .unwrap();
        let out = run(&conn, uid, &jiff::tz::TimeZone::UTC, date, at("2026-09-20T09:00:00Z")).unwrap();
        assert_eq!(
            (out.placed[0].start.as_str(), out.placed[0].end.as_str()),
            ("16:00", "16:45"),
            "42 minutes rounds up to the grain"
        );
    }

    #[test]
    fn a_dated_task_is_laid_before_an_undated_one_of_the_same_rank() {
        let (conn, uid, _tmp) = env();
        let date = day(&conn, uid);
        let undated = new_task(&conn, uid, "tidy the desk", Some(30));
        let dated = new_task(&conn, uid, "hand in the form", Some(30));
        conn.execute("UPDATE tasks SET due_at = '2026-09-25T09:00:00Z' WHERE id = ?1", [dated])
            .unwrap();
        let mut order =
            candidates(&conn, uid, &jiff::tz::TimeZone::UTC, date, 600, at("2026-09-20T09:00:00Z"))
                .unwrap();
        assert_eq!(order.iter().find(|c| c.id == dated).unwrap().due, Some("2026-09-25".parse().unwrap()));
        order.sort_by_cached_key(|c| rank_key(c.is_now, c.urgency_rank, c.due, &c.created, c.id));
        let ids: Vec<i64> = order.iter().map(|c| c.id).collect();
        assert_eq!(ids, [dated, undated]);
    }

    #[test]
    fn the_due_date_is_read_in_the_users_zone() {
        let (conn, uid, _tmp) = env();
        let date = day(&conn, uid);
        let id = new_task(&conn, uid, "hand in the form", Some(30));
        conn.execute("UPDATE tasks SET due_at = '2026-09-25T03:00:00Z' WHERE id = ?1", [id]).unwrap();
        let la = jiff::tz::TimeZone::get("America/Los_Angeles").unwrap();
        let c = candidates(&conn, uid, &la, date, 600, at("2026-09-20T09:00:00Z")).unwrap();
        assert_eq!(c[0].due, Some("2026-09-24".parse().unwrap()));
    }
}
