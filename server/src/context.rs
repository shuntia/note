use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// The whole standing document is prepended to every system prompt, so its
/// size is a per-call cost on every session, not just a disk figure.
pub const MAX_STANDING_BYTES: usize = 64 * 1024;

/// Everything after the standing document is rebuilt on every call, so it is a
/// per-turn token cost rather than a per-edit one.
pub const MAX_DYNAMIC_BYTES: usize = 6 * 1024;

#[derive(Debug, Error)]
pub enum EditError {
    #[error("standing.md does not exist yet; use append")]
    Missing,
    #[error("find text not present in standing.md")]
    NoMatch,
    #[error("find text matches {0} places in standing.md; it must match exactly one")]
    Ambiguous(usize),
    #[error("standing.md may hold at most {MAX_STANDING_BYTES} bytes; replace or trim what is there instead of adding to it")]
    TooLarge,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn standing_path(config_dir: &Path, user: &str) -> PathBuf {
    config_dir.join("users").join(user).join("standing.md")
}

/// Writes through a sibling temp file so a crash mid-write can never leave a
/// half-rendered file behind.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// The nightly brief rides in the dynamic block, so it is capped far below the
/// standing document.
pub const MAX_NIGHTLY_NOTES_BYTES: usize = 3 * 1024;

/// Past this age the notes are still shown, but labelled stale.
const NIGHTLY_NOTES_FRESH_DAYS: i32 = 3;

pub fn nightly_notes_path(config_dir: &Path, user: &str) -> PathBuf {
    config_dir.join("users").join(user).join("nightly_notes.md")
}

/// Replaces the notes wholesale, under a marker line dating them so a session
/// can discount what it has outlived.
pub fn write_nightly_notes(
    config_dir: &Path,
    user: &str,
    text: &str,
    date: jiff::civil::Date,
) -> std::io::Result<()> {
    let path = nightly_notes_path(config_dir, user);
    std::fs::create_dir_all(path.parent().expect("nightly_notes.md always has a parent"))?;
    write_atomic(&path, &format!("<!-- written {date} -->\n{}\n", text.trim()))
}

/// The notes as (written date, body); a file without a readable marker keeps
/// its whole text and an empty date, which renders as an unknown age.
fn read_nightly_notes(config_dir: &Path, user: &str) -> Option<(String, String)> {
    let raw = std::fs::read_to_string(nightly_notes_path(config_dir, user)).ok()?;
    let (head, rest) = raw.split_once('\n').unwrap_or((raw.as_str(), ""));
    let dated = head
        .trim()
        .strip_prefix("<!-- written")
        .and_then(|s| s.strip_suffix("-->"))
        .map(|d| (d.trim().to_string(), rest));
    let (date, body) = dated.unwrap_or_else(|| (String::new(), raw.as_str()));
    let body = body.trim();
    (!body.is_empty()).then(|| (date, body.to_string()))
}

pub fn edit_replace(config_dir: &Path, user: &str, find: &str, replace: &str) -> Result<(), EditError> {
    let path = standing_path(config_dir, user);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(EditError::Missing),
        Err(e) => return Err(e.into()),
    };
    if find.is_empty() {
        return Err(EditError::NoMatch);
    }
    match text.matches(find).count() {
        0 => Err(EditError::NoMatch),
        1 => {
            let next = text.replacen(find, replace, 1);
            if next.len() > MAX_STANDING_BYTES {
                return Err(EditError::TooLarge);
            }
            write_atomic(&path, &next)?;
            Ok(())
        }
        n => Err(EditError::Ambiguous(n)),
    }
}

pub fn edit_append(config_dir: &Path, user: &str, text: &str) -> Result<(), EditError> {
    let path = standing_path(config_dir, user);
    std::fs::create_dir_all(path.parent().expect("standing.md always has a parent"))?;
    let mut cur = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    if !cur.is_empty() && !cur.ends_with('\n') {
        cur.push('\n');
    }
    cur.push_str(text);
    cur.push('\n');
    if cur.len() > MAX_STANDING_BYTES {
        return Err(EditError::TooLarge);
    }
    write_atomic(&path, &cur)?;
    Ok(())
}

/// Rows that say how the server ran rather than what the user did.
const OPERATIONAL_LOG_KINDS: &[&str] = &[
    "agent_max_turns",
    "agent_session",
    "delivery_degraded",
    "delivery_ok",
    "login_error",
    "memory_embed_error",
    "memory_expired",
    "nightly_notes_missing",
    "memory_index_error",
    "passkey_added",
    "passkey_removed",
    "passkeys_unavailable",
    "runner_error",
    "token_created",
    "token_revoked",
    "totp_enrolled",
    "totp_removed",
    "voice_unavailable",
];

/// How much of the material that can be shortened survives.
#[derive(Clone, Copy)]
struct Caps {
    later: usize,
    debrief: usize,
    activity: usize,
    notes: bool,
}

/// Walked in order until the block fits: the Later list gives way first, then
/// the debrief, then the activity tail, and last night's notes only once all
/// of those are gone. The real-time line, the Now list and the plan are never
/// among them.
const CAPS: [Caps; 7] = [
    Caps { later: 10, debrief: 600, activity: 10, notes: true },
    Caps { later: 4, debrief: 600, activity: 10, notes: true },
    Caps { later: 0, debrief: 600, activity: 10, notes: true },
    Caps { later: 0, debrief: 200, activity: 10, notes: true },
    Caps { later: 0, debrief: 0, activity: 10, notes: true },
    Caps { later: 0, debrief: 0, activity: 3, notes: true },
    Caps { later: 0, debrief: 0, activity: 3, notes: false },
];

/// Renders the full injection context: the standing document verbatim, then a
/// dynamic block from the DB. Ordered standing-first for prompt-cache
/// stability — the standing doc changes rarely, the dynamic block every call.
pub fn assemble(conn: &Connection, config_dir: &Path, user_id: i64, username: &str, now: jiff::Timestamp) -> Result<String> {
    let ucfg = crate::config::UserConfig::load(config_dir, username)?;
    let (tz, tz_label) = match jiff::tz::TimeZone::get(&ucfg.timezone) {
        Ok(tz) => (tz, ucfg.timezone.clone()),
        Err(_) => (jiff::tz::TimeZone::UTC, "UTC (configured timezone invalid)".into()),
    };
    let local = now.to_zoned(tz.clone());
    let today = local.date();
    let tomorrow = today.tomorrow()?;
    let now_min = i64::from(local.hour()) * 60 + i64::from(local.minute());
    let standing = std::fs::read_to_string(standing_path(config_dir, username))
        .unwrap_or_else(|_| "(no standing context yet)".into());

    let events = crate::plan::events_for(conn, user_id, today)?;
    let tasks = crate::tasks::list(conn, user_id)?;
    let (now_tasks, later): (Vec<_>, Vec<_>) = tasks.iter().partition(|t| t.task.is_now);
    let mut later: Vec<_> = later
        .into_iter()
        .filter(|t| t.task.state == "open" || t.task.state == "in_progress")
        .collect();
    // the nearest deadline first, then the newest of what carries none
    later.sort_by(|a, b| match (&a.task.due_at, &b.task.due_at) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => b.task.id.cmp(&a.task.id),
    });
    let done_today = crate::tasks::done_between(
        conn,
        user_id,
        today.to_zoned(tz.clone())?.timestamp(),
        tomorrow.to_zoned(tz.clone())?.timestamp(),
    )?;
    let category = crate::auth::category(conn, username)?
        .unwrap_or_else(|| crate::config::CATEGORY_MEMBER.into());
    let debrief = latest_debrief(conn, user_id, today)?;
    let activity = recent_activity(conn, user_id)?;
    let notes = read_nightly_notes(config_dir, username);

    let quiet = crate::calendar::quiet_window(conn, user_id, &tz, now)?;
    let calendar = crate::calendar::occurrences(conn, user_id, today)?;

    let now_s =
        now_section(&local, &tz_label, now, &events, now_min, &ucfg.nightly_time, quiet.as_ref());
    let plan_s = plan_section(
        &events, &calendar, now_min, tomorrow, crate::plan::exists(conn, user_id, tomorrow)?,
    );
    let settings_s = settings_section(
        &ucfg,
        &tz_label,
        ucfg.features(&category),
        crate::memory::live_count(conn, username).unwrap_or(0),
    );
    let render = |caps: &Caps| {
        let mut s = String::with_capacity(2048);
        s.push_str(&notes_section(notes.as_ref(), today, caps.notes));
        s.push_str(&now_s);
        s.push_str(&plan_s);
        s.push_str(&tasks_section(&now_tasks, &later, done_today, caps.later, &tz, today, now));
        s.push_str(&debrief_section(debrief.as_ref(), today, caps.debrief));
        s.push_str(&settings_s);
        s.push_str(&activity_section(&activity, &tz, caps.activity));
        s
    };

    let mut block = render(&CAPS[0]);
    for caps in &CAPS[1..] {
        if block.len() <= MAX_DYNAMIC_BYTES {
            break;
        }
        block = render(caps);
    }
    if block.len() > MAX_DYNAMIC_BYTES {
        let mut cut = MAX_DYNAMIC_BYTES;
        while !block.is_char_boundary(cut) {
            cut -= 1;
        }
        block.truncate(cut);
    }
    Ok(format!("# Standing context\n\n{}\n\n{block}", standing.trim()))
}

fn part_of_day(hour: i8) -> &'static str {
    match hour {
        5..=7 => "early morning",
        8..=10 => "morning",
        11..=13 => "midday",
        14..=17 => "afternoon",
        18..=21 => "evening",
        _ => "night",
    }
}

fn in_words(mins: i64) -> String {
    if mins < 60 {
        format!("{mins} min")
    } else {
        format!("{}h{:02}m", mins / 60, mins % 60)
    }
}

fn end_of(e: &crate::plan::PlanEvent) -> &str {
    e.end_wall_time.as_deref().unwrap_or(&e.wall_time)
}

fn now_section(
    local: &jiff::Zoned,
    tz_label: &str,
    now: jiff::Timestamp,
    events: &[crate::plan::PlanEvent],
    now_min: i64,
    nightly_time: &str,
    quiet: Option<&crate::calendar::QuietWindow>,
) -> String {
    let mut s = String::from("# Now\n\n");
    s.push_str(&format!(
        "{} {tz_label} {} | {} | {}\n",
        local.strftime("%A %Y-%m-%d %H:%M"),
        local.strftime("UTC%:z"),
        now.strftime("%Y-%m-%dT%H:%MZ"),
        part_of_day(local.hour()),
    ));
    match events.first() {
        None => s.push_str("Day's plan: none generated for today\n"),
        Some(first) => {
            let last = events.iter().map(end_of).max().unwrap_or(&first.wall_time);
            let left = events
                .iter()
                .filter(|e| crate::templates::wall_minutes(&e.wall_time) > now_min)
                .count();
            s.push_str(&format!(
                "Day's plan: {}-{last}; now {}, {left} event{} left\n",
                first.wall_time,
                local.strftime("%H:%M"),
                if left == 1 { "" } else { "s" },
            ));
        }
    }
    let until = (crate::templates::wall_minutes(nightly_time) - now_min).rem_euclid(24 * 60);
    s.push_str(&format!("Nightly run {nightly_time}, in {}\n", in_words(until)));
    if let Some(q) = quiet {
        s.push_str(&format!("Quiet until {} ({})\n", q.end, q.title));
    }
    s.push('\n');
    s
}

/// The standing commitments a day is built around, above the plan itself. They
/// are never trimmed, so the list itself is bounded.
const MAX_CALENDAR_LINES: usize = 12;

fn calendar_lines(calendar: &[crate::calendar::Occurrence]) -> String {
    if calendar.is_empty() {
        return String::new();
    }
    let mut s = String::from("Calendar:\n");
    for o in calendar.iter().take(MAX_CALENDAR_LINES) {
        let marks = if o.quiet { format!("{}, quiet", o.kind) } else { o.kind.clone() };
        s.push_str(&format!("- {}-{} {} [{marks}]\n", o.start, o.end, o.title));
    }
    if let Some(rest) = calendar.len().checked_sub(MAX_CALENDAR_LINES).filter(|n| *n > 0) {
        s.push_str(&format!("- (+{rest} more)\n"));
    }
    s.push('\n');
    s
}

fn plan_section(
    events: &[crate::plan::PlanEvent],
    calendar: &[crate::calendar::Occurrence],
    now_min: i64,
    tomorrow: jiff::civil::Date,
    tomorrow_planned: bool,
) -> String {
    let mut s = String::from("# Today's plan\n\n");
    s.push_str(&calendar_lines(calendar));
    if events.is_empty() {
        s.push_str("(no plan generated for today)\n");
    }
    let start_of = |e: &crate::plan::PlanEvent| crate::templates::wall_minutes(&e.wall_time);
    let current = events
        .iter()
        .position(|e| start_of(e) <= now_min && now_min < crate::templates::wall_minutes(end_of(e)));
    let next = events.iter().position(|e| start_of(e) > now_min);
    for (i, e) in events.iter().enumerate() {
        let mark = if Some(i) == current {
            " <- now".into()
        } else if Some(i) == next {
            format!(" <- next, in {}", in_words(start_of(e) - now_min))
        } else {
            String::new()
        };
        if e.entry == "block" {
            s.push_str(&format!(
                "- {}-{} {} [{}] block (event_id {}){mark}\n",
                e.wall_time,
                end_of(e),
                e.kind,
                e.status,
                e.id,
            ));
        } else {
            s.push_str(&format!(
                "- {}-{} {} [{}] routine via {}{} (event_id {}){mark}\n",
                e.wall_time,
                end_of(e),
                e.kind,
                e.status,
                e.channel,
                if e.alert { "" } else { " (silent)" },
                e.id,
            ));
        }
    }
    if !events.is_empty() {
        let n = |status: &str| events.iter().filter(|e| e.status == status).count();
        s.push_str(&format!(
            "{} pending, {} fired, {} done, {} dropped, {} snoozed\n",
            n("pending"), n("fired"), n("done"), n("dropped"), n("snoozed"),
        ));
    }
    s.push_str(&format!(
        "Tomorrow's plan ({tomorrow}): {}\n\n",
        if tomorrow_planned { "generated" } else { "not generated yet" },
    ));
    s
}

fn duration(min: Option<u32>) -> String {
    min.map(|d| format!(" {d}m")).unwrap_or_default()
}

/// How a deadline reads next to a title: the near ones in words, the rest as
/// the local day they fall on.
fn due(
    task: &crate::tasks::Task,
    tz: &jiff::tz::TimeZone,
    today: jiff::civil::Date,
    now: jiff::Timestamp,
) -> String {
    let Some(at) = task.due_at.as_deref().and_then(|d| d.parse::<jiff::Timestamp>().ok()) else {
        return String::new();
    };
    if at < now {
        return " overdue".into();
    }
    let day = at.to_zoned(tz.clone()).date();
    match day.since(today).ok().map(|s| s.get_days()) {
        Some(0) => " due today".into(),
        Some(1) => " due tomorrow".into(),
        _ => format!(" due {day}"),
    }
}

/// Tasks with a deadline inside the next three days, and tasks already past
/// theirs.
fn due_soon(tasks: &[&crate::tasks::TaskNode], now: jiff::Timestamp) -> (usize, usize) {
    let horizon = now + jiff::Span::new().hours(24 * DUE_SOON_DAYS);
    let dates = tasks
        .iter()
        .filter_map(|t| t.task.due_at.as_deref())
        .filter_map(|d| d.parse::<jiff::Timestamp>().ok());
    let mut soon = 0;
    let mut overdue = 0;
    for at in dates {
        if at < now {
            overdue += 1;
        } else if at < horizon {
            soon += 1;
        }
    }
    (soon, overdue)
}

const DUE_SOON_DAYS: i64 = 3;

fn tasks_section(
    now_tasks: &[&crate::tasks::TaskNode],
    later: &[&crate::tasks::TaskNode],
    done_today: i64,
    cap: usize,
    tz: &jiff::tz::TimeZone,
    today: jiff::civil::Date,
    now: jiff::Timestamp,
) -> String {
    let mut s = String::from("# Tasks\n\n");
    if now_tasks.is_empty() && later.is_empty() && done_today == 0 {
        s.push_str("(no tasks)\n\n");
        return s;
    }
    if now_tasks.is_empty() {
        s.push_str("Now: (none)\n");
    } else {
        s.push_str("Now:\n");
        for n in now_tasks {
            s.push_str(&format!(
                "- {} [{}]{}{}\n",
                n.task.title,
                n.task.state,
                duration(n.task.duration_min),
                due(&n.task, tz, today, now),
            ));
            for c in &n.children {
                let mark = match c.state.as_str() {
                    "done" => 'x',
                    "in_progress" => '~',
                    _ => ' ',
                };
                s.push_str(&format!("  - [{mark}] {}{}\n", c.title, duration(c.duration_min)));
            }
        }
    }
    if later.is_empty() {
        s.push_str("Later: (none)\n");
    } else if cap == 0 {
        s.push_str(&format!("Later: {} open (titles trimmed for size)\n", later.len()));
    } else {
        s.push_str(&format!("Later ({} open):\n", later.len()));
        for t in later.iter().take(cap) {
            s.push_str(&format!(
                "- {}{}{}\n",
                t.task.title,
                duration(t.task.duration_min),
                due(&t.task, tz, today, now),
            ));
        }
        if let Some(rest) = later.len().checked_sub(cap).filter(|r| *r > 0) {
            s.push_str(&format!("- ... and {rest} more\n"));
        }
    }
    let dated: Vec<_> = now_tasks.iter().chain(later.iter()).copied().collect();
    let (soon, overdue) = due_soon(&dated, now);
    if soon > 0 || overdue > 0 {
        s.push_str(&format!(
            "Due soon: {soon} in the next {DUE_SOON_DAYS} days, {overdue} overdue\n"
        ));
    }
    s.push_str(&format!("Done today: {done_today}\n\n"));
    s
}

fn excerpt(text: &str, max: usize) -> String {
    let flat: String =
        text.trim().chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if flat.chars().count() <= max {
        return flat;
    }
    let mut out: String = flat.chars().take(max).collect();
    out.push('…');
    out
}

fn days_since(date: &str, today: jiff::civil::Date) -> Option<i32> {
    date.parse::<jiff::civil::Date>()
        .ok()
        .and_then(|d| d.until((jiff::Unit::Day, today)).ok())
        .map(|span| span.get_days())
}

fn days_ago(date: &str, today: jiff::civil::Date) -> String {
    match days_since(date, today) {
        None => "date unreadable".into(),
        Some(0) => "today".into(),
        Some(1) => "yesterday".into(),
        Some(n) => format!("{n} days ago"),
    }
}

/// What last night's run left for today's sessions, dated so the model can
/// weigh it, and marked stale once it has outlived its day.
fn notes_section(
    notes: Option<&(String, String)>,
    today: jiff::civil::Date,
    keep: bool,
) -> String {
    let Some((date, body)) = notes.filter(|_| keep) else {
        return String::new();
    };
    let stale = days_since(date, today).is_some_and(|d| d > NIGHTLY_NOTES_FRESH_DAYS);
    format!(
        "# {}Notes from last night (written {date}, {})\n\n{body}\n\n",
        if stale { "(stale) " } else { "" },
        days_ago(date, today),
    )
}

fn debrief_section(row: Option<&(String, String)>, today: jiff::civil::Date, cap: usize) -> String {
    let mut s = String::from("# Latest debrief\n\n");
    match row {
        None => s.push_str("(no debrief yet)\n\n"),
        Some((date, content)) if cap == 0 => {
            s.push_str(&format!("{date} ({}): (trimmed for size)\n\n", days_ago(date, today)));
        }
        Some((date, content)) => {
            s.push_str(&format!(
                "{date} ({}): {}\n\n",
                days_ago(date, today),
                excerpt(content, cap),
            ));
        }
    }
    s
}

fn settings_section(
    cfg: &crate::config::UserConfig,
    tz_label: &str,
    features: crate::config::Features,
    memory_facts: i64,
) -> String {
    let on = |b: bool| if b { "on" } else { "off" };
    format!(
        "# Settings\n\n{} | {tz_label} | nightly_time {} | template {} | counter {} | nightly {} | checkins {}\n\
         Memory: {memory_facts} fact{}\n\n",
        cfg.display_name,
        cfg.nightly_time,
        cfg.template,
        cfg.counter,
        on(features.nightly),
        on(features.checkins),
        if memory_facts == 1 { "" } else { "s" },
    )
}

fn activity_section(
    rows: &[(String, String, String)],
    tz: &jiff::tz::TimeZone,
    cap: usize,
) -> String {
    let mut s = String::from("# Recent activity\n\n");
    if cap == 0 {
        s.push_str("(trimmed for size)\n");
        return s;
    }
    if rows.is_empty() {
        s.push_str("(none)\n");
    }
    for (ts, kind, detail) in rows.iter().take(cap) {
        let when = ts
            .parse::<jiff::Timestamp>()
            .map(|t| t.to_zoned(tz.clone()).strftime("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|_| ts.clone());
        s.push_str(&format!("- {when} {kind}: {detail}\n"));
    }
    s
}

fn latest_debrief(
    conn: &Connection,
    user_id: i64,
    today: jiff::civil::Date,
) -> Result<Option<(String, String)>> {
    Ok(conn
        .query_row(
            "SELECT date, content FROM debriefs
             WHERE user_id = ?1 AND date <= ?2 ORDER BY date DESC LIMIT 1",
            (user_id, today.to_string()),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

fn recent_activity(conn: &Connection, user_id: i64) -> Result<Vec<(String, String, String)>> {
    let denied =
        OPERATIONAL_LOG_KINDS.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT ts, kind, detail FROM event_log
         WHERE user_id = ?1 AND kind NOT IN ({denied})
               AND kind NOT LIKE 'admin\\_%' ESCAPE '\\'
         ORDER BY id DESC LIMIT 10"
    ))?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map([user_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Monday; 21:00 in Asia/Tokyo, 08:00 in America/New_York.
    const NOW: &str = "2026-08-31T12:00:00Z";

    fn now_ts() -> jiff::Timestamp {
        NOW.parse().unwrap()
    }

    fn cfg_dir_tz(tz: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("defaults/user.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            p,
            format!("display_name = \"X\"\ntimezone = \"{tz}\"\ntemplate = \"default\"\n"),
        )
        .unwrap();
        tmp
    }

    fn cfg_dir() -> tempfile::TempDir {
        cfg_dir_tz("Asia/Tokyo")
    }

    fn user() -> (rusqlite::Connection, i64) {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        (conn, uid)
    }

    /// The part rebuilt on every call, which is what the size ceiling covers.
    fn dynamic(out: &str) -> &str {
        let start = ["# Notes from last night", "# (stale) Notes from last night", "# Now"]
            .iter()
            .filter_map(|h| out.find(h))
            .min()
            .expect("a dynamic block");
        &out[start..]
    }

    fn routine(kind: &str, time: &str) -> crate::templates::TemplateEvent {
        crate::templates::TemplateEvent {
            kind: kind.into(),
            time: time.into(),
            days: vec!["mon".into()],
            channel: "push".into(),
            ..Default::default()
        }
    }

    fn plan_today(conn: &rusqlite::Connection, uid: i64, events: Vec<crate::templates::TemplateEvent>) {
        let tmpl = crate::templates::Template { events };
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(conn, uid, &tmpl, date).unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn task(
        conn: &rusqlite::Connection,
        uid: i64,
        title: &str,
        state: &str,
        duration: Option<u32>,
        is_now: bool,
        parent: Option<i64>,
        updated: &str,
    ) -> i64 {
        conn.execute(
            "INSERT INTO tasks (user_id, title, state, source, parent_id, duration_min,
                                duration_source, is_now, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'manual', ?4, ?5, ?6, ?7, ?8, ?8)",
            rusqlite::params![
                uid,
                title,
                state,
                parent,
                duration,
                if duration.is_some() { "user" } else { "none" },
                is_now,
                updated,
            ],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn the_standing_document_has_a_ceiling() {
        let tmp = cfg_dir();
        let path = standing_path(tmp.path(), "aki");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let filled = "x".repeat(MAX_STANDING_BYTES - 16);
        std::fs::write(&path, &filled).unwrap();

        assert!(matches!(
            edit_append(tmp.path(), "aki", &"y".repeat(32)),
            Err(EditError::TooLarge)
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), filled);

        assert!(matches!(
            edit_replace(tmp.path(), "aki", &filled, &"y".repeat(MAX_STANDING_BYTES + 1)),
            Err(EditError::TooLarge)
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), filled);

        edit_append(tmp.path(), "aki", "short").unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().ends_with("short\n"));
    }

    #[test]
    fn append_creates_then_replace_edits() {
        let tmp = cfg_dir();
        edit_append(tmp.path(), "aki", "- prefers morning calls").unwrap();
        edit_append(tmp.path(), "aki", "- studying for exams").unwrap();
        edit_replace(tmp.path(), "aki", "morning calls", "evening calls").unwrap();
        let text = std::fs::read_to_string(standing_path(tmp.path(), "aki")).unwrap();
        assert!(text.contains("evening calls"));
        assert!(text.contains("studying for exams"));
    }

    #[test]
    fn replace_rejects_missing_ambiguous_and_absent_file() {
        let tmp = cfg_dir();
        assert!(matches!(edit_replace(tmp.path(), "aki", "x", "y"), Err(EditError::Missing)));
        edit_append(tmp.path(), "aki", "dup dup").unwrap();
        assert!(matches!(edit_replace(tmp.path(), "aki", "nope", "y"), Err(EditError::NoMatch)));
        assert!(matches!(edit_replace(tmp.path(), "aki", "dup", "y"), Err(EditError::Ambiguous(2))));
        assert!(matches!(edit_replace(tmp.path(), "aki", "", "y"), Err(EditError::NoMatch)));
    }

    #[test]
    fn assemble_renders_all_sections_in_user_tz() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        edit_append(tmp.path(), "aki", "remember: hates mornings").unwrap();
        let tmpl = crate::templates::Template {
            events: vec![crate::templates::TemplateEvent {
                kind: "checkin_call".into(), time: "09:00".into(),
                days: vec!["mon".into()], flexibility: Some("slide".into()),
                slide_window_min: Some(60), channel: "voice".into(), ..Default::default()
            }],
        };
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, uid, &tmpl, date).unwrap();
        crate::log::record(&conn, Some(uid), "event_fired", "event 1").unwrap();
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("hates mornings"), "{out}");
        assert!(out.contains("2026-08-31 21:00"), "{out}");
        assert!(out.contains("Asia/Tokyo"), "{out}");
        assert!(out.contains("- 09:00-09:15 checkin_call [pending] routine via voice"), "{out}");
        assert!(out.contains("event_fired"), "{out}");
    }

    fn commitment(conn: &rusqlite::Connection, uid: i64, title: &str, start: &str, end: &str,
                  kind: &str, quiet: bool) {
        crate::calendar::create(conn, uid, crate::calendar::Fields {
            title: title.into(), kind: kind.into(), quiet: Some(quiet),
            start_time: start.into(), end_time: end.into(),
            days: Some(crate::calendar::day_mask(&["mon"]).unwrap()), ..Default::default()
        }).unwrap();
    }

    #[test]
    fn the_now_line_says_how_long_the_quiet_lasts() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        plan_today(&conn, uid, vec![routine("checkin_call", "09:00")]);
        commitment(&conn, uid, "swim practice", "20:00", "22:00", "fixed", true);

        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("Quiet until 22:00 (swim practice)"), "{out}");
    }

    #[test]
    fn a_day_with_nothing_quiet_running_says_nothing_about_it() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        plan_today(&conn, uid, vec![routine("checkin_call", "09:00")]);
        commitment(&conn, uid, "commute", "20:00", "22:00", "busy", false);

        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(!out.contains("Quiet until"), "{out}");
    }

    #[test]
    fn the_plan_lists_the_days_commitments_before_its_events() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        plan_today(&conn, uid, vec![routine("checkin_call", "09:00")]);
        commitment(&conn, uid, "school", "08:15", "15:30", "fixed", true);
        commitment(&conn, uid, "bin day", "07:00", "07:30", "note", false);

        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        let plan = out.find("# Today's plan").expect("a plan section");
        let calendar = out[plan..].find("Calendar:").expect("a calendar sub-line") + plan;
        let school = out[plan..].find("- 08:15-15:30 school [fixed, quiet]").expect("school") + plan;
        let event = out[plan..].find("- 09:00-09:15 checkin_call").expect("the event") + plan;
        assert!(out.contains("- 07:00-07:30 bin day [note]"), "{out}");
        assert!(calendar < school && school < event, "{out}");
    }

    #[test]
    fn a_crowded_calendar_is_capped_and_counted() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        plan_today(&conn, uid, vec![routine("checkin_call", "09:00")]);
        for i in 0..MAX_CALENDAR_LINES + 3 {
            commitment(&conn, uid, &format!("class {i}"), &format!("{:02}:00", i),
                       &format!("{:02}:30", i), "fixed", true);
        }
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert_eq!(out.matches("class ").count(), MAX_CALENDAR_LINES);
        assert!(out.contains("- (+3 more)"), "{out}");
    }

    #[test]
    fn the_plan_section_distinguishes_blocks_and_silent_routines() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        plan_today(&conn, uid, vec![
            crate::templates::TemplateEvent {
                kind: "Work time".into(), time: "09:30".into(), days: vec!["mon".into()],
                entry: crate::templates::Entry::Block, end_time: Some("12:30".into()),
                channel: "push".into(), ..Default::default() },
            crate::templates::TemplateEvent {
                kind: "meds".into(), time: "08:00".into(), days: vec!["mon".into()],
                alert: Some(false), channel: "push".into(), ..Default::default() },
        ]);
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        let block: i64 = conn
            .query_row("SELECT id FROM events WHERE kind = 'Work time'", [], |r| r.get(0))
            .unwrap();
        assert!(
            out.contains(&format!("- 09:30-12:30 Work time [pending] block (event_id {block})")),
            "{out}"
        );
        assert!(out.contains("- 08:00-08:15 meds [pending] routine via push (silent)"), "{out}");
    }

    #[test]
    fn invalid_tz_is_labeled_as_utc_fallback() {
        let tmp = cfg_dir_tz("Not/AZone");
        let (conn, uid) = user();
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("UTC (configured timezone invalid)"), "{out}");
        assert!(!out.contains("Not/AZone"), "{out}");
    }

    #[test]
    fn assemble_without_standing_or_plan_still_works() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("(no standing context yet)"), "{out}");
        assert!(out.contains("(no plan generated for today)"), "{out}");
        assert!(out.contains("(no tasks)"), "{out}");
        assert!(out.contains("(no debrief yet)"), "{out}");
        assert!(out.contains("Day's plan: none generated for today"), "{out}");
    }

    #[test]
    fn the_now_line_names_weekday_offset_and_part_of_day() {
        let (conn, uid) = user();
        let tokyo = cfg_dir();
        let out = assemble(&conn, tokyo.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("Monday 2026-08-31 21:00 Asia/Tokyo UTC+09:00"), "{out}");
        assert!(out.contains("2026-08-31T12:00Z"), "{out}");
        assert!(out.contains("evening"), "{out}");

        let ny = cfg_dir_tz("America/New_York");
        let out = assemble(&conn, ny.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("Monday 2026-08-31 08:00 America/New_York UTC-04:00"), "{out}");
        assert!(out.contains("2026-08-31T12:00Z"), "{out}");
        assert!(out.contains("morning"), "{out}");
    }

    #[test]
    fn the_now_section_places_the_day_between_its_edges_and_the_nightly_run() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        plan_today(&conn, uid, vec![routine("meds", "08:00"), routine("wind down", "21:30")]);
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("Day's plan: 08:00-21:45; now 21:00, 1 event left"), "{out}");
        assert!(out.contains("Nightly run 03:00, in 6h00m"), "{out}");
    }

    #[test]
    fn the_plan_marks_the_current_event_and_the_next_one() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        plan_today(&conn, uid, vec![
            crate::templates::TemplateEvent {
                kind: "Work time".into(), time: "20:30".into(), days: vec!["mon".into()],
                entry: crate::templates::Entry::Block, end_time: Some("22:00".into()),
                channel: "push".into(), ..Default::default() },
            routine("wind down", "21:30"),
        ]);
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        let id = |kind: &str| -> i64 {
            conn.query_row("SELECT id FROM events WHERE kind = ?1", [kind], |r| r.get(0)).unwrap()
        };
        assert!(
            out.contains(&format!(
                "- 20:30-22:00 Work time [pending] block (event_id {}) <- now",
                id("Work time")
            )),
            "{out}"
        );
        assert!(
            out.contains(&format!(
                "- 21:30-21:45 wind down [pending] routine via push (event_id {}) <- next, in 30 min",
                id("wind down")
            )),
            "{out}"
        );
    }

    #[test]
    fn the_plan_counts_every_status() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        plan_today(&conn, uid, vec![
            routine("a", "07:00"), routine("b", "08:00"), routine("c", "09:00"),
            routine("d", "10:00"), routine("e", "11:00"),
        ]);
        for (kind, status) in [("a", "done"), ("b", "dropped"), ("c", "snoozed"), ("d", "fired")] {
            conn.execute("UPDATE events SET status = ?1 WHERE kind = ?2", (status, kind)).unwrap();
        }
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("1 pending, 1 fired, 1 done, 1 dropped, 1 snoozed"), "{out}");
        assert!(out.contains("- 07:00-07:15 a [done] routine via push"), "{out}");
        assert!(out.contains("- 08:00-08:15 b [dropped] routine via push"), "{out}");
    }

    #[test]
    fn the_now_list_carries_durations_and_step_marks() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        let t = task(&conn, uid, "Write the essay", "in_progress", Some(90), true, None, NOW);
        task(&conn, uid, "outline", "done", Some(30), false, Some(t), NOW);
        task(&conn, uid, "draft", "open", Some(60), false, Some(t), NOW);
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("Now:\n- Write the essay [in_progress] 90m\n"), "{out}");
        assert!(out.contains("  - [x] outline 30m\n"), "{out}");
        assert!(out.contains("  - [ ] draft 60m\n"), "{out}");
        assert!(!out.contains("description"), "{out}");
    }

    fn dated(conn: &rusqlite::Connection, uid: i64, title: &str, is_now: bool, due: &str) -> i64 {
        let id = task(conn, uid, title, "open", None, is_now, None, NOW);
        conn.execute("UPDATE tasks SET due_at = ?1 WHERE id = ?2", (due, id)).unwrap();
        id
    }

    #[test]
    fn the_task_lists_carry_their_deadlines_soonest_first() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        dated(&conn, uid, "essay", true, "2026-08-31T14:00:00Z");
        dated(&conn, uid, "reading", false, "2026-09-05T10:00:00Z");
        dated(&conn, uid, "quiz", false, "2026-08-30T14:00:00Z");
        dated(&conn, uid, "lab", false, "2026-09-01T10:00:00Z");
        task(&conn, uid, "loose", "open", None, false, None, NOW);

        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("Now:\n- essay [open] due today\n"), "{out}");
        assert!(
            out.contains(
                "Later (4 open):\n- quiz overdue\n- lab due tomorrow\n\
                 - reading due 2026-09-05\n- loose\n"
            ),
            "{out}"
        );
        assert!(out.contains("Due soon: 2 in the next 3 days, 1 overdue"), "{out}");
    }

    #[test]
    fn a_list_with_no_deadlines_says_nothing_about_them() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        task(&conn, uid, "loose", "open", Some(15), false, None, NOW);
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("- loose 15m\n"), "{out}");
        assert!(!out.contains("Due soon"), "{out}");
    }

    #[test]
    fn the_later_list_stops_at_ten_titles() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        for i in 1..=12 {
            task(&conn, uid, &format!("later-{i:02}"), "open", Some(15), false, None, NOW);
        }
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("Later (12 open):"), "{out}");
        assert!(out.contains("- later-12 15m"), "{out}");
        assert!(out.contains("- later-03 15m"), "{out}");
        assert!(!out.contains("later-02"), "{out}");
        assert!(out.contains("and 2 more"), "{out}");
        assert!(out.contains("Now: (none)"), "{out}");
    }

    #[test]
    fn done_today_counts_only_todays_completions() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        // The local day runs 2026-08-30T15:00Z .. 2026-08-31T15:00Z in Tokyo.
        task(&conn, uid, "a", "done", None, false, None, "2026-08-30T16:00:00Z");
        task(&conn, uid, "b", "done", None, false, None, "2026-08-31T02:00:00Z");
        task(&conn, uid, "c", "done", None, false, None, "2026-08-30T02:00:00Z");
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("Done today: 2"), "{out}");
    }

    #[test]
    fn the_debrief_excerpt_is_dated_and_capped() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        let content = format!("{}NEEDLE", "d".repeat(650));
        conn.execute(
            "INSERT INTO debriefs (user_id, date, content, created_at)
             VALUES (?1, '2026-08-30', ?2, 't')",
            (uid, &content),
        )
        .unwrap();
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("2026-08-30 (yesterday):"), "{out}");
        assert!(out.contains(&"d".repeat(600)), "{out}");
        assert!(!out.contains(&"d".repeat(601)), "{out}");
        assert!(!out.contains("NEEDLE"), "{out}");
    }

    #[test]
    fn tomorrows_plan_is_flagged_once_it_exists() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("Tomorrow's plan (2026-09-01): not generated yet"), "{out}");

        let tmpl = crate::templates::Template { events: vec![routine("meds", "08:00")] };
        let date: jiff::civil::Date = "2026-09-01".parse().unwrap();
        crate::plan::generate(&conn, uid, &tmpl, date).unwrap();
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("Tomorrow's plan (2026-09-01): generated"), "{out}");
    }

    #[test]
    fn recent_activity_leaves_out_operational_rows() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        for kind in
            ["delivery_ok", "agent_session", "token_created", "admin_user_create", "delivery_degraded"]
        {
            crate::log::record(&conn, Some(uid), kind, "x").unwrap();
        }
        for kind in ["event_fired", "talk_error", "nightly_fallback"] {
            crate::log::record(&conn, Some(uid), kind, "x").unwrap();
        }
        conn.execute(
            "INSERT INTO event_log (ts, user_id, kind, detail)
             VALUES ('2026-08-31T11:30:00Z', ?1, 'event_fired', 'event 3')",
            [uid],
        )
        .unwrap();
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        let tail = &out[out.find("# Recent activity").unwrap()..];
        for kind in
            ["delivery_ok", "agent_session", "token_created", "admin_user_create", "delivery_degraded"]
        {
            assert!(!tail.contains(kind), "{kind} should be filtered out\n{tail}");
        }
        for kind in ["event_fired", "talk_error", "nightly_fallback"] {
            assert!(tail.contains(kind), "{kind} should be kept\n{tail}");
        }
        assert!(tail.contains("- 2026-08-31 20:30 event_fired: event 3"), "{tail}");
    }

    #[test]
    fn the_settings_line_names_what_shapes_advice() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(
            out.contains(
                "X | Asia/Tokyo | nightly_time 03:00 | template default | counter remaining \
                 | nightly on | checkins on"
            ),
            "{out}"
        );
    }

    #[test]
    fn the_dynamic_block_stays_under_its_ceiling() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        edit_append(tmp.path(), "aki", &"s".repeat(4096)).unwrap();
        plan_today(&conn, uid, vec![routine("meds", "08:00")]);
        task(&conn, uid, "Write the essay", "in_progress", Some(90), true, None, NOW);
        for i in 0..120 {
            task(&conn, uid, &format!("later-{i:03} {}", "t".repeat(60)), "open", Some(15), false, None, NOW);
        }
        conn.execute(
            "INSERT INTO debriefs (user_id, date, content, created_at)
             VALUES (?1, '2026-08-30', ?2, 't')",
            (uid, "d".repeat(4000)),
        )
        .unwrap();
        for _ in 0..20 {
            crate::log::record(&conn, Some(uid), "event_fired", &"e".repeat(300)).unwrap();
        }
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        let block = dynamic(&out);
        assert!(block.len() <= MAX_DYNAMIC_BYTES, "{} bytes", block.len());
        assert!(block.contains("Monday 2026-08-31 21:00"), "{block}");
        assert!(block.contains("- 08:00-08:15 meds [pending] routine via push"), "{block}");
        assert!(block.contains("- Write the essay [in_progress] 90m"), "{block}");
        assert!(out.contains(&"s".repeat(4096)), "the standing document is never trimmed");
    }

    #[test]
    fn a_typical_day_stays_terse() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        edit_append(tmp.path(), "aki", "- prefers evening calls").unwrap();
        plan_today(&conn, uid, vec![
            routine("meds", "08:00"), routine("checkin", "12:00"), routine("wind down", "21:30"),
        ]);
        let t = task(&conn, uid, "Write the essay", "in_progress", Some(90), true, None, NOW);
        task(&conn, uid, "outline", "done", Some(30), false, Some(t), NOW);
        task(&conn, uid, "draft", "open", Some(60), false, Some(t), NOW);
        for i in 1..=5 {
            task(&conn, uid, &format!("later task {i}"), "open", Some(30), false, None, NOW);
        }
        conn.execute(
            "INSERT INTO debriefs (user_id, date, content, created_at)
             VALUES (?1, '2026-08-30', ?2, 't')",
            (uid, "Yesterday went well. Two steps left on the essay."),
        )
        .unwrap();
        for _ in 0..4 {
            crate::log::record(&conn, Some(uid), "event_fired", "event 3 due 2026-08-31T03:00:00Z")
                .unwrap();
        }
        let block = dynamic(&assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap()).to_string();
        assert!(block.len() <= 1600, "a typical day is {} bytes:\n{block}", block.len());
    }

    fn notes(tmp: &tempfile::TempDir, date: &str, body: &str) {
        write_nightly_notes(tmp.path(), "aki", body, date.parse().unwrap()).unwrap();
    }

    #[test]
    fn last_nights_notes_sit_between_the_standing_document_and_the_day() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        edit_append(tmp.path(), "aki", "- prefers evening calls").unwrap();
        notes(&tmp, "2026-08-30", "the essay is the one that matters\nlow energy after 21:00");
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        let standing = out.find("# Standing context").expect("a standing section");
        let header = out
            .find("# Notes from last night (written 2026-08-30, yesterday)")
            .unwrap_or_else(|| panic!("{out}"));
        let now = out.find("# Now").expect("a Now section");
        assert!(standing < header && header < now, "{out}");
        assert!(out.contains("low energy after 21:00"), "{out}");
    }

    #[test]
    fn no_notes_file_means_no_notes_section() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(!out.contains("Notes from last night"), "{out}");
    }

    #[test]
    fn notes_older_than_three_days_are_labeled_stale() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        notes(&tmp, "2026-08-28", "the essay is the one that matters");
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(
            out.contains("# Notes from last night (written 2026-08-28, 3 days ago)"),
            "{out}"
        );

        notes(&tmp, "2026-08-27", "the essay is the one that matters");
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(
            out.contains("# (stale) Notes from last night (written 2026-08-27, 4 days ago)"),
            "{out}"
        );
        assert!(out.contains("the essay is the one that matters"), "{out}");
    }

    #[test]
    fn last_nights_notes_outlive_the_later_list_and_the_debrief() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        notes(&tmp, "2026-08-30", "the essay is the one that matters\nlow energy after 21:00");
        plan_today(&conn, uid, vec![routine("meds", "08:00")]);
        for i in 0..120 {
            task(&conn, uid, &format!("later-{i:03} {}", "t".repeat(60)), "open", Some(15), false, None, NOW);
        }
        conn.execute(
            "INSERT INTO debriefs (user_id, date, content, created_at)
             VALUES (?1, '2026-08-30', ?2, 't')",
            (uid, "d".repeat(4000)),
        )
        .unwrap();
        for _ in 0..20 {
            crate::log::record(&conn, Some(uid), "event_fired", &"e".repeat(700)).unwrap();
        }
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        let block = dynamic(&out);
        assert!(block.len() <= MAX_DYNAMIC_BYTES, "{} bytes", block.len());
        assert!(block.contains("Later: 120 open (titles trimmed for size)"), "{block}");
        assert!(block.contains("(trimmed for size)"), "{block}");
        assert!(block.contains("low energy after 21:00"), "{block}");
    }

    #[test]
    fn the_settings_section_counts_the_searchable_memory() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("Memory: 0 facts"), "{out}");

        for summary in ["sister is called Rin", "hates phone calls"] {
            crate::memory::add(&conn, tmp.path(), "aki", "semantic", summary, "body", None).unwrap();
        }
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("Memory: 2 facts"), "{out}");
    }
}
