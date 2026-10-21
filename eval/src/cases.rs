use crate::checks::Fixture;
use crate::client::Client;
use anyhow::{bail, Context, Result};
use jiff::civil::Date;
use serde::Deserialize;
use serde_json::json;
use std::fmt::Write as _;
use std::path::Path;

pub const NAMES: [&str; 6] =
    ["visual-arts", "reschedule-day", "recurring-calendar", "checkin-follow-up", "nightly-plan", "breakdown-progress"];

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Talk,
    Trigger,
    Nightly,
}

impl Kind {
    /// The `agent_traces.kind` its sessions are written under.
    pub fn trace_kind(self) -> &'static str {
        match self {
            Kind::Talk => "Talk",
            Kind::Trigger => "Trigger",
            Kind::Nightly => "Nightly",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    #[serde(skip)]
    pub name: String,
    pub kind: Kind,
    /// The user's local hour the case runs at; the scratch user's zone is
    /// picked so that the clock reads it.
    pub local_hour: i8,
    #[serde(default)]
    pub request: String,
    #[serde(default)]
    pub prompt: String,
}

pub fn load(dir: &Path, name: &str) -> Result<Case> {
    let path = dir.join(format!("{name}.toml"));
    let raw = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut case: Case = toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    case.name = name.to_string();
    Ok(case)
}

/// `{+N}` becomes the `M/D` of N days after `today`.
pub fn render(text: &str, today: Date) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("{+") {
        out.push_str(&rest[..open]);
        let tail = &rest[open + 2..];
        if let Some((close, n)) = tail.find('}').and_then(|close| tail[..close].parse::<i64>().ok().map(|n| (close, n))) {
            let d = plus(today, n);
            let _ = write!(out, "{}/{}", d.month(), d.day());
            rest = &tail[close + 1..];
        } else {
            out.push_str("{+");
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

pub fn plus(d: Date, days: i64) -> Date {
    d.checked_add(jiff::Span::new().days(days)).unwrap_or(d)
}

/// A fixed-offset zone in which `now` reads as `hour` o'clock.
pub fn zone_for(hour: i8, now: jiff::Timestamp) -> String {
    let utc_hour = i32::from(now.to_zoned(jiff::tz::TimeZone::UTC).hour());
    let mut offset = i32::from(hour) - utc_hour;
    if offset > 14 {
        offset -= 24;
    }
    if offset < -12 {
        offset += 24;
    }
    match offset {
        0 => "Etc/UTC".to_string(),
        o if o > 0 => format!("Etc/GMT-{o}"),
        o => format!("Etc/GMT+{}", -o),
    }
}

/// The next December 12 on or after `today`.
pub fn next_dec_12(today: Date) -> Date {
    let this = jiff::civil::date(today.year(), 12, 12);
    if this >= today {
        this
    } else {
        jiff::civil::date(today.year() + 1, 12, 12)
    }
}

pub fn user_toml(case: &Case, zone: &str, now: jiff::Timestamp) -> String {
    let mut t = toml::toml! {
        display_name = "Sam"
        timezone = (zone.to_string())
        template = "eval"
        language = "en"
        close_day_time = ""
        day_start = ""
        idle_nudge_min = 0
    };
    match case.kind {
        Kind::Talk => {}
        Kind::Trigger => {
            t.insert("checkins_enabled".into(), true.into());
        }
        Kind::Nightly => {
            let tz = jiff::tz::TimeZone::get(zone).unwrap_or(jiff::tz::TimeZone::UTC);
            let at = now.to_zoned(tz).time().saturating_add(jiff::SignedDuration::from_mins(NIGHTLY_LEAD_MIN));
            t.insert("nightly_enabled".into(), true.into());
            t.insert("nightly_time".into(), format!("{:02}:{:02}", at.hour(), at.minute()).into());
        }
    }
    toml::to_string(&t).unwrap_or_default()
}

/// How far ahead of the scratch server's start the nightly run is set, which
/// is the time the seed has to finish in.
pub const NIGHTLY_LEAD_MIN: i64 = 3;

fn id_of(v: &serde_json::Value) -> Result<i64> {
    v["id"].as_i64().or_else(|| v["task"]["id"].as_i64()).with_context(|| format!("no id in {v}"))
}

fn task(c: &Client, body: &serde_json::Value) -> Result<i64> {
    id_of(&c.must("POST", "/api/tasks", body)?)
}

/// The day's plan row, made the way any day read makes it.
fn plan_id(c: &Client, db: &rusqlite::Connection, date: Date) -> Result<i64> {
    c.must_get(&format!("/api/day/{date}"))?;
    Ok(db.query_row("SELECT id FROM plans WHERE date = ?1 AND bare = 0", [date.to_string()], |r| r.get(0))?)
}

/// A timed block holding a task, as `plan_tasks` lays one.
fn block(db: &rusqlite::Connection, plan: i64, task: i64, title: &str, start: &str, end: &str, minutes: i64) -> Result<()> {
    db.execute(
        "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, end_wall_time,
                             flexibility, slide_window_min, channel, alert, span_min, origin)
         VALUES (?1, ?2, ?3, ?3, ?4, 'drop', 0, 'push', 0, ?5, 'agent')",
        (plan, title, start, end, minutes),
    )?;
    let event = db.last_insert_rowid();
    db.execute("INSERT INTO event_tasks (event_id, task_id) VALUES (?1, ?2)", (event, task))?;
    if crate::snapshot::has_table(db, "day_items")? {
        db.execute(
            "INSERT INTO day_items (user_id, date, position, task_id, event_id)
             SELECT p.user_id, p.date,
                    (SELECT COALESCE(MAX(d.position) + 1, 0) FROM day_items d WHERE d.user_id = p.user_id AND d.date = p.date),
                    ?2, ?3
             FROM plans p WHERE p.id = ?1
             ON CONFLICT (user_id, date, task_id) DO UPDATE SET event_id = excluded.event_id",
            (plan, task, event),
        )?;
    }
    Ok(())
}

pub fn seed(case: &Case, c: &Client, db: &rusqlite::Connection, zone: &str) -> Result<Fixture> {
    let tz = jiff::tz::TimeZone::get(zone).unwrap_or(jiff::tz::TimeZone::UTC);
    let today = jiff::Timestamp::now().to_zoned(tz.clone()).date();
    let mut fx = Fixture { today: Some(today), ..Default::default() };
    let tomorrow = plus(today, 1);
    fx.dates.insert("today".into(), today);
    fx.dates.insert("tomorrow".into(), tomorrow);
    let d = |n: i64| plus(today, n).to_string();
    match case.name.as_str() {
        "visual-arts" => {
            for (k, n) in [("unit1", 17), ("unit2", 29), ("unit3", 44), ("unit4", 58), ("unit5", 58), ("exam", 59)] {
                fx.dates.insert(k.into(), plus(today, n));
            }
        }
        "reschedule-day" => {
            let specs = [
                ("email", "Email advisor", 15),
                ("laundry", "Laundry", 30),
                ("reading", "Read chapter 4", 45),
                ("physics", "Physics problem set", 60),
                ("grocery", "Grocery run", 45),
                ("essay", "Essay draft", 60),
            ];
            let mut placed = Vec::new();
            for (key, title, min) in specs {
                let id = task(c, &json!({ "title": title, "duration_min": min }))?;
                fx.ids.insert(key.into(), id);
                placed.push(id);
            }
            c.must("PUT", "/api/order", &json!({ "task_ids": placed }))?;
            let plan = plan_id(c, db, today)?;
            block(db, plan, fx.ids["physics"], "Physics problem set", "14:00", "15:00", 60)?;
            block(db, plan, fx.ids["grocery"], "Grocery run", "16:30", "17:15", 45)?;
            block(db, plan, fx.ids["essay"], "Essay draft", "20:00", "21:00", 60)?;
            let dentist = c.must(
                "POST",
                "/api/calendar",
                &json!({ "title": "Dentist", "kind": "fixed", "start_time": "14:00", "end_time": "15:00", "on_date": tomorrow.to_string() }),
            )?;
            fx.lists.insert("calendar".into(), vec![id_of(&dentist)?]);
            fx.lists.insert("placed".into(), placed);
        }
        "recurring-calendar" => {
            let school = c.must(
                "POST",
                "/api/calendar",
                &json!({ "title": "School", "kind": "fixed", "start_time": "08:00", "end_time": "15:00", "days": 31 }),
            )?;
            fx.lists.insert("calendar".into(), vec![id_of(&school)?]);
            fx.dates.insert("until".into(), next_dec_12(today));
        }
        "checkin-follow-up" => {
            let lab = task(c, &json!({ "title": "Lab report", "due_on": d(3) }))?;
            let outline = task(c, &json!({ "title": "Outline", "parent_id": lab, "duration_min": 30, "state": "done" }))?;
            let draft = task(c, &json!({ "title": "Draft", "parent_id": lab, "duration_min": 90 }))?;
            let figures = task(c, &json!({ "title": "Figures", "parent_id": lab, "duration_min": 45 }))?;
            fx.ids.extend([("lab".into(), lab), ("outline".into(), outline), ("draft".into(), draft), ("figures".into(), figures)]);
            let plan = plan_id(c, db, today)?;
            let now = jiff::Timestamp::now();
            let local = now.to_zoned(tz.clone()).time();
            let wall = format!("{:02}:{:02}", local.hour(), local.minute());
            let laid = now.checked_sub(jiff::SignedDuration::from_hours(2))?;
            db.execute(
                "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, flexibility, slide_window_min,
                                     channel, origin, prompt, created_at)
                 VALUES (?1, 'trigger', ?2, ?2, 'drop', 0, 'push', 'agent', ?3, ?4)",
                (plan, &wall, &case.prompt, laid.to_string()),
            )?;
            let event = db.last_insert_rowid();
            db.execute("INSERT INTO event_tasks (event_id, task_id) VALUES (?1, ?2)", (event, draft))?;
            fx.ids.insert("wake".into(), event);
        }
        "nightly-plan" => {
            fx.dates.insert("target".into(), tomorrow);
            let chem = id_of(&c.must("POST", "/api/goals", &json!({ "title": "Pass chemistry", "due_on": d(20) }))?)?;
            let site = id_of(&c.must("POST", "/api/goals", &json!({ "title": "Ship portfolio site", "due_on": d(40) }))?)?;
            let root = |title: &str, goal: i64, urgency: &str, due: Option<i64>, min: Option<u32>| -> Result<i64> {
                let mut b = json!({ "title": title, "goal_id": goal, "urgency": urgency });
                if let Some(n) = due {
                    b["due_on"] = json!(d(n));
                }
                if let Some(m) = min {
                    b["duration_min"] = json!(m);
                }
                task(c, &b)
            };
            let leaf = |title: &str, parent: i64, min: u32| task(c, &json!({ "title": title, "parent_id": parent, "duration_min": min }));
            let lab = root("Chem lab writeup", chem, "high", Some(1), None)?;
            leaf("Data table", lab, 30)?;
            leaf("Discussion section", lab, 45)?;
            let pset = root("Chem problem set 5", chem, "normal", Some(2), Some(45))?;
            root("Review chem notes", chem, "low", None, Some(30))?;
            let plan = root("Chem exam study plan", chem, "normal", Some(12), None)?;
            leaf("List exam topics", plan, 20)?;
            leaf("Make flashcards", plan, 40)?;
            let landing = root("Portfolio landing page", site, "high", Some(6), None)?;
            leaf("Hero section", landing, 45)?;
            leaf("Contact form", landing, 30)?;
            root("Write case study", site, "normal", Some(15), Some(60))?;
            root("Pick color palette", site, "low", Some(25), Some(20))?;
            root("Set up hosting", site, "normal", None, Some(30))?;
            fx.lists.insert("pressing".into(), vec![lab, pset, landing]);
        }
        "breakdown-progress" => {
            let essay = task(c, &json!({ "title": "History essay", "duration_min": 90, "is_now": true, "due_on": d(4) }))?;
            fx.ids.insert("essay".into(), essay);
        }
        other => bail!("no seed for case {other}"),
    }
    Ok(fx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn relative_dates_render_as_month_and_day() {
        let today = date(2026, 10, 20);
        assert_eq!(render("due {+17} and {+59}.", today), "due 11/6 and 12/18.");
        assert_eq!(render("no {+x} here {", today), "no {+x} here {");
    }

    #[test]
    fn the_zone_puts_the_clock_at_the_hour() {
        for utc in [0, 5, 13, 23] {
            let now: jiff::Timestamp = format!("2026-10-20T{utc:02}:30:00Z").parse().unwrap();
            for hour in [9, 10, 20] {
                let zone = zone_for(hour, now);
                let tz = jiff::tz::TimeZone::get(&zone).unwrap();
                assert_eq!(now.to_zoned(tz).hour(), hour, "{zone} at {utc} UTC");
            }
        }
    }

    #[test]
    fn december_twelfth_rolls_over_once_passed() {
        assert_eq!(next_dec_12(date(2026, 10, 20)), date(2026, 12, 12));
        assert_eq!(next_dec_12(date(2026, 12, 13)), date(2027, 12, 12));
    }

    #[test]
    fn every_case_file_loads() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("cases");
        for name in NAMES {
            let case = load(&dir, name).unwrap();
            match case.kind {
                Kind::Talk => assert!(!case.request.is_empty(), "{name}"),
                Kind::Trigger => assert!(!case.prompt.is_empty(), "{name}"),
                Kind::Nightly => {}
            }
        }
    }

    #[test]
    fn the_syllabus_lists_forty_lessons() {
        let case = load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("cases"), "visual-arts").unwrap();
        let lessons = case.request.lines().filter(|l| l.starts_with("- ")).count();
        assert_eq!(lessons, 40);
    }

    #[test]
    fn a_nightly_case_sets_its_run_just_ahead() {
        let case = Case { name: "n".into(), kind: Kind::Nightly, local_hour: 20, request: String::new(), prompt: String::new() };
        let now: jiff::Timestamp = "2026-10-20T20:10:00Z".parse().unwrap();
        let t: toml::Value = toml::from_str(&user_toml(&case, "Etc/UTC", now)).unwrap();
        assert_eq!(t["nightly_time"].as_str(), Some("20:13"));
        assert_eq!(t["nightly_enabled"].as_bool(), Some(true));
    }
}
