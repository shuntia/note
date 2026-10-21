//! `day_find`, `day_save` and `day_remove`: the day as one list of task items,
//! wake-ups, routines and calendar events, and the old plan, order, schedule
//! and trigger tools read as calls of them.

use super::loose::{self, Envelope, Obj, Ref, Target, When};
use super::{SessionKind, ToolCtx, ToolError};
use crate::order::{self, Slot};
use crate::triggers::{self, Cancel, Lay, Refusal};
use jiff::civil::Date;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};

/// How far ahead a task may be placed.
const MAX_DAYS_AHEAD: i64 = 14;
const DEFAULT_BLOCK_MIN: i64 = 25;
const MAX_FIND_DAYS: i64 = 7;
const LISTED_WAKES: usize = 20;
const LATER_WAKE_DAYS: i64 = 5;
const MAX_KIND_CHARS: usize = 60;
const MAX_ROUTINE_CHARS: usize = 100;
const MAX_WINDOW_MIN: i64 = 720;
const DEFAULT_WINDOW_MIN: i64 = 30;
const END_OF_DAY: i64 = 24 * 60;

/// Fields a model hands `day_save` that another tool takes.
const ELSEWHERE: &[(&str, &str)] = &[
    ("state", "task_save"),
    ("due", "task_save"),
    ("progress", "task_save"),
    ("now", "task_save"),
    ("days", "calendar_save"),
    ("until", "calendar_save"),
    ("quiet", "calendar_save"),
];

const ITEM_FIELDS: &[&str] = &["task", "task_id", "wake", "prompt", "routine", "at", "time", "before", "snooze_min"];

fn internal(e: impl std::fmt::Display) -> ToolError {
    ToolError::internal(e.to_string())
}

fn tz(ctx: &ToolCtx) -> jiff::tz::TimeZone {
    triggers::timezone(ctx.config_dir, ctx.username)
}

fn wall(minutes: i64) -> String {
    let m = minutes.clamp(0, 23 * 60 + 59);
    format!("{:02}:{:02}", m / 60, m % 60)
}

fn minutes_of(wall: &str) -> i64 {
    crate::templates::wall_minutes(wall)
}

/// What an item id names on a day.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ItemId {
    Task(i64),
    Event(i64),
    Calendar(i64),
    /// A bare number, which may be either.
    Either(i64),
}

fn item_id(v: &Value) -> Option<ItemId> {
    match v {
        Value::Number(_) => loose::id(v).map(ItemId::Either),
        Value::String(s) => {
            let s = s.trim().trim_start_matches('#');
            let (head, rest) = s.split_at(s.find(|c: char| c.is_ascii_digit())?);
            let n: i64 = rest.trim().parse().ok().filter(|n| *n > 0)?;
            match head.trim_end_matches(['#', '-', ' ', '_']).to_lowercase().as_str() {
                "" => Some(ItemId::Either(n)),
                "t" | "task" => Some(ItemId::Task(n)),
                "e" | "event" | "w" | "wake" | "r" | "routine" => Some(ItemId::Event(n)),
                "c" | "cal" | "calendar" => Some(ItemId::Calendar(n)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// One event of the user's, as the day tools need it.
struct EventRow {
    id: i64,
    date: Date,
    kind: String,
    wall: String,
    end: Option<String>,
    status: String,
    flexibility: String,
    orig: String,
    window: i64,
    prompt: String,
    /// The task a block delivers.
    task: Option<i64>,
}

impl EventRow {
    fn is_wake(&self) -> bool {
        self.kind == triggers::KIND
    }
    fn settled(&self) -> bool {
        matches!(self.status.as_str(), "done" | "dropped")
    }
}

fn event(conn: &Connection, user_id: i64, id: i64) -> Result<Option<EventRow>, ToolError> {
    conn.query_row(
        "SELECT e.id, p.date, e.kind, e.wall_time, e.end_wall_time, e.status, e.flexibility, e.orig_wall_time,
                e.slide_window_min, e.prompt,
                CASE WHEN e.end_wall_time IS NOT NULL
                     THEN (SELECT task_id FROM event_tasks WHERE event_id = e.id LIMIT 1) END
         FROM events e JOIN plans p ON p.id = e.plan_id WHERE e.id = ?1 AND p.user_id = ?2",
        (id, user_id),
        |r| {
            let date: String = r.get(1)?;
            Ok(EventRow {
                id: r.get(0)?,
                date: date.parse().unwrap_or(Date::MIN),
                kind: r.get(2)?,
                wall: r.get(3)?,
                end: r.get(4)?,
                status: r.get(5)?,
                flexibility: r.get(6)?,
                orig: r.get(7)?,
                window: r.get(8)?,
                prompt: r.get(9)?,
                task: r.get(10)?,
            })
        },
    )
    .optional()
    .map_err(internal)
}

/// A day the model named, or why it could not be read.
fn day_arg(v: &Value, today: Date) -> Result<Date, String> {
    let raw = loose::text(v).unwrap_or_default();
    loose::day(&raw, today).ok_or_else(|| format!("{raw:?} is not a day; give {}", loose::DAY_FORMS))
}

fn task_title(conn: &Connection, id: i64) -> String {
    conn.query_row("SELECT title FROM tasks WHERE id = ?1", [id], |r| r.get(0)).unwrap_or_default()
}

fn path(conn: &Connection, user_id: i64, id: i64) -> String {
    super::task_query::path_of(conn, user_id, id).map(|p| p.join(" › ")).unwrap_or_default()
}

// --- day_find ---

/// `day_find`.
pub fn find(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, raw: &str) -> Result<Value, ToolError> {
    let mut o = Obj::new(loose::args(raw)?);
    let now = jiff::Timestamp::now();
    let tz = tz(ctx);
    let today = now.to_zoned(tz.clone()).date();
    let mut warnings = Vec::new();
    let mut date = super::trigger_ops::target_date(ctx, kind, now);
    if let Some(v) = o.take(&["date", "day", "on", "from"]) {
        match day_arg(&v, today) {
            Ok(d) => date = d,
            Err(why) => warnings.push(format!("{why}; read as {date}")),
        }
    }
    let mut days = o.take(&["days", "n", "count"]).and_then(|v| loose::integer(&v)).unwrap_or(1).clamp(1, MAX_FIND_DAYS);
    for field in o.rest() {
        warnings.push(format!("{field} is not a day_find field"));
    }
    if let Some(scope) = &ctx.share {
        let (start, end) = scope.horizon(today);
        if date < start || date >= end {
            let last = end.yesterday().map_err(internal)?;
            warnings.push(format!("this link shares {start} to {last}"));
            return Ok(json!({ "total": 0, "items": [], "warnings": warnings }));
        }
        days = days.min((end - date).get_days().into());
    }
    let mut shown = Vec::new();
    let mut d = date;
    for _ in 0..days {
        shown.push((d, render(conn, ctx, d)?));
        d = d.tomorrow().map_err(internal)?;
    }
    let mut out = if shown.len() == 1 {
        let (d, day) = shown.pop().expect("one day");
        let mut v = day.into_value();
        v["date"] = d.to_string().into();
        v
    } else {
        let list: Vec<Value> = shown
            .into_iter()
            .map(|(d, day)| {
                let mut v = day.into_value();
                v["date"] = d.to_string().into();
                v
            })
            .collect();
        json!({ "days": list })
    };
    if date <= today && ctx.share.is_none() {
        if let Some(q) = crate::calendar::quiet_window(conn, ctx.user_id, &tz, now).map_err(internal)? {
            out["quiet_now"] = json!({ "title": q.title, "until": q.end });
        }
    }
    if ctx.share.is_none() {
        let last = date.checked_add(jiff::Span::new().days(days - 1)).map_err(internal)?;
        let later = later_wake_days(conn, ctx.user_id, last).map_err(internal)?;
        if !later.is_empty() {
            out["later_wake_days"] = later.into();
        }
    }
    if !warnings.is_empty() {
        out["warnings"] = warnings.into();
    }
    Ok(out)
}

/// The day's one list as `day_find` shows it, for the HTTP API.
pub fn items(conn: &Connection, config_dir: &std::path::Path, username: &str, user_id: i64, date: Date) -> Result<Vec<Value>, ToolError> {
    let ctx = ToolCtx {
        config_dir,
        data_dir: config_dir,
        user_id,
        username,
        vectors: super::PreparedVectors::default(),
        task_scope: None,
        inbox_source: None,
        memory_source: None,
        share: None,
        share_thread: None,
    };
    Ok(render(conn, &ctx, date)?.items)
}

/// One day's list as `day_find` shows it.
struct Rendered {
    items: Vec<Value>,
    free: Vec<Value>,
    wakes_not_shown: usize,
}

impl Rendered {
    fn into_value(self) -> Value {
        let mut v = json!({ "total": self.items.len(), "items": self.items });
        if !self.free.is_empty() {
            v["free"] = self.free.into();
        }
        if self.wakes_not_shown > 0 {
            v["wakes_not_shown"] = self.wakes_not_shown.into();
        }
        v
    }
}

fn due_label(t: &crate::tasks::Task, tz: &jiff::tz::TimeZone) -> Option<String> {
    if let Some(d) = &t.effective_due_on {
        return Some(d.clone());
    }
    let at: jiff::Timestamp = t.effective_due_at.as_deref()?.parse().ok()?;
    let (d, w) = crate::zone::civil(at, tz);
    Some(format!("{d} {w}"))
}

fn render(conn: &Connection, ctx: &ToolCtx, date: Date) -> Result<Rendered, ToolError> {
    let tz = tz(ctx);
    // (minute, tier, seq): a timed thing sits at its time; an untimed task item
    // after the timed task item before it in the list.
    let mut keyed: Vec<((i64, u8, usize), Value)> = Vec::new();
    let mut seq = 0usize;
    let mut next = || {
        seq += 1;
        seq
    };
    let mut linked: Vec<i64> = Vec::new();
    {
        let mut stmt = conn
            .prepare_cached(
                "SELECT d.task_id, e.id, e.wall_time, e.end_wall_time, e.status,
                        (SELECT r.category FROM tasks r WHERE r.id = t.root_id)
                 FROM day_items d JOIN tasks t ON t.id = d.task_id
                 LEFT JOIN events e ON e.id = d.event_id
                 WHERE d.user_id = ?1 AND d.date = ?2 AND t.user_id = ?1 AND t.state != 'dropped'
                   AND (e.id IS NULL OR e.status != 'dropped')
                 ORDER BY d.position",
            )
            .map_err(internal)?;
        type Row = (i64, Option<i64>, Option<String>, Option<String>, Option<String>, Option<String>);
        let rows: Vec<Row> = stmt
            .query_map((ctx.user_id, date.to_string()), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
            })
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(internal)?;
        let mut anchor = -1;
        for (task_id, event_id, at, end, status, category) in rows {
            linked.extend(event_id);
            let Some(t) = crate::tasks::get(conn, ctx.user_id, task_id).map_err(internal)? else { continue };
            let hidden = ctx.share.as_ref().is_some_and(|s| !s.allows_category(category.as_deref().unwrap_or("")));
            let key = match &at {
                Some(a) => {
                    anchor = minutes_of(a);
                    (anchor, 0, next())
                }
                None => (anchor, 1, next()),
            };
            if hidden {
                if let Some(a) = &at {
                    keyed.push((key, json!({ "kind": "busy", "at": a, "end": end })));
                }
                continue;
            }
            let mut v = json!({ "id": format!("t{task_id}"), "kind": "task", "task": task_id, "path": path(conn, ctx.user_id, task_id), "state": t.state });
            if let Some(a) = &at {
                v["at"] = a.clone().into();
                v["end"] = end.clone().into();
                if let Some(e) = &end {
                    v["minutes"] = (minutes_of(e) - minutes_of(a)).into();
                }
                if status.as_deref() != Some("pending") {
                    v["block"] = status.into();
                }
            } else if let Some(m) = t.duration_min {
                v["minutes"] = m.into();
            }
            if let Some(due) = due_label(&t, &tz) {
                v["due"] = due.into();
            }
            keyed.push((key, v));
        }
    }
    let mut wakes = 0usize;
    let mut wakes_not_shown = 0usize;
    for e in crate::plan::events_for(conn, ctx.user_id, date).map_err(internal)? {
        if e.status == "dropped" || linked.contains(&e.id) {
            continue;
        }
        let at = minutes_of(&e.wall_time);
        if e.kind == triggers::KIND {
            if ctx.share.is_some() {
                continue;
            }
            wakes += 1;
            if wakes > LISTED_WAKES {
                wakes_not_shown += 1;
                continue;
            }
            let (cancel_if, session, task): (Option<String>, Option<i64>, Option<i64>) = conn
                .query_row(
                    "SELECT cancel_if, work_session_id, (SELECT task_id FROM event_tasks WHERE event_id = ?1 LIMIT 1)
                     FROM events WHERE id = ?1",
                    [e.id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .map_err(internal)?;
            let mut v = json!({ "id": format!("e{}", e.id), "kind": "wake", "at": e.wall_time, "prompt": e.prompt, "status": e.status, "origin": e.origin });
            if e.channel == crate::channels::RING_CHANNEL {
                v["ring"] = true.into();
            }
            if let Some(c) = cancel_if {
                v["cancel_on"] = match c.as_str() {
                    "replied" => "reply",
                    "task_done" => "task",
                    "event_decided" => "event",
                    other => other,
                }
                .into();
            }
            if session.is_some() {
                v["session_check"] = true.into();
            }
            if let Some(t) = task {
                v["task"] = t.into();
            }
            keyed.push(((at, 0, next()), v));
            continue;
        }
        let hidden = ctx.share.as_ref().is_some_and(|s| e.task.as_ref().is_some_and(|t| !s.allows_category(&t.category)));
        if hidden {
            keyed.push(((at, 0, next()), json!({ "kind": "busy", "at": e.wall_time, "end": e.end_wall_time })));
            continue;
        }
        let mut v = json!({ "id": format!("e{}", e.id), "kind": "routine", "name": e.kind, "at": e.wall_time, "flexibility": e.flexibility, "status": e.status });
        if e.entry == "block" {
            v["end"] = e.end_wall_time.clone().into();
        }
        if e.slide_window_min > 0 && e.flexibility == "slide" {
            v["window"] = e.slide_window_min.into();
        }
        if let Some(t) = &e.task {
            v["task"] = t.id.into();
        }
        keyed.push(((at, 0, next()), v));
    }
    let occurrences = crate::calendar::occurrences(conn, ctx.user_id, date).map_err(internal)?;
    for o in occurrences.iter().filter(|o| o.kind != "free") {
        let mut v = json!({ "id": format!("c{}", o.entry_id), "kind": "event", "title": o.title, "at": o.start, "end": o.end, "calendar": o.kind });
        if o.quiet {
            v["quiet"] = true.into();
        }
        keyed.push(((minutes_of(&o.start), 0, next()), v));
    }
    keyed.sort_by_key(|(k, _)| *k);
    let free = crate::calendar::free_windows(&occurrences)
        .into_iter()
        .map(|w| json!({ "at": w.start_wall(), "end": w.end_wall() }))
        .collect();
    Ok(Rendered { items: keyed.into_iter().map(|(_, v)| v).collect(), free, wakes_not_shown })
}

/// The next few days past `date` that hold a pending wake-up, with how many.
fn later_wake_days(conn: &Connection, user_id: i64, date: Date) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare_cached(
        "SELECT p.date, COUNT(*) FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND e.kind = ?2 AND e.status IN ('pending', 'snoozed') AND p.date > ?3
         GROUP BY p.date ORDER BY p.date LIMIT ?4",
    )?;
    let days = stmt
        .query_map((user_id, triggers::KIND, date.to_string(), LATER_WAKE_DAYS), |r| {
            Ok(json!({ "date": r.get::<_, String>(0)?, "wakes": r.get::<_, i64>(1)? }))
        })?
        .collect();
    days
}

// --- day_save ---

/// When an item's `at` asks for it.
#[derive(Debug, Clone, PartialEq)]
enum At {
    Wall(Date, String),
    /// `+N` minutes from now, for a wake-up.
    In(i64),
    Untimed,
}

/// The state of one `day_save` or `day_remove` as it lands item by item.
struct Day<'a> {
    conn: &'a Connection,
    ctx: &'a ToolCtx<'a>,
    kind: SessionKind,
    tz: jiff::tz::TimeZone,
    now: jiff::Timestamp,
    today: Date,
    env: Envelope,
    /// The running work session's leaf and everything above it, for a wake-up.
    held: Vec<i64>,
    /// The days whose task list this call changed.
    touched: Vec<Date>,
}

impl<'a> Day<'a> {
    fn new(conn: &'a Connection, ctx: &'a ToolCtx<'a>, kind: SessionKind) -> Result<Self, ToolError> {
        let tz = tz(ctx);
        let now = jiff::Timestamp::now();
        let held = if kind == SessionKind::Trigger {
            super::task_ops::running_line(conn, ctx.user_id).map_err(internal)?
        } else {
            Vec::new()
        };
        Ok(Self { conn, ctx, kind, today: now.to_zoned(tz.clone()).date(), tz, now, env: Envelope::default(), held, touched: Vec::new() })
    }

    fn ignore(&mut self, r: &Ref, field: &str, why: impl Into<String>) {
        self.env.ignored(Some(r), field, why);
    }

    fn touch(&mut self, date: Date) {
        if !self.touched.contains(&date) {
            self.touched.push(date);
        }
    }

    fn now_min(&self) -> i64 {
        let z = self.now.to_zoned(self.tz.clone());
        i64::from(z.hour()) * 60 + i64::from(z.minute())
    }

    fn gone_by(&self, date: Date, at: &str) -> bool {
        date < self.today || (date == self.today && minutes_of(at) < self.now_min())
    }

    /// An `at` read on `date`, in `zone` when one was named; `None` with the
    /// reason when it cannot be read.
    fn at(&self, given: &Value, date: Date, zone: Option<&jiff::tz::TimeZone>) -> Result<At, String> {
        if given.as_bool() == Some(false) {
            return Ok(At::Untimed);
        }
        let raw = loose::text(given).unwrap_or_default();
        let text = raw.trim();
        let lower = text.to_lowercase();
        if text.is_empty() || matches!(lower.as_str(), "none" | "untimed" | "null" | "clear" | "-") {
            return Ok(At::Untimed);
        }
        if let Some(rest) = lower.strip_prefix('+').or_else(|| lower.strip_prefix("in ")) {
            if let Some(ahead) = loose::minutes(&Value::String(rest.to_string())).filter(|m| *m >= 0) {
                return Ok(At::In(ahead));
            }
        }
        let place = |day: Date, clock: String| -> At {
            match zone {
                Some(there) => crate::zone::translate(day, &clock, there, &self.tz)
                    .map_or_else(|| At::Wall(day, clock.clone()), |(d, w)| At::Wall(d, w)),
                None => At::Wall(day, clock),
            }
        };
        if let Some(clock) = loose::wall_time(text) {
            return Ok(place(date, clock));
        }
        let read_tz = zone.cloned().unwrap_or_else(|| self.tz.clone());
        match loose::when(text, &read_tz, self.now) {
            Some(When::At(moment)) => {
                let (day, clock) = crate::zone::civil(moment, &self.tz);
                Ok(At::Wall(day, clock))
            }
            _ => Err(format!("{raw:?} is not a time; give HH:MM, 9am, 21時 or +20min")),
        }
    }

    fn zone(&mut self, r: Option<&Ref>, v: Option<Value>) -> Option<jiff::tz::TimeZone> {
        let name = loose::text(&v?)?;
        match super::trigger_ops::zone_arg(Some(&name)) {
            Ok(z) => z,
            Err(e) => {
                self.env.ignored(r, "tz", e.message);
                None
            }
        }
    }

    // --- tasks ---

    /// The open task an item names, refusing it with the reason when there is none.
    fn task_target(&mut self, r: &Ref, v: &Value) -> Option<i64> {
        let id = match loose::target(v) {
            Some(Target::Id(id)) => id,
            Some(Target::Title(t)) => {
                let open = match open_tasks(self.conn, self.ctx.user_id) {
                    Ok(o) => o,
                    Err(e) => {
                        self.env.refused(r, e.message);
                        return None;
                    }
                };
                match loose::by_title_listed(&t, &open, "task", |ids| {
                    super::task_ops::labeled(self.conn, self.ctx.user_id, ids)
                        .unwrap_or_else(|_| ids.iter().map(i64::to_string).collect::<Vec<_>>().join(", "))
                }) {
                    Ok(id) => {
                        self.env.adjusted(Some(r), "task", id.into(), format!("{t:?} is task {id}"));
                        id
                    }
                    Err(why) => {
                        self.env.refused(r, why);
                        return None;
                    }
                }
            }
            None => {
                self.env.refused(r, "a task is an id like 41, or the exact title of one open task");
                return None;
            }
        };
        match crate::tasks::get(self.conn, self.ctx.user_id, id) {
            Ok(Some(t)) if matches!(t.state.as_str(), "open" | "in_progress") => Some(id),
            Ok(Some(t)) => {
                self.env.refused(r, format!("task {id} is {}; reopen it with task_save to place it", t.state));
                None
            }
            Ok(None) => {
                self.env.refused(r, format!("no task {id}"));
                None
            }
            Err(e) => {
                self.env.refused(r, e.to_string());
                None
            }
        }
    }

    /// The open leaves a task places, in tree order.
    fn leaves(&self, id: i64) -> Result<Vec<(i64, Option<u32>)>, ToolError> {
        let Some(node) = crate::tasks::node(self.conn, self.ctx.user_id, id).map_err(internal)? else {
            return Ok(Vec::new());
        };
        Ok(node
            .leaves()
            .into_iter()
            .filter(|t| matches!(t.state.as_str(), "open" | "in_progress"))
            .map(|t| (t.id, t.duration_min))
            .collect())
    }

    /// The first open item of the day.
    fn head(&self, date: Date) -> Result<Option<i64>, ToolError> {
        Ok(order::ids(self.conn, self.ctx.user_id, date).map_err(internal)?.first().copied())
    }

    /// Why placing `leaf` would move the running session's work off the head
    /// of today: a wake-up leaves that alone until the session ends.
    fn unseats(&self, date: Date, leaf: i64, anchor: Option<i64>, start: Option<i64>, moving: bool) -> Result<Option<String>, ToolError> {
        if self.held.is_empty() || date != self.today {
            return Ok(None);
        }
        let open = order::list(self.conn, self.ctx.user_id, date).map_err(internal)?;
        let Some(head) = open.first().filter(|h| self.held.contains(&h.task_id)) else { return Ok(None) };
        let running = "the work session running now";
        if leaf == head.task_id && moving {
            return Ok(Some(format!("task {leaf} is {running}; it stays where it is until the session ends")));
        }
        if anchor == Some(head.task_id) || head.at.as_deref().zip(start).is_some_and(|(h, s)| s < minutes_of(h)) {
            return Ok(Some(format!("task {leaf} would go ahead of {running}; leave the head of the day until it ends")));
        }
        Ok(None)
    }

    /// The minutes `[start, start + len)` on `date` take once fixed commitments
    /// and the day's other blocks are stepped past, with what was stepped past;
    /// `None` when no room is left before midnight.
    fn fit(&self, date: Date, mut start: i64, len: i64, own: Option<i64>) -> Result<Option<(i64, Vec<String>)>, ToolError> {
        let busy = crate::calendar::occurrences(self.conn, self.ctx.user_id, date).map_err(internal)?;
        let blocks: Vec<(i64, i64, String)> = {
            let mut stmt = self
                .conn
                .prepare_cached(
                    "SELECT e.id, e.wall_time, e.end_wall_time, e.kind FROM events e JOIN plans p ON p.id = e.plan_id
                     WHERE p.user_id = ?1 AND p.date = ?2 AND e.end_wall_time IS NOT NULL AND e.status IN ('pending','snoozed','fired')",
                )
                .map_err(internal)?;
            let rows = stmt
                .query_map((self.ctx.user_id, date.to_string()), |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))
                })
                .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
                .map_err(internal)?;
            rows.into_iter()
                .filter(|(id, ..)| Some(*id) != own)
                .map(|(_, s, e, k)| (minutes_of(&s), minutes_of(&e), format!("{k} {s}-{e}")))
                .collect()
        };
        let fixed: Vec<(i64, i64, String)> = busy
            .iter()
            .filter(|o| o.kind == "fixed")
            .map(|o| (minutes_of(&o.start), minutes_of(&o.end), format!("{} {}-{}", o.title, o.start, o.end)))
            .collect();
        let mut passed = Vec::new();
        for _ in 0..64 {
            let end = start + len;
            if end > END_OF_DAY {
                return Ok(None);
            }
            let clash = fixed.iter().chain(blocks.iter()).filter(|(s, e, _)| *s < end && start < *e).max_by_key(|(_, e, _)| *e);
            match clash {
                Some((_, e, what)) => {
                    passed.push(what.clone());
                    start = *e;
                }
                None => return Ok(Some((start, passed))),
            }
        }
        Ok(None)
    }

    fn block_len(&self, size: Option<u32>) -> Result<i64, ToolError> {
        let factor = crate::learn::plan_factor(self.conn, self.ctx.user_id).map_err(internal)?;
        Ok(crate::learn::stretch(size.map_or(DEFAULT_BLOCK_MIN, i64::from), factor))
    }

    /// Lays or reshapes the block of `task_id` on `date`, returning its event.
    fn lay_block(&self, date: Date, task_id: i64, existing: Option<i64>, start: i64, len: i64) -> Result<i64, ToolError> {
        let (s, e) = (wall(start), wall(start + len));
        if let Some(id) = existing {
            self.conn
                .execute(
                    "UPDATE events SET wall_time = ?1, end_wall_time = ?2, span_min = ?3,
                         status = CASE WHEN status = 'snoozed' THEN 'pending' ELSE status END
                     WHERE id = ?4",
                    (&s, &e, len.max(1), id),
                )
                .map_err(internal)?;
            return Ok(id);
        }
        let plan_id = crate::plan::holding(self.conn, self.ctx.config_dir, self.ctx.username, self.ctx.user_id, date, self.today)
            .map_err(internal)?;
        let kind: String = task_title(self.conn, task_id).chars().take(MAX_KIND_CHARS).collect();
        self.conn
            .execute(
                "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, end_wall_time,
                                     flexibility, slide_window_min, channel, alert, span_min, origin, created_at)
                 VALUES (?1, ?2, ?3, ?3, ?4, 'drop', 0, 'push', 0, ?5, 'agent', ?6)",
                (plan_id, &kind, &s, &e, len.max(1), self.now.to_string()),
            )
            .map_err(internal)?;
        let id = self.conn.last_insert_rowid();
        self.conn.execute("INSERT INTO event_tasks (event_id, task_id) VALUES (?1, ?2)", (id, task_id)).map_err(internal)?;
        Ok(id)
    }

    /// The live block a slot holds.
    fn live(&self, slot: Option<&Slot>) -> Result<Option<EventRow>, ToolError> {
        let Some(id) = slot.and_then(|s| s.event_id) else { return Ok(None) };
        Ok(event(self.conn, self.ctx.user_id, id)?.filter(|e| e.status != "dropped"))
    }

    /// Places the tasks of one item, each expanded to its open leaves.
    #[allow(clippy::too_many_arguments)]
    fn place(
        &mut self,
        r: &Ref,
        date: Date,
        tasks: Vec<i64>,
        at: Option<&At>,
        minutes: Option<i64>,
        before: Option<&Value>,
        placed: &mut Vec<(Date, i64)>,
    ) -> Result<(), ToolError> {
        if date < self.today {
            self.env.refused(r, format!("{date} has gone by; today is {}", self.today));
            return Ok(());
        }
        let horizon = self.today.checked_add(jiff::Span::new().days(MAX_DAYS_AHEAD)).map_err(internal)?;
        if date > horizon {
            self.env.refused(r, format!("a task goes on a day at most {MAX_DAYS_AHEAD} days ahead, by {horizon}"));
            return Ok(());
        }
        let mut leaves: Vec<(i64, Option<u32>)> = Vec::new();
        for id in tasks {
            let mine = self.leaves(id)?;
            if mine.len() > 1 || mine.first().is_some_and(|(l, _)| *l != id) {
                let title = task_title(self.conn, id);
                let ids: Vec<i64> = mine.iter().map(|(l, _)| *l).collect();
                self.env.adjusted(
                    Some(r),
                    "task",
                    json!(ids),
                    format!("{title} → its {} open part{} in order, since the day holds the work itself", ids.len(), if ids.len() == 1 { "" } else { "s" }),
                );
            }
            if mine.is_empty() {
                self.env.refused(r, format!("task {id} has nothing open left to place"));
                continue;
            }
            for leaf in mine {
                if !leaves.iter().any(|(k, _)| *k == leaf.0) {
                    leaves.push(leaf);
                }
            }
        }
        let mut cursor: Option<i64> = None;
        let mut slots = order::slots(self.conn, self.ctx.user_id, date).map_err(internal)?;
        let mut anchor = match before {
            None => None,
            Some(v) => self.before_target(r, v, &slots, date),
        };
        for (n, (leaf, size)) in leaves.iter().copied().enumerate() {
            let existing = slots.iter().position(|s| s.task_id == leaf);
            if existing.is_none() && slots.len() >= order::MAX_ITEMS {
                let left: Vec<String> = leaves[n..].iter().map(|(l, _)| task_title(self.conn, *l)).collect();
                self.env.warn(format!("{date} holds at most {} items; not placed: {}", order::MAX_ITEMS, left.join(", ")));
                break;
            }
            let start = match at {
                Some(At::Wall(_, w)) => Some(cursor.unwrap_or_else(|| minutes_of(w))),
                _ => None,
            };
            if let Some(why) = self.unseats(date, leaf, anchor, start, at.is_some() || before.is_some())? {
                self.env.refused(r, why);
                continue;
            }
            let block = self.live(existing.map(|i| &slots[i]))?;
            let mut op = if existing.is_some() { "unchanged" } else { "created" };
            let mut event_id = existing.and_then(|i| slots[i].event_id);
            match at {
                Some(At::Wall(_, w)) => {
                    let start = cursor.unwrap_or_else(|| minutes_of(w));
                    let len = match minutes {
                        Some(m) if leaves.len() == 1 => m,
                        _ => self.block_len(size)?,
                    };
                    if date == self.today && start < self.now_min() {
                        self.env.adjusted(Some(r), "at", Value::Null, format!("{} has gone by today, so it stays untimed", wall(start)));
                        if let Some(b) = &block {
                            order::drop_block(self.conn, Some(b.id)).map_err(internal)?;
                            event_id = None;
                            op = "updated";
                        }
                    } else {
                        match self.fit(date, start, len, block.as_ref().map(|b| b.id))? {
                            Some((fitted, passed)) => {
                                if !passed.is_empty() {
                                    self.env.adjusted(Some(r), "at", wall(fitted).into(), format!("moved past {}", passed.join(", ")));
                                }
                                let id = self.lay_block(date, leaf, block.as_ref().map(|b| b.id), fitted, len)?;
                                event_id = Some(id);
                                op = if existing.is_some() { "updated" } else { "created" };
                                cursor = Some(fitted + len);
                            }
                            None => {
                                self.env.adjusted(Some(r), "at", Value::Null, format!("no room is left on {date} after {}, so it stays untimed", wall(start)));
                            }
                        }
                    }
                }
                Some(At::Untimed) => {
                    if let Some(b) = &block {
                        order::drop_block(self.conn, Some(b.id)).map_err(internal)?;
                        event_id = None;
                        op = "updated";
                    }
                }
                Some(At::In(_)) => {
                    self.ignore(r, "at", "a task's at is a time of day; +N minutes is for a wake-up");
                }
                None => {
                    if let (Some(m), Some(b)) = (minutes, &block) {
                        let start = minutes_of(&b.wall);
                        let len = m;
                        if start + len <= END_OF_DAY {
                            self.lay_block(date, leaf, Some(b.id), start, len)?;
                            op = "updated";
                        }
                    } else if minutes.is_some() && n == 0 {
                        self.ignore(r, "minutes", "an untimed item runs its task's size; give at to make a block, or set minutes with task_save");
                    }
                }
            }
            let slot = Slot { task_id: leaf, event_id };
            let mut after = slots.clone();
            let last = matches!(before, Some(Value::String(word)) if is_end(word));
            if let Some(i) = existing {
                after.remove(i);
                if before.is_some() && op == "unchanged" {
                    op = "updated";
                }
            }
            let place = match (existing, anchor) {
                (Some(i), None) if !last => i.min(after.len()),
                (_, Some(a)) => after.iter().position(|s| s.task_id == a).unwrap_or(after.len()),
                _ => after.len(),
            };
            after.insert(place, slot);
            order::write(self.conn, self.ctx.user_id, date, &after).map_err(internal)?;
            slots = order::slots(self.conn, self.ctx.user_id, date).map_err(internal)?;
            if anchor.is_some() {
                anchor = slots.iter().skip_while(|s| s.task_id != leaf).nth(1).map(|s| s.task_id);
            }
            self.touch(date);
            placed.push((date, leaf));
            let mut saved = json!({ "ref": r.0, "id": format!("t{leaf}"), "title": path(self.conn, self.ctx.user_id, leaf), "op": op });
            if let Some(e) = event_id.and_then(|id| event(self.conn, self.ctx.user_id, id).ok().flatten()) {
                saved["at"] = e.wall.into();
                saved["end"] = e.end.into();
            }
            if date != self.today {
                saved["date"] = date.to_string().into();
            }
            self.env.saved.push(saved);
        }
        Ok(())
    }

    /// The task id an item is placed in front of, or why it is placed last.
    fn before_target(&mut self, r: &Ref, v: &Value, slots: &[Slot], date: Date) -> Option<i64> {
        if let Value::String(s) = v {
            if is_end(s) {
                return None;
            }
        }
        let on_day: Vec<(i64, String)> = slots.iter().map(|s| (s.task_id, task_title(self.conn, s.task_id))).collect();
        let found = match item_id(v) {
            Some(ItemId::Task(n) | ItemId::Either(n)) => on_day.iter().find(|(id, _)| *id == n).map(|(id, _)| *id),
            Some(ItemId::Event(n)) => event(self.conn, self.ctx.user_id, n).ok().flatten().and_then(|e| e.task).filter(|t| on_day.iter().any(|(id, _)| id == t)),
            Some(ItemId::Calendar(_)) => None,
            None => loose::text(v).and_then(|t| loose::by_title(&t, &on_day, "item").ok()),
        };
        if found.is_none() {
            self.ignore(r, "before", format!("{} is not a task item of {date}'s list; it went last", loose::text(v).unwrap_or_default()));
        }
        found
    }

    // --- wake-ups ---

    #[allow(clippy::too_many_arguments)]
    fn wake(&mut self, r: &Ref, date: Date, existing: Option<&EventRow>, prompt: Option<String>, mut o: Obj, zone: Option<&jiff::tz::TimeZone>) -> Result<(), ToolError> {
        let at = match o.take(&["at", "time", "when", "start"]) {
            Some(v) => match self.at(&v, date, zone) {
                Ok(At::Untimed) => {
                    self.ignore(r, "at", "a wake-up always has a time");
                    None
                }
                Ok(a) => Some(a),
                Err(why) => {
                    self.ignore(r, "at", why);
                    None
                }
            },
            None => None,
        };
        let ring = o.take(&["ring", "call"]).and_then(|v| loose::boolean(&v));
        let session_check = o.take(&["session_check", "session"]).and_then(|v| loose::boolean(&v)).unwrap_or(false);
        let thread = o.take(&["thread", "conversation", "conversation_id", "thread_id"]).and_then(|v| loose::id(&v));
        let task = o.take(&["task", "task_id"]);
        let cancel_event = o.take(&["event", "cancel_event"]);
        let cancel_on = o.take(&["cancel_on", "cancel_if", "until"]).and_then(|v| loose::text(&v));
        o.take(&["also", "kind"]);
        for field in o.rest() {
            let why = loose::unknown(&field, ELSEWHERE);
            self.ignore(r, &field, why);
        }
        if let Some(e) = existing {
            return self.rework_wake(r, e, prompt, at, ring);
        }
        let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) else {
            self.env.refused(r, "a wake-up says in wake what it follows up");
            return Ok(());
        };
        let Some(at) = at else {
            self.env.refused(r, "a wake-up needs at: HH:MM on its day, or +20min");
            return Ok(());
        };
        let task_id = match &task {
            Some(v) => self.task_target(r, v),
            None => None,
        };
        if task.is_some() && task_id.is_none() {
            return Ok(());
        }
        let event_ref = match cancel_event.as_ref().and_then(item_id) {
            Some(ItemId::Event(n) | ItemId::Either(n)) => Some(n),
            _ => None,
        };
        let cancel = match cancel_on.as_deref().map(|c| c.trim().to_lowercase()).as_deref() {
            None => None,
            Some("reply" | "replied" | "answer" | "answered") => Some(Cancel::Replied),
            Some("task" | "task_done" | "done") => {
                if task_id.is_none() {
                    self.ignore(r, "cancel_on", "cancel_on task needs task, the one whose finish calls it off");
                }
                task_id.map(Cancel::TaskDone)
            }
            Some("event" | "event_decided") => {
                if event_ref.is_none() {
                    self.ignore(r, "cancel_on", "cancel_on event needs event, the e id whose settling calls it off");
                }
                event_ref.map(Cancel::EventDecided)
            }
            Some(_) => {
                self.ignore(r, "cancel_on", "cancel_on is reply, task or event");
                None
            }
        };
        let (date, at_text) = match &at {
            At::Wall(d, w) => (*d, w.clone()),
            At::In(m) => (date, format!("+{m}min")),
            At::Untimed => unreachable!("an untimed at is ignored above"),
        };
        let lay = Lay {
            config_dir: self.ctx.config_dir,
            user_id: self.ctx.user_id,
            username: self.ctx.username,
            at: &at_text,
            prompt: prompt.trim(),
            date,
            cancel,
            conversation_id: thread,
            work_session_id: None,
            session_check,
            system: false,
            now: self.now,
        };
        let near = match triggers::aimed_at(&lay) {
            Ok(target) => triggers::near(self.conn, self.ctx.user_id, &self.tz, target).map_err(internal)?,
            Err(_) => None,
        };
        let laid = match triggers::lay(self.conn, &lay) {
            Ok(l) => l,
            Err(e) => {
                let why = self.refusal(e);
                self.env.refused(r, why);
                return Ok(());
            }
        };
        if ring == Some(true) {
            triggers::set_ring(self.conn, laid.event_id).map_err(internal)?;
        }
        if let Some(t) = task_id {
            self.conn
                .execute("INSERT OR IGNORE INTO event_tasks (event_id, task_id) VALUES (?1, ?2)", (laid.event_id, t))
                .map_err(internal)?;
        }
        if let Some(n) = near {
            self.env.warn(format!(
                "wake-up e{} is already laid for {} {} ({:?}), within {} minutes of {}; keep both, reword one, or take one off with day_remove",
                n.event_id, n.date, n.at, n.prompt, triggers::NEAR_MIN, r.0
            ));
        }
        let landed: String = self
            .conn
            .query_row("SELECT p.date FROM events e JOIN plans p ON p.id = e.plan_id WHERE e.id = ?1", [laid.event_id], |x| x.get(0))
            .map_err(internal)?;
        let mut saved = json!({ "ref": r.0, "id": format!("e{}", laid.event_id), "title": excerpt(prompt.trim()), "op": "created", "at": laid.at });
        if landed != self.today.to_string() {
            saved["date"] = landed.into();
        }
        if let Some(c) = laid.cancel_if {
            saved["cancel_if"] = c.into();
        }
        if ring == Some(true) {
            saved["ring"] = true.into();
        }
        self.env.saved.push(saved);
        Ok(())
    }

    fn refusal(&self, e: Refusal) -> String {
        match e {
            Refusal::CapReached { date, allowance, spent } => {
                if self.kind == SessionKind::Talk {
                    format!("{spent} of {date}'s {allowance} wake-ups are laid; ask the user, and once they agree pass extra_wakes {{n, reason}} with day_save")
                } else {
                    format!("{spent} of {date}'s {allowance} wake-ups are laid; only the user can raise that, in a chat")
                }
            }
            Refusal::Past { at } => format!("{at} has gone by; pick a time still ahead, or a later date"),
            Refusal::NotFound(m) | Refusal::Rejected(m) | Refusal::Internal(m) => m,
        }
    }

    fn rework_wake(&mut self, r: &Ref, e: &EventRow, prompt: Option<String>, at: Option<At>, ring: Option<bool>) -> Result<(), ToolError> {
        if !matches!(e.status.as_str(), "pending" | "snoozed") {
            self.env.refused(r, format!("wake-up e{} has already {}", e.id, if e.status == "fired" { "fired" } else { "been settled" }));
            return Ok(());
        }
        let mut changed = false;
        if let Some(p) = prompt.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()) {
            if p.len() > triggers::MAX_PROMPT_BYTES {
                self.ignore(r, "wake", format!("a wake-up's words are at most {} bytes", triggers::MAX_PROMPT_BYTES));
            } else if p != e.prompt {
                self.conn.execute("UPDATE events SET prompt = ?1 WHERE id = ?2", (&p, e.id)).map_err(internal)?;
                changed = true;
            }
        }
        let mut landed = (e.date, e.wall.clone());
        if let Some(at) = at {
            let (date, w) = match at {
                At::Wall(d, w) => (d, w),
                At::In(m) => {
                    let t = self.now.checked_add(jiff::Span::new().minutes(m)).map_err(internal)?;
                    crate::zone::civil(t, &self.tz)
                }
                At::Untimed => (e.date, e.wall.clone()),
            };
            if self.gone_by(date, &w) {
                self.ignore(r, "at", format!("{date} {w} has gone by; it stays at {}", e.wall));
            } else if (date, w.clone()) != (e.date, e.wall.clone()) {
                let plan_id = crate::plan::holding(self.conn, self.ctx.config_dir, self.ctx.username, self.ctx.user_id, date, self.today)
                    .map_err(internal)?;
                self.conn
                    .execute(
                        "UPDATE events SET plan_id = ?1, wall_time = ?2, orig_wall_time = ?2, status = 'pending' WHERE id = ?3",
                        (plan_id, &w, e.id),
                    )
                    .map_err(internal)?;
                landed = (date, w);
                changed = true;
            }
        }
        if let Some(ring) = ring {
            let channel = if ring { crate::channels::RING_CHANNEL } else { "push" };
            changed |= self.conn.execute("UPDATE events SET channel = ?1 WHERE id = ?2 AND channel != ?1", (channel, e.id)).map_err(internal)? > 0;
        }
        let prompt: String = self.conn.query_row("SELECT prompt FROM events WHERE id = ?1", [e.id], |x| x.get(0)).map_err(internal)?;
        let mut saved = json!({ "ref": r.0, "id": format!("e{}", e.id), "title": excerpt(&prompt), "op": if changed { "updated" } else { "unchanged" }, "at": landed.1 });
        if landed.0 != self.today {
            saved["date"] = landed.0.to_string().into();
        }
        self.env.saved.push(saved);
        Ok(())
    }

    // --- routines ---

    fn routine(&mut self, r: &Ref, date: Date, name: &str, mut o: Obj, zone: Option<&jiff::tz::TimeZone>) -> Result<(), ToolError> {
        let at = o.take(&["at", "time", "start"]);
        let flexibility = o.take(&["flexibility", "flex"]).and_then(|v| loose::text(&v));
        let window = o.take(&["slide_window_min", "window", "slide"]).and_then(|v| loose::integer(&v));
        o.take(&["channel", "kind"]);
        for field in o.rest() {
            let why = loose::unknown(&field, ELSEWHERE);
            self.ignore(r, &field, why);
        }
        if !matches!(self.kind, SessionKind::Talk | SessionKind::Nightly) {
            self.ignore(r, "routine", "routines are added in a chat or by the nightly run, not here");
            return Ok(());
        }
        let name = name.trim().to_string();
        if name.is_empty() || name.chars().count() > MAX_ROUTINE_CHARS {
            self.env.refused(r, format!("a routine's name is 1 to {MAX_ROUTINE_CHARS} characters"));
            return Ok(());
        }
        let (date, w) = match at.map(|v| self.at(&v, date, zone)) {
            Some(Ok(At::Wall(d, w))) => (d, w),
            Some(Ok(At::In(m))) => {
                let t = self.now.checked_add(jiff::Span::new().minutes(m)).map_err(internal)?;
                crate::zone::civil(t, &self.tz)
            }
            Some(Err(why)) => {
                self.env.refused(r, why);
                return Ok(());
            }
            Some(Ok(At::Untimed)) | None => {
                self.env.refused(r, "a routine needs at, HH:MM on its day");
                return Ok(());
            }
        };
        if self.gone_by(date, &w) {
            self.env.refused(r, format!("{date} {w} has gone by; put it later in the day"));
            return Ok(());
        }
        let flexibility = match flexibility.as_deref().map(|f| loose::choice(f, &["fixed", "slide", "drop"], &[("flexible", "slide"), ("move", "slide"), ("optional", "drop"), ("droppable", "drop"), ("skip", "drop")])) {
            None => "slide",
            Some(Some(f)) => f,
            Some(None) => {
                self.ignore(r, "flexibility", "flexibility is fixed, slide or drop; it is slide");
                "slide"
            }
        };
        let window = match (flexibility, window) {
            ("slide", None) => DEFAULT_WINDOW_MIN,
            ("slide", Some(n)) => n.clamp(0, MAX_WINDOW_MIN),
            _ => 0,
        };
        let mut start = minutes_of(&w);
        let inside = crate::calendar::overlaps(self.conn, self.ctx.user_id, date, &w, &w).map_err(internal)?;
        if let Some(o) = inside.iter().max_by_key(|o| minutes_of(&o.end)) {
            start = minutes_of(&o.end);
            self.env.adjusted(Some(r), "at", wall(start).into(), format!("moved past {} {}-{}", o.title, o.start, o.end));
        }
        let plan_id = crate::plan::holding(self.conn, self.ctx.config_dir, self.ctx.username, self.ctx.user_id, date, self.today)
            .map_err(internal)?;
        self.conn
            .execute(
                "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, flexibility, slide_window_min, channel, origin, created_at)
                 VALUES (?1, ?2, ?3, ?3, ?4, ?5, 'push', 'agent', ?6)",
                (plan_id, &name, wall(start), flexibility, window, self.now.to_string()),
            )
            .map_err(internal)?;
        let id = self.conn.last_insert_rowid();
        let mut saved = json!({ "ref": r.0, "id": format!("e{id}"), "title": name, "op": "created", "at": wall(start) });
        if date != self.today {
            saved["date"] = date.to_string().into();
        }
        self.env.saved.push(saved);
        Ok(())
    }

    fn rework_routine(&mut self, r: &Ref, ev: &EventRow, at: Option<Value>, snooze: Option<Value>, minutes: Option<i64>, zone: Option<&jiff::tz::TimeZone>) -> Result<(), ToolError> {
        let e = ev;
        if e.settled() {
            self.env.refused(r, format!("{} at {} is already {}; the user decided it", e.kind, e.wall, e.status));
            return Ok(());
        }
        let mut changed = false;
        if let Some(v) = snooze {
            match loose::minutes(&v).filter(|m| (1..=1440).contains(m)) {
                Some(m) => match crate::plan::snooze(self.conn, self.ctx.user_id, e.id, m) {
                    Ok(Some(())) => changed = true,
                    Ok(None) => self.ignore(r, "snooze_min", "a block has no delivery to hold back"),
                    Err(err) => self.ignore(r, "snooze_min", err.to_string()),
                },
                None => self.ignore(r, "snooze_min", "snooze_min is 1 to 1440"),
            }
        }
        if let Some(v) = at {
            match self.at(&v, e.date, zone) {
                Ok(At::Wall(day, clock)) if day == e.date => {
                    let target = minutes_of(&clock);
                    if let Some(end) = &e.end {
                        let len = minutes_of(end) - minutes_of(&e.wall);
                        let (from, to) = (wall(target), wall(target + len));
                        match crate::plan::reshape(self.conn, self.ctx.user_id, e.id, &from, &to) {
                            Ok(Some(())) => changed = true,
                            Ok(None) => self.ignore(r, "at", "not a block of this user's"),
                            Err(err) => self.env.refused(r, err.to_string()),
                        }
                    } else if e.flexibility == "fixed" {
                        self.env.refused(r, format!("{} is a fixed routine and stays at {}", e.kind, e.wall));
                    } else {
                        let orig = minutes_of(if e.orig.is_empty() { &e.wall } else { &e.orig });
                        let mut goal = target;
                        if e.window > 0 && (goal - orig).abs() > e.window {
                            goal = goal.clamp(orig - e.window, orig + e.window);
                            self.env.adjusted(Some(r), "at", wall(goal).into(), format!("{} slides at most {} minutes from {}", e.kind, e.window, wall(orig)));
                        }
                        let inside = crate::calendar::conflict(self.conn, self.ctx.user_id, e.date, &wall(goal), &wall(goal)).map_err(internal)?;
                        if let Some(why) = inside {
                            self.env.refused(r, format!("{} is {why}", wall(goal)));
                        } else {
                            match crate::plan::shift(self.conn, self.ctx.user_id, e.id, goal - minutes_of(&e.wall)) {
                                Ok(Some(())) => changed = true,
                                Ok(None) => self.env.refused(r, format!("{} cannot move", e.kind)),
                                Err(err) => self.env.refused(r, err.to_string()),
                            }
                        }
                    }
                }
                Ok(At::Wall(..)) => self.ignore(r, "at", "a routine moves within its own day; drop it with day_remove and add one there"),
                Ok(_) => self.ignore(r, "at", "a routine's at is HH:MM"),
                Err(why) => self.ignore(r, "at", why),
            }
        } else if let (Some(m), Some(_)) = (minutes, &e.end) {
            let start = minutes_of(&e.wall);
            match crate::plan::reshape(self.conn, self.ctx.user_id, e.id, &e.wall, &wall(start + m)) {
                Ok(Some(())) => changed = true,
                Ok(None) => self.ignore(r, "minutes", "not a block of this user's"),
                Err(err) => self.env.refused(r, err.to_string()),
            }
        } else if minutes.is_some() {
            self.ignore(r, "minutes", "a routine keeps its length; move it with at");
        }
        let wall_now: String = self.conn.query_row("SELECT wall_time FROM events WHERE id = ?1", [e.id], |x| x.get(0)).map_err(internal)?;
        self.env.saved.push(json!({ "ref": r.0, "id": format!("e{}", e.id), "title": e.kind, "op": if changed { "updated" } else { "unchanged" }, "at": wall_now }));
        Ok(())
    }

    // --- one item ---

    fn item(&mut self, r: &Ref, given: Value, top: Date, top_zone: Option<&jiff::tz::TimeZone>, placed: &mut Vec<(Date, i64)>) -> Result<(), ToolError> {
        let mut o = match given {
            Value::Object(m) => Obj::new(m),
            other => {
                let mut m = Map::new();
                m.insert("task".into(), other);
                Obj::new(m)
            }
        };
        let date = match o.take(&["date", "day", "on"]) {
            Some(v) => match day_arg(&v, self.today) {
                Ok(d) => d,
                Err(why) => {
                    self.ignore(r, "date", format!("{why}; read as {top}"));
                    top
                }
            },
            None => top,
        };
        let zone = match o.take(&["tz", "timezone"]) {
            Some(v) => self.zone(Some(r), Some(v)),
            None => top_zone.cloned(),
        };
        let hint = o.take(&["type", "kind"]).and_then(|v| loose::text(&v)).map(|k| k.trim().to_lowercase());
        let wake = o.take(&["wake", "prompt", "reminder", "remind", "message"]).and_then(|v| loose::text(&v));
        let routine = o.take(&["routine", "name"]).and_then(|v| loose::text(&v));
        let id = o.take(&["id", "event_id", "item", "item_id"]);
        let task = o.take(&["task", "task_id", "task_ids", "tasks", "leaf", "title"]);
        let target = id.as_ref().map(|v| (v.clone(), item_id(v)));
        if let Some((_, Some(ItemId::Calendar(n)))) = target {
            self.env.refused(r, format!("c{n} is a calendar entry; change it with calendar_save, or skip the day there"));
            return Ok(());
        }
        let as_event = |me: &Self, n: i64| event(me.conn, me.ctx.user_id, n);
        let wants_event = wake.is_some() || o.has(&["snooze_min", "snooze"]) || hint.as_deref().is_some_and(|h| matches!(h, "wake" | "routine" | "trigger" | "reminder"));
        let resolved_event = match target {
            Some((_, Some(ItemId::Event(n)))) => {
                let Some(e) = as_event(self, n)? else {
                    self.env.refused(r, format!("no event e{n} of this user's; day_find lists the day's ids"));
                    return Ok(());
                };
                Some(e)
            }
            Some((_, Some(ItemId::Either(n)))) if task.is_none() => {
                let is_task = !wants_event && crate::tasks::get(self.conn, self.ctx.user_id, n).map_err(internal)?.is_some();
                if is_task {
                    None
                } else {
                    match as_event(self, n)? {
                        Some(e) => Some(e),
                        None if crate::tasks::get(self.conn, self.ctx.user_id, n).map_err(internal)?.is_some() => None,
                        None => {
                            self.env.refused(r, format!("no task or event {n} of this user's"));
                            return Ok(());
                        }
                    }
                }
            }
            _ => None,
        };
        if let Some(e) = resolved_event {
            if e.is_wake() {
                return self.wake(r, e.date, Some(&e), wake, o, zone.as_ref());
            }
            if let Some(t) = e.task {
                let at = o.take(&["at", "time", "start", "when"]);
                let end = o.take(&["end", "end_time", "until"]);
                return self.task_item(r, e.date, vec![t], at, end, o, zone.as_ref(), placed);
            }
            let at = o.take(&["at", "time", "start", "when"]);
            let snooze = o.take(&["snooze_min", "snooze"]);
            let minutes = o.take(&["minutes", "duration_min", "duration", "length"]).and_then(|v| loose::minutes(&v)).filter(|m| *m > 0);
            for field in o.rest() {
                let why = loose::unknown(&field, ELSEWHERE);
                self.ignore(r, &field, why);
            }
            return self.rework_routine(r, &e, at, snooze, minutes, zone.as_ref());
        }
        if wake.is_some() || hint.as_deref().is_some_and(|h| matches!(h, "wake" | "trigger" | "reminder")) {
            if let Some(t) = task {
                o = with(o, "task", t);
            }
            return self.wake(r, date, None, wake, o, zone.as_ref());
        }
        if let Some(name) = routine {
            return self.routine(r, date, &name, o, zone.as_ref());
        }
        let task = task.or_else(|| match target {
            Some((v, Some(ItemId::Task(n) | ItemId::Either(n)))) => {
                let _ = v;
                Some(Value::from(n))
            }
            Some((v, None)) => Some(v),
            _ => None,
        });
        let Some(task) = task else {
            self.env.refused(r, "an item names a task (task), a wake-up (wake with at) or a routine (routine with at)");
            return Ok(());
        };
        let tasks: Vec<Value> = loose::list(task);
        let mut ids = Vec::new();
        for t in &tasks {
            if let Some(id) = self.task_target(r, t) {
                ids.push(id);
            }
        }
        if ids.is_empty() {
            return Ok(());
        }
        let at = o.take(&["at", "time", "start", "when", "scheduled_at"]);
        let end = o.take(&["end", "end_time"]);
        self.task_item(r, date, ids, at, end, o, zone.as_ref(), placed)
    }

    #[allow(clippy::too_many_arguments)]
    fn task_item(
        &mut self,
        r: &Ref,
        date: Date,
        ids: Vec<i64>,
        at: Option<Value>,
        end: Option<Value>,
        mut o: Obj,
        zone: Option<&jiff::tz::TimeZone>,
        placed: &mut Vec<(Date, i64)>,
    ) -> Result<(), ToolError> {
        let minutes = o.take(&["minutes", "duration_min", "duration", "length", "size"]).and_then(|v| match loose::minutes(&v) {
            Some(m) if m > 0 => Some(loose::round_minutes(m)),
            _ => None,
        });
        let before = o.take(&["before", "before_task_id", "before_id", "ahead_of"]);
        if let Some(v) = o.take(&["after", "after_task_id"]) {
            self.ignore(r, "after", format!("place it with before (the item it goes in front of), not after {}", loose::text(&v).unwrap_or_default()));
        }
        o.take(&["type", "kind"]);
        for field in o.rest() {
            let why = loose::unknown(&field, ELSEWHERE);
            self.ignore(r, &field, why);
        }
        let mut at = match at {
            Some(v) => match self.at(&v, date, zone) {
                Ok(a) => Some(a),
                Err(why) => {
                    self.ignore(r, "at", why);
                    None
                }
            },
            None => None,
        };
        let mut date = date;
        if let Some(At::Wall(d, _)) = &at {
            date = *d;
        }
        let mut minutes = minutes;
        if let (Some(At::Wall(_, s)), Some(v)) = (&at, end) {
            match loose::text(&v).and_then(|e| loose::wall_time(&e)) {
                Some(e) if minutes_of(&e) > minutes_of(s) => minutes = Some(minutes_of(&e) - minutes_of(s)),
                _ => self.ignore(r, "end", "end is HH:MM after at"),
            }
        }
        if before.is_some() && matches!(at, Some(At::Wall(..))) {
            self.ignore(r, "before", "a block sits at its time; before orders untimed items");
        }
        let before = if matches!(at, Some(At::Wall(..))) { None } else { before };
        if let Some(At::In(_)) = at {
            self.ignore(r, "at", "a task's at is a time of day; +N minutes is for a wake-up");
            at = None;
        }
        self.place(r, date, ids, at.as_ref(), minutes, before.as_ref(), placed)
    }
}

fn is_end(s: &str) -> bool {
    matches!(s.trim().to_lowercase().as_str(), "end" | "last" | "bottom")
}

fn with(o: Obj, key: &str, v: Value) -> Obj {
    let mut m = o.into_map();
    m.insert(key.into(), v);
    Obj::new(m)
}

fn excerpt(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 80 {
        return flat;
    }
    let mut out: String = flat.chars().take(79).collect();
    out.push('…');
    out
}

fn open_tasks(conn: &Connection, user_id: i64) -> Result<Vec<(i64, String)>, ToolError> {
    let mut stmt = conn
        .prepare_cached("SELECT id, title FROM tasks WHERE user_id = ?1 AND state IN ('open','in_progress') ORDER BY id")
        .map_err(internal)?;
    let rows = stmt
        .query_map([user_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .map_err(internal)?;
    Ok(rows)
}

/// `day_save`.
pub fn save(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, raw: &str) -> Result<Value, ToolError> {
    super::task_query::unscoped(ctx)?;
    let mut o = Obj::new(loose::args(raw)?);
    let mut day = Day::new(conn, ctx, kind)?;
    let mut top = super::trigger_ops::target_date(ctx, kind, day.now);
    if let Some(v) = o.take(&["date", "day", "on", "plan_date"]) {
        match day_arg(&v, day.today) {
            Ok(d) => top = d,
            Err(why) => day.env.ignored(None, "date", format!("{why}; read as {top}")),
        }
    }
    let zone_v = o.take(&["tz", "timezone", "zone"]);
    let zone = day.zone(None, zone_v);
    let replace = o.take(&["replace", "replace_all"]).and_then(|v| loose::boolean(&v)).unwrap_or(false);
    let carry = o.take(&["carry", "carry_over", "carry_rest"]).and_then(|v| loose::boolean(&v)).unwrap_or(false);
    let extra = o.take(&["extra_wakes", "extra", "budget"]);
    let (items, warning) = loose::items(&mut o, "items", &["tasks", "order", "list", "entries", "events", "blocks", "wakes", "task_ids", "plan"], ITEM_FIELDS);
    if let Some(w) = warning {
        day.env.warn(w);
    }
    for field in o.rest() {
        day.env.ignored(None, &field, loose::unknown(&field, ELSEWHERE));
    }
    if let Some(v) = extra {
        raise(&mut day, top, &v)?;
    }
    if carry {
        if kind == SessionKind::Nightly {
            day.env.ignored(None, "carry", "the nightly run plans the day ahead; carrying a day over is the user's call");
        } else {
            let carried = crate::plan::carry(conn, ctx.config_dir, ctx.username, ctx.user_id, top, day.now).map_err(internal)?;
            day.env.saved.push(json!({
                "ref": "carry", "op": "carried", "date": top.to_string(),
                "blocks": carried.blocks, "untimed": carried.untimed, "wakes_dropped": carried.dropped,
            }));
            day.touch(top);
        }
    }
    if items.is_empty() && !replace && !carry && day.env.saved.is_empty() {
        let gated = |i: &Value| i.get("ref").is_none() && matches!(i["field"].as_str(), Some("extra_wakes" | "carry"));
        if day.env.ignored.is_empty() {
            day.env.warn("nothing to save: items was empty");
        } else if day.env.refused.is_empty() && !day.env.ignored.iter().all(gated) {
            day.env.refuse_call("nothing was saved: the day's items go in items, a list");
        }
        return Ok(day.env.into_value());
    }
    let mut placed: Vec<(Date, i64)> = Vec::new();
    for (i, v) in items.into_iter().enumerate() {
        day.item(&Ref::top(i), v, top, zone.as_ref(), &mut placed)?;
    }
    if replace {
        replace_list(&mut day, top, &placed)?;
    }
    log_lists(&day)?;
    Ok(day.env.into_value())
}

/// `replace: true`: the task items placed on `date` in this call become its
/// whole list, in call order; the open ones left out leave with their blocks.
fn replace_list(day: &mut Day, date: Date, placed: &[(Date, i64)]) -> Result<(), ToolError> {
    let order_now = order::slots(day.conn, day.ctx.user_id, date).map_err(internal)?;
    let open = order::ids(day.conn, day.ctx.user_id, date).map_err(internal)?;
    let named: Vec<i64> = placed.iter().filter(|(d, _)| *d == date).map(|(_, t)| *t).collect();
    let mut after: Vec<Slot> = named.iter().filter_map(|t| order_now.iter().find(|s| s.task_id == *t).copied()).collect();
    let head = day.head(date)?;
    let mut gone = Vec::new();
    for s in &order_now {
        if named.contains(&s.task_id) {
            continue;
        }
        if !open.contains(&s.task_id) {
            after.push(*s);
        } else if head == Some(s.task_id) && day.held.contains(&s.task_id) && date == day.today {
            after.insert(0, *s);
            day.env.adjusted(None, "items", json!(format!("t{}", s.task_id)), "the task of the work session running now stays first");
        } else {
            order::drop_block(day.conn, s.event_id).map_err(internal)?;
            gone.push(s.task_id);
        }
    }
    order::write(day.conn, day.ctx.user_id, date, &after).map_err(internal)?;
    for t in gone {
        day.env.removed.push(json!({ "id": format!("t{t}"), "title": path(day.conn, day.ctx.user_id, t) }));
    }
    day.touch(date);
    Ok(())
}

/// `extra_wakes`: raises the day's wake-up budget, in a chat with the user only.
fn raise(day: &mut Day, date: Date, v: &Value) -> Result<(), ToolError> {
    if day.kind != SessionKind::Talk {
        day.env.ignored(None, "extra_wakes", "only a chat with the user raises the day's wake-ups, once they agree");
        return Ok(());
    }
    let n = match v {
        Value::Object(m) => m.get("n").or_else(|| m.get("extra")).or_else(|| m.get("count")).and_then(loose::integer),
        other => loose::integer(other),
    };
    let reason = v.get("reason").and_then(loose::text).map(|s| s.trim().to_string()).unwrap_or_default();
    let Some(n) = n.filter(|n| *n > 0) else {
        day.env.ignored(None, "extra_wakes", "extra_wakes is {n, reason}: how many more, and what the user agreed to");
        return Ok(());
    };
    if reason.is_empty() {
        day.env.ignored(None, "extra_wakes", "say in reason what the user agreed to");
        return Ok(());
    }
    let reason = loose::truncate(&reason, super::MAX_TEXT_BYTES).unwrap_or(reason);
    let Some(base) = triggers::allowance_of(day.ctx.config_dir, day.ctx.username) else {
        day.env.ignored(None, "extra_wakes", "the user set no daily limit; nothing to raise");
        return Ok(());
    };
    let raised = triggers::allowance_over(day.conn, Some(base), day.ctx.user_id, date)
        .map_err(internal)?
        .map_or(0, |a| a - base);
    let room = i64::from(base.saturating_sub(raised).min(triggers::MAX_EXTRA));
    if room == 0 {
        day.env.refuse_call(format!("{date} is already raised by {raised}; a day reaches at most twice its {base} wake-ups"));
        return Ok(());
    }
    let take = n.min(room);
    if take != n {
        day.env.adjusted(None, "extra_wakes", take.into(), format!("{date} can rise by {room} more at most"));
    }
    let extra = triggers::add_extra(day.conn, day.ctx.user_id, date, u32::try_from(take).unwrap_or(1)).map_err(internal)?;
    crate::log::record(day.conn, Some(day.ctx.user_id), "trigger_budget", &format!("+{take} on {date} ({reason})")).map_err(internal)?;
    let allowance = triggers::allowance(day.conn, day.ctx.config_dir, day.ctx.username, day.ctx.user_id, date).map_err(internal)?;
    let spent = triggers::spent(day.conn, day.ctx.user_id, date).map_err(internal)?;
    day.env.saved.push(json!({ "ref": "extra_wakes", "op": "raised", "date": date.to_string(), "extra": extra, "allowance": allowance, "spent": spent }));
    Ok(())
}

/// One `order_changed` line per day whose task list moved.
fn log_lists(day: &Day) -> Result<(), ToolError> {
    for date in &day.touched {
        let ids = order::ids(day.conn, day.ctx.user_id, *date).map_err(internal)?;
        let line = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(", ");
        crate::log::record(day.conn, Some(day.ctx.user_id), "order_changed", &format!("{date}: {line}")).map_err(internal)?;
    }
    Ok(())
}

// --- day_remove ---

/// `day_remove`.
pub fn remove(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, raw: &str) -> Result<Value, ToolError> {
    super::task_query::unscoped(ctx)?;
    let mut o = Obj::new(loose::args(raw)?);
    let mut day = Day::new(conn, ctx, kind)?;
    let mut date = super::trigger_ops::target_date(ctx, kind, day.now);
    if let Some(v) = o.take(&["date", "day", "on"]) {
        match day_arg(&v, day.today) {
            Ok(d) => date = d,
            Err(why) => day.env.ignored(None, "date", format!("{why}; read as {date}")),
        }
    }
    let ids = loose::ids(&mut o, &["ids", "id", "items", "task_ids", "task_id", "event_ids", "event_id", "tasks"], &mut day.env);
    for field in o.rest() {
        day.env.ignored(None, &field, "not a day_remove field");
    }
    if ids.is_empty() {
        loose::nothing_removed(&mut day.env);
        return Ok(day.env.into_value());
    }
    for (i, v) in ids.into_iter().enumerate() {
        let r = Ref::top(i);
        take_off(&mut day, &r, date, &v)?;
    }
    log_lists(&day)?;
    Ok(day.env.into_value())
}

fn take_off(day: &mut Day, r: &Ref, date: Date, named: &Value) -> Result<(), ToolError> {
    let slots = order::slots(day.conn, day.ctx.user_id, date).map_err(internal)?;
    let on_day = |t: i64| slots.iter().any(|s| s.task_id == t);
    let task = match item_id(named) {
        Some(ItemId::Calendar(num)) => {
            day.env.refused(r, format!("c{num} is a calendar entry; skip the day with calendar_save, or delete it with calendar_remove"));
            return Ok(());
        }
        Some(ItemId::Task(num)) => num,
        Some(ItemId::Either(num)) if on_day(num) => num,
        Some(ItemId::Either(num) | ItemId::Event(num)) => {
            let Some(ev) = event(day.conn, day.ctx.user_id, num)? else {
                day.env.refused(r, format!("nothing on {date} is {num}; day_find lists the day's ids"));
                return Ok(());
            };
            match ev.task {
                Some(held) if order::slots(day.conn, day.ctx.user_id, ev.date).map_err(internal)?.iter().any(|s| s.task_id == held) => {
                    return remove_task(day, r, ev.date, held);
                }
                _ => return remove_event(day, r, &ev),
            }
        }
        None => {
            let Some(title) = loose::text(named) else {
                day.env.refused(r, "an id is t412, e88 or the exact title of a task on the day");
                return Ok(());
            };
            let titled: Vec<(i64, String)> = slots.iter().map(|s| (s.task_id, task_title(day.conn, s.task_id))).collect();
            let Ok(found) = loose::by_title(&title, &titled, "item") else {
                day.env.refused(r, format!("no task item of {date} is titled {title:?}; day_find lists the day"));
                return Ok(());
            };
            day.env.adjusted(Some(r), "id", format!("t{found}").into(), format!("{title:?} is t{found}"));
            found
        }
    };
    remove_task(day, r, date, task)
}

fn remove_task(day: &mut Day, r: &Ref, date: Date, task: i64) -> Result<(), ToolError> {
    let slots = order::slots(day.conn, day.ctx.user_id, date).map_err(internal)?;
    let Some(slot) = slots.iter().find(|s| s.task_id == task).copied() else {
        day.env.warn(format!("task {task} was not on {date}'s list"));
        return Ok(());
    };
    if date == day.today && day.held.contains(&task) {
        day.env.refused(r, format!("task {task} is the work session running now; leave it on the day until it ends"));
        return Ok(());
    }
    order::drop_block(day.conn, slot.event_id).map_err(internal)?;
    let after: Vec<Slot> = slots.into_iter().filter(|s| s.task_id != task).collect();
    order::write(day.conn, day.ctx.user_id, date, &after).map_err(internal)?;
    day.touch(date);
    day.env.removed.push(json!({ "id": format!("t{task}"), "title": path(day.conn, day.ctx.user_id, task) }));
    Ok(())
}

fn remove_event(day: &mut Day, r: &Ref, e: &EventRow) -> Result<(), ToolError> {
    if e.settled() {
        day.env.warn(format!("e{} was already {}", e.id, e.status));
        return Ok(());
    }
    if e.is_wake() {
        if e.status == "fired" {
            day.env.refused(r, format!("wake-up e{} is firing now", e.id));
            return Ok(());
        }
        crate::plan::set_status(day.conn, day.ctx.user_id, e.id, "dropped").map_err(internal)?;
        crate::log::record(day.conn, Some(day.ctx.user_id), "trigger_dropped", &format!("event {}", e.id)).map_err(internal)?;
        day.env.removed.push(json!({ "id": format!("e{}", e.id), "title": excerpt(&e.prompt) }));
        return Ok(());
    }
    if e.flexibility != "drop" {
        day.env.refused(r, format!("{} at {} is a {} routine; only droppable ones come off the day", e.kind, e.wall, e.flexibility));
        return Ok(());
    }
    crate::plan::set_status(day.conn, day.ctx.user_id, e.id, "dropped").map_err(internal)?;
    day.env.removed.push(json!({ "id": format!("e{}", e.id), "title": e.kind }));
    Ok(())
}

// --- the old names ---

fn old(raw: &str) -> Result<Map<String, Value>, ToolError> {
    loose::args(raw)
}

fn moved(a: &mut Map<String, Value>, from: &str, to: &mut Map<String, Value>, key: &str) {
    if let Some(v) = a.remove(from).filter(|v| !v.is_null()) {
        to.insert(key.into(), v);
    }
}

fn rest_into(a: Map<String, Value>, to: &mut Map<String, Value>) {
    for (k, v) in a {
        to.entry(k).or_insert(v);
    }
}

fn task_ref(v: &Value) -> Value {
    match v {
        Value::Number(n) => format!("t{n}").into(),
        other => other.clone(),
    }
}

/// `plan_tasks`: consecutive blocks from `start`, as one item.
pub fn plan_tasks(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, raw: &str) -> Result<Value, ToolError> {
    let mut a = old(raw)?;
    let mut top = Map::new();
    moved(&mut a, "date", &mut top, "date");
    moved(&mut a, "tz", &mut top, "tz");
    let mut item = Map::new();
    moved(&mut a, "task_ids", &mut item, "task");
    moved(&mut a, "start", &mut item, "at");
    a.remove("gap_min");
    a.remove("end");
    rest_into(a, &mut item);
    top.insert("items".into(), json!([item]));
    save(conn, ctx, kind, &Value::Object(top).to_string())
}

pub fn plan_carry(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, raw: &str) -> Result<Value, ToolError> {
    let mut a = old(raw)?;
    a.insert("carry".into(), true.into());
    save(conn, ctx, kind, &Value::Object(a).to_string())
}

pub fn order_set(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, raw: &str) -> Result<Value, ToolError> {
    let mut a = old(raw)?;
    let ids = a.remove("task_ids").map(loose::list).unwrap_or_default();
    let items: Vec<Value> = ids.into_iter().map(|t| json!({ "task": t })).collect();
    let mut top = Map::new();
    top.insert("replace".into(), true.into());
    top.insert("items".into(), items.into());
    rest_into(a, &mut top);
    save(conn, ctx, kind, &Value::Object(top).to_string())
}

pub fn order_move(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, raw: &str) -> Result<Value, ToolError> {
    let mut a = old(raw)?;
    let mut item = Map::new();
    moved(&mut a, "task_id", &mut item, "task");
    match a.remove("before_task_id").filter(|v| !v.is_null()) {
        Some(b) => item.insert("before".into(), task_ref(&b)),
        None => item.insert("before".into(), "end".into()),
    };
    rest_into(a, &mut item);
    save(conn, ctx, kind, &json!({ "items": [item] }).to_string())
}

pub fn order_drop(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, raw: &str) -> Result<Value, ToolError> {
    let mut a = old(raw)?;
    let id = a.remove("task_id").map_or(Value::Null, |v| task_ref(&v));
    let mut top = Map::new();
    top.insert("ids".into(), json!([id]));
    rest_into(a, &mut top);
    remove(conn, ctx, kind, &Value::Object(top).to_string())
}

fn event_ref(v: &Value) -> Value {
    match v {
        Value::Number(n) => format!("e{n}").into(),
        other => other.clone(),
    }
}

pub fn schedule_move(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, raw: &str) -> Result<Value, ToolError> {
    let mut a = old(raw)?;
    let id = a.remove("event_id").unwrap_or(Value::Null);
    let mut item = Map::new();
    item.insert("id".into(), event_ref(&id));
    let snooze = a.remove("snooze").as_ref().and_then(loose::boolean).unwrap_or(false);
    let shift = a.remove("minutes").as_ref().and_then(loose::integer);
    match shift {
        Some(m) if snooze => {
            item.insert("snooze_min".into(), m.into());
        }
        Some(m) => {
            let at = loose::id(&id).and_then(|n| event(conn, ctx.user_id, n).ok().flatten()).map(|e| crate::templates::wall_add(&e.wall, m));
            if let Some(at) = at {
                item.insert("at".into(), at.into());
            }
        }
        None => {
            let start = a.remove("start").and_then(|v| loose::text(&v));
            let end = a.remove("end").and_then(|v| loose::text(&v));
            let current = loose::id(&id).and_then(|n| event(conn, ctx.user_id, n).ok().flatten());
            let s = start.or_else(|| current.as_ref().map(|e| e.wall.clone()));
            if let Some(s) = &s {
                item.insert("at".into(), s.clone().into());
            }
            let e = end.or_else(|| current.as_ref().and_then(|e| e.end.clone()));
            if let (Some(s), Some(e)) = (s, e) {
                let len = minutes_of(&e) - minutes_of(&s);
                if len > 0 {
                    item.insert("minutes".into(), len.into());
                }
            }
        }
    }
    rest_into(a, &mut item);
    save(conn, ctx, kind, &json!({ "items": [item] }).to_string())
}

pub fn schedule_drop(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, raw: &str) -> Result<Value, ToolError> {
    let mut a = old(raw)?;
    let id = a.remove("event_id").map_or(Value::Null, |v| event_ref(&v));
    a.remove("moved_to_event_id");
    let mut top = Map::new();
    top.insert("ids".into(), json!([id]));
    rest_into(a, &mut top);
    remove(conn, ctx, kind, &Value::Object(top).to_string())
}

pub fn schedule_insert(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, raw: &str) -> Result<Value, ToolError> {
    let mut a = old(raw)?;
    let mut top = Map::new();
    moved(&mut a, "date", &mut top, "date");
    moved(&mut a, "tz", &mut top, "tz");
    let mut item = Map::new();
    moved(&mut a, "kind", &mut item, "routine");
    moved(&mut a, "time", &mut item, "at");
    a.remove("channel");
    rest_into(a, &mut item);
    top.insert("items".into(), json!([item]));
    save(conn, ctx, kind, &Value::Object(top).to_string())
}

pub fn trigger_set(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, raw: &str) -> Result<Value, ToolError> {
    let mut a = old(raw)?;
    let mut top = Map::new();
    moved(&mut a, "date", &mut top, "date");
    moved(&mut a, "tz", &mut top, "tz");
    let mut item = Map::new();
    moved(&mut a, "prompt", &mut item, "wake");
    moved(&mut a, "task_id", &mut item, "task");
    moved(&mut a, "event_id", &mut item, "event");
    if let Some(id) = a.remove("replace").filter(|v| !v.is_null()) {
        item.insert("id".into(), event_ref(&id));
    }
    a.remove("also");
    rest_into(a, &mut item);
    top.insert("items".into(), json!([item]));
    save(conn, ctx, kind, &Value::Object(top).to_string())
}

pub fn trigger_budget(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, raw: &str) -> Result<Value, ToolError> {
    let mut a = old(raw)?;
    let mut top = Map::new();
    moved(&mut a, "date", &mut top, "date");
    let n = a.remove("extra").unwrap_or(Value::Null);
    let reason = a.remove("reason").unwrap_or(Value::Null);
    top.insert("extra_wakes".into(), json!({ "n": n, "reason": reason }));
    rest_into(a, &mut top);
    save(conn, ctx, kind, &Value::Object(top).to_string())
}

#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, PreparedVectors, SessionKind, ToolCtx};
    use rusqlite::Connection;
    use serde_json::{json, Value};

    /// A user whose clock reads midday, so 14:00 is ahead and 09:00 gone by.
    fn env() -> (Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aitest', 'x', 'member')", []).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("defaults")).unwrap();
        std::fs::write(
            tmp.path().join("defaults/user.toml"),
            format!(
                "display_name = \"X\"\ntimezone = \"{}\"\ntemplate = \"default\"\nnightly_time = \"22:00\"\ntriggers_per_day = 4\n",
                crate::triggers::midday_zone().iana_name().unwrap()
            ),
        )
        .unwrap();
        (conn, tmp)
    }

    fn ctx(tmp: &tempfile::TempDir) -> ToolCtx<'_> {
        ToolCtx {
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            user_id: 1,
            username: "aitest",
            vectors: PreparedVectors::default(),
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            share: None,
            share_thread: None,
        }
    }

    fn call(conn: &Connection, tmp: &tempfile::TempDir, kind: SessionKind, name: &str, args: impl std::fmt::Display) -> Value {
        dispatch(conn, &ctx(tmp), kind, name, &args.to_string()).unwrap()
    }

    fn talk(conn: &Connection, tmp: &tempfile::TempDir, name: &str, args: impl std::fmt::Display) -> Value {
        call(conn, tmp, SessionKind::Talk, name, args)
    }

    fn today() -> jiff::civil::Date {
        jiff::Timestamp::now().to_zoned(crate::triggers::midday_zone()).date()
    }

    fn task(conn: &Connection, tmp: &tempfile::TempDir, node: Value) -> i64 {
        let mut node = node;
        node["new"] = true.into();
        talk(conn, tmp, "task_save", json!({ "tasks": [node] }))["saved"][0]["id"].as_i64().unwrap()
    }

    fn list(conn: &Connection) -> Vec<(i64, Option<String>)> {
        crate::order::list(conn, 1, today()).unwrap().into_iter().map(|i| (i.task_id, i.at)).collect()
    }

    fn ids(conn: &Connection) -> Vec<i64> {
        list(conn).into_iter().map(|(t, _)| t).collect()
    }

    #[test]
    fn a_task_a_wake_up_points_at_is_not_on_the_day_and_placing_one_twice_updates_it() {
        let (conn, tmp) = env();
        let draft = task(&conn, &tmp, json!({ "title": "Draft", "minutes": 60 }));
        let wake = talk(&conn, &tmp, "day_save", json!({ "items": [{ "wake": "see whether the draft started", "at": "+90min", "task": draft }] }));
        assert_eq!(wake["ok"], true, "{wake}");
        assert!(ids(&conn).is_empty(), "a wake-up's link is not a place on the day");
        let out = call(&conn, &tmp, SessionKind::Trigger, "day_save", json!({ "items": [{ "task": draft }] }));
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["saved"][0]["op"], "created");
        let again = call(&conn, &tmp, SessionKind::Trigger, "day_save", json!({ "items": [{ "task": draft }] }));
        assert_eq!((again["ok"].clone(), again["saved"][0]["op"].clone()), (json!(true), json!("unchanged")), "{again}");
        let timed = talk(&conn, &tmp, "day_save", json!({ "items": [{ "task": "Draft", "at": "15:00" }] }));
        assert_eq!(timed["saved"][0]["op"], "updated", "{timed}");
        assert_eq!(list(&conn), [(draft, Some("15:00".to_string()))]);
        let old = call(&conn, &tmp, SessionKind::Trigger, "plan_tasks", json!({ "date": today().to_string(), "task_ids": [draft], "start": "16:00" }));
        assert_eq!(old["ok"], true, "{old}");
        assert_eq!(list(&conn), [(draft, Some("16:00".to_string()))], "the old name moves the block it already has");
        assert!(old["warnings"].to_string().contains("plan_tasks is now day_save"), "{old}");
    }

    #[test]
    fn a_parent_places_its_open_parts_in_order_and_says_so() {
        let (conn, tmp) = env();
        let lab = task(&conn, &tmp, json!({ "title": "Chem lab writeup", "children": [
            { "title": "Data table", "minutes": 30, "state": "done" },
            { "title": "Discussion", "minutes": 45 },
            { "title": "Conclusion", "minutes": 20 },
        ] }));
        let out = talk(&conn, &tmp, "day_save", json!({ "items": [{ "task": lab }] }));
        let leaves: Vec<i64> = (lab + 2..=lab + 3).collect();
        assert_eq!(ids(&conn), leaves);
        assert_eq!(out["adjusted"][0]["to"], json!(leaves), "{out}");
        assert!(out["adjusted"][0]["why"].as_str().unwrap().contains("Chem lab writeup → its 2 open parts"));
        assert_eq!(out["saved"][1]["title"], "Chem lab writeup › Conclusion");
        let timed = talk(&conn, &tmp, "day_save", json!({ "items": [{ "task": lab, "at": "14:00" }] }));
        assert_eq!(timed["ok"], true, "{timed}");
        assert_eq!(list(&conn), [(lab + 2, Some("14:00".into())), (lab + 3, Some("14:45".into()))], "one after the other");
    }

    #[test]
    fn replace_makes_the_whole_list_and_what_it_leaves_out_goes_with_its_block() {
        let (conn, tmp) = env();
        let (a, b, c, d) = (
            task(&conn, &tmp, json!({ "title": "a" })),
            task(&conn, &tmp, json!({ "title": "b" })),
            task(&conn, &tmp, json!({ "title": "c" })),
            task(&conn, &tmp, json!({ "title": "d" })),
        );
        talk(&conn, &tmp, "day_save", json!({ "items": [a, { "task": b, "at": "14:00" }, c, d] }));
        talk(&conn, &tmp, "task_save", json!({ "tasks": [{ "id": d, "state": "done" }] }));
        let out = talk(&conn, &tmp, "day_save", json!({ "replace": true, "items": [{ "task": c }, { "task": a }] }));
        assert_eq!(ids(&conn), [c, a]);
        assert_eq!(out["removed"][0]["id"], format!("t{b}"), "{out}");
        let block: String = conn.query_row("SELECT status FROM events WHERE end_wall_time IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(block, "dropped");
        let kept: i64 = conn.query_row("SELECT COUNT(*) FROM day_items WHERE task_id = ?1", [d], |r| r.get(0)).unwrap();
        assert_eq!(kept, 1, "a finished item stays on its day");
        let old = talk(&conn, &tmp, "order_set", json!({ "task_ids": { "item": [a, b] } }));
        assert_eq!(old["ok"], true, "{old}");
        assert_eq!(ids(&conn), [a, b], "a list wrapped in item is read as the list");
        talk(&conn, &tmp, "order_move", json!({ "task_id": c, "before_task_id": a }));
        assert_eq!(ids(&conn), [c, a, b]);
        talk(&conn, &tmp, "order_move", json!({ "task_id": c }));
        assert_eq!(ids(&conn), [a, b, c]);
        talk(&conn, &tmp, "order_drop", json!({ "task_id": a }));
        assert_eq!(ids(&conn), [b, c]);
    }

    #[test]
    fn a_block_steps_past_a_commitment_and_another_block_and_a_time_gone_by_stays_untimed() {
        let (conn, tmp) = env();
        talk(&conn, &tmp, "calendar_save", json!({ "entries": [{ "title": "Dentist", "kind": "fixed", "start": "14:00", "end": "15:00", "on": today().to_string() }] }));
        let (a, b, c) = (
            task(&conn, &tmp, json!({ "title": "a", "minutes": 30 })),
            task(&conn, &tmp, json!({ "title": "b", "minutes": 30 })),
            task(&conn, &tmp, json!({ "title": "c", "minutes": 30 })),
        );
        let out = talk(&conn, &tmp, "day_save", json!({ "items": [{ "task": a, "at": "14:30" }, { "task": b, "at": "15:10" }, { "task": c, "at": "09:00" }] }));
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(list(&conn), [(a, Some("15:00".into())), (b, Some("15:30".into())), (c, None)]);
        let why: Vec<&str> = out["adjusted"].as_array().unwrap().iter().map(|x| x["why"].as_str().unwrap()).collect();
        assert!(why[0].contains("Dentist") && why[1].contains("a 15:00-15:30") && why[2].contains("gone by"), "{why:?}");
        talk(&conn, &tmp, "day_save", json!({ "items": [{ "id": format!("t{a}"), "at": "" }] }));
        assert_eq!(list(&conn), [(a, None), (b, Some("15:30".into())), (c, None)]);
    }

    #[test]
    fn a_wake_up_beside_another_is_laid_with_a_warning_and_reworded_by_its_id() {
        let (conn, tmp) = env();
        let first = talk(&conn, &tmp, "day_save", json!({ "items": [{ "wake": "ask about the essay", "at": "+90min" }] }));
        let id = first["saved"][0]["id"].as_str().unwrap().to_string();
        let near = talk(&conn, &tmp, "day_save", json!({ "items": [{ "wake": "essay?", "at": "+100min" }] }));
        assert_eq!(near["ok"], true);
        assert!(near["warnings"][0].as_str().unwrap().contains(&format!("{id} is already laid")), "{near}");
        let out = talk(&conn, &tmp, "day_save", json!({ "items": [{ "id": id, "wake": "ask about the essay, sharper", "ring": true }] }));
        assert_eq!(out["saved"][0]["op"], "updated", "{out}");
        let (prompt, channel): (String, String) = conn
            .query_row("SELECT prompt, channel FROM events WHERE id = ?1", [id[1..].parse::<i64>().unwrap()], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((prompt.as_str(), channel.as_str()), ("ask about the essay, sharper", crate::channels::RING_CHANNEL));
        let day = talk(&conn, &tmp, "day_find", json!({}));
        let wakes: Vec<&Value> = day["items"].as_array().unwrap().iter().filter(|i| i["kind"] == "wake").collect();
        assert_eq!(wakes.len(), 2);
        assert_eq!(wakes[0]["ring"], true);
        let old = talk(&conn, &tmp, "trigger_set", json!({ "at": "+200min", "prompt": "did they answer?", "cancel_on": "reply" }));
        assert_eq!(old["saved"][0]["cancel_if"], "replied", "{old}");
    }

    #[test]
    fn the_budget_refuses_a_wake_up_until_the_user_agrees_and_only_a_chat_raises_it() {
        let (conn, tmp) = env();
        for n in 1..=4 {
            talk(&conn, &tmp, "day_save", json!({ "items": [{ "wake": format!("check {n}"), "at": format!("+{}min", 60 + n * 30) }] }));
        }
        let over = talk(&conn, &tmp, "day_save", json!({ "items": [{ "wake": "one more", "at": "+240min" }] }));
        assert_eq!(over["ok"], false);
        assert!(over["refused"][0]["why"].as_str().unwrap().contains("extra_wakes"), "{over}");
        let ignored = call(&conn, &tmp, SessionKind::Trigger, "day_save", json!({ "extra_wakes": { "n": 2, "reason": "x" }, "items": [] }));
        assert_eq!((ignored["ok"].clone(), ignored["ignored"][0]["field"].clone()), (json!(true), json!("extra_wakes")), "{ignored}");
        let out = talk(&conn, &tmp, "day_save", json!({ "extra_wakes": { "n": 9, "reason": "they asked to be pushed" }, "items": [{ "wake": "one more", "at": "+240min" }] }));
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["adjusted"][0]["to"], 4, "a day reaches at most twice its allowance");
        assert_eq!(out["saved"][0]["allowance"], 8);
        let old = talk(&conn, &tmp, "trigger_budget", json!({ "extra": 1, "reason": "again" }));
        assert_eq!(old["ok"], false, "{old}");
    }

    #[test]
    fn the_nightly_lays_on_the_day_it_plans_and_carries_nothing() {
        let (conn, tmp) = env();
        let a = task(&conn, &tmp, json!({ "title": "a" }));
        let out = call(&conn, &tmp, SessionKind::Nightly, "day_save", json!({ "carry": true, "items": [{ "task": a }, { "wake": "after the first block", "at": "09:00" }] }));
        assert_eq!(out["ignored"][0]["field"], "carry", "{out}");
        let tomorrow = today().tomorrow().unwrap();
        assert_eq!(out["saved"][0]["date"], tomorrow.to_string());
        assert_eq!(crate::order::ids(&conn, 1, tomorrow).unwrap(), [a]);
        assert_eq!(out["saved"][1]["date"], tomorrow.to_string(), "{out}");
    }

    #[test]
    fn routines_are_added_where_the_user_or_the_night_is_and_slide_inside_their_window() {
        let (conn, tmp) = env();
        let out = talk(&conn, &tmp, "day_save", json!({ "items": [{ "routine": "stretch", "at": "15:00", "flexibility": "slide" }] }));
        let id = out["saved"][0]["id"].as_str().unwrap().to_string();
        let moved = talk(&conn, &tmp, "day_save", json!({ "items": [{ "id": id, "at": "16:30" }] }));
        assert_eq!(moved["adjusted"][0]["to"], "15:30", "{moved}");
        let woken = call(&conn, &tmp, SessionKind::Trigger, "day_save", json!({ "items": [{ "routine": "walk", "at": "16:00" }] }));
        assert_eq!(woken["ignored"][0]["field"], "routine", "{woken}");
        let snoozed = talk(&conn, &tmp, "schedule_move", json!({ "event_id": id[1..].parse::<i64>().unwrap(), "minutes": 10, "snooze": true }));
        assert_eq!(snoozed["ok"], true, "{snoozed}");
        let fixed = talk(&conn, &tmp, "schedule_insert", json!({ "date": today().to_string(), "kind": "meds", "time": "17:00", "flexibility": "fixed", "channel": "push" }));
        let meds = fixed["saved"][0]["id"].clone();
        let refused = talk(&conn, &tmp, "day_remove", json!({ "ids": [meds] }));
        assert_eq!(refused["ok"], false, "a fixed routine stays: {refused}");
    }

    #[test]
    fn day_remove_takes_items_off_and_points_a_calendar_event_at_calendar_save() {
        let (conn, tmp) = env();
        let a = task(&conn, &tmp, json!({ "title": "Laundry" }));
        let wake = talk(&conn, &tmp, "day_save", json!({ "items": [{ "task": a, "at": "15:00" }, { "wake": "check", "at": "+90min" }] }));
        let e = wake["saved"][1]["id"].clone();
        let out = talk(&conn, &tmp, "day_remove", json!({ "ids": ["laundry", e, "c4"] }));
        assert_eq!(out["removed"].as_array().unwrap().len(), 2, "{out}");
        assert!(out["refused"][0]["why"].as_str().unwrap().contains("calendar_save"));
        assert!(ids(&conn).is_empty());
        assert_eq!(crate::tasks::get(&conn, 1, a).unwrap().unwrap().state, "open", "the task itself stays");
    }

    #[test]
    fn the_day_reads_as_one_list_in_time_order() {
        let (conn, tmp) = env();
        talk(&conn, &tmp, "calendar_save", json!({ "entries": [{ "title": "Swim", "kind": "busy", "start": "17:00", "end": "18:00", "on": today().to_string() }] }));
        let (a, b, c) = (
            task(&conn, &tmp, json!({ "title": "email", "minutes": 15 })),
            task(&conn, &tmp, json!({ "title": "essay", "minutes": 60 })),
            task(&conn, &tmp, json!({ "title": "laundry" })),
        );
        talk(&conn, &tmp, "day_save", json!({ "items": [a, { "task": b, "at": "14:00" }, c, { "wake": "how did the essay go?", "at": "16:00" }] }));
        let day = talk(&conn, &tmp, "day_find", json!({}));
        let kinds: Vec<String> = day["items"].as_array().unwrap().iter().map(|i| format!("{} {}", i["kind"].as_str().unwrap(), i["id"].as_str().unwrap_or(""))).collect();
        assert_eq!(kinds[..3], [format!("task t{a}"), format!("task t{b}"), format!("task t{c}")]);
        assert!(kinds[3].starts_with("wake e") && kinds[4].starts_with("event c"), "{kinds:?}");
        assert_eq!(day["items"][1]["minutes"], 60);
        let share = crate::shares::ShareScope { today: true, ..Default::default() };
        let visitor = ToolCtx { share: Some(share), share_thread: Some(1), ..ctx(&tmp) };
        let seen = dispatch(&conn, &visitor, SessionKind::Share, "day_find", "{}").unwrap();
        assert!(seen["items"].as_array().unwrap().iter().all(|i| i["kind"] != "wake"), "{seen}");
        let old = talk(&conn, &tmp, "plan_list", json!({}));
        assert_eq!(old["total"], day["total"]);
    }

    #[test]
    fn a_wake_up_leaves_the_head_of_the_day_to_the_session_running_now() {
        let (conn, tmp) = env();
        let (essay, laundry) = (task(&conn, &tmp, json!({ "title": "essay" })), task(&conn, &tmp, json!({ "title": "laundry" })));
        talk(&conn, &tmp, "day_save", json!({ "items": [essay, laundry] }));
        conn.execute(
            "INSERT INTO work_sessions (user_id, task_id, leaf_id, title, started_at) VALUES (1, ?1, ?1, 'essay', '2026-10-07T09:00:00Z')",
            [essay],
        )
        .unwrap();
        let out = call(&conn, &tmp, SessionKind::Trigger, "day_save", json!({ "items": [{ "task": laundry, "before": essay }] }));
        assert_eq!(out["ok"], false, "{out}");
        let out = call(&conn, &tmp, SessionKind::Trigger, "day_save", json!({ "replace": true, "items": [laundry] }));
        assert_eq!(ids(&conn), [essay, laundry], "{out}");
        let out = call(&conn, &tmp, SessionKind::Trigger, "day_remove", json!({ "ids": [essay] }));
        assert_eq!(out["ok"], false);
        let out = talk(&conn, &tmp, "day_save", json!({ "items": [{ "task": laundry, "before": essay }] }));
        assert_eq!(out["ok"], true, "the user may reorder it themselves");
        assert_eq!(ids(&conn), [laundry, essay]);
    }

    #[test]
    fn now_offers_a_block_only_once_it_is_near() {
        let (conn, tmp) = env();
        let (late, soon) = (task(&conn, &tmp, json!({ "title": "late" })), task(&conn, &tmp, json!({ "title": "soon" })));
        talk(&conn, &tmp, "day_save", json!({ "items": [{ "task": late, "at": "20:00" }, soon] }));
        let tz = crate::triggers::midday_zone();
        let offered = crate::order::candidates(&conn, 1, &tz, jiff::Timestamp::now(), 5).unwrap();
        assert_eq!(offered[0].task.task.id, soon);
        assert!(offered.iter().all(|e| e.reason != "order" || e.task.task.id != late));
    }

    #[test]
    fn carry_moves_the_rest_to_tomorrow_and_the_old_name_does_the_same() {
        let (conn, tmp) = env();
        let (a, b) = (task(&conn, &tmp, json!({ "title": "a" })), task(&conn, &tmp, json!({ "title": "b" })));
        talk(&conn, &tmp, "day_save", json!({ "items": [a, { "task": b, "at": "15:00" }] }));
        let out = talk(&conn, &tmp, "plan_carry", json!({}));
        assert_eq!(out["saved"][0]["op"], "carried", "{out}");
        let tomorrow = crate::order::list(&conn, 1, today().tomorrow().unwrap()).unwrap();
        assert_eq!(tomorrow.iter().map(|i| (i.task_id, i.at.clone())).collect::<Vec<_>>(), [(a, None), (b, Some("15:00".into()))]);
        assert!(ids(&conn).is_empty());
    }
}
