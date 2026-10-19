use crate::triggers::{self, Cancel};
use crate::model_text as mt;
use crate::text::Lang;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;
use std::fmt::Write as _;

pub const ORIGIN: &str = "idle";


struct Seen {
    user_id: i64,
    username: String,
    category: String,
    last_active: String,
}

/// Lays an idle trigger for every user who has gone quiet and
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
    if triggers::working(conn, user.user_id)?.is_some() {
        return Ok(None);
    }
    let tz = crate::zone::named(&cfg.timezone);
    let local = now.to_zoned(tz.clone());
    let Some(last) = quiet_since(&cfg, &tz, &local, user.last_active.parse()?) else {
        return Ok(None);
    };
    let since = match last_idle_laid(conn, user.user_id)? {
        Some(laid) if laid > last => laid,
        _ => last,
    };
    let outcomes = outcomes_since(conn, user.user_id, last)?;
    if now.as_second() - since.as_second() < threshold * 60 * backoff(&outcomes) {
        return Ok(None);
    }
    if let Some(spoke) = outcomes.iter().find_map(|o| o.spoke_at) {
        if now.as_second() - spoke.as_second() < SPOKE_HOLD_MIN * 60 && !changed_since(conn, user.user_id, spoke)? {
            return Ok(None);
        }
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
        crate::model_text::idle_prompt(crate::text::Lang::for_user(config_dir, &user.username)),
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

/// How long past `day_start` a user who has not shown up yet counts as having
/// gone quiet from.
pub const NO_SHOW_GRACE_MIN: i64 = 30;

/// Whether `last` falls on today's date at or after `wake::EARLIEST`.
fn here_today(tz: &jiff::tz::TimeZone, last: jiff::Timestamp, now: jiff::Timestamp) -> bool {
    let seen = last.to_zoned(tz.clone());
    let wall = format!("{:02}:{:02}", seen.hour(), seen.minute());
    seen.date() == now.to_zoned(tz.clone()).date() && wall.as_str() >= crate::wake::EARLIEST
}

/// When the user's quiet began: their last activity when it was today, or for
/// a day they have not shown up to, `day_start` plus `NO_SHOW_GRACE_MIN`.
/// Activity before `wake::EARLIEST` is the night before's. `None` with no
/// `day_start` to count a no-show from.
fn quiet_since(
    cfg: &crate::config::UserConfig,
    tz: &jiff::tz::TimeZone,
    local: &jiff::Zoned,
    last: jiff::Timestamp,
) -> Option<jiff::Timestamp> {
    if here_today(tz, last, local.timestamp()) {
        return Some(last);
    }
    let start = cfg.day_start();
    if start.is_empty() {
        return None;
    }
    let begun = crate::zone::instant(tz, local.date(), start)?;
    Some(begun + jiff::Span::new().minutes(NO_SHOW_GRACE_MIN))
}

/// How long after an idle nudge that spoke another is not worth a session
/// while nothing has changed; the trigger prompt's "nudged about in the last hour".
const SPOKE_HOLD_MIN: i64 = 60;
/// The idle wait doubles per quiet ending in a row, up to this many times.
const MAX_BACKOFF_STEPS: u32 = 2;

/// One idle check's ending; `spoke_at` is set when its session said or rang.
struct Outcome {
    quiet: bool,
    spoke_at: Option<jiff::Timestamp>,
}

/// The idle checks laid after `since`, newest first, with how each ended.
fn outcomes_since(conn: &Connection, user_id: i64, since: jiff::Timestamp) -> rusqlite::Result<Vec<Outcome>> {
    let mut stmt = conn.prepare_cached(
        "SELECT e.decided_at,
                (SELECT l.kind FROM event_log l
                  WHERE l.user_id = p.user_id AND l.kind IN ('trigger_said', 'trigger_quiet')
                    AND l.detail LIKE 'event ' || e.id || ':%' LIMIT 1)
         FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND e.kind = ?2 AND e.origin = ?3 AND e.created_at > ?4
         ORDER BY e.created_at DESC LIMIT 10",
    )?;
    let rows = stmt.query_map((user_id, triggers::KIND, ORIGIN, since.to_string()), |r| {
        let decided: Option<String> = r.get(0)?;
        let ending: Option<String> = r.get(1)?;
        Ok(Outcome {
            quiet: ending.as_deref() == Some("trigger_quiet"),
            spoke_at: (ending.as_deref() == Some("trigger_said"))
                .then(|| decided.and_then(|d| d.parse().ok()))
                .flatten(),
        })
    })?;
    rows.collect()
}

/// 1, 2 or 4: how many idle waits to let pass after this many quiet endings in a row.
fn backoff(outcomes: &[Outcome]) -> i64 {
    let quiet = outcomes.iter().take_while(|o| o.quiet).count();
    1 << u32::try_from(quiet).unwrap_or(u32::MAX).min(MAX_BACKOFF_STEPS)
}

/// Whether the user's day moved since `at`: a message, a task change, or a plan
/// event other than an idle check firing or being settled.
fn changed_since(conn: &Connection, user_id: i64, at: jiff::Timestamp) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM tasks WHERE user_id = ?1 AND updated_at > ?2)
             OR EXISTS (SELECT 1 FROM talk_messages m JOIN conversations c ON c.id = m.conversation_id
                        WHERE c.user_id = ?1 AND m.role = 'user' AND m.created_at > ?2)
             OR EXISTS (SELECT 1 FROM events e JOIN plans p ON p.id = e.plan_id
                        WHERE p.user_id = ?1 AND e.origin != ?3
                          AND (e.fired_at > ?2 OR e.decided_at > ?2 OR e.created_at > ?2))",
        (user_id, at.to_string(), ORIGIN),
        |r| r.get(0),
    )
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

fn ago(ts: &str, l: Lang, now: jiff::Timestamp) -> String {
    let Ok(t) = ts.parse::<jiff::Timestamp>() else {
        return ts.to_string();
    };
    mt::ago_short(l, (now.as_second() - t.as_second()).max(0) / 60)
}

/// What an idle trigger's session reads under its prompt: how long the user
/// has been quiet, or that they have not been in today, then each active note
/// with its id, age and last nudge.
pub fn context(
    conn: &Connection,
    data_dir: &Path,
    user_id: i64,
    username: &str,
    tz: &jiff::tz::TimeZone,
    l: Lang,
    now: jiff::Timestamp,
) -> Result<String> {
    let mut s = String::new();
    if let Some(at) = crate::presence::last_active(conn, user_id)? {
        if here_today(tz, at, now) {
            let _ = writeln!(s, "{}", mt::quiet_for(l, (now.as_second() - at.as_second()).max(0) / 60));
        } else {
            let _ = writeln!(s, "{}", mt::not_in_today(l));
        }
    }
    let notes = crate::notes::active(conn, data_dir, username, now)?;
    if !notes.is_empty() {
        s.push_str(mt::open_working_lines(l));
    }
    for n in notes.into_iter().take(20) {
        let nudge = match &n.last_nudged_at {
            Some(t) => mt::nudged_ago(l, &ago(t, l, now)),
            None => mt::never_nudged(l).to_string(),
        };
        let _ = writeln!(s, "{}", mt::working_line(l, crate::notes::short(&n.id), &n.title, &ago(&n.created, l, now), &nudge));
    }
    Ok(s)
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

    const MORNING: &str = "2026-09-30T08:00:00Z";

    fn note(conn: &Connection, tmp: &tempfile::TempDir, title: &str, made: &str, until: Option<&str>) -> String {
        crate::notes::add(conn, tmp.path(), "aki", title, None, until.map(str::to_owned), at(made)).unwrap().id
    }

    fn noon() -> jiff::Timestamp {
        at("2026-09-30T12:00:00Z")
    }

    #[test]
    fn twenty_quiet_minutes_with_an_open_note_lay_one_idle_trigger() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:40:00Z");
        note(&conn, &tmp, "call the bank", MORNING, None);

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
    fn twenty_quiet_minutes_lay_an_idle_trigger_with_no_note_at_all() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:40:00Z");
        assert_eq!(check(&conn, tmp.path(), noon()).unwrap().len(), 1);
    }

    #[test]
    fn short_of_the_threshold_nothing_is_laid() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:41:00Z");
        note(&conn, &tmp, "call the bank", MORNING, None);
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());
    }

    #[test]
    fn the_setting_moves_the_threshold_and_zero_turns_it_off() {
        let (conn, tmp, uid) = env("idle_nudge_min = 5\n");
        seen(&conn, uid, "2026-09-30T11:55:00Z");
        note(&conn, &tmp, "call the bank", MORNING, None);
        assert_eq!(check(&conn, tmp.path(), noon()).unwrap().len(), 1);

        let (conn, tmp, uid) = env("idle_nudge_min = 0\n");
        seen(&conn, uid, "2026-09-30T08:00:00Z");
        note(&conn, &tmp, "call the bank", MORNING, None);
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());
    }

    #[test]
    fn a_running_session_keeps_it_quiet() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:00:00Z");
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
    fn a_paused_session_does_not_keep_it_quiet() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:00:00Z");
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, started_at, paused_at)
             VALUES (?1, 'essay', '2026-09-30T10:30:00Z', '2026-09-30T11:00:00Z')",
            [uid],
        )
        .unwrap();
        assert_eq!(check(&conn, tmp.path(), noon()).unwrap().len(), 1);
    }

    #[test]
    fn a_quiet_window_or_the_close_of_the_day_holds_it_back() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:00:00Z");
        note(&conn, &tmp, "call the bank", MORNING, None);
        crate::calendar::create(&conn, uid, crate::calendar::Fields {
            title: "school".into(), kind: "fixed".into(), quiet: Some(true),
            start_time: "11:30".into(), end_time: "12:30".into(),
            days: Some(crate::calendar::day_mask(&["wed"]).unwrap()), ..Default::default()
        })
        .unwrap();
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());

        let (conn, tmp, uid) = env("close_day_time = \"11:30\"\n");
        seen(&conn, uid, "2026-09-30T11:00:00Z");
        note(&conn, &tmp, "call the bank", MORNING, None);
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());

        let (conn, tmp, uid) = env("close_day_time = \"\"\n");
        seen(&conn, uid, "2026-09-30T22:30:00Z");
        note(&conn, &tmp, "call the bank", MORNING, None);
        assert_eq!(check(&conn, tmp.path(), at("2026-09-30T23:00:00Z")).unwrap().len(), 1,
            "a blank close of day is no cutoff");
    }

    #[test]
    fn a_spent_budget_lays_nothing() {
        let (conn, tmp, uid) = env("triggers_per_day = 1\n");
        seen(&conn, uid, "2026-09-30T11:00:00Z");
        note(&conn, &tmp, "call the bank", MORNING, None);
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
        note(&conn, &tmp, "call the bank", MORNING, None);
        let first = check(&conn, tmp.path(), noon()).unwrap();
        conn.execute("UPDATE events SET status = 'done' WHERE id = ?1", [first[0]]).unwrap();

        assert!(check(&conn, tmp.path(), at("2026-09-30T12:01:00Z")).unwrap().is_empty());
        assert!(check(&conn, tmp.path(), at("2026-09-30T12:19:00Z")).unwrap().is_empty());
        assert_eq!(check(&conn, tmp.path(), at("2026-09-30T12:20:00Z")).unwrap().len(), 1);
    }

    fn ended(conn: &Connection, uid: i64, id: i64, kind: &str, ts: &str) {
        conn.execute("UPDATE events SET status = 'done', decided_at = ?1 WHERE id = ?2", (ts, id)).unwrap();
        crate::log::record(conn, Some(uid), kind, &format!("event {id}: x")).unwrap();
    }

    fn lays(conn: &Connection, tmp: &tempfile::TempDir, ts: &str) -> Option<i64> {
        check(conn, tmp.path(), at(ts)).unwrap().first().copied()
    }

    #[test]
    fn quiet_endings_in_a_row_stretch_the_wait_until_the_user_returns() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:40:00Z");
        let first = lays(&conn, &tmp, "2026-09-30T12:00:00Z").unwrap();
        ended(&conn, uid, first, "trigger_quiet", "2026-09-30T12:01:00Z");
        assert!(lays(&conn, &tmp, "2026-09-30T12:39:00Z").is_none(), "40 minutes after one quiet ending");
        let second = lays(&conn, &tmp, "2026-09-30T12:40:00Z").unwrap();
        ended(&conn, uid, second, "trigger_quiet", "2026-09-30T12:41:00Z");
        assert!(lays(&conn, &tmp, "2026-09-30T13:59:00Z").is_none(), "80 after two");
        let third = lays(&conn, &tmp, "2026-09-30T14:00:00Z").unwrap();
        ended(&conn, uid, third, "trigger_quiet", "2026-09-30T14:01:00Z");
        assert!(lays(&conn, &tmp, "2026-09-30T15:19:00Z").is_none());
        let fourth = lays(&conn, &tmp, "2026-09-30T15:20:00Z").unwrap();
        ended(&conn, uid, fourth, "trigger_quiet", "2026-09-30T15:21:00Z");

        seen(&conn, uid, "2026-09-30T15:30:00Z");
        assert!(lays(&conn, &tmp, "2026-09-30T15:50:00Z").is_some(), "activity resets the wait");
    }

    #[test]
    fn within_an_hour_of_a_nudge_that_spoke_only_a_change_reopens_it() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:40:00Z");
        let first = lays(&conn, &tmp, "2026-09-30T12:00:00Z").unwrap();
        ended(&conn, uid, first, "trigger_said", "2026-09-30T12:01:00Z");
        assert!(lays(&conn, &tmp, "2026-09-30T12:30:00Z").is_none(), "nothing moved since it spoke");
        let thread = crate::talk::create(&conn, uid, "chat", at("2026-09-30T08:00:00Z")).unwrap();
        crate::talk::append_text(&conn, thread, "user", "later", at("2026-09-30T12:10:00Z")).unwrap();
        assert!(lays(&conn, &tmp, "2026-09-30T12:30:00Z").is_some(), "a message since is a change");

        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:40:00Z");
        let first = lays(&conn, &tmp, "2026-09-30T12:00:00Z").unwrap();
        ended(&conn, uid, first, "trigger_said", "2026-09-30T12:01:00Z");
        assert!(lays(&conn, &tmp, "2026-09-30T13:00:00Z").is_none());
        assert!(lays(&conn, &tmp, "2026-09-30T13:01:00Z").is_some(), "an hour on, it may speak again");
    }

    #[test]
    fn a_user_who_never_used_note_is_left_alone() {
        let (conn, tmp, _uid) = env("");
        note(&conn, &tmp, "call the bank", MORNING, None);
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());
    }

    #[test]
    fn a_user_not_seen_today_has_gone_quiet_since_the_start_of_their_day() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-29T22:00:00Z");
        assert!(check(&conn, tmp.path(), at("2026-09-30T00:30:00Z")).unwrap().is_empty());
        // 08:00 start, 30 minutes' grace, then the usual 20 of quiet
        assert!(check(&conn, tmp.path(), at("2026-09-30T08:49:00Z")).unwrap().is_empty());
        assert_eq!(check(&conn, tmp.path(), at("2026-09-30T08:50:00Z")).unwrap().len(), 1);
    }

    #[test]
    fn activity_in_the_small_hours_belongs_to_the_night_before() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T01:30:00Z");
        assert!(check(&conn, tmp.path(), at("2026-09-30T02:00:00Z")).unwrap().is_empty());
        assert_eq!(check(&conn, tmp.path(), at("2026-09-30T08:50:00Z")).unwrap().len(), 1);
    }

    #[test]
    fn a_blank_day_start_leaves_a_no_show_alone() {
        let (conn, tmp, uid) = env("day_start = \"\"\n");
        seen(&conn, uid, "2026-09-29T22:00:00Z");
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());
    }

    #[test]
    fn an_account_without_checkins_is_left_alone() {
        let (conn, tmp, uid) = env("");
        crate::auth::set_category(&conn, "aki", "test").unwrap();
        seen(&conn, uid, "2026-09-30T11:00:00Z");
        note(&conn, &tmp, "call the bank", MORNING, None);
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());
    }

    #[test]
    fn the_idle_note_lists_active_notes_with_ages_and_last_nudges() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:40:00Z");
        let bank = note(&conn, &tmp, "call the bank", MORNING, None);
        crate::notes::nudged(&conn, tmp.path(), "aki", std::slice::from_ref(&bank), at("2026-09-30T11:00:00Z")).unwrap();
        let milk = note(&conn, &tmp, "milk", "2026-09-30T11:30:00Z", None);
        note(&conn, &tmp, "past thing", MORNING, Some("2026-09-30T09:00:00Z"));

        let text = context(&conn, tmp.path(), uid, "aki", &jiff::tz::TimeZone::UTC, Lang::En, noon()).unwrap();
        assert!(text.contains("Nothing from the user for 20 min."), "{text}");
        assert!(text.contains(&format!("- {}: \"call the bank\", added 4 h ago, nudged 1 h ago", &bank[..8])), "{text}");
        assert!(text.contains(&format!("- {}: \"milk\", added 30 min ago, never nudged", &milk[..8])), "{text}");
        assert!(!text.contains("past thing"), "{text}");
    }

    #[test]
    fn the_idle_note_says_when_the_user_has_not_been_in_today() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-29T22:00:00Z");
        let text = context(&conn, tmp.path(), uid, "aki", &jiff::tz::TimeZone::UTC, Lang::En, noon()).unwrap();
        assert!(text.starts_with("The user has not been in today."), "{text}");
    }

    #[test]
    fn the_idle_note_has_no_working_header_without_active_notes() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:40:00Z");
        let text = context(&conn, tmp.path(), uid, "aki", &jiff::tz::TimeZone::UTC, Lang::En, noon()).unwrap();
        assert!(!text.contains("Working notes"), "{text}");
    }
}
