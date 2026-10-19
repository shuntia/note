use crate::model_text as mt;
use crate::text::Lang;
use crate::triggers;
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

pub const ORIGIN: &str = "lay_day";
/// Activity before this belongs to the night before, not the new day.
pub const EARLIEST: &str = "05:00";

struct Seen {
    user_id: i64,
    username: String,
    category: String,
    last_active: Option<String>,
}

fn wall(t: &jiff::Zoned) -> String {
    format!("{:02}:{:02}", t.hour(), t.minute())
}

/// Lays the day-laying trigger for every user whose day has begun and has
/// none yet, and returns the events laid. One user's failure is logged and
/// skips only them.
pub fn check(conn: &Connection, config_dir: &Path, now: jiff::Timestamp) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT id, username, category, last_active_at FROM users WHERE disabled = 0")?;
    let users: Vec<Seen> = stmt
        .query_map([], |r| {
            Ok(Seen { user_id: r.get(0)?, username: r.get(1)?, category: r.get(2)?, last_active: r.get(3)? })
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut laid = Vec::new();
    for user in users {
        match check_one(conn, config_dir, &user, now) {
            Ok(Some(id)) => laid.push(id),
            Ok(None) => {}
            Err(e) => {
                crate::log::record_throttled(
                    conn,
                    Some(user.user_id),
                    "runner_error",
                    &format!("lay day: {e:#}"),
                    now,
                    crate::log::ERROR_LOG_WINDOW_MINS,
                )?;
            }
        }
    }
    Ok(laid)
}

fn check_one(conn: &Connection, config_dir: &Path, user: &Seen, now: jiff::Timestamp) -> Result<Option<i64>> {
    let Ok(cfg) = crate::config::UserConfig::load(config_dir, &user.username) else {
        return Ok(None);
    };
    let start = cfg.day_start();
    if start.is_empty() || !cfg.features(&user.category).checkins {
        return Ok(None);
    }
    let tz = crate::zone::named(&cfg.timezone);
    let local = now.to_zoned(tz.clone());
    let at = wall(&local);
    let close = cfg.close_day_time();
    if at.as_str() < EARLIEST || (!close.is_empty() && at.as_str() >= close) {
        return Ok(None);
    }
    let date = local.date();
    if laid_on(conn, user.user_id, date)? {
        return Ok(None);
    }
    let up_today = user
        .last_active
        .as_deref()
        .and_then(|s| s.parse::<jiff::Timestamp>().ok())
        .map(|t| t.to_zoned(tz.clone()))
        .is_some_and(|t| t.date() == date && wall(&t).as_str() >= EARLIEST);
    if !up_today && at.as_str() < start {
        return Ok(None);
    }
    let plan_id = crate::plan::ensure(conn, config_dir, &user.username, user.user_id, date)?;
    let id = triggers::insert(
        conn,
        plan_id,
        &at,
        mt::lay_day_prompt(Lang::for_user(config_dir, &user.username)),
        ORIGIN,
        None,
        None,
        None,
        now,
    )?;
    crate::log::record(conn, Some(user.user_id), "trigger_laid", &format!("event {id} at {date} {at}: lay the day"))?;
    Ok(Some(id))
}

/// Whether the day already holds its `lay_day` trigger, whatever became of it;
/// one that never decided is retried by `triggers::reclaim`, not laid again.
fn laid_on(conn: &Connection, user_id: i64, date: jiff::civil::Date) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND p.date = ?2 AND e.kind = ?3 AND e.origin = ?4)",
        (user_id, date.to_string(), triggers::KIND, ORIGIN),
        |r| r.get(0),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Busy {
    Quiet { title: String, until: String },
    Event { title: String, until: String },
    Working { title: String },
    Night { start: String, end: String },
}

impl Busy {
    pub fn reason(&self) -> String {
        match self {
            Busy::Quiet { title, until } => format!("a quiet window ({title}) runs until {until}"),
            Busy::Event { title, until } => format!("{title} is on until {until}"),
            Busy::Working { title } => format!("a work session on {title} is running"),
            Busy::Night { start, end } => format!("it is outside the user's day ({start} to {end})"),
        }
    }
}

/// Why now is not a moment to call, if it is not. `day` is the user's
/// `day_start` and `close_day_time`; either may be blank.
pub fn seems_busy(
    conn: &Connection,
    user_id: i64,
    tz: &jiff::tz::TimeZone,
    day: (&str, &str),
    now: jiff::Timestamp,
) -> Result<Option<Busy>> {
    if let Some(w) = crate::calendar::quiet_window(conn, user_id, tz, now)? {
        return Ok(Some(Busy::Quiet { title: w.title, until: w.end }));
    }
    let local = now.to_zoned(tz.clone());
    let at = wall(&local);
    if let Some(o) = crate::calendar::occurrences(conn, user_id, local.date())?.into_iter().find(|o| {
        matches!(o.kind.as_str(), "fixed" | "busy") && o.start.as_str() <= at.as_str() && at.as_str() < o.end.as_str()
    }) {
        return Ok(Some(Busy::Event { title: o.title, until: o.end }));
    }
    if let Some(s) = triggers::working(conn, user_id)? {
        return Ok(Some(Busy::Working { title: s.title }));
    }
    let night = at.as_str() < day.0 || (!day.1.is_empty() && at.as_str() >= day.1);
    let start = if day.0.is_empty() { "00:00" } else { day.0 };
    let end = if day.1.is_empty() { "24:00" } else { day.1 };
    Ok(night.then(|| Busy::Night { start: start.into(), end: end.into() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-07 is a Wednesday.
    fn env(extra: &str) -> (Connection, tempfile::TempDir, i64) {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("defaults/user.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, format!("display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n{extra}"))
            .unwrap();
        (conn, tmp, uid)
    }

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    fn seen(conn: &Connection, uid: i64, ts: &str) {
        conn.execute("UPDATE users SET last_active_at = ?1 WHERE id = ?2", (ts, uid)).unwrap();
    }

    fn laid(conn: &Connection) -> Vec<(String, String, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT p.date, e.wall_time, e.origin FROM events e JOIN plans p ON p.id = e.plan_id
                 WHERE e.kind = 'trigger' ORDER BY e.id",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn the_day_is_laid_once_at_the_first_sign_of_the_user() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-10-07T07:10:00Z");
        assert_eq!(check(&conn, tmp.path(), at("2026-10-07T07:11:00Z")).unwrap().len(), 1);
        assert!(check(&conn, tmp.path(), at("2026-10-07T09:00:00Z")).unwrap().is_empty());
        assert_eq!(laid(&conn), [("2026-10-07".into(), "07:11".into(), "lay_day".into())]);
        let date: jiff::civil::Date = "2026-10-07".parse().unwrap();
        assert_eq!(crate::triggers::spent(&conn, uid, date).unwrap(), 0, "it costs the day nothing");
    }

    fn fire(conn: &Connection, id: i64, ts: &str, decided: bool) {
        conn.execute(
            "UPDATE events SET status = ?1, fired_at = ?2, decided_at = ?3 WHERE id = ?4",
            (if decided { "done" } else { "fired" }, ts, decided.then_some(ts), id),
        )
        .unwrap();
    }

    #[test]
    fn an_undecided_lay_day_is_not_laid_again() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-10-07T07:10:00Z");
        let first = check(&conn, tmp.path(), at("2026-10-07T07:11:00Z")).unwrap();
        fire(&conn, first[0], "2026-10-07T07:11:30Z", false);
        assert!(check(&conn, tmp.path(), at("2026-10-07T07:30:00Z")).unwrap().is_empty());
        assert_eq!(laid(&conn).len(), 1);
    }

    #[test]
    fn a_decided_lay_day_is_not_laid_again() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-10-07T07:10:00Z");
        let first = check(&conn, tmp.path(), at("2026-10-07T07:11:00Z")).unwrap();
        fire(&conn, first[0], "2026-10-07T07:11:00Z", true);
        assert!(check(&conn, tmp.path(), at("2026-10-07T07:12:00Z")).unwrap().is_empty());
    }

    #[test]
    fn with_no_sign_of_the_user_the_day_is_laid_at_its_start() {
        let (conn, tmp, _uid) = env("");
        assert!(check(&conn, tmp.path(), at("2026-10-07T07:59:00Z")).unwrap().is_empty());
        assert_eq!(check(&conn, tmp.path(), at("2026-10-07T08:00:00Z")).unwrap().len(), 1);
        assert_eq!(check(&conn, tmp.path(), at("2026-10-08T08:00:00Z")).unwrap().len(), 1, "a new day lays its own");
    }

    #[test]
    fn activity_before_five_belongs_to_the_night_before() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-10-07T00:30:00Z");
        assert!(check(&conn, tmp.path(), at("2026-10-07T00:31:00Z")).unwrap().is_empty());
        assert!(check(&conn, tmp.path(), at("2026-10-07T04:59:00Z")).unwrap().is_empty());
        assert!(check(&conn, tmp.path(), at("2026-10-07T06:00:00Z")).unwrap().is_empty());
        seen(&conn, uid, "2026-10-07T05:00:00Z");
        assert_eq!(check(&conn, tmp.path(), at("2026-10-07T05:00:00Z")).unwrap().len(), 1);
    }

    #[test]
    fn the_close_of_the_day_a_blank_start_or_no_checkins_lay_nothing() {
        let (late, late_cfg, _) = env("");
        assert!(check(&late, late_cfg.path(), at("2026-10-07T21:30:00Z")).unwrap().is_empty());
        let (off, off_cfg, _) = env("day_start = \"\"\n");
        assert!(check(&off, off_cfg.path(), at("2026-10-07T12:00:00Z")).unwrap().is_empty());
        let (test, test_cfg, _) = env("");
        crate::auth::set_category(&test, "aki", "test").unwrap();
        assert!(check(&test, test_cfg.path(), at("2026-10-07T12:00:00Z")).unwrap().is_empty());
    }

    #[test]
    fn a_day_laid_inside_a_quiet_window_fires_at_its_end() {
        let (conn, tmp, uid) = env("");
        crate::calendar::create(&conn, uid, crate::calendar::Fields {
            title: "school".into(), kind: "fixed".into(), quiet: Some(true),
            start_time: "07:00".into(), end_time: "09:00".into(),
            days: Some(crate::calendar::day_mask(&["wed"]).unwrap()), ..Default::default()
        })
        .unwrap();
        check(&conn, tmp.path(), at("2026-10-07T08:00:00Z")).unwrap();
        assert!(crate::runner::fire_due(&conn, tmp.path(), at("2026-10-07T08:01:00Z")).unwrap().is_empty());
        assert_eq!(laid(&conn)[0].1, "09:00");
        let fired = crate::runner::fire_due(&conn, tmp.path(), at("2026-10-07T09:00:00Z")).unwrap();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].kind, crate::triggers::KIND);
    }

    fn noon() -> jiff::Timestamp {
        at("2026-10-07T12:00:00Z")
    }

    fn busy(conn: &Connection, uid: i64) -> Option<Busy> {
        seems_busy(conn, uid, &jiff::tz::TimeZone::UTC, ("08:00", "21:30"), noon()).unwrap()
    }

    fn entry(conn: &Connection, uid: i64, kind: &str, quiet: bool) {
        crate::calendar::create(conn, uid, crate::calendar::Fields {
            title: "class".into(), kind: kind.into(), quiet: Some(quiet),
            start_time: "11:00".into(), end_time: "13:00".into(),
            days: Some(crate::calendar::day_mask(&["wed"]).unwrap()), ..Default::default()
        })
        .unwrap();
    }

    #[test]
    fn a_user_here_with_nothing_on_is_free() {
        let (conn, _tmp, uid) = env("");
        seen(&conn, uid, "2026-10-07T11:00:00Z");
        assert_eq!(busy(&conn, uid), None);
        entry(&conn, uid, "free", false);
        entry(&conn, uid, "note", false);
        assert_eq!(busy(&conn, uid), None, "free time and notes are not commitments");
    }

    #[test]
    fn a_user_away_from_the_app_is_free_but_not_outside_their_day() {
        let (conn, _tmp, uid) = env("");
        assert_eq!(busy(&conn, uid), None, "being away is when a call reaches them");
        let at = |day| seems_busy(&conn, uid, &jiff::tz::TimeZone::UTC, day, noon()).unwrap();
        let night = |start: &str, end: &str| Some(Busy::Night { start: start.into(), end: end.into() });
        assert_eq!(at(("12:01", "21:30")), night("12:01", "21:30"));
        assert_eq!(at(("08:00", "12:00")), night("08:00", "12:00"));
        assert_eq!(at(("", "")), None);
        assert_eq!(at(("13:00", "")), night("13:00", "24:00"));
    }

    #[test]
    fn work_events_and_quiet_windows_each_make_the_user_busy() {
        let (conn, _tmp, uid) = env("");
        seen(&conn, uid, "2026-10-07T11:59:00Z");
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, started_at) VALUES (?1, 'essay', '2026-10-07T11:30:00Z')",
            [uid],
        )
        .unwrap();
        assert_eq!(busy(&conn, uid), Some(Busy::Working { title: "essay".into() }));
        entry(&conn, uid, "busy", false);
        assert_eq!(busy(&conn, uid), Some(Busy::Event { title: "class".into(), until: "13:00".into() }));
        entry(&conn, uid, "fixed", true);
        assert!(matches!(busy(&conn, uid), Some(Busy::Quiet { .. })));
    }

    #[test]
    fn a_paused_session_is_not_work() {
        let (conn, _tmp, uid) = env("");
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, started_at, paused_at)
             VALUES (?1, 'essay', '2026-10-07T11:00:00Z', '2026-10-07T11:10:00Z')",
            [uid],
        )
        .unwrap();
        assert_eq!(busy(&conn, uid), None);
    }
}
