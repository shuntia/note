use crate::triggers::{self, Cancel};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;
use std::fmt::Write as _;

pub const ORIGIN: &str = "idle";

pub const PROMPT: &str = "The user has gone quiet with notes still open. Decide whether one \
     of them is worth a nudge now.";

struct Seen {
    user_id: i64,
    username: String,
    category: String,
    last_active: String,
}

/// Lays an idle trigger for every user who has gone quiet with open notes and
/// returns the events laid. One user's failure is logged and skips only them.
pub fn check(conn: &Connection, config_dir: &Path, now: jiff::Timestamp) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT id, username, category, last_active_at FROM users
         WHERE disabled = 0 AND last_active_at IS NOT NULL",
    )?;
    let seen: Vec<Seen> = stmt
        .query_map([], |r| {
            Ok(Seen {
                user_id: r.get(0)?,
                username: r.get(1)?,
                category: r.get(2)?,
                last_active: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut laid = Vec::new();
    for user in seen {
        match check_one(conn, config_dir, &user, now) {
            Ok(Some(id)) => laid.push(id),
            Ok(None) => {}
            Err(e) => {
                crate::log::record_throttled(
                    conn,
                    Some(user.user_id),
                    "runner_error",
                    &format!("idle check: {e:#}"),
                    now,
                    crate::log::ERROR_LOG_WINDOW_MINS,
                )?;
            }
        }
    }
    Ok(laid)
}

fn check_one(
    conn: &Connection,
    config_dir: &Path,
    user: &Seen,
    now: jiff::Timestamp,
) -> Result<Option<i64>> {
    let Ok(cfg) = crate::config::UserConfig::load(config_dir, &user.username) else {
        return Ok(None);
    };
    let threshold = i64::from(cfg.idle_nudge_min());
    if threshold == 0 || !cfg.features(&user.category).checkins {
        return Ok(None);
    }
    if triggers::open_work_session(conn, user.user_id)?.is_some() {
        return Ok(None);
    }
    let tz = jiff::tz::TimeZone::get(&cfg.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
    let local = now.to_zoned(tz.clone());
    let last: jiff::Timestamp = user.last_active.parse()?;
    if last.to_zoned(tz.clone()).date() != local.date() {
        return Ok(None);
    }
    let since = match last_idle_laid(conn, user.user_id)? {
        Some(laid) if laid > last => laid,
        _ => last,
    };
    if now.as_second() - since.as_second() < threshold * 60 {
        return Ok(None);
    }
    if idle_pending(conn, user.user_id)? {
        return Ok(None);
    }
    if crate::calendar::quiet_window(conn, user.user_id, &tz, now)?.is_some() {
        return Ok(None);
    }
    let wall = format!("{:02}:{:02}", local.hour(), local.minute());
    let close = cfg.close_day_time();
    if !close.is_empty() && wall.as_str() >= close {
        return Ok(None);
    }
    if open_notes(conn, user.user_id)? == 0 {
        return Ok(None);
    }
    let date = local.date();
    let allowance = triggers::allowance(conn, config_dir, &user.username, user.user_id, date)?;
    if triggers::spent(conn, user.user_id, date)? >= allowance {
        return Ok(None);
    }
    let plan_id = crate::plan::ensure(conn, config_dir, &user.username, user.user_id, date)?;
    let id = triggers::insert(
        conn,
        plan_id,
        &wall,
        PROMPT,
        ORIGIN,
        Some(Cancel::Active),
        None,
        None,
        now,
    )?;
    crate::log::record(
        conn,
        Some(user.user_id),
        "trigger_laid",
        &format!("event {id} at {date} {wall}: idle"),
    )?;
    Ok(Some(id))
}

fn last_idle_laid(conn: &Connection, user_id: i64) -> rusqlite::Result<Option<jiff::Timestamp>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT MAX(e.created_at) FROM events e JOIN plans p ON p.id = e.plan_id
             WHERE p.user_id = ?1 AND e.kind = ?2 AND e.origin = ?3",
            (user_id, triggers::KIND, ORIGIN),
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(raw.and_then(|s| s.parse().ok()))
}

fn idle_pending(conn: &Connection, user_id: i64) -> rusqlite::Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND e.kind = ?2 AND e.origin = ?3
           AND e.status IN ('pending', 'snoozed', 'fired')",
        (user_id, triggers::KIND, ORIGIN),
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn open_notes(conn: &Connection, user_id: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM notes WHERE user_id = ?1 AND done_at IS NULL AND pinned = 0",
        [user_id],
        |r| r.get(0),
    )
}

fn ago(ts: &str, now: jiff::Timestamp) -> String {
    let Ok(t) = ts.parse::<jiff::Timestamp>() else {
        return ts.to_string();
    };
    let min = (now.as_second() - t.as_second()).max(0) / 60;
    match min {
        0..=59 => format!("{min} min"),
        60..=2879 => format!("{} h", min / 60),
        _ => format!("{} d", min / 1440),
    }
}

/// What an idle trigger's session reads under its prompt: how long the user
/// has been quiet, then each open unpinned note with its id, age and last nudge.
pub fn context(conn: &Connection, user_id: i64, now: jiff::Timestamp) -> rusqlite::Result<String> {
    let mut s = String::new();
    if let Some(at) = crate::presence::last_active(conn, user_id)? {
        let _ = writeln!(
            s,
            "Nothing from the user for {} min.",
            (now.as_second() - at.as_second()).max(0) / 60
        );
    }
    s.push_str("Open notes (id: text, age, last nudge):\n");
    let mut stmt = conn.prepare(
        "SELECT id, text, created_at, last_nudged_at FROM notes
         WHERE user_id = ?1 AND done_at IS NULL AND pinned = 0
         ORDER BY created_at, id LIMIT 20",
    )?;
    let rows = stmt.query_map([user_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    for row in rows {
        let (id, text, created, nudged) = row?;
        let nudge = match nudged {
            Some(t) => format!("nudged {} ago", ago(&t, now)),
            None => "never nudged".to_string(),
        };
        let _ = writeln!(s, "- {id}: {text:?}, added {} ago, {nudge}", ago(&created, now));
    }
    Ok(s)
}

/// Marks the notes a nudge named; ids that are not this user's open notes are
/// ignored. Returns how many were stamped.
pub fn stamp_nudged(
    conn: &Connection,
    user_id: i64,
    ids: &[i64],
    now: jiff::Timestamp,
) -> rusqlite::Result<usize> {
    let mut n = 0;
    for id in ids {
        n += conn.execute(
            "UPDATE notes SET last_nudged_at = ?1
             WHERE id = ?2 AND user_id = ?3 AND done_at IS NULL",
            (now.to_string(), id, user_id),
        )?;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09-30 is a Wednesday.
    fn env(extra: &str) -> (Connection, tempfile::TempDir, i64) {
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

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    fn seen(conn: &Connection, uid: i64, ts: &str) {
        conn.execute("UPDATE users SET last_active_at = ?1 WHERE id = ?2", (ts, uid)).unwrap();
    }

    fn note(conn: &Connection, uid: i64, text: &str, pinned: bool) -> i64 {
        conn.execute(
            "INSERT INTO notes (user_id, text, pinned, created_at)
             VALUES (?1, ?2, ?3, '2026-09-30T08:00:00Z')",
            (uid, text, i64::from(pinned)),
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn noon() -> jiff::Timestamp {
        at("2026-09-30T12:00:00Z")
    }

    #[test]
    fn twenty_quiet_minutes_with_an_open_note_lay_one_idle_trigger() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:40:00Z");
        note(&conn, uid, "call the bank", false);

        let laid = check(&conn, tmp.path(), noon()).unwrap();
        assert_eq!(laid.len(), 1);
        let row: (String, String, Option<String>, String, String, Option<i64>) = conn
            .query_row(
                "SELECT e.kind, e.origin, e.cancel_if, e.wall_time, p.date, e.work_session_id
                 FROM events e JOIN plans p ON p.id = e.plan_id WHERE e.id = ?1",
                [laid[0]],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            ("trigger".into(), "idle".into(), Some("active".into()), "12:00".into(),
             "2026-09-30".into(), None)
        );
        assert!(check(&conn, tmp.path(), at("2026-09-30T12:00:30Z")).unwrap().is_empty(),
            "one pending at a time");
    }

    #[test]
    fn short_of_the_threshold_nothing_is_laid() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:41:00Z");
        note(&conn, uid, "call the bank", false);
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());
    }

    #[test]
    fn the_setting_moves_the_threshold_and_zero_turns_it_off() {
        let (conn, tmp, uid) = env("idle_nudge_min = 5\n");
        seen(&conn, uid, "2026-09-30T11:55:00Z");
        note(&conn, uid, "call the bank", false);
        assert_eq!(check(&conn, tmp.path(), noon()).unwrap().len(), 1);

        let (conn, tmp, uid) = env("idle_nudge_min = 0\n");
        seen(&conn, uid, "2026-09-30T08:00:00Z");
        note(&conn, uid, "call the bank", false);
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());
    }

    #[test]
    fn a_pinned_or_done_note_or_a_running_session_keeps_it_quiet() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:00:00Z");
        note(&conn, uid, "pinned", true);
        let done = note(&conn, uid, "done", false);
        conn.execute("UPDATE notes SET done_at = '2026-09-30T09:00:00Z' WHERE id = ?1", [done])
            .unwrap();
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());

        note(&conn, uid, "open", false);
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, planned_min, started_at)
             VALUES (?1, 'essay', 60, '2026-09-30T11:30:00Z')",
            [uid],
        )
        .unwrap();
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());
        conn.execute("UPDATE work_sessions SET ended_at = '2026-09-30T11:40:00Z'", []).unwrap();
        assert_eq!(check(&conn, tmp.path(), noon()).unwrap().len(), 1);
    }

    #[test]
    fn a_quiet_window_or_the_close_of_the_day_holds_it_back() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:00:00Z");
        note(&conn, uid, "call the bank", false);
        crate::calendar::create(&conn, uid, crate::calendar::Fields {
            title: "school".into(), kind: "fixed".into(), quiet: Some(true),
            start_time: "11:30".into(), end_time: "12:30".into(),
            days: Some(crate::calendar::day_mask(&["wed"]).unwrap()), ..Default::default()
        })
        .unwrap();
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());

        let (conn, tmp, uid) = env("close_day_time = \"11:30\"\n");
        seen(&conn, uid, "2026-09-30T11:00:00Z");
        note(&conn, uid, "call the bank", false);
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());

        let (conn, tmp, uid) = env("close_day_time = \"\"\n");
        seen(&conn, uid, "2026-09-30T22:30:00Z");
        note(&conn, uid, "call the bank", false);
        assert_eq!(check(&conn, tmp.path(), at("2026-09-30T23:00:00Z")).unwrap().len(), 1,
            "a blank close of day is no cutoff");
    }

    #[test]
    fn a_spent_budget_lays_nothing() {
        let (conn, tmp, uid) = env("triggers_per_day = 1\n");
        seen(&conn, uid, "2026-09-30T11:00:00Z");
        note(&conn, uid, "call the bank", false);
        let date: jiff::civil::Date = "2026-09-30".parse().unwrap();
        let plan_id = crate::plan::ensure(&conn, tmp.path(), "aki", uid, date).unwrap();
        crate::triggers::insert(&conn, plan_id, "15:00", "ask", "agent", None, None, None,
            at("2026-09-30T08:00:00Z")).unwrap();
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());
    }

    #[test]
    fn the_idle_clock_restarts_at_the_last_idle_trigger() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:40:00Z");
        note(&conn, uid, "call the bank", false);
        let first = check(&conn, tmp.path(), noon()).unwrap();
        conn.execute("UPDATE events SET status = 'done' WHERE id = ?1", [first[0]]).unwrap();

        assert!(check(&conn, tmp.path(), at("2026-09-30T12:01:00Z")).unwrap().is_empty());
        assert!(check(&conn, tmp.path(), at("2026-09-30T12:19:00Z")).unwrap().is_empty());
        assert_eq!(check(&conn, tmp.path(), at("2026-09-30T12:20:00Z")).unwrap().len(), 1);
    }

    #[test]
    fn a_user_not_seen_today_is_left_alone() {
        let (conn, tmp, uid) = env("");
        note(&conn, uid, "call the bank", false);
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty(), "never seen");
        seen(&conn, uid, "2026-09-29T22:00:00Z");
        assert!(check(&conn, tmp.path(), at("2026-09-30T00:30:00Z")).unwrap().is_empty());
        assert!(check(&conn, tmp.path(), at("2026-09-30T09:00:00Z")).unwrap().is_empty());
    }

    #[test]
    fn an_account_without_checkins_is_left_alone() {
        let (conn, tmp, uid) = env("");
        crate::auth::set_category(&conn, "aki", "test").unwrap();
        seen(&conn, uid, "2026-09-30T11:00:00Z");
        note(&conn, uid, "call the bank", false);
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());
    }

    #[test]
    fn the_idle_note_lists_open_notes_with_ages_and_last_nudges() {
        let (conn, _tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:40:00Z");
        let bank = note(&conn, uid, "call the bank", false);
        conn.execute("UPDATE notes SET last_nudged_at = '2026-09-30T11:00:00Z' WHERE id = ?1", [bank])
            .unwrap();
        let milk = note(&conn, uid, "milk", false);
        conn.execute("UPDATE notes SET created_at = '2026-09-30T11:30:00Z' WHERE id = ?1", [milk])
            .unwrap();
        note(&conn, uid, "pinned thing", true);

        let text = context(&conn, uid, noon()).unwrap();
        assert!(text.contains("Nothing from the user for 20 min."), "{text}");
        assert!(text.contains(&format!("- {bank}: \"call the bank\", added 4 h ago, nudged 1 h ago")),
            "{text}");
        assert!(text.contains(&format!("- {milk}: \"milk\", added 30 min ago, never nudged")), "{text}");
        assert!(!text.contains("pinned thing"), "{text}");
    }

    #[test]
    fn a_nudge_stamps_only_the_users_own_open_notes() {
        let (conn, _tmp, uid) = env("");
        let other = crate::auth::create_user(&conn, "bo", "p", false).unwrap();
        let mine = note(&conn, uid, "mine", false);
        let done = note(&conn, uid, "done", false);
        conn.execute("UPDATE notes SET done_at = '2026-09-30T09:00:00Z' WHERE id = ?1", [done])
            .unwrap();
        let theirs = note(&conn, other, "theirs", false);

        assert_eq!(stamp_nudged(&conn, uid, &[mine, done, theirs, 404], noon()).unwrap(), 1);
        let stamped: Vec<(i64, Option<String>)> = {
            let mut stmt = conn.prepare("SELECT id, last_nudged_at FROM notes ORDER BY id").unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(
            stamped,
            vec![(mine, Some(noon().to_string())), (done, None), (theirs, None)]
        );
    }
}
