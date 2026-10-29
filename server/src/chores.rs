//! Tidy-up wake-ups: standing daily system triggers at `chores_times`, whose
//! sessions merge, fix and prune tasks and long-term memory, then offer one
//! small task.

use crate::config::UserConfig;
use crate::model_text as mt;
use crate::text::Lang;
use crate::triggers;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::fmt::Write as _;
use std::path::Path;

pub const ORIGIN: &str = "chores";
const STALE_DAYS: i64 = 14;
const OVERDUE_DAYS: i64 = 3;
/// A root this long with no steps under it is too vague to start.
const BIG_MIN: u32 = 90;
const SMALL_MIN: u32 = 30;
const MAX_CANDIDATES: usize = 25;
pub const MEMORY_SLICE: usize = 30;
const MAX_TITLE_CHARS: usize = 80;
/// Titles at least this long may differ by `NEAR_EDITS` and still be one task.
const NEAR_MIN_CHARS: usize = 8;
const NEAR_EDITS: usize = 2;

/// The user's tidy-up times that fall inside their day, sorted and distinct.
pub fn times(cfg: &UserConfig) -> Vec<&str> {
    let (start, close) = (cfg.day_start(), cfg.close_day_time());
    let mut out: Vec<&str> = cfg
        .chores_times()
        .into_iter()
        .filter(|t| crate::templates::valid_time(t))
        .filter(|t| start.is_empty() || *t > start)
        .filter(|t| close.is_empty() || *t < close)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Lays `date`'s tidy-ups still ahead of `now` and takes back pending ones at
/// times the user no longer keeps. A time already laid on that date stays as
/// it is, whatever became of it, so this can run any number of times.
pub fn lay(
    conn: &Connection,
    config_dir: &Path,
    username: &str,
    user_id: i64,
    date: jiff::civil::Date,
    now: jiff::Timestamp,
) -> Result<Vec<i64>> {
    let cfg = UserConfig::load(config_dir, username)?;
    lay_for(conn, config_dir, &cfg, username, user_id, date, now)
}

fn lay_for(
    conn: &Connection,
    config_dir: &Path,
    cfg: &UserConfig,
    username: &str,
    user_id: i64,
    date: jiff::civil::Date,
    now: jiff::Timestamp,
) -> Result<Vec<i64>> {
    let times = times(cfg);
    let laid = laid_on(conn, user_id, date)?;
    for (id, at, status) in &laid {
        if matches!(status.as_str(), "pending" | "snoozed") && !times.contains(&at.as_str()) {
            conn.execute("DELETE FROM events WHERE id = ?1", [id])?;
        }
    }
    let tz = crate::zone::named(&cfg.timezone);
    let today = now.to_zoned(tz.clone()).date();
    let prompt = mt::chores_prompt(Lang::for_user(config_dir, username));
    let mut out = Vec::new();
    for at in times {
        if laid.iter().any(|(_, a, _)| a == at) {
            continue;
        }
        if crate::zone::instant(&tz, date, at).is_none_or(|t| t <= now) {
            continue;
        }
        let plan_id = crate::plan::holding(conn, config_dir, username, user_id, date, today)?;
        let id = triggers::insert(conn, plan_id, at, prompt, ORIGIN, None, None, None, now)?;
        crate::log::record(conn, Some(user_id), "trigger_laid", &format!("event {id} at {date} {at}: chores"))?;
        out.push(id);
    }
    Ok(out)
}

/// `(event, time first laid for, status)` of every tidy-up on `date`.
fn laid_on(conn: &Connection, user_id: i64, date: jiff::civil::Date) -> rusqlite::Result<Vec<(i64, String, String)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT e.id, COALESCE(e.orig_wall_time, e.wall_time), e.status FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND p.date = ?2 AND e.kind = ?3 AND e.origin = ?4",
    )?;
    let rows = stmt.query_map((user_id, date.to_string(), triggers::KIND, ORIGIN), |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?;
    rows.collect()
}

/// Lays today's remaining tidy-ups for every user with check-ins on, so a day
/// whose nightly run came before them, or a server started mid-day, still gets
/// them. One user's failure is logged and skips only them.
pub fn check(conn: &Connection, config_dir: &Path, now: jiff::Timestamp) -> Result<()> {
    let mut stmt = conn.prepare_cached("SELECT id, username, category FROM users WHERE disabled = 0")?;
    let users: Vec<(i64, String, String)> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for (user_id, username, category) in users {
        let Ok(cfg) = UserConfig::load(config_dir, &username) else {
            continue;
        };
        if !cfg.features(&category).checkins {
            continue;
        }
        let today = now.to_zoned(crate::zone::named(&cfg.timezone)).date();
        if let Err(e) = lay_for(conn, config_dir, &cfg, &username, user_id, today, now) {
            crate::log::record_throttled(
                conn,
                Some(user_id),
                "runner_error",
                &format!("lay chores: {e:#}"),
                now,
                crate::log::ERROR_LOG_WINDOW_MINS,
            )?;
        }
    }
    Ok(())
}

struct Live {
    id: i64,
    title: String,
    parent_id: Option<i64>,
    due_at: Option<jiff::Timestamp>,
    duration_min: Option<u32>,
    children: i64,
    /// The latest change anywhere in a root's tree; a step's own.
    touched: Option<jiff::Timestamp>,
}

fn live_tasks(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<Live>> {
    let mut stmt = conn.prepare_cached(
        "SELECT t.id, t.title, t.parent_id, t.due_at, t.duration_min,
                (SELECT COUNT(*) FROM tasks c WHERE c.parent_id = t.id AND c.state != 'dropped'),
                CASE WHEN t.parent_id IS NULL
                     THEN (SELECT MAX(d.updated_at) FROM tasks d WHERE d.root_id = t.id)
                     ELSE t.updated_at END
         FROM tasks t WHERE t.user_id = ?1 AND t.state IN ('open', 'in_progress') ORDER BY t.id",
    )?;
    let stamp = |s: Option<String>| s.and_then(|s| s.parse().ok());
    let rows = stmt.query_map([user_id], |r| {
        Ok(Live {
            id: r.get(0)?,
            title: r.get(1)?,
            parent_id: r.get(2)?,
            due_at: stamp(r.get(3)?),
            duration_min: r.get(4)?,
            children: r.get(5)?,
            touched: stamp(r.get(6)?),
        })
    })?;
    rows.collect()
}

/// Letters and digits only, lowercased, single-spaced.
fn folded(title: &str) -> String {
    title
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_lowercase().next().unwrap_or(c) } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn edits(a: &[char], b: &[char]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (prev + usize::from(ca != cb)).min(row[j] + 1).min(row[j + 1] + 1);
            prev = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

fn same_title(a: &[char], b: &[char]) -> bool {
    if a == b {
        return true;
    }
    a.len().min(b.len()) >= NEAR_MIN_CHARS && a.len().abs_diff(b.len()) <= NEAR_EDITS && edits(a, b) <= NEAR_EDITS
}

fn days(d: jiff::SignedDuration) -> i64 {
    d.as_secs() / 86_400
}

/// One line per task worth a look, most pressing reason first, at most
/// `MAX_CANDIDATES`.
fn candidates(live: &[Live], l: Lang, now: jiff::Timestamp) -> Vec<String> {
    let titles: Vec<Vec<char>> = live.iter().map(|t| folded(&t.title).chars().collect()).collect();
    let mut flagged: Vec<(usize, i64, String, Vec<String>)> = Vec::new();
    for (i, t) in live.iter().enumerate() {
        let mut rank = usize::MAX;
        let mut flags = Vec::new();
        let twins: Vec<i64> = live
            .iter()
            .enumerate()
            .filter(|(j, o)| *j != i && o.parent_id == t.parent_id && !titles[i].is_empty())
            .filter(|(j, _)| same_title(&titles[i], &titles[*j]))
            .map(|(_, o)| o.id)
            .collect();
        let mut flag = |r: usize, text: String| {
            rank = rank.min(r);
            flags.push(text);
        };
        if !twins.is_empty() {
            flag(0, mt::chores_duplicate(l, &twins));
        }
        if let Some(late) = t.due_at.map(|due| days(now.duration_since(due))).filter(|d| *d >= OVERDUE_DAYS) {
            flag(1, mt::chores_overdue(l, late));
        }
        if t.parent_id.is_none() {
            if let Some(idle) = t.touched.map(|at| days(now.duration_since(at))).filter(|d| *d >= STALE_DAYS) {
                flag(2, mt::chores_untouched(l, idle));
            }
        }
        if t.parent_id.is_none() && t.children == 0 && t.duration_min.is_some_and(|m| m >= BIG_MIN) {
            flag(3, mt::chores_no_steps(l, t.duration_min.unwrap_or_default()));
        }
        if t.children == 0 && t.duration_min.is_none() {
            flag(4, mt::chores_no_duration(l));
        }
        if !flags.is_empty() {
            let title: String = t.title.chars().take(MAX_TITLE_CHARS).collect();
            flagged.push((rank, t.id, title, flags));
        }
    }
    flagged.sort_by_key(|(rank, id, ..)| (*rank, *id));
    flagged
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|(_, id, title, flags)| mt::chores_task_line(l, id, &title, &flags))
        .collect()
}

/// A small open step chosen by the clock, so the offer varies run to run.
fn random_pick(live: &[Live], now: jiff::Timestamp) -> Option<&Live> {
    let small: Vec<&Live> =
        live.iter().filter(|t| t.children == 0 && t.duration_min.is_none_or(|m| m <= SMALL_MIN)).collect();
    let i = usize::try_from(now.subsec_nanosecond().unsigned_abs()).unwrap_or(0) % small.len().max(1);
    small.get(i).copied()
}

/// `(id, category, summary)` of a long-term fact.
type Fact = (String, String, String);

/// The next `MEMORY_SLICE` live facts after the user's cursor, wrapping to the
/// start, and how many there are in all. The cursor moves to the last one
/// shown, so successive runs walk the whole memory.
fn memory_round(conn: &Connection, user_id: i64, username: &str) -> Result<(Vec<Fact>, usize)> {
    let after: String = conn
        .query_row("SELECT after FROM memory_rounds WHERE user_id = ?1", [user_id], |r| r.get(0))
        .optional()?
        .unwrap_or_default();
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memory_index WHERE user = ?1 AND archived = 0 AND category != 'note'",
        [username],
        |r| r.get(0),
    )?;
    let slice = |cmp: &str, limit: usize| -> rusqlite::Result<Vec<Fact>> {
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT id, category, summary FROM memory_index
             WHERE user = ?1 AND archived = 0 AND category != 'note' AND id {cmp} ?2
             ORDER BY id LIMIT ?3"
        ))?;
        let rows = stmt.query_map((username, &after, limit as i64), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect()
    };
    let mut rows = slice(">", MEMORY_SLICE)?;
    if rows.len() < MEMORY_SLICE {
        rows.extend(slice("<=", MEMORY_SLICE - rows.len())?);
    }
    if let Some((last, ..)) = rows.last() {
        conn.execute(
            "INSERT INTO memory_rounds (user_id, after) VALUES (?1, ?2)
             ON CONFLICT (user_id) DO UPDATE SET after = excluded.after",
            (user_id, last),
        )?;
    }
    Ok((rows, usize::try_from(total).unwrap_or(0)))
}

/// What the earlier tidy-ups today ended with, so this one does not repeat them.
fn earlier_today(conn: &Connection, user_id: i64, today: jiff::civil::Date) -> Result<Vec<(String, Option<String>)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT e.id, e.wall_time FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND p.date = ?2 AND e.kind = ?3 AND e.origin = ?4 AND e.status = 'done'
         ORDER BY e.wall_time, e.id",
    )?;
    let done: Vec<(i64, String)> = stmt
        .query_map((user_id, today.to_string(), triggers::KIND, ORIGIN), |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if done.is_empty() {
        return Ok(Vec::new());
    }
    let endings = triggers::endings(conn, user_id, today)?;
    Ok(done
        .into_iter()
        .map(|(id, at)| {
            let said = endings.get(&id).filter(|(o, _)| *o == "said" || *o == "rang").and_then(|(_, t)| t.clone());
            (at, said)
        })
        .collect())
}

/// The part of a tidy-up's opening that is its own: what to do, the tasks
/// worth a look, a random small pick, this run's slice of memory, and what the
/// earlier tidy-ups today said. Moves the memory cursor.
pub fn opening(
    conn: &Connection,
    user_id: i64,
    username: &str,
    tz: &jiff::tz::TimeZone,
    l: Lang,
    now: jiff::Timestamp,
) -> Result<String> {
    let mut s = mt::chores_brief(l).to_string();
    let live = live_tasks(conn, user_id)?;
    let lines = candidates(&live, l, now);
    if lines.is_empty() {
        s.push_str(mt::chores_no_tasks(l));
    } else {
        s.push_str(mt::chores_tasks_heading(l));
        for line in lines {
            let _ = writeln!(s, "{line}");
        }
    }
    if let Some(t) = random_pick(&live, now) {
        let title: String = t.title.chars().take(MAX_TITLE_CHARS).collect();
        s.push_str(&mt::chores_random_pick(l, t.id, &title));
    }
    let (facts, total) = memory_round(conn, user_id, username)?;
    if !facts.is_empty() {
        s.push_str(&mt::chores_memory_heading(l, facts.len(), total));
        for (id, category, summary) in &facts {
            let _ = writeln!(s, "{}", mt::chores_memory_line(id, category, summary));
        }
    }
    let earlier = earlier_today(conn, user_id, now.to_zoned(tz.clone()).date())?;
    if !earlier.is_empty() {
        s.push_str(mt::chores_earlier_heading(l));
        for (at, said) in &earlier {
            let _ = writeln!(s, "{}", mt::chores_earlier_line(l, at, said.as_deref()));
        }
    }
    Ok(s)
}

/// The newest event id now: a tidy-up's session lays nothing past it that stays.
pub fn fence(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |r| r.get(0))
}

/// Drops the wake-ups a tidy-up's session laid for the user after `fence`.
pub fn drop_laid_since(conn: &Connection, user_id: i64, fence: i64, now: jiff::Timestamp) -> Result<usize> {
    let mut stmt = conn.prepare_cached(
        "UPDATE events SET status = 'dropped', decided_at = ?1
         WHERE id > ?2 AND kind = ?3 AND origin = 'agent' AND status IN ('pending', 'snoozed')
           AND plan_id IN (SELECT id FROM plans WHERE user_id = ?4)
         RETURNING id",
    )?;
    let ids: Vec<i64> =
        stmt.query_map((now.to_string(), fence, triggers::KIND, user_id), |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    for id in &ids {
        crate::log::record(conn, Some(user_id), "trigger_dropped", &format!("event {id}: laid by a tidy-up"))?;
    }
    Ok(ids.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-29 is a Thursday.
    fn env(extra: &str) -> (Connection, tempfile::TempDir, i64) {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("defaults/user.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, format!("display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n{extra}")).unwrap();
        (conn, tmp, uid)
    }

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    fn day() -> jiff::civil::Date {
        "2026-10-29".parse().unwrap()
    }

    fn laid(conn: &Connection) -> Vec<(String, String)> {
        let mut stmt = conn
            .prepare("SELECT wall_time, status FROM events WHERE origin = 'chores' ORDER BY wall_time")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    }

    fn walls(conn: &Connection) -> Vec<String> {
        laid(conn).into_iter().map(|(w, _)| w).collect()
    }

    #[test]
    fn only_times_inside_the_day_count() {
        let (_, tmp, _) = env("day_start = \"12:00\"\nclose_day_time = \"18:00\"\nchores_times = [\"19:00\", \"11:00\", \"15:00\", \"12:00\", \"15:00\"]\n");
        let cfg = UserConfig::load(tmp.path(), "aki").unwrap();
        assert_eq!(times(&cfg), ["15:00"]);
        let (_, tmp, _) = env("");
        let cfg = UserConfig::load(tmp.path(), "aki").unwrap();
        assert_eq!(times(&cfg), crate::config::DEFAULT_CHORES_TIMES);
        let (_, tmp, _) = env("day_start = \"\"\nclose_day_time = \"\"\nchores_times = [\"23:00\", \"06:00\"]\n");
        assert_eq!(times(&UserConfig::load(tmp.path(), "aki").unwrap()), ["06:00", "23:00"]);
    }

    #[test]
    fn a_bad_time_fails_the_config() {
        let (_, tmp, _) = env("chores_times = [\"9:00\"]\n");
        assert!(UserConfig::load(tmp.path(), "aki").is_err());
    }

    #[test]
    fn laying_is_idempotent_and_skips_what_has_gone_by() {
        let (conn, tmp, uid) = env("");
        let noon = at("2026-10-29T12:00:00Z");
        assert_eq!(lay(&conn, tmp.path(), "aki", uid, day(), noon).unwrap().len(), 2);
        assert!(lay(&conn, tmp.path(), "aki", uid, day(), noon).unwrap().is_empty());
        check(&conn, tmp.path(), noon).unwrap();
        assert_eq!(walls(&conn), ["15:00", "19:00"]);
        assert_eq!(crate::triggers::spent(&conn, uid, day()).unwrap(), 0, "the day's furniture costs nothing");

        conn.execute("UPDATE events SET status = 'done' WHERE wall_time = '15:00'", []).unwrap();
        lay(&conn, tmp.path(), "aki", uid, day(), at("2026-10-29T08:00:00Z")).unwrap();
        assert_eq!(walls(&conn), ["11:00", "15:00", "19:00"], "a fired one is never laid again");
    }

    #[test]
    fn a_time_taken_out_takes_its_pending_wake_up_with_it() {
        let (conn, tmp, uid) = env("");
        let morning = at("2026-10-29T06:00:00Z");
        lay(&conn, tmp.path(), "aki", uid, day(), morning).unwrap();
        conn.execute("UPDATE events SET status = 'done' WHERE wall_time = '11:00'", []).unwrap();
        let mine = tmp.path().join("users/aki/user.toml");
        std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
        std::fs::write(&mine, "chores_times = [\"11:00\", \"16:00\"]\n").unwrap();
        crate::config::forget_cached(&mine);
        lay(&conn, tmp.path(), "aki", uid, day(), morning).unwrap();
        assert_eq!(laid(&conn), [("11:00".into(), "done".into()), ("16:00".into(), "pending".into())]);
        std::fs::write(&mine, "chores_times = []\n").unwrap();
        crate::config::forget_cached(&mine);
        lay(&conn, tmp.path(), "aki", uid, day(), morning).unwrap();
        assert_eq!(walls(&conn), ["11:00"]);
    }

    #[test]
    fn a_user_without_check_ins_gets_none_from_the_sweep() {
        let (conn, tmp, _) = env("checkins_enabled = false\n");
        check(&conn, tmp.path(), at("2026-10-29T06:00:00Z")).unwrap();
        assert!(laid(&conn).is_empty());
    }

    fn task(conn: &Connection, uid: i64, title: &str, parent: Option<i64>, updated: &str, due: Option<&str>, min: Option<u32>) -> i64 {
        conn.execute(
            "INSERT INTO tasks (user_id, title, parent_id, created_at, updated_at, due_at, duration_min)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6)",
            (uid, title, parent, updated, due, min),
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn fact(conn: &Connection, n: usize) {
        conn.execute(
            "INSERT INTO memory_index (user, id, category, summary, path) VALUES ('aki', ?1, 'semantic', ?2, 'x')",
            (format!("m{n:03}"), format!("fact {n}")),
        )
        .unwrap();
    }

    #[test]
    fn the_opening_lists_tasks_worth_a_look() {
        let (conn, _tmp, uid) = env("");
        let now = at("2026-10-29T12:00:00Z");
        let fresh = "2026-10-28T12:00:00Z";
        let a = task(&conn, uid, "Write the lab report", None, fresh, None, Some(30));
        let b = task(&conn, uid, "write the lab-report!", None, fresh, None, Some(30));
        let late = task(&conn, uid, "Pay the bill", None, fresh, Some("2026-10-20T12:00:00Z"), Some(15));
        let old = task(&conn, uid, "Sort photos", None, "2026-10-01T12:00:00Z", None, Some(30));
        let vague = task(&conn, uid, "Thesis", None, fresh, None, Some(240));
        let parent = task(&conn, uid, "Move house", None, fresh, None, None);
        let bare = task(&conn, uid, "Book the van", Some(parent), fresh, None, None);
        let fine = task(&conn, uid, "Call mum", None, fresh, None, Some(10));
        conn.execute("UPDATE tasks SET state = 'dropped' WHERE id = ?1", [fine]).unwrap();
        let s = opening(&conn, uid, "aki", &jiff::tz::TimeZone::UTC, Lang::En, now).unwrap();
        assert!(s.contains(mt::chores_brief(Lang::En)));
        assert!(s.contains(&format!("- t{a} \"Write the lab report\": looks like t{b}")), "{s}");
        assert!(s.contains(&format!("- t{late} \"Pay the bill\": overdue 9 days")), "{s}");
        assert!(s.contains(&format!("- t{old} \"Sort photos\": untouched 28 days")), "{s}");
        assert!(s.contains(&format!("- t{vague} \"Thesis\": 240 min with no steps")), "{s}");
        assert!(s.contains(&format!("- t{bare} \"Book the van\": no duration")), "{s}");
        assert!(!s.contains("Move house\":"), "a parent's size is its steps'\n{s}");
        assert!(!s.contains("Call mum"), "{s}");
        assert!(s.find(&format!("t{a} ")).unwrap() < s.find(&format!("t{late} ")).unwrap(), "duplicates come first");
        assert!(s.contains("Random small pick: t"), "{s}");
    }

    #[test]
    fn the_task_list_is_capped() {
        let (conn, _tmp, uid) = env("");
        for n in 0..40 {
            task(&conn, uid, &format!("errand number {n}"), None, "2026-10-28T12:00:00Z", None, None);
        }
        let s = opening(&conn, uid, "aki", &jiff::tz::TimeZone::UTC, Lang::En, at("2026-10-29T12:00:00Z")).unwrap();
        assert_eq!(s.lines().filter(|l| l.starts_with("- t")).count(), MAX_CANDIDATES);
    }

    #[test]
    fn each_run_looks_over_the_next_slice_of_memory_and_wraps() {
        let (conn, _tmp, uid) = env("");
        for n in 0..45 {
            fact(&conn, n);
        }
        conn.execute("UPDATE memory_index SET archived = 1 WHERE id = 'm001'", []).unwrap();
        let now = at("2026-10-29T12:00:00Z");
        let run = || opening(&conn, uid, "aki", &jiff::tz::TimeZone::UTC, Lang::En, now).unwrap();
        let first = run();
        assert!(first.contains("30 of 44 facts"), "{first}");
        assert!(first.contains("- m000 [semantic] fact 0\n"));
        assert!(!first.contains("m001"), "an archived fact is left alone");
        assert!(first.contains("- m030 [semantic] fact 30\n"));
        assert!(!first.contains("m031"));
        let second = run();
        assert!(second.contains("- m031 [semantic] fact 31\n"), "{second}");
        assert!(second.contains("- m044 "));
        assert!(second.contains("- m000 "), "it wraps to the start");
        assert!(second.contains("- m016 "));
        assert!(!second.contains("m017"));
        assert!(second.find("m044").unwrap() < second.find("m000").unwrap());
        assert!(run().contains("- m017 "));
    }

    #[test]
    fn the_opening_names_what_earlier_tidy_ups_said() {
        let (conn, tmp, uid) = env("");
        let now = jiff::Timestamp::now();
        let today = now.to_zoned(jiff::tz::TimeZone::UTC).date();
        let plan = crate::plan::ensure(&conn, tmp.path(), "aki", uid, today).unwrap();
        let spoke = triggers::insert(&conn, plan, "00:00", "tidy", ORIGIN, None, None, None, now).unwrap();
        let quiet = triggers::insert(&conn, plan, "00:01", "tidy", ORIGIN, None, None, None, now).unwrap();
        triggers::insert(&conn, plan, "23:59", "tidy", ORIGIN, None, None, None, now).unwrap();
        conn.execute("UPDATE events SET status = 'done' WHERE id IN (?1, ?2)", [spoke, quiet]).unwrap();
        crate::log::record(&conn, Some(uid), "trigger_said", &format!("event {spoke}: merged two lab reports")).unwrap();
        crate::log::record(&conn, Some(uid), "trigger_quiet", &format!("event {quiet}: nothing to tidy")).unwrap();
        let s = opening(&conn, uid, "aki", &jiff::tz::TimeZone::UTC, Lang::En, now).unwrap();
        assert!(s.contains("Earlier tidy-ups today:\n- 00:00 said: merged two lab reports\n- 00:01 stayed quiet\n"), "{s}");
        assert!(!s.contains("23:59"), "{s}");
    }

    #[test]
    fn wake_ups_a_tidy_up_laid_are_taken_back() {
        let (conn, tmp, uid) = env("");
        let plan = crate::plan::ensure(&conn, tmp.path(), "aki", uid, day()).unwrap();
        let before = triggers::insert(&conn, plan, "16:00", "earlier", "agent", None, None, None, jiff::Timestamp::now()).unwrap();
        let fence = fence(&conn).unwrap();
        let own = triggers::insert(&conn, plan, "17:00", "later", "agent", None, None, None, jiff::Timestamp::now()).unwrap();
        let wait = triggers::insert(&conn, plan, "17:00", "wait", triggers::AWAIT_ORIGIN, None, None, None, jiff::Timestamp::now()).unwrap();
        assert_eq!(drop_laid_since(&conn, uid, fence, jiff::Timestamp::now()).unwrap(), 1);
        let status = |id: i64| -> String { conn.query_row("SELECT status FROM events WHERE id = ?1", [id], |r| r.get(0)).unwrap() };
        assert_eq!((status(before).as_str(), status(own).as_str(), status(wait).as_str()), ("pending", "dropped", "pending"));
    }

    #[test]
    fn a_tidy_up_waits_out_a_work_session_and_fires_alone() {
        let (conn, tmp, uid) = env("");
        let now = jiff::Timestamp::now();
        let today = now.to_zoned(jiff::tz::TimeZone::UTC).date();
        let plan = crate::plan::ensure(&conn, tmp.path(), "aki", uid, today).unwrap();
        let wall = now.to_zoned(jiff::tz::TimeZone::UTC).strftime("%H:%M").to_string();
        let chore = triggers::insert(&conn, plan, &wall, "tidy", ORIGIN, None, None, None, now).unwrap();
        let other = triggers::insert(&conn, plan, &wall, "check", "agent", None, None, None, now).unwrap();
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, started_at) VALUES (?1, 'essay', ?2)",
            (uid, now.to_string()),
        )
        .unwrap();
        let fired = crate::runner::fire_due(&conn, tmp.path(), now).unwrap();
        assert_eq!(fired.iter().map(|f| f.event_id).collect::<Vec<_>>(), [other]);
        assert!(fired[0].folded.is_empty());
        conn.execute("UPDATE work_sessions SET ended_at = ?1", [now.to_string()]).unwrap();
        let fired = crate::runner::fire_due(&conn, tmp.path(), now).unwrap();
        assert_eq!(fired.iter().map(|f| f.event_id).collect::<Vec<_>>(), [chore]);
    }
}
