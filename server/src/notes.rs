use crate::memory::{self, MemoryFile, NOTE};
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

pub const MAX_TITLE_CHARS: usize = 80;
const UPCOMING_HOURS: i64 = 12;
const STALE_DAYS: i64 = 3;

/// One line of Note's working memory; every instant is an RFC 3339 string.
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub from: Option<String>,
    pub until: Option<String>,
    pub touched_at: Option<String>,
    pub last_nudged_at: Option<String>,
    pub created: String,
}

#[derive(Debug, thiserror::Error)]
pub enum NoteError {
    #[error("{0}")]
    Invalid(String),
    #[error("no note {0}")]
    NotFound(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// `Some(None)` clears a bound.
#[derive(Default)]
pub struct Change {
    pub title: Option<String>,
    pub from: Option<Option<String>>,
    pub until: Option<Option<String>>,
}

pub enum Outcome<'a> {
    Memory { category: Option<&'a str>, summary: Option<&'a str>, body: Option<&'a str> },
    Drop,
}

fn instant(s: Option<&str>) -> Option<jiff::Timestamp> {
    s.and_then(|s| s.parse().ok())
}

impl Note {
    fn of(f: MemoryFile) -> Self {
        Self {
            id: f.id,
            title: f.summary,
            from: f.from,
            until: f.until,
            touched_at: f.touched_at,
            last_nudged_at: f.last_nudged_at,
            created: f.created,
        }
    }

    pub fn is_active(&self, now: jiff::Timestamp) -> bool {
        instant(self.from.as_deref()).is_none_or(|t| t <= now)
            && instant(self.until.as_deref()).is_none_or(|t| t > now)
    }

    pub fn is_upcoming(&self, now: jiff::Timestamp) -> bool {
        let horizon = now + jiff::SignedDuration::from_hours(UPCOMING_HOURS);
        instant(self.from.as_deref()).is_some_and(|t| t > now && t <= horizon)
    }

    /// Past its `until`, or three days since the latest of when it was touched,
    /// written, or its window opened.
    pub fn is_due(&self, now: jiff::Timestamp) -> bool {
        if instant(self.until.as_deref()).is_some_and(|t| t <= now) {
            return true;
        }
        let last = [self.touched_at.as_deref(), self.from.as_deref(), Some(self.created.as_str())]
            .into_iter()
            .filter_map(instant)
            .max();
        last.is_some_and(|t| t <= now - jiff::SignedDuration::from_hours(STALE_DAYS * 24))
    }

    pub fn windowed(&self) -> bool {
        self.from.is_some() || self.until.is_some()
    }
}

/// Whole-second RFC 3339 UTC, so stamps compare correctly as text.
pub fn stamp(ts: jiff::Timestamp) -> String {
    jiff::Timestamp::from_second(ts.as_second()).expect("a whole second of a valid instant").to_string()
}

pub fn checked_title(raw: &str) -> Result<String, NoteError> {
    let title = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let n = title.chars().count();
    if n == 0 || n > MAX_TITLE_CHARS {
        return Err(NoteError::Invalid(format!(
            "title must be one line of 1..={MAX_TITLE_CHARS} characters"
        )));
    }
    Ok(title)
}

/// An RFC 3339 instant, or a wall time or date in `tz`, as an RFC 3339 instant
/// carrying `tz`'s offset.
pub fn parse_when(raw: &str, tz: &jiff::tz::TimeZone) -> Result<String, NoteError> {
    let raw = raw.trim();
    let zoned = if let Ok(ts) = raw.parse::<jiff::Timestamp>() {
        Ok(ts.to_zoned(tz.clone()))
    } else if let Ok(dt) = raw.parse::<jiff::civil::DateTime>() {
        dt.to_zoned(tz.clone())
    } else if let Ok(d) = raw.parse::<jiff::civil::Date>() {
        d.to_zoned(tz.clone())
    } else {
        return Err(NoteError::Invalid(format!(
            "{raw:?} is not a time: use RFC 3339, YYYY-MM-DDTHH:MM or YYYY-MM-DD"
        )));
    };
    let zoned = zoned.map_err(|e| NoteError::Invalid(e.to_string()))?;
    Ok(zoned.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string())
}

fn check_window(from: Option<&str>, until: Option<&str>) -> Result<(), NoteError> {
    if let (Some(f), Some(u)) = (instant(from), instant(until)) {
        if u <= f {
            return Err(NoteError::Invalid("until must come after from".into()));
        }
    }
    Ok(())
}

fn save(conn: &Connection, data_dir: &Path, user: &str, note: &Note) -> Result<()> {
    memory::put(
        conn,
        data_dir,
        user,
        &MemoryFile {
            id: note.id.clone(),
            category: NOTE.into(),
            summary: note.title.clone(),
            body: String::new(),
            supersedes: None,
            until: note.until.clone(),
            created: note.created.clone(),
            archived: false,
            source: None,
            from: note.from.clone(),
            touched_at: note.touched_at.clone(),
            last_nudged_at: note.last_nudged_at.clone(),
        },
    )
}

/// Newest first.
pub fn all(data_dir: &Path, user: &str) -> Result<Vec<Note>> {
    let mut notes: Vec<Note> = memory::live_files(data_dir, user, NOTE)?.into_iter().map(Note::of).collect();
    notes.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| a.id.cmp(&b.id)));
    Ok(notes)
}

/// Newest first.
pub fn active(data_dir: &Path, user: &str, now: jiff::Timestamp) -> Result<Vec<Note>> {
    Ok(all(data_dir, user)?.into_iter().filter(|n| n.is_active(now)).collect())
}

/// Notes whose window opens within the next twelve hours, soonest first.
pub fn upcoming(data_dir: &Path, user: &str, now: jiff::Timestamp) -> Result<Vec<Note>> {
    let mut notes: Vec<Note> = all(data_dir, user)?.into_iter().filter(|n| n.is_upcoming(now)).collect();
    notes.sort_by_key(|n| instant(n.from.as_deref()));
    Ok(notes)
}

pub fn due_to_settle(data_dir: &Path, user: &str, now: jiff::Timestamp) -> Result<Vec<Note>> {
    Ok(all(data_dir, user)?.into_iter().filter(|n| n.is_due(now)).collect())
}

/// `None` for an id that is not one of the user's live notes.
pub fn get(data_dir: &Path, user: &str, id: &str) -> Result<Option<Note>> {
    Ok(memory::read(data_dir, user, id)?
        .filter(|f| f.category == NOTE && !f.archived)
        .map(Note::of))
}

fn need(data_dir: &Path, user: &str, id: &str) -> Result<Note, NoteError> {
    get(data_dir, user, id)?.ok_or_else(|| NoteError::NotFound(id.into()))
}

/// `from` and `until` are instants already read by `parse_when`.
pub fn add(
    conn: &Connection,
    data_dir: &Path,
    user: &str,
    title: &str,
    from: Option<String>,
    until: Option<String>,
    now: jiff::Timestamp,
) -> Result<Note, NoteError> {
    let title = checked_title(title)?;
    check_window(from.as_deref(), until.as_deref())?;
    let at = stamp(now);
    let note = Note {
        id: uuid::Uuid::new_v4().to_string(),
        title,
        from,
        until,
        touched_at: Some(at.clone()),
        last_nudged_at: None,
        created: at,
    };
    save(conn, data_dir, user, &note)?;
    Ok(note)
}

pub fn update(
    conn: &Connection,
    data_dir: &Path,
    user: &str,
    id: &str,
    change: Change,
    now: jiff::Timestamp,
) -> Result<Note, NoteError> {
    let mut note = need(data_dir, user, id)?;
    if let Some(title) = change.title {
        note.title = checked_title(&title)?;
    }
    if let Some(from) = change.from {
        note.from = from;
    }
    if let Some(until) = change.until {
        note.until = until;
    }
    check_window(note.from.as_deref(), note.until.as_deref())?;
    note.touched_at = Some(stamp(now));
    save(conn, data_dir, user, &note)?;
    Ok(note)
}

pub fn remove(conn: &Connection, data_dir: &Path, user: &str, id: &str) -> Result<(), NoteError> {
    need(data_dir, user, id)?;
    memory::remove(conn, data_dir, user, id)?;
    Ok(())
}

/// Stamps `touched_at` on each of `ids` that is one of the user's notes;
/// returns how many.
pub fn touch(conn: &Connection, data_dir: &Path, user: &str, ids: &[String], now: jiff::Timestamp) -> Result<usize> {
    stamp_each(conn, data_dir, user, ids, now, false)
}

/// `touch` that also records the nudge in `last_nudged_at`.
pub fn nudged(conn: &Connection, data_dir: &Path, user: &str, ids: &[String], now: jiff::Timestamp) -> Result<usize> {
    stamp_each(conn, data_dir, user, ids, now, true)
}

fn stamp_each(
    conn: &Connection,
    data_dir: &Path,
    user: &str,
    ids: &[String],
    now: jiff::Timestamp,
    nudge: bool,
) -> Result<usize> {
    let at = stamp(now);
    let mut n = 0;
    for id in ids {
        let Some(mut note) = get(data_dir, user, id)? else { continue };
        note.touched_at = Some(at.clone());
        if nudge {
            note.last_nudged_at = Some(at.clone());
        }
        save(conn, data_dir, user, &note)?;
        n += 1;
    }
    Ok(n)
}

/// Takes a note out of working memory; returns the long-term memory it became.
pub fn settle(
    conn: &Connection,
    data_dir: &Path,
    user: &str,
    id: &str,
    outcome: &Outcome,
) -> Result<Option<String>, NoteError> {
    let note = need(data_dir, user, id)?;
    let kept = match outcome {
        Outcome::Drop => None,
        Outcome::Memory { category, summary, body } => {
            let category = category.unwrap_or(if note.windowed() { "episodic" } else { "semantic" });
            if !memory::CATEGORIES.contains(&category) {
                return Err(NoteError::Invalid(format!("category must be one of {:?}", memory::CATEGORIES)));
            }
            let fact = memory::Fact {
                category,
                summary: summary.unwrap_or(&note.title),
                body: body.unwrap_or(""),
                until: None,
            };
            Some(memory::add_until(conn, data_dir, user, &fact, None)?)
        }
    };
    memory::remove(conn, data_dir, user, id)?;
    Ok(kept)
}

/// Keeps every note still due as a memory, word for word; returns how many.
pub fn settle_leftovers(conn: &Connection, data_dir: &Path, user: &str, now: jiff::Timestamp) -> Result<usize> {
    let due = due_to_settle(data_dir, user, now)?;
    let verbatim = Outcome::Memory { category: None, summary: None, body: None };
    for note in &due {
        settle(conn, data_dir, user, &note.id, &verbatim)?;
    }
    Ok(due.len())
}

/// Writes the rows schema v52 left in `legacy_notes` as working notes, one row
/// at a time, then drops the table; returns how many became notes.
pub fn adopt_legacy(conn: &Connection, data_dir: &Path) -> Result<usize> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'legacy_notes'",
        [],
        |r| r.get(0),
    )?;
    if exists == 0 {
        return Ok(0);
    }
    let rows: Vec<(i64, String, String, String, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT n.id, u.username, n.text, n.created_at, n.last_nudged_at
             FROM legacy_notes n JOIN users u ON u.id = n.user_id ORDER BY n.id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut adopted = 0;
    for (row_id, user, text, created, nudged) in rows {
        let folded = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let title = folded.chars().take(MAX_TITLE_CHARS).collect::<String>().trim_end().to_string();
        if !title.is_empty() {
            let note = Note {
                id: uuid::Uuid::new_v4().to_string(),
                title,
                from: None,
                until: None,
                touched_at: Some(created.clone()),
                last_nudged_at: nudged,
                created,
            };
            save(conn, data_dir, &user, &note)?;
            adopted += 1;
        }
        conn.execute("DELETE FROM legacy_notes WHERE id = ?1", [row_id])?;
    }
    conn.execute_batch("DROP TABLE legacy_notes")?;
    Ok(adopted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    fn env() -> (Connection, tempfile::TempDir) {
        (crate::db::open_memory().unwrap(), tempfile::tempdir().unwrap())
    }

    fn note(conn: &Connection, tmp: &tempfile::TempDir, title: &str, window: (Option<&str>, Option<&str>), made: &str) -> Note {
        add(conn, tmp.path(), "aki", title, window.0.map(str::to_owned), window.1.map(str::to_owned), at(made)).unwrap()
    }

    fn ids(notes: Vec<Note>) -> Vec<String> {
        notes.into_iter().map(|n| n.id).collect()
    }

    #[test]
    fn stamps_are_whole_seconds_in_utc() {
        assert_eq!(stamp(at("2026-09-30T12:00:00.987654321Z")), "2026-09-30T12:00:00Z");
        assert_eq!(stamp(at("2026-09-30T21:00:00+09:00")), "2026-09-30T12:00:00Z");
    }

    #[test]
    fn a_title_is_one_line_of_at_most_80_characters() {
        assert_eq!(checked_title("  call\n the   bank\t").unwrap(), "call the bank");
        assert_eq!(checked_title(&"あ".repeat(80)).unwrap().chars().count(), 80);
        assert!(checked_title(&"あ".repeat(81)).is_err());
        assert!(checked_title(" \n\t ").is_err());
    }

    #[test]
    fn a_time_is_read_in_the_users_zone_and_kept_with_its_offset() {
        let tokyo = jiff::tz::TimeZone::get("Asia/Tokyo").unwrap();
        assert_eq!(parse_when("2026-10-07T06:00:00Z", &tokyo).unwrap(), "2026-10-07T15:00:00+09:00");
        assert_eq!(parse_when("2026-10-07T15:00", &tokyo).unwrap(), "2026-10-07T15:00:00+09:00");
        assert_eq!(parse_when("2026-10-08", &tokyo).unwrap(), "2026-10-08T00:00:00+09:00");
        assert!(matches!(parse_when("friday", &tokyo), Err(NoteError::Invalid(_))));
    }

    #[test]
    fn a_note_is_active_inside_its_window_and_upcoming_within_twelve_hours() {
        let (conn, tmp) = env();
        let open = note(&conn, &tmp, "standing", (None, None), "2026-10-07T00:00:00Z");
        let inside = note(&conn, &tmp, "inside", (Some("2026-10-07T11:00:00Z"), Some("2026-10-07T13:00:00Z")), "2026-10-07T00:00:01Z");
        note(&conn, &tmp, "ended", (None, Some("2026-10-07T12:00:00Z")), "2026-10-07T00:00:02Z");
        let soon = note(&conn, &tmp, "soon", (Some("2026-10-07T23:00:00Z"), None), "2026-10-07T00:00:03Z");
        note(&conn, &tmp, "later", (Some("2026-10-08T01:00:00Z"), None), "2026-10-07T00:00:04Z");
        let now = at("2026-10-07T12:00:00Z");
        assert_eq!(ids(active(tmp.path(), "aki", now).unwrap()), vec![inside.id, open.id], "newest first");
        assert_eq!(ids(upcoming(tmp.path(), "aki", now).unwrap()), vec![soon.id]);
    }

    #[test]
    fn a_window_must_close_after_it_opens() {
        let (conn, tmp) = env();
        let e = add(&conn, tmp.path(), "aki", "x", Some("2026-10-07T12:00:00Z".into()),
            Some("2026-10-07T12:00:00Z".into()), at("2026-10-07T00:00:00Z")).unwrap_err();
        assert!(matches!(e, NoteError::Invalid(_)));
        let n = note(&conn, &tmp, "x", (Some("2026-10-07T12:00:00Z"), None), "2026-10-07T00:00:00Z");
        let change = Change { until: Some(Some("2026-10-07T11:00:00Z".into())), ..Default::default() };
        let e = update(&conn, tmp.path(), "aki", &n.id, change, at("2026-10-07T01:00:00Z")).unwrap_err();
        assert!(matches!(e, NoteError::Invalid(_)));
    }

    #[test]
    fn update_keep_and_a_nudge_stamp_touched_at_and_remove_deletes() {
        let (conn, tmp) = env();
        let n = note(&conn, &tmp, "call the bank", (None, Some("2026-10-07T17:00:00Z")), "2026-10-07T00:00:00Z");
        assert_eq!(n.touched_at.as_deref(), Some("2026-10-07T00:00:00Z"));

        let change = Change { title: Some("call the bank back".into()), until: Some(None), ..Default::default() };
        let u = update(&conn, tmp.path(), "aki", &n.id, change, at("2026-10-07T01:00:00Z")).unwrap();
        assert_eq!(
            (u.title.as_str(), u.until.as_deref(), u.touched_at.as_deref()),
            ("call the bank back", None, Some("2026-10-07T01:00:00Z"))
        );

        let missing = "00000000-0000-4000-8000-000000000404".to_string();
        assert_eq!(touch(&conn, tmp.path(), "aki", &[n.id.clone(), missing], at("2026-10-07T02:00:00Z")).unwrap(), 1);
        assert_eq!(get(tmp.path(), "aki", &n.id).unwrap().unwrap().touched_at.as_deref(), Some("2026-10-07T02:00:00Z"));

        assert_eq!(nudged(&conn, tmp.path(), "aki", std::slice::from_ref(&n.id), at("2026-10-07T03:00:00Z")).unwrap(), 1);
        let g = get(tmp.path(), "aki", &n.id).unwrap().unwrap();
        assert_eq!(
            (g.touched_at.as_deref(), g.last_nudged_at.as_deref()),
            (Some("2026-10-07T03:00:00Z"), Some("2026-10-07T03:00:00Z"))
        );

        remove(&conn, tmp.path(), "aki", &n.id).unwrap();
        assert!(get(tmp.path(), "aki", &n.id).unwrap().is_none());
        assert!(matches!(remove(&conn, tmp.path(), "aki", &n.id), Err(NoteError::NotFound(_))));
    }

    #[test]
    fn a_long_term_memory_is_not_a_note() {
        let (conn, tmp) = env();
        let fact = crate::memory::add(&conn, tmp.path(), "aki", "semantic", "s", "b", None).unwrap();
        assert!(get(tmp.path(), "aki", &fact).unwrap().is_none());
        assert!(matches!(remove(&conn, tmp.path(), "aki", &fact), Err(NoteError::NotFound(_))));
        assert_eq!(touch(&conn, tmp.path(), "aki", std::slice::from_ref(&fact), at("2026-10-07T00:00:00Z")).unwrap(), 0);
        assert!(crate::memory::read(tmp.path(), "aki", &fact).unwrap().is_some());
    }

    #[test]
    fn a_note_falls_due_past_its_until_or_three_days_untouched() {
        let (conn, tmp) = env();
        let ended = note(&conn, &tmp, "ended", (None, Some("2026-10-10T11:00:00Z")), "2026-10-10T00:00:00Z");
        let stale = note(&conn, &tmp, "stale", (None, None), "2026-10-07T12:00:00Z");
        note(&conn, &tmp, "fresh", (None, None), "2026-10-07T12:00:01Z");
        note(&conn, &tmp, "waiting", (Some("2026-10-09T00:00:00Z"), None), "2026-10-01T00:00:00Z");
        let mut due = ids(due_to_settle(tmp.path(), "aki", at("2026-10-10T12:00:00Z")).unwrap());
        due.sort();
        let mut want = vec![ended.id, stale.id];
        want.sort();
        assert_eq!(due, want, "a note written ahead counts from when its window opens");
    }

    #[test]
    fn settling_writes_a_memory_of_the_right_kind_or_drops_the_note() {
        let (conn, tmp) = env();
        let made = "2026-10-07T00:00:00Z";
        let windowed = note(&conn, &tmp, "dentist at three", (Some("2026-10-07T06:00:00Z"), Some("2026-10-07T07:00:00Z")), made);
        let plain = note(&conn, &tmp, "prefers short check-ins", (None, None), made);
        let rewritten = note(&conn, &tmp, "essay draft", (None, None), made);
        let gone = note(&conn, &tmp, "milk", (None, None), made);
        let verbatim = Outcome::Memory { category: None, summary: None, body: None };
        let read = |id: &str| crate::memory::read(tmp.path(), "aki", id).unwrap().unwrap();

        let id = settle(&conn, tmp.path(), "aki", &windowed.id, &verbatim).unwrap().unwrap();
        assert_eq!((read(&id).category.as_str(), read(&id).summary.as_str()), ("episodic", "dentist at three"));

        let id = settle(&conn, tmp.path(), "aki", &plain.id, &verbatim).unwrap().unwrap();
        assert_eq!(read(&id).category, "semantic");

        let own = Outcome::Memory {
            category: Some("procedural"),
            summary: Some("essay drafts start from an outline"),
            body: Some("she writes faster from bullets"),
        };
        let id = settle(&conn, tmp.path(), "aki", &rewritten.id, &own).unwrap().unwrap();
        let f = read(&id);
        assert_eq!(
            (f.category.as_str(), f.summary.as_str(), f.body.as_str()),
            ("procedural", "essay drafts start from an outline", "she writes faster from bullets")
        );

        assert_eq!(settle(&conn, tmp.path(), "aki", &gone.id, &Outcome::Drop).unwrap(), None);
        assert!(all(tmp.path(), "aki").unwrap().is_empty(), "a settled note leaves working memory");
        assert_eq!(crate::memory::live_count(&conn, "aki").unwrap(), 3);
        assert!(matches!(settle(&conn, tmp.path(), "aki", &gone.id, &Outcome::Drop), Err(NoteError::NotFound(_))));
    }

    #[test]
    fn a_bad_category_settles_nothing() {
        let (conn, tmp) = env();
        let n = note(&conn, &tmp, "x", (None, None), "2026-10-07T00:00:00Z");
        let bad = Outcome::Memory { category: Some("note"), summary: None, body: None };
        assert!(matches!(settle(&conn, tmp.path(), "aki", &n.id, &bad), Err(NoteError::Invalid(_))));
        assert!(get(tmp.path(), "aki", &n.id).unwrap().is_some());
    }

    #[test]
    fn leftovers_are_kept_word_for_word_and_touched_notes_stay() {
        let (conn, tmp) = env();
        note(&conn, &tmp, "asked about the essay twice", (None, None), "2026-10-01T00:00:00Z");
        let fresh = note(&conn, &tmp, "sat score lands friday", (None, None), "2026-10-01T00:00:00Z");
        touch(&conn, tmp.path(), "aki", std::slice::from_ref(&fresh.id), at("2026-10-09T00:00:00Z")).unwrap();

        assert_eq!(settle_leftovers(&conn, tmp.path(), "aki", at("2026-10-10T00:00:00Z")).unwrap(), 1);
        assert_eq!(ids(all(tmp.path(), "aki").unwrap()), vec![fresh.id]);
        let kept = crate::memory::list(&conn, "aki", None, 10).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!((kept[0].category.as_str(), kept[0].summary.as_str()), ("semantic", "asked about the essay twice"));
    }
}
