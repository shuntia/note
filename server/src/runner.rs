use crate::{
    config::{Features, UserConfig},
    AppState,
};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::Path;

struct Candidate {
    event_id: i64,
    user_id: i64,
    username: String,
    category: String,
    date: String,
    wall_time: String,
    kind: String,
    channel: String,
    message: String,
}

#[derive(Clone, Debug)]
pub struct FiredEvent {
    pub event_id: i64,
    pub user_id: i64,
    pub username: String,
    pub kind: String,
    pub wall_time: String,
    pub date: String,
    pub channel: String,
    pub message: String,
}

/// Resolves the candidate's stored wall time to an instant in `tz`. `date` and
/// `wall_time` come from user-authored templates and are stored unvalidated, so
/// both parses can fail. DST gaps resolve forward to the next valid instant.
fn due_at(tz: &jiff::tz::TimeZone, c: &Candidate) -> Result<jiff::Timestamp> {
    let date: jiff::civil::Date = c.date.parse()?;
    let time: jiff::civil::Time = format!("{}:00", c.wall_time).parse()?;
    Ok(tz.to_ambiguous_zoned(date.to_datetime(time)).compatible()?.timestamp())
}

#[derive(Clone)]
struct UserRuntime {
    tz: jiff::tz::TimeZone,
    features: Features,
}

/// Resolves what the sweep needs from a user's config, memoized so it reads
/// that config — and reports a bad timezone — only once. A missing config or an
/// unknown timezone name degrades to UTC; the unknown name is recorded as a
/// `runner_error` rather than silently changing when the user's events fire.
fn user_runtime(
    conn: &Connection,
    config_dir: &Path,
    c: &Candidate,
    cache: &mut HashMap<String, UserRuntime>,
    now: jiff::Timestamp,
) -> Result<UserRuntime> {
    if let Some(rt) = cache.get(&c.username) {
        return Ok(rt.clone());
    }
    let cfg = UserConfig::load(config_dir, &c.username).ok();
    let features = cfg
        .as_ref()
        .map_or_else(|| Features::for_category(&c.category), |u| u.features(&c.category));
    let name = cfg.map_or_else(|| "UTC".into(), |u| u.timezone);
    let tz = match jiff::tz::TimeZone::get(&name) {
        Ok(tz) => tz,
        Err(_) if !features.checkins => jiff::tz::TimeZone::UTC,
        Err(_) => {
            crate::log::record_throttled(
                conn,
                Some(c.user_id),
                "runner_error",
                &format!("user {} has unknown timezone {name:?}; using UTC", c.username),
                now,
                crate::log::ERROR_LOG_WINDOW_MINS,
            )?;
            jiff::tz::TimeZone::UTC
        }
    };
    let rt = UserRuntime { tz, features };
    cache.insert(c.username.clone(), rt.clone());
    Ok(rt)
}

/// Holds a due delivery that lands inside a quiet calendar window: the event's
/// wall time moves to the window's end, where it fires by the normal path. A
/// candidate always carries a bare start — the schema forbids an end on
/// anything that alerts — so its wall time is the whole of its shape.
fn defer_while_quiet(
    conn: &Connection,
    c: &Candidate,
    tz: &jiff::tz::TimeZone,
    now: jiff::Timestamp,
) -> Result<bool> {
    let Some(window) = crate::calendar::quiet_window(conn, c.user_id, tz, now)? else {
        return Ok(false);
    };
    conn.execute("UPDATE events SET wall_time = ?1 WHERE id = ?2", (&window.end, c.event_id))?;
    crate::log::record_throttled(
        conn,
        Some(c.user_id),
        "delivery_deferred",
        &format!("event {} held until {} by calendar {}", c.event_id, window.end, window.title),
        now,
        crate::log::ERROR_LOG_WINDOW_MINS,
    )?;
    Ok(true)
}

/// Settles a due trigger whose reason has already taken care of itself, before
/// it is ever marked fired: the user replied, finished the task, or decided the
/// event it was waiting on.
fn cancel_settled_trigger(
    conn: &Connection,
    c: &Candidate,
    now: jiff::Timestamp,
) -> Result<bool> {
    let Some(ev) = crate::triggers::read(conn, c.event_id)? else {
        return Ok(false);
    };
    if !crate::triggers::cancelled(conn, c.user_id, &ev)? {
        return Ok(false);
    }
    crate::triggers::cancel(conn, c.user_id, &ev, now)?;
    Ok(true)
}

/// Fires every pending or snoozed event whose wall time, resolved in its user's
/// timezone on the plan date, has arrived by `now`. A candidate that cannot be
/// resolved is logged as `runner_error` and skipped, so one unusable row cannot
/// stall the sweep for every other user. `alert = 0` leaves an event out of the
/// sweep entirely: the schema forces it on every block, and the user chooses it
/// per routine.
pub fn fire_due(
    conn: &Connection,
    config_dir: &Path,
    now: jiff::Timestamp,
) -> Result<Vec<FiredEvent>> {
    let mut stmt = conn.prepare(
        "SELECT e.id, p.user_id, u.username, u.category, p.date, e.wall_time, e.kind,
                e.channel, e.message
         FROM events e
         JOIN plans p ON p.id = e.plan_id
         JOIN users u ON u.id = p.user_id
         WHERE e.status IN ('pending','snoozed') AND e.alert = 1",
    )?;
    let candidates: Vec<Candidate> = stmt
        .query_map([], |r| {
            Ok(Candidate {
                event_id: r.get(0)?,
                user_id: r.get(1)?,
                username: r.get(2)?,
                category: r.get(3)?,
                date: r.get(4)?,
                wall_time: r.get(5)?,
                kind: r.get(6)?,
                channel: r.get(7)?,
                message: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut fired = Vec::new();
    let mut tz_cache = HashMap::new();
    for c in candidates {
        let rt = user_runtime(conn, config_dir, &c, &mut tz_cache, now)?;
        if !rt.features.checkins {
            continue;
        }
        let due = match due_at(&rt.tz, &c) {
            Ok(due) => due,
            Err(e) => {
                crate::log::record_throttled(
                    conn,
                    Some(c.user_id),
                    "runner_error",
                    &format!(
                        "event {} unresolvable ({} {}): {e}",
                        c.event_id, c.date, c.wall_time
                    ),
                    now,
                    crate::log::ERROR_LOG_WINDOW_MINS,
                )?;
                continue;
            }
        };
        if due <= now {
            if defer_while_quiet(conn, &c, &rt.tz, now)? {
                continue;
            }
            if c.kind == crate::triggers::KIND && cancel_settled_trigger(conn, &c, now)? {
                continue;
            }
            conn.execute(
                "UPDATE events SET status='fired', fired_at=?1 WHERE id=?2",
                (now.to_string(), c.event_id),
            )?;
            crate::log::record(
                conn,
                Some(c.user_id),
                "event_fired",
                &format!("event {} due {}", c.event_id, due),
            )?;
            fired.push(FiredEvent {
                event_id: c.event_id,
                user_id: c.user_id,
                username: c.username.clone(),
                kind: c.kind.clone(),
                wall_time: c.wall_time.clone(),
                date: c.date.clone(),
                channel: c.channel.clone(),
                message: c.message.clone(),
            });
        }
    }
    Ok(fired)
}

/// A block whose task asks to be announced, at the moment the block starts.
#[derive(Clone, Debug)]
pub struct BlockStart {
    pub event_id: i64,
    pub user_id: i64,
    pub username: String,
    pub date: String,
    pub wall_time: String,
    /// The task the block holds, and when the block gives it back.
    pub task: String,
    pub end_wall_time: String,
    pub notify: String,
}

/// Fires every block that has arrived and holds a task with something to say.
/// A quiet window holds a block start exactly as it holds a routine: the start
/// moves to the window's end and lands there.
pub fn block_starts(
    conn: &Connection,
    config_dir: &Path,
    now: jiff::Timestamp,
) -> Result<Vec<BlockStart>> {
    let mut stmt = conn.prepare(
        "SELECT e.id, p.user_id, u.username, u.category, p.date, e.wall_time,
                e.end_wall_time, t.title, t.notify
         FROM events e
         JOIN plans p ON p.id = e.plan_id
         JOIN users u ON u.id = p.user_id
         JOIN event_tasks et ON et.event_id = e.id
         JOIN tasks t ON t.id = et.task_id
         WHERE e.status = 'pending' AND e.end_wall_time IS NOT NULL AND t.notify != 'none'",
    )?;
    let waiting: Vec<(Candidate, String, String, String)> = stmt
        .query_map([], |r| {
            Ok((
                Candidate {
                    event_id: r.get(0)?,
                    user_id: r.get(1)?,
                    username: r.get(2)?,
                    category: r.get(3)?,
                    date: r.get(4)?,
                    wall_time: r.get(5)?,
                    kind: String::new(),
                    channel: String::new(),
                    message: String::new(),
                },
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut started = Vec::new();
    let mut tz_cache = HashMap::new();
    for (c, end_wall_time, task, notify) in waiting {
        let rt = user_runtime(conn, config_dir, &c, &mut tz_cache, now)?;
        if !rt.features.checkins {
            continue;
        }
        let Ok(due) = due_at(&rt.tz, &c) else { continue };
        if due > now || defer_while_quiet(conn, &c, &rt.tz, now)? {
            continue;
        }
        conn.execute(
            "UPDATE events SET status='fired', fired_at=?1 WHERE id=?2",
            (now.to_string(), c.event_id),
        )?;
        crate::log::record(
            conn,
            Some(c.user_id),
            "block_started",
            &format!("event {} holds {task:?} until {end_wall_time} ({notify})", c.event_id),
        )?;
        started.push(BlockStart {
            event_id: c.event_id,
            user_id: c.user_id,
            username: c.username,
            date: c.date,
            wall_time: c.wall_time,
            task,
            end_wall_time,
            notify,
        });
    }
    Ok(started)
}

/// Expired sessions only ever accumulate; sweeping them here keeps logout and
/// expiry cheap without a dedicated task. The comparison is lexicographic on
/// RFC 3339 UTC strings, whose fractional-second part varies in width, so it is
/// exact only to the second — a session outlives its expiry by under a second at
/// worst, and auth re-parses the expiry before trusting it.
pub fn gc_sessions(conn: &Connection, now: jiff::Timestamp) -> Result<()> {
    conn.execute("DELETE FROM sessions WHERE expires_at < ?1", [now.to_string()])?;
    Ok(())
}

/// One sweep: session GC and firing under a single lock, then delivery with the
/// lock released. Blocking throughout, so async callers must wrap it in
/// `spawn_blocking`.
pub fn sweep_once(state: &AppState) {
    let now = jiff::Timestamp::now();
    state.login_limiter.sweep(now);
    state.join_limiter.sweep(now);
    let (fired, started, flips) = {
        let conn = state.db();
        if let Err(e) = gc_sessions(&conn, now) {
            let _ = crate::log::record_throttled(
                &conn,
                None,
                "runner_error",
                &e.to_string(),
                now,
                crate::log::ERROR_LOG_WINDOW_MINS,
            );
        }
        let note = |e: anyhow::Error| {
            let _ = crate::log::record_throttled(
                &conn,
                None,
                "runner_error",
                &e.to_string(),
                now,
                crate::log::ERROR_LOG_WINDOW_MINS,
            );
        };
        if let Err(e) = crate::idle::check(&conn, &state.config_dir, &state.data_dir, now) {
            note(e);
        }
        let fired = fire_due(&conn, &state.config_dir, now).unwrap_or_else(|e| {
            note(e);
            Vec::new()
        });
        let started = block_starts(&conn, &state.config_dir, now).unwrap_or_else(|e| {
            note(e);
            Vec::new()
        });
        let mut flips = crate::work::tick(&conn, &state.config_dir, now).unwrap_or_else(|e| {
            note(e);
            Vec::new()
        });
        flips.extend(crate::work::planned_end(&conn, &state.config_dir, now).unwrap_or_else(|e| {
            note(e);
            Vec::new()
        }));
        (fired, started, flips)
    };
    for start in &started {
        crate::channels::deliver_block_start(
            &state.db,
            &state.channels,
            &state.hub,
            start,
            crate::text::Lang::for_user(&state.config_dir, &start.username),
        );
    }
    for flip in &flips {
        if let Some(msg) = &flip.message {
            crate::channels::deliver_via(
                &state.db,
                &state.channels,
                flip.user_id,
                &flip.username,
                msg,
            );
        }
        state.hub.broadcast_changed(flip.user_id);
    }
    for ev in &fired {
        // A trigger has nothing canned to deliver: it runs a session first, and
        // that session decides whether anything is sent at all.
        if ev.kind == crate::triggers::KIND {
            crate::triggers::fire(state, ev);
        } else {
            crate::channels::deliver_event(
                &state.db,
                &state.channels,
                ev,
                crate::text::Lang::for_user(&state.config_dir, &ev.username),
            );
        }
    }
}

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            // The whole sweep runs on the blocking pool: no DB guard is ever
            // live across the `await`, which would make the task non-Send.
            let st = state.clone();
            let _ = tokio::task::spawn_blocking(move || sweep_once(&st)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::{Template, TemplateEvent};

    fn setup(tz: &str) -> (rusqlite::Connection, tempfile::TempDir, i64) {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("users/aki");
        std::fs::create_dir_all(tmp.path().join("defaults")).unwrap();
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            tmp.path().join("defaults/user.toml"),
            format!("display_name = \"X\"\ntimezone = \"{tz}\"\ntemplate = \"default\"\n"),
        ).unwrap();
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        (conn, tmp, uid)
    }

    fn one_event_template(time: &str) -> Template {
        Template { events: vec![TemplateEvent {
            kind: "nudge".into(), time: time.into(),
            days: vec!["mon".into(),"tue".into(),"wed".into(),"thu".into(),"fri".into(),"sat".into(),"sun".into()],
            flexibility: Some("slide".into()), slide_window_min: Some(60), channel: "push".into(), ..Default::default()
        }]}
    }

    #[test]
    fn fires_only_when_due_in_user_tz() {
        let (conn, tmp, uid) = setup("Asia/Tokyo");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, uid, &one_event_template("09:00"), date).unwrap();

        // 08:59 JST on the plan date = 2026-08-30T23:59Z
        let early: jiff::Timestamp = "2026-08-30T23:59:00Z".parse().unwrap();
        assert!(fire_due(&conn, tmp.path(), early).unwrap().is_empty());

        // 09:01 JST
        let due: jiff::Timestamp = "2026-08-31T00:01:00Z".parse().unwrap();
        let fired = fire_due(&conn, tmp.path(), due).unwrap();
        assert_eq!(fired.len(), 1);

        // second run does not double-fire
        assert!(fire_due(&conn, tmp.path(), due).unwrap().is_empty());
    }

    #[test]
    fn a_test_accounts_events_never_fire_until_checkins_are_switched_on() {
        let (conn, tmp, uid) = setup("UTC");
        crate::auth::set_category(&conn, "aki", "test").unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, uid, &one_event_template("09:00"), date).unwrap();

        let now: jiff::Timestamp = "2026-08-31T09:30:00Z".parse().unwrap();
        assert!(fire_due(&conn, tmp.path(), now).unwrap().is_empty());
        let logged: i64 =
            conn.query_row("SELECT COUNT(*) FROM event_log", [], |r| r.get(0)).unwrap();
        assert_eq!(logged, 0);
        let status: String = conn
            .query_row("SELECT status FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "pending");

        std::fs::write(tmp.path().join("users/aki/user.toml"), "checkins_enabled = true\n")
            .unwrap();
        assert_eq!(fire_due(&conn, tmp.path(), now).unwrap().len(), 1);
    }

    #[test]
    fn unresolvable_event_is_logged_and_skipped() {
        let (conn, tmp, uid) = setup("UTC");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let plan_id = crate::plan::generate(&conn, uid, &one_event_template("00:00"), date).unwrap();
        let good_id = crate::plan::events_for(&conn, uid, date).unwrap()[0].id;
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time) VALUES (?1, 'nudge', 'noon')",
            [plan_id],
        ).unwrap();

        let now: jiff::Timestamp = "2026-08-31T12:00:00Z".parse().unwrap();
        let fired: Vec<i64> =
            fire_due(&conn, tmp.path(), now).unwrap().iter().map(|f| f.event_id).collect();
        assert_eq!(fired, vec![good_id]);

        let errors: i64 = conn.query_row(
            "SELECT COUNT(*) FROM event_log WHERE kind='runner_error'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(errors, 1);

        let again: jiff::Timestamp = "2026-08-31T12:00:30Z".parse().unwrap();
        fire_due(&conn, tmp.path(), again).unwrap();
        let errors: i64 = conn.query_row(
            "SELECT COUNT(*) FROM event_log WHERE kind='runner_error'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(errors, 1);

        let still_pending: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE wall_time='noon' AND status='pending'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(still_pending, 1);
    }

    #[test]
    fn unknown_timezone_falls_back_to_utc_and_is_logged() {
        let (conn, tmp, uid) = setup("Asia/Toyko");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, uid, &one_event_template("09:00"), date).unwrap();

        let now: jiff::Timestamp = "2026-08-31T09:30:00Z".parse().unwrap();
        assert_eq!(fire_due(&conn, tmp.path(), now).unwrap().len(), 1);

        let detail: String = conn.query_row(
            "SELECT detail FROM event_log WHERE kind='runner_error'", [], |r| r.get(0),
        ).unwrap();
        assert!(detail.contains("Asia/Toyko"), "unexpected detail: {detail}");
    }

    #[test]
    fn snoozed_events_fire_when_due() {
        let (conn, tmp, uid) = setup("UTC");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, uid, &one_event_template("09:00"), date).unwrap();
        conn.execute("UPDATE events SET status='snoozed', wall_time='09:30'", []).unwrap();
        let now: jiff::Timestamp = "2026-08-31T09:31:00Z".parse().unwrap();
        assert_eq!(fire_due(&conn, tmp.path(), now).unwrap().len(), 1);
    }

    #[test]
    fn a_silent_routine_never_fires_but_stays_on_the_plan() {
        let (conn, tmp, uid) = setup("UTC");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let mut t = one_event_template("09:00");
        t.events[0].alert = Some(false);
        crate::plan::generate(&conn, uid, &t, date).unwrap();

        let now: jiff::Timestamp = "2026-08-31T12:00:00Z".parse().unwrap();
        assert!(fire_due(&conn, tmp.path(), now).unwrap().is_empty());

        let evs = crate::plan::events_for(&conn, uid, date).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].status, "pending");
        crate::plan::set_status(&conn, uid, evs[0].id, "done").unwrap();
        assert_eq!(crate::plan::events_for(&conn, uid, date).unwrap()[0].status, "done");
    }

    #[test]
    fn a_block_is_never_a_delivery_candidate() {
        let (conn, tmp, uid) = setup("UTC");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let t = Template { events: vec![TemplateEvent {
            kind: "Work time".into(), time: "09:30".into(),
            days: vec!["mon".into(),"tue".into(),"wed".into(),"thu".into(),"fri".into(),"sat".into(),"sun".into()],
            entry: crate::templates::Entry::Block, end_time: Some("12:30".into()),
            channel: "push".into(), ..Default::default()
        }]};
        crate::plan::generate(&conn, uid, &t, date).unwrap();
        let now: jiff::Timestamp = "2026-08-31T23:00:00Z".parse().unwrap();
        assert!(fire_due(&conn, tmp.path(), now).unwrap().is_empty());
        assert_eq!(crate::plan::events_for(&conn, uid, date).unwrap()[0].status, "pending");
    }

    fn calendar_entry(
        conn: &rusqlite::Connection, uid: i64, title: &str, kind: &str, quiet: bool, days: &[&str],
    ) -> i64 {
        crate::calendar::create(conn, uid, crate::calendar::Fields {
            title: title.into(), kind: kind.into(), quiet: Some(quiet),
            start_time: "08:15".into(), end_time: "15:30".into(),
            days: Some(crate::calendar::day_mask(days).unwrap()), ..Default::default()
        }).unwrap().id
    }

    fn school(conn: &rusqlite::Connection, uid: i64) -> i64 {
        calendar_entry(conn, uid, "school", "fixed", true, &["mon", "tue", "wed", "thu", "fri"])
    }

    /// 2026-09-15 is a Tuesday, and a 10:00 delivery falls inside school.
    fn day_with_a_ten_oclock_event(tz: &str) -> (rusqlite::Connection, tempfile::TempDir, i64) {
        let (conn, tmp, uid) = setup(tz);
        let date: jiff::civil::Date = "2026-09-15".parse().unwrap();
        crate::plan::generate(&conn, uid, &one_event_template("10:00"), date).unwrap();
        (conn, tmp, uid)
    }

    fn wall_time(conn: &rusqlite::Connection) -> String {
        conn.query_row("SELECT wall_time FROM events", [], |r| r.get(0)).unwrap()
    }

    fn deferrals(conn: &rusqlite::Connection) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM event_log WHERE kind='delivery_deferred'", [], |r| r.get(0),
        ).unwrap()
    }

    #[test]
    fn a_delivery_inside_a_quiet_window_waits_for_its_end() {
        let (conn, tmp, uid) = day_with_a_ten_oclock_event("UTC");
        school(&conn, uid);

        let inside: jiff::Timestamp = "2026-09-15T10:00:00Z".parse().unwrap();
        assert!(fire_due(&conn, tmp.path(), inside).unwrap().is_empty());
        assert_eq!(wall_time(&conn), "15:30");

        let detail: String = conn.query_row(
            "SELECT detail FROM event_log WHERE kind='delivery_deferred'", [], |r| r.get(0),
        ).unwrap();
        assert!(detail.ends_with("held until 15:30 by calendar school"), "{detail}");

        let still_inside: jiff::Timestamp = "2026-09-15T15:29:00Z".parse().unwrap();
        assert!(fire_due(&conn, tmp.path(), still_inside).unwrap().is_empty());
        assert_eq!(deferrals(&conn), 1, "a sweep a minute does not log a row a minute");

        let ended: jiff::Timestamp = "2026-09-15T15:30:00Z".parse().unwrap();
        assert_eq!(fire_due(&conn, tmp.path(), ended).unwrap().len(), 1);
    }

    #[test]
    fn a_window_that_is_not_quiet_holds_nothing_back() {
        let (conn, tmp, uid) = day_with_a_ten_oclock_event("UTC");
        calendar_entry(&conn, uid, "commute", "busy", false, &["tue"]);
        calendar_entry(&conn, uid, "bin day", "note", true, &["tue"]);

        let inside: jiff::Timestamp = "2026-09-15T10:00:00Z".parse().unwrap();
        assert_eq!(fire_due(&conn, tmp.path(), inside).unwrap().len(), 1);
        assert_eq!(deferrals(&conn), 0);
    }

    #[test]
    fn a_skipped_occurrence_leaves_the_day_loud() {
        let (conn, tmp, uid) = day_with_a_ten_oclock_event("UTC");
        let id = school(&conn, uid);
        crate::calendar::skip(&conn, uid, id, "2026-09-15").unwrap();

        let inside: jiff::Timestamp = "2026-09-15T10:00:00Z".parse().unwrap();
        assert_eq!(fire_due(&conn, tmp.path(), inside).unwrap().len(), 1);
    }

    #[test]
    fn a_one_off_entry_quiets_its_own_day() {
        let (conn, tmp, uid) = day_with_a_ten_oclock_event("UTC");
        crate::calendar::create(&conn, uid, crate::calendar::Fields {
            title: "exam".into(), kind: "fixed".into(),
            start_time: "09:00".into(), end_time: "11:30".into(),
            on_date: Some("2026-09-15".into()), ..Default::default()
        }).unwrap();

        let inside: jiff::Timestamp = "2026-09-15T10:00:00Z".parse().unwrap();
        assert!(fire_due(&conn, tmp.path(), inside).unwrap().is_empty());
        assert_eq!(wall_time(&conn), "11:30");
    }

    #[test]
    fn a_quiet_window_is_another_users_business_alone() {
        let (conn, tmp, uid) = day_with_a_ten_oclock_event("UTC");
        let other = crate::auth::create_user(&conn, "rin", "p", false).unwrap();
        school(&conn, other);
        assert_ne!(uid, other);

        let inside: jiff::Timestamp = "2026-09-15T10:00:00Z".parse().unwrap();
        assert_eq!(fire_due(&conn, tmp.path(), inside).unwrap().len(), 1);
    }

    #[test]
    fn expired_sessions_are_garbage_collected() {
        let (conn, _tmp, uid) = setup("UTC");
        conn.execute(
            "INSERT INTO sessions (token, user_id, expires_at) VALUES ('old', ?1, '2020-01-01T00:00:00Z')",
            [uid],
        ).unwrap();
        conn.execute(
            "INSERT INTO sessions (token, user_id, expires_at) VALUES ('new', ?1, '2099-01-01T00:00:00Z')",
            [uid],
        ).unwrap();
        gc_sessions(&conn, "2026-08-31T00:00:00Z".parse().unwrap()).unwrap();
        let left: i64 = conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 1);
    }

    #[test]
    fn sweep_evicts_elapsed_login_limiter_entries() {
        let (conn, tmp, _uid) = setup("UTC");
        let state = AppState::new(conn, tmp.path().into(), tmp.path().into());
        state
            .login_limiter
            .try_attempt("aki", "2020-01-01T00:00:00Z".parse().unwrap());
        state.login_limiter.try_attempt("now", jiff::Timestamp::now());

        sweep_once(&state);

        assert_eq!(state.login_limiter.tracked(), 1);
    }

    #[test]
    fn sweep_delivers_fired_events_through_the_ladder() {
        let (conn, tmp, uid) = setup("UTC");
        let date: jiff::civil::Date = "2020-01-01".parse().unwrap();
        crate::plan::generate(&conn, uid, &one_event_template("00:00"), date).unwrap();
        let mock = std::sync::Arc::new(crate::channels::mock::MockChannel::new("mock"));
        let ladder: Vec<std::sync::Arc<dyn crate::channels::Channel>> = vec![mock.clone()];
        let state =
            AppState::new(conn, tmp.path().into(), tmp.path().into()).with_channels(ladder);

        sweep_once(&state);

        assert_eq!(mock.seen().len(), 1);
        let conn = state.db.lock().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='delivery_ok'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn fired_events_carry_kind_channel_and_message() {
        let (conn, tmp, uid) = setup("UTC");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, uid, &one_event_template("00:00"), date).unwrap();
        conn.execute("UPDATE events SET message='remember the thing'", []).unwrap();
        let now: jiff::Timestamp = "2026-08-31T12:00:00Z".parse().unwrap();
        let fired = fire_due(&conn, tmp.path(), now).unwrap();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].kind, "nudge");
        assert_eq!(fired[0].channel, "push");
        assert_eq!(fired[0].message, "remember the thing");
        assert_eq!(fired[0].date, "2026-08-31");
    }

    #[test]
    fn fired_events_are_logged() {
        let (conn, tmp, uid) = setup("UTC");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, uid, &one_event_template("00:00"), date).unwrap();
        let now: jiff::Timestamp = "2026-08-31T12:00:00Z".parse().unwrap();
        fire_due(&conn, tmp.path(), now).unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM event_log WHERE kind='event_fired'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(n, 1);
    }
}
