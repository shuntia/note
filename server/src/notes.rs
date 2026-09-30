use crate::tasks::UpdateError;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

pub const MAX_CHARS: usize = 200;
/// How long a done note stays for undo before the nightly run deletes it.
pub const KEEP_DONE_DAYS: i64 = 7;

/// `done_at` is `None` while the note is open.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Note {
    pub id: i64,
    pub text: String,
    pub pinned: bool,
    pub created_at: String,
    pub done_at: Option<String>,
    pub last_nudged_at: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewNote {
    pub text: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotePatch {
    pub text: Option<String>,
    pub pinned: Option<bool>,
    pub done: Option<bool>,
}

/// Whole-second RFC 3339 UTC, so stamps compare correctly as text.
pub fn stamp(ts: jiff::Timestamp) -> String {
    jiff::Timestamp::from_second(ts.as_second()).expect("a whole second of a valid instant").to_string()
}

/// Folds every run of whitespace to one space; the rest must be 1 to
/// `MAX_CHARS` characters.
pub fn checked_text(raw: &str) -> Result<String, UpdateError> {
    let text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let n = text.chars().count();
    if n == 0 || n > MAX_CHARS {
        return Err(UpdateError::Invalid(format!(
            "text must be one line of 1..={MAX_CHARS} characters"
        )));
    }
    Ok(text)
}

fn cutoff(now: jiff::Timestamp) -> String {
    stamp(now - jiff::SignedDuration::from_hours(KEEP_DONE_DAYS * 24))
}

const COLS: &str = "id, text, pinned, created_at, done_at, last_nudged_at";

fn row_to_note(r: &rusqlite::Row) -> rusqlite::Result<Note> {
    Ok(Note {
        id: r.get(0)?,
        text: r.get(1)?,
        pinned: r.get(2)?,
        created_at: r.get(3)?,
        done_at: r.get(4)?,
        last_nudged_at: r.get(5)?,
    })
}

/// Open notes, pinned first and then in the order they were added, followed by
/// the ones done within the last `KEEP_DONE_DAYS`.
pub fn list(conn: &Connection, user_id: i64, now: jiff::Timestamp) -> rusqlite::Result<Vec<Note>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM notes
         WHERE user_id = ?1 AND (done_at IS NULL OR done_at >= ?2)
         ORDER BY done_at IS NOT NULL, pinned DESC, id"
    ))?;
    let rows = stmt.query_map((user_id, cutoff(now)), row_to_note)?;
    rows.collect()
}

pub fn get(conn: &Connection, user_id: i64, id: i64) -> rusqlite::Result<Option<Note>> {
    conn.query_row(
        &format!("SELECT {COLS} FROM notes WHERE id = ?1 AND user_id = ?2"),
        (id, user_id),
        row_to_note,
    )
    .optional()
}

pub fn create(
    conn: &Connection,
    user_id: i64,
    new: NewNote,
    now: jiff::Timestamp,
) -> Result<Note, UpdateError> {
    let text = checked_text(&new.text)?;
    conn.execute(
        "INSERT INTO notes (user_id, text, created_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![user_id, text, stamp(now)],
    )?;
    Ok(get(conn, user_id, conn.last_insert_rowid())?.expect("row was just created"))
}

/// `done: true` keeps an existing `done_at`; `done: false` clears it.
/// `Ok(None)` when the note is not this user's.
pub fn update(
    conn: &Connection,
    user_id: i64,
    id: i64,
    patch: NotePatch,
    now: jiff::Timestamp,
) -> Result<Option<Note>, UpdateError> {
    let text = patch.text.as_deref().map(checked_text).transpose()?;
    let changed = conn.execute(
        "UPDATE notes SET
            text = COALESCE(?1, text),
            pinned = COALESCE(?2, pinned),
            done_at = CASE ?3 WHEN 1 THEN COALESCE(done_at, ?4) WHEN 0 THEN NULL ELSE done_at END
         WHERE id = ?5 AND user_id = ?6",
        rusqlite::params![text, patch.pinned, patch.done, stamp(now), id, user_id],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    Ok(get(conn, user_id, id)?)
}

pub fn delete(conn: &Connection, user_id: i64, id: i64) -> rusqlite::Result<bool> {
    Ok(conn.execute("DELETE FROM notes WHERE id = ?1 AND user_id = ?2", (id, user_id))? > 0)
}

/// Deletes this user's notes done more than `KEEP_DONE_DAYS` before `now`.
pub fn purge_done(conn: &Connection, user_id: i64, now: jiff::Timestamp) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM notes WHERE user_id = ?1 AND done_at IS NOT NULL AND done_at < ?2",
        (user_id, cutoff(now)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    fn db() -> (Connection, i64, i64) {
        let conn = crate::db::open_memory().unwrap();
        let aki = crate::auth::create_user(&conn, "aki", "pw", false).unwrap();
        let bo = crate::auth::create_user(&conn, "bo", "pw", false).unwrap();
        (conn, aki, bo)
    }

    fn add(conn: &Connection, uid: i64, text: &str, now: &str) -> Note {
        create(conn, uid, NewNote { text: text.into() }, at(now)).unwrap()
    }

    fn mark(conn: &Connection, uid: i64, id: i64, done: bool, now: &str) -> Note {
        update(conn, uid, id, NotePatch { done: Some(done), ..Default::default() }, at(now))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn stamps_are_whole_seconds_in_utc() {
        assert_eq!(stamp(at("2026-09-30T12:00:00.987654321Z")), "2026-09-30T12:00:00Z");
        assert_eq!(stamp(at("2026-09-30T21:00:00+09:00")), "2026-09-30T12:00:00Z");
    }

    #[test]
    fn text_is_one_line_of_at_most_200_characters() {
        assert_eq!(checked_text("  call\n the   bank\t").unwrap(), "call the bank");
        assert_eq!(checked_text(&"あ".repeat(200)).unwrap().chars().count(), 200);
        assert!(checked_text(&"あ".repeat(201)).is_err());
        assert!(checked_text(" \n\t ").is_err());
    }

    #[test]
    fn the_list_is_open_notes_pinned_first_then_the_last_weeks_done_ones() {
        let (conn, aki, bo) = db();
        let now = "2026-09-30T12:00:00Z";
        let first = add(&conn, aki, "first", now).id;
        let second = add(&conn, aki, "second", now).id;
        let recent = add(&conn, aki, "recent", now).id;
        let old = add(&conn, aki, "old", now).id;
        add(&conn, bo, "theirs", now);
        update(&conn, aki, second, NotePatch { pinned: Some(true), ..Default::default() }, at(now))
            .unwrap();
        mark(&conn, aki, recent, true, "2026-09-23T12:00:00Z");
        mark(&conn, aki, old, true, "2026-09-23T11:59:59Z");

        let ids: Vec<i64> = list(&conn, aki, at(now)).unwrap().into_iter().map(|n| n.id).collect();
        assert_eq!(ids, vec![second, first, recent]);
    }

    #[test]
    fn checking_off_twice_keeps_the_first_time_and_reopening_clears_it() {
        let (conn, aki, _) = db();
        let id = add(&conn, aki, "milk", "2026-09-30T09:00:00Z").id;
        assert_eq!(
            mark(&conn, aki, id, true, "2026-09-30T10:00:00Z").done_at.as_deref(),
            Some("2026-09-30T10:00:00Z")
        );
        assert_eq!(
            mark(&conn, aki, id, true, "2026-09-30T11:00:00Z").done_at.as_deref(),
            Some("2026-09-30T10:00:00Z")
        );
        assert!(mark(&conn, aki, id, false, "2026-09-30T11:30:00Z").done_at.is_none());

        let later = at("2026-09-30T12:00:00Z");
        let n = update(&conn, aki, id, NotePatch { text: Some("oat milk".into()), ..Default::default() }, later)
            .unwrap()
            .unwrap();
        assert_eq!(
            (n.text.as_str(), n.pinned, n.created_at.as_str()),
            ("oat milk", false, "2026-09-30T09:00:00Z")
        );
        assert!(update(&conn, aki, id, NotePatch { text: Some(" ".into()), ..Default::default() }, later)
            .is_err());
    }

    #[test]
    fn another_users_note_is_out_of_reach() {
        let (conn, aki, bo) = db();
        let theirs = add(&conn, bo, "theirs", "2026-09-30T09:00:00Z").id;
        let now = at("2026-09-30T10:00:00Z");
        assert!(get(&conn, aki, theirs).unwrap().is_none());
        assert!(update(&conn, aki, theirs, NotePatch { done: Some(true), ..Default::default() }, now)
            .unwrap()
            .is_none());
        assert!(!delete(&conn, aki, theirs).unwrap());
        assert!(get(&conn, bo, theirs).unwrap().unwrap().done_at.is_none());
        assert!(delete(&conn, bo, theirs).unwrap());
        assert!(get(&conn, bo, theirs).unwrap().is_none());
    }

    #[test]
    fn purging_drops_only_this_users_notes_done_over_a_week_ago() {
        let (conn, aki, bo) = db();
        let make = |uid: i64, text: &str, done: Option<&str>| {
            let id = add(&conn, uid, text, "2026-09-01T00:00:00Z").id;
            if let Some(ts) = done {
                mark(&conn, uid, id, true, ts);
            }
            id
        };
        let open = make(aki, "open", None);
        let kept = make(aki, "kept", Some("2026-09-23T12:00:00Z"));
        let gone = make(aki, "gone", Some("2026-09-23T11:59:59Z"));
        let theirs = make(bo, "theirs", Some("2026-09-01T00:00:00Z"));

        assert_eq!(purge_done(&conn, aki, at("2026-09-30T12:00:00Z")).unwrap(), 1);
        assert!(get(&conn, aki, open).unwrap().is_some());
        assert!(get(&conn, aki, kept).unwrap().is_some());
        assert!(get(&conn, aki, gone).unwrap().is_none());
        assert!(get(&conn, bo, theirs).unwrap().is_some());
    }
}
