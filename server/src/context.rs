use anyhow::Result;
use rusqlite::Connection;
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

/// Renders the full injection context: the standing document verbatim, then a
/// dynamic block from the DB. Ordered standing-first for prompt-cache
/// stability — the standing doc changes rarely, the dynamic block every call.
pub fn assemble(conn: &Connection, config_dir: &Path, user_id: i64, username: &str, now: jiff::Timestamp) -> Result<String> {
    let ucfg = crate::config::UserConfig::load(config_dir, username)?;
    let (tz, tz_label) = match jiff::tz::TimeZone::get(&ucfg.timezone) {
        Ok(tz) => (tz, ucfg.timezone.clone()),
        Err(_) => (jiff::tz::TimeZone::UTC, "UTC (configured timezone invalid)".into()),
    };
    let local = now.to_zoned(tz);
    let standing = std::fs::read_to_string(standing_path(config_dir, username))
        .unwrap_or_else(|_| "(no standing context yet)".into());

    let mut out = String::new();
    out.push_str("# Standing context\n\n");
    out.push_str(standing.trim());
    out.push_str("\n\n# Now\n\n");
    out.push_str(&format!("{} ({})\n\n", local.strftime("%Y-%m-%d %H:%M"), tz_label));

    out.push_str("# Today's plan\n\n");
    let events = crate::plan::events_for(conn, user_id, local.date())?;
    if events.is_empty() {
        out.push_str("(no plan generated for today)\n");
    }
    for e in &events {
        if let ("block", Some(end)) = (e.entry.as_str(), e.end_wall_time.as_deref()) {
            out.push_str(&format!("- {}-{} {} [block]\n", e.wall_time, end, e.kind));
        } else {
            out.push_str(&format!(
                "- {} {} [{}] via {}{}\n",
                e.wall_time,
                e.kind,
                e.status,
                e.channel,
                if e.alert { "" } else { " (silent)" },
            ));
        }
    }

    out.push_str("\n# Recent activity\n\n");
    let mut stmt = conn.prepare(
        "SELECT ts, kind, detail FROM event_log WHERE user_id = ?1 ORDER BY id DESC LIMIT 10",
    )?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map([user_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        out.push_str("(none)\n");
    }
    for (ts, kind, detail) in rows {
        out.push_str(&format!("- {ts} {kind}: {detail}\n"));
    }
    Ok(out)
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
        &out[out.find("# Now").expect("a Now section")..]
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
        assert!(out.contains("- 09:30-12:30 Work time [pending] block"), "{out}");
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
        assert!(out.contains("- 20:30-22:00 Work time [pending] block <- now"), "{out}");
        assert!(
            out.contains("- 21:30-21:45 wind down [pending] routine via push <- next, in 30 min"),
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

    #[test]
    fn the_later_list_stops_at_ten_titles() {
        let tmp = cfg_dir();
        let (conn, uid) = user();
        for i in 1..=12 {
            task(&conn, uid, &format!("later-{i:02}"), "open", Some(15), false, None, NOW);
        }
        let out = assemble(&conn, tmp.path(), uid, "aki", now_ts()).unwrap();
        assert!(out.contains("Later (12 open):"), "{out}");
        assert!(out.contains("- later-10 15m"), "{out}");
        assert!(!out.contains("later-11"), "{out}");
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
}
