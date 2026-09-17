use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAX_ENTRIES: i64 = 100;
pub const MAX_TITLE_CHARS: usize = 80;
pub const KINDS: [&str; 3] = ["fixed", "busy", "note"];
pub const DAY_NAMES: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

#[derive(Debug, Error)]
pub enum CalendarError {
    #[error("{0}")]
    Invalid(String),
    #[error("a calendar holds at most {MAX_ENTRIES} entries")]
    TooMany,
    #[error("no calendar entry {0}")]
    NotFound(i64),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

fn invalid(message: impl Into<String>) -> CalendarError {
    CalendarError::Invalid(message.into())
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub id: i64,
    pub title: String,
    pub kind: String,
    pub quiet: bool,
    pub start_time: String,
    pub end_time: String,
    /// Bitmask, Mon = 1 … Sun = 64; 0 for a one-off entry.
    pub days: i64,
    pub day_names: Vec<&'static str>,
    pub on_date: Option<String>,
    pub from_date: Option<String>,
    pub until_date: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub exceptions: Vec<String>,
}

/// One entry as it lands on a given local date.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Occurrence {
    pub entry_id: i64,
    pub title: String,
    pub kind: String,
    pub quiet: bool,
    pub start: String,
    pub end: String,
}

/// The quiet window `now` sits inside, merged across adjacent and overlapping
/// entries, named after the one that ends it.
#[derive(Debug, Clone, PartialEq)]
pub struct QuietWindow {
    pub title: String,
    pub end: String,
    pub until: jiff::Timestamp,
}

/// Every field an entry is written from; the same shape validates a create and
/// the result of applying a patch.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fields {
    pub title: String,
    pub kind: String,
    #[serde(default)]
    pub quiet: Option<bool>,
    pub start_time: String,
    pub end_time: String,
    #[serde(default)]
    pub days: Option<i64>,
    #[serde(default)]
    pub on_date: Option<String>,
    #[serde(default)]
    pub from_date: Option<String>,
    #[serde(default)]
    pub until_date: Option<String>,
}

/// Any subset of an entry's fields. `days` above zero makes an entry recurring
/// and clears its date; a non-blank `on_date` makes it one-off; a blank string
/// clears an optional date.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub quiet: Option<bool>,
    #[serde(default)]
    pub start_time: Option<String>,
    #[serde(default)]
    pub end_time: Option<String>,
    #[serde(default)]
    pub days: Option<i64>,
    #[serde(default)]
    pub on_date: Option<String>,
    #[serde(default)]
    pub from_date: Option<String>,
    #[serde(default)]
    pub until_date: Option<String>,
}

pub fn day_names(mask: i64) -> Vec<&'static str> {
    DAY_NAMES
        .iter()
        .enumerate()
        .filter(|(i, _)| mask & (1 << i) != 0)
        .map(|(_, d)| *d)
        .collect()
}

pub fn day_mask(names: &[impl AsRef<str>]) -> Result<i64, CalendarError> {
    let mut mask = 0;
    for name in names {
        let name = name.as_ref();
        let i = DAY_NAMES
            .iter()
            .position(|d| *d == name)
            .ok_or_else(|| invalid(format!("unknown day {name:?}")))?;
        mask |= 1 << i;
    }
    Ok(mask)
}

fn weekday_bit(date: jiff::civil::Date) -> i64 {
    1 << (date.weekday().to_monday_zero_offset() as i64)
}

fn blank_to_none(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn check_date(field: &str, value: &Option<String>) -> Result<(), CalendarError> {
    match value {
        Some(v) if v.parse::<jiff::civil::Date>().is_err() => {
            Err(invalid(format!("{field} must be YYYY-MM-DD, got {v:?}")))
        }
        _ => Ok(()),
    }
}

/// The one gate every write passes: a stored row always satisfies it, so
/// reading code never has to.
fn validate(f: Fields) -> Result<Fields, CalendarError> {
    let title = f.title.trim().to_string();
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS {
        return Err(invalid(format!(
            "title must be 1 to {MAX_TITLE_CHARS} characters"
        )));
    }
    if !KINDS.contains(&f.kind.as_str()) {
        return Err(invalid(format!("kind must be one of {}", KINDS.join(", "))));
    }
    for (field, value) in [("start_time", &f.start_time), ("end_time", &f.end_time)] {
        if !crate::templates::valid_time(value) {
            return Err(invalid(format!(
                "{field} must be zero-padded HH:MM, got {value:?}"
            )));
        }
    }
    if f.end_time <= f.start_time {
        return Err(invalid(format!(
            "an entry must end after it starts, got {}-{}",
            f.start_time, f.end_time
        )));
    }
    let days = f.days.unwrap_or(0);
    if !(0..=127).contains(&days) {
        return Err(invalid(
            "days must be a bitmask in 0..=127, Mon = 1 … Sun = 64",
        ));
    }
    let on_date = blank_to_none(f.on_date);
    let from_date = blank_to_none(f.from_date);
    let until_date = blank_to_none(f.until_date);
    check_date("on_date", &on_date)?;
    check_date("from_date", &from_date)?;
    check_date("until_date", &until_date)?;
    if days == 0 && on_date.is_none() {
        return Err(invalid(
            "an entry with no days needs on_date, the one day it happens",
        ));
    }
    if days > 0 && on_date.is_some() {
        return Err(invalid("a recurring entry has days or on_date, not both"));
    }
    if days == 0 && (from_date.is_some() || until_date.is_some()) {
        return Err(invalid(
            "from_date and until_date bound a recurring entry, not a one-off",
        ));
    }
    if let (Some(from), Some(until)) = (&from_date, &until_date) {
        if from > until {
            return Err(invalid(format!(
                "from_date {from} is after until_date {until}"
            )));
        }
    }
    // An informational entry is never a reason to hold a delivery.
    let quiet = f.kind != "note" && f.quiet.unwrap_or(true);
    Ok(Fields {
        title,
        quiet: Some(quiet),
        days: Some(days),
        on_date,
        from_date,
        until_date,
        ..f
    })
}

const COLUMNS: &str = "id, title, kind, quiet, start_time, end_time, days, on_date, from_date,
                       until_date, created_at, updated_at";

fn row_to_entry(r: &rusqlite::Row) -> rusqlite::Result<Entry> {
    let days: i64 = r.get(6)?;
    Ok(Entry {
        id: r.get(0)?,
        title: r.get(1)?,
        kind: r.get(2)?,
        quiet: r.get(3)?,
        start_time: r.get(4)?,
        end_time: r.get(5)?,
        days,
        day_names: day_names(days),
        on_date: r.get(7)?,
        from_date: r.get(8)?,
        until_date: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
        exceptions: Vec::new(),
    })
}

fn exceptions_of(conn: &Connection, entry_id: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT date FROM calendar_exceptions WHERE entry_id = ?1 ORDER BY date")?;
    let rows = stmt.query_map([entry_id], |r| r.get(0))?;
    rows.collect()
}

pub fn get(conn: &Connection, user_id: i64, id: i64) -> rusqlite::Result<Option<Entry>> {
    let entry = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM calendar_entries WHERE id = ?1 AND user_id = ?2"),
            (id, user_id),
            row_to_entry,
        )
        .optional()?;
    entry
        .map(|mut e| {
            e.exceptions = exceptions_of(conn, e.id)?;
            Ok(e)
        })
        .transpose()
}

pub fn list(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<Entry>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM calendar_entries WHERE user_id = ?1 ORDER BY start_time, id"
    ))?;
    let rows = stmt.query_map([user_id], row_to_entry)?;
    let mut entries: Vec<Entry> = rows.collect::<rusqlite::Result<_>>()?;
    for e in &mut entries {
        e.exceptions = exceptions_of(conn, e.id)?;
    }
    Ok(entries)
}

pub fn create(conn: &Connection, user_id: i64, fields: Fields) -> Result<Entry, CalendarError> {
    let f = validate(fields)?;
    let held: i64 = conn.query_row(
        "SELECT COUNT(*) FROM calendar_entries WHERE user_id = ?1",
        [user_id],
        |r| r.get(0),
    )?;
    if held >= MAX_ENTRIES {
        return Err(CalendarError::TooMany);
    }
    let now = jiff::Timestamp::now().to_string();
    conn.execute(
        "INSERT INTO calendar_entries
            (user_id, title, kind, quiet, start_time, end_time, days, on_date, from_date,
             until_date, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
        rusqlite::params![
            user_id,
            f.title,
            f.kind,
            f.quiet,
            f.start_time,
            f.end_time,
            f.days,
            f.on_date,
            f.from_date,
            f.until_date,
            now,
        ],
    )?;
    let id = conn.last_insert_rowid();
    Ok(get(conn, user_id, id)?.expect("the row just written is the caller's"))
}

pub fn update(
    conn: &Connection,
    user_id: i64,
    id: i64,
    patch: Patch,
) -> Result<Entry, CalendarError> {
    let cur = get(conn, user_id, id)?.ok_or(CalendarError::NotFound(id))?;
    let recurring = patch.days.is_some_and(|d| d > 0);
    let one_off = patch
        .on_date
        .as_deref()
        .is_some_and(|d| !d.trim().is_empty());
    if recurring && one_off {
        return Err(invalid(
            "an entry recurs on days or happens on one date, not both",
        ));
    }
    let f = Fields {
        title: patch.title.unwrap_or(cur.title),
        kind: patch.kind.unwrap_or(cur.kind),
        quiet: Some(patch.quiet.unwrap_or(cur.quiet)),
        start_time: patch.start_time.unwrap_or(cur.start_time),
        end_time: patch.end_time.unwrap_or(cur.end_time),
        days: Some(match (patch.days, one_off) {
            (Some(d), _) => d,
            (None, true) => 0,
            (None, false) => cur.days,
        }),
        on_date: match (patch.on_date, recurring) {
            (_, true) => None,
            (Some(d), _) => Some(d),
            (None, _) => cur.on_date,
        },
        from_date: patch.from_date.or(cur.from_date),
        until_date: patch.until_date.or(cur.until_date),
    };
    let f = validate(f)?;
    conn.execute(
        "UPDATE calendar_entries SET title = ?1, kind = ?2, quiet = ?3, start_time = ?4,
             end_time = ?5, days = ?6, on_date = ?7, from_date = ?8, until_date = ?9,
             updated_at = ?10
         WHERE id = ?11 AND user_id = ?12",
        rusqlite::params![
            f.title,
            f.kind,
            f.quiet,
            f.start_time,
            f.end_time,
            f.days,
            f.on_date,
            f.from_date,
            f.until_date,
            jiff::Timestamp::now().to_string(),
            id,
            user_id,
        ],
    )?;
    Ok(get(conn, user_id, id)?.expect("the row just written is the caller's"))
}

pub fn delete(conn: &Connection, user_id: i64, id: i64) -> rusqlite::Result<bool> {
    conn.execute("DELETE FROM calendar_exceptions WHERE entry_id = ?1", [id])?;
    let n = conn.execute(
        "DELETE FROM calendar_entries WHERE id = ?1 AND user_id = ?2",
        (id, user_id),
    )?;
    Ok(n > 0)
}

/// Skips one occurrence. Skipping a date twice is the state the caller asked
/// for, so it succeeds.
pub fn skip(conn: &Connection, user_id: i64, id: i64, date: &str) -> Result<(), CalendarError> {
    if date.parse::<jiff::civil::Date>().is_err() {
        return Err(invalid(format!("date must be YYYY-MM-DD, got {date:?}")));
    }
    if get(conn, user_id, id)?.is_none() {
        return Err(CalendarError::NotFound(id));
    }
    conn.execute(
        "INSERT OR IGNORE INTO calendar_exceptions (entry_id, date) VALUES (?1, ?2)",
        (id, date),
    )?;
    Ok(())
}

pub fn unskip(conn: &Connection, user_id: i64, id: i64, date: &str) -> Result<(), CalendarError> {
    if get(conn, user_id, id)?.is_none() {
        return Err(CalendarError::NotFound(id));
    }
    conn.execute(
        "DELETE FROM calendar_exceptions WHERE entry_id = ?1 AND date = ?2",
        (id, date),
    )?;
    Ok(())
}

/// What the user's calendar puts on one local date, earliest first.
pub fn occurrences(
    conn: &Connection,
    user_id: i64,
    date: jiff::civil::Date,
) -> rusqlite::Result<Vec<Occurrence>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, kind, quiet, start_time, end_time
         FROM calendar_entries e
         WHERE user_id = ?1
           AND ( (days = 0 AND on_date = ?2)
              OR (days > 0 AND (days & ?3) != 0
                  AND (from_date IS NULL OR from_date <= ?2)
                  AND (until_date IS NULL OR until_date >= ?2)) )
           AND NOT EXISTS (
                 SELECT 1 FROM calendar_exceptions x
                 WHERE x.entry_id = e.id AND x.date = ?2)
         ORDER BY start_time, end_time, id",
    )?;
    let rows = stmt.query_map((user_id, date.to_string(), weekday_bit(date)), |r| {
        Ok(Occurrence {
            entry_id: r.get(0)?,
            title: r.get(1)?,
            kind: r.get(2)?,
            quiet: r.get(3)?,
            start: r.get(4)?,
            end: r.get(5)?,
        })
    })?;
    rows.collect()
}

fn covers(occ: &Occurrence, start: &str, end: &str) -> bool {
    if start == end {
        occ.start.as_str() <= start && start < occ.end.as_str()
    } else {
        occ.start.as_str() < end && start < occ.end.as_str()
    }
}

/// The hard commitments a `start`-`end` range runs into. A range whose end
/// equals its start is a moment, and is inside a window rather than touching
/// it.
pub fn overlaps(
    conn: &Connection,
    user_id: i64,
    date: jiff::civil::Date,
    start: &str,
    end: &str,
) -> rusqlite::Result<Vec<Occurrence>> {
    Ok(occurrences(conn, user_id, date)?
        .into_iter()
        .filter(|o| o.kind == "fixed" && covers(o, start, end))
        .collect())
}

/// Why the day cannot hold something at `start`-`end`, phrased for whoever
/// asked to put it there.
pub fn conflict(
    conn: &Connection,
    user_id: i64,
    date: jiff::civil::Date,
    start: &str,
    end: &str,
) -> rusqlite::Result<Option<String>> {
    Ok(overlaps(conn, user_id, date, start, end)?
        .first()
        .map(|o| format!("inside {} {}-{}", o.title, o.start, o.end)))
}

/// The merged quiet window `now` falls inside, if any: adjacent and
/// overlapping quiet occurrences make one window, which the latest-ending of
/// them names and closes.
pub fn quiet_window(
    conn: &Connection,
    user_id: i64,
    tz: &jiff::tz::TimeZone,
    now: jiff::Timestamp,
) -> Result<Option<QuietWindow>> {
    let local = now.to_zoned(tz.clone());
    let date = local.date();
    let now_time = format!("{:02}:{:02}", local.hour(), local.minute());
    let quiet: Vec<Occurrence> = occurrences(conn, user_id, date)?
        .into_iter()
        .filter(|o| o.quiet)
        .collect();

    let mut window: Option<(String, String, String)> = None; // start, end, title
    for occ in quiet {
        window = match window {
            Some((start, end, title)) if occ.start <= end => {
                if occ.end > end {
                    Some((start, occ.end, occ.title))
                } else {
                    Some((start, end, title))
                }
            }
            Some((start, end, title)) if start <= now_time && now_time < end => {
                return Ok(Some(window_at(tz, date, &title, &end)?));
            }
            _ => Some((occ.start, occ.end, occ.title)),
        };
    }
    match window {
        Some((start, end, title)) if start <= now_time && now_time < end => {
            Ok(Some(window_at(tz, date, &title, &end)?))
        }
        _ => Ok(None),
    }
}

fn window_at(
    tz: &jiff::tz::TimeZone,
    date: jiff::civil::Date,
    title: &str,
    end: &str,
) -> Result<QuietWindow> {
    let time: jiff::civil::Time = format!("{end}:00").parse()?;
    Ok(QuietWindow {
        title: title.to_string(),
        end: end.to_string(),
        until: tz
            .to_ambiguous_zoned(date.to_datetime(time))
            .compatible()?
            .timestamp(),
    })
}

/// When the current quiet window ends, for a caller that only needs the
/// instant.
pub fn quiet_until(
    conn: &Connection,
    user_id: i64,
    tz: &jiff::tz::TimeZone,
    now: jiff::Timestamp,
) -> Result<Option<jiff::Timestamp>> {
    Ok(quiet_window(conn, user_id, tz, now)?.map(|w| w.until))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Connection {
        let conn = crate::db::open_memory().unwrap();
        for name in ["aki", "rin"] {
            conn.execute(
                "INSERT INTO users (username, pass_hash, role) VALUES (?1, 'x', 'member')",
                [name],
            )
            .unwrap();
        }
        conn
    }

    fn fields(title: &str, kind: &str, start: &str, end: &str, days: &[&str]) -> Fields {
        Fields {
            title: title.into(),
            kind: kind.into(),
            start_time: start.into(),
            end_time: end.into(),
            days: Some(day_mask(days).unwrap()),
            ..Default::default()
        }
    }

    fn school(conn: &Connection) -> Entry {
        create(
            conn,
            1,
            fields(
                "school",
                "fixed",
                "08:15",
                "15:30",
                &["mon", "tue", "wed", "thu", "fri"],
            ),
        )
        .unwrap()
    }

    fn date(s: &str) -> jiff::civil::Date {
        s.parse().unwrap()
    }

    fn utc() -> jiff::tz::TimeZone {
        jiff::tz::TimeZone::UTC
    }

    #[test]
    fn a_recurring_entry_lands_on_its_weekdays_only() {
        let conn = env();
        school(&conn);
        // 2026-09-14 is a Monday
        let names: Vec<Vec<String>> = (14..=20)
            .map(|d| {
                occurrences(&conn, 1, date(&format!("2026-09-{d}")))
                    .unwrap()
                    .into_iter()
                    .map(|o| o.title)
                    .collect()
            })
            .collect();
        assert_eq!(names[0], vec!["school".to_string()], "monday");
        assert_eq!(names[4], vec!["school".to_string()], "friday");
        assert!(names[5].is_empty(), "saturday");
        assert!(names[6].is_empty(), "sunday");
    }

    #[test]
    fn a_one_off_entry_lands_on_its_date_alone() {
        let conn = env();
        create(
            &conn,
            1,
            Fields {
                on_date: Some("2026-09-19".into()),
                ..fields("dentist", "fixed", "10:00", "11:00", &[])
            },
        )
        .unwrap();
        assert_eq!(occurrences(&conn, 1, date("2026-09-19")).unwrap().len(), 1);
        assert!(occurrences(&conn, 1, date("2026-09-20"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_validity_range_bounds_a_recurring_entry() {
        let conn = env();
        create(
            &conn,
            1,
            Fields {
                from_date: Some("2026-09-15".into()),
                until_date: Some("2026-09-16".into()),
                ..fields("term", "fixed", "08:15", "15:30", &["mon", "tue", "wed"])
            },
        )
        .unwrap();
        assert!(
            occurrences(&conn, 1, date("2026-09-14"))
                .unwrap()
                .is_empty(),
            "before"
        );
        assert_eq!(occurrences(&conn, 1, date("2026-09-15")).unwrap().len(), 1);
        assert_eq!(occurrences(&conn, 1, date("2026-09-16")).unwrap().len(), 1);
        assert!(
            occurrences(&conn, 1, date("2026-09-17"))
                .unwrap()
                .is_empty(),
            "after"
        );
    }

    #[test]
    fn a_skipped_date_drops_that_occurrence_alone() {
        let conn = env();
        let e = school(&conn);
        skip(&conn, 1, e.id, "2026-09-15").unwrap();
        skip(&conn, 1, e.id, "2026-09-15").unwrap();
        assert!(occurrences(&conn, 1, date("2026-09-15"))
            .unwrap()
            .is_empty());
        assert_eq!(occurrences(&conn, 1, date("2026-09-16")).unwrap().len(), 1);
        assert_eq!(
            get(&conn, 1, e.id).unwrap().unwrap().exceptions,
            vec!["2026-09-15"]
        );

        unskip(&conn, 1, e.id, "2026-09-15").unwrap();
        assert_eq!(occurrences(&conn, 1, date("2026-09-15")).unwrap().len(), 1);
    }

    #[test]
    fn occurrences_are_sorted_and_scoped_to_one_user() {
        let conn = env();
        school(&conn);
        create(
            &conn,
            1,
            fields("commute", "busy", "07:30", "08:15", &["tue"]),
        )
        .unwrap();
        create(
            &conn,
            2,
            fields("their class", "fixed", "09:00", "10:00", &["tue"]),
        )
        .unwrap();
        let mine: Vec<String> = occurrences(&conn, 1, date("2026-09-15"))
            .unwrap()
            .into_iter()
            .map(|o| o.start)
            .collect();
        assert_eq!(mine, vec!["07:30", "08:15"]);
        assert_eq!(occurrences(&conn, 2, date("2026-09-15")).unwrap().len(), 1);
    }

    #[test]
    fn validation_refuses_what_the_schema_could_not_express() {
        let conn = env();
        let cases: Vec<(&str, Fields)> = vec![
            ("title", fields("   ", "fixed", "08:00", "09:00", &["mon"])),
            (
                "title",
                fields(&"x".repeat(81), "fixed", "08:00", "09:00", &["mon"]),
            ),
            ("kind", fields("x", "party", "08:00", "09:00", &["mon"])),
            (
                "start_time",
                fields("x", "fixed", "8:00", "09:00", &["mon"]),
            ),
            ("end_time", fields("x", "fixed", "08:00", "9:00", &["mon"])),
            (
                "end after",
                fields("x", "fixed", "09:00", "08:00", &["mon"]),
            ),
            (
                "needs on_date",
                Fields {
                    days: Some(0),
                    ..fields("x", "fixed", "08:00", "09:00", &[])
                },
            ),
            (
                "not both",
                Fields {
                    on_date: Some("2026-09-15".into()),
                    ..fields("x", "fixed", "08:00", "09:00", &["mon"])
                },
            ),
            (
                "bitmask",
                Fields {
                    days: Some(128),
                    ..fields("x", "fixed", "08:00", "09:00", &[])
                },
            ),
            (
                "one-off",
                Fields {
                    on_date: Some("2026-09-15".into()),
                    from_date: Some("2026-09-01".into()),
                    ..fields("x", "fixed", "08:00", "09:00", &[])
                },
            ),
            (
                "after until_date",
                Fields {
                    from_date: Some("2026-09-20".into()),
                    until_date: Some("2026-09-01".into()),
                    ..fields("x", "fixed", "08:00", "09:00", &["mon"])
                },
            ),
            (
                "YYYY-MM-DD",
                Fields {
                    on_date: Some("15/09/2026".into()),
                    ..fields("x", "fixed", "08:00", "09:00", &[])
                },
            ),
        ];
        for (needle, f) in cases {
            let e = create(&conn, 1, f).unwrap_err();
            assert!(
                matches!(&e, CalendarError::Invalid(m) if m.contains(needle)),
                "expected {needle:?}, got {e}"
            );
        }
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM calendar_entries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "a rejected entry leaves nothing behind");
    }

    #[test]
    fn a_note_is_never_quiet_and_quiet_defaults_on() {
        let conn = env();
        let e = create(
            &conn,
            1,
            fields("school", "fixed", "08:15", "15:30", &["mon"]),
        )
        .unwrap();
        assert!(e.quiet);
        let n = create(
            &conn,
            1,
            Fields {
                quiet: Some(true),
                ..fields("bin day", "note", "07:00", "08:00", &["mon"])
            },
        )
        .unwrap();
        assert!(!n.quiet);
        let b = create(
            &conn,
            1,
            Fields {
                quiet: Some(false),
                ..fields("commute", "busy", "07:00", "08:00", &["mon"])
            },
        )
        .unwrap();
        assert!(!b.quiet);
    }

    #[test]
    fn the_hundredth_entry_is_the_last() {
        let conn = env();
        for i in 0..MAX_ENTRIES {
            create(
                &conn,
                1,
                fields(&format!("e{i}"), "busy", "08:00", "09:00", &["mon"]),
            )
            .unwrap();
        }
        assert!(matches!(
            create(
                &conn,
                1,
                fields("one more", "busy", "08:00", "09:00", &["mon"])
            ),
            Err(CalendarError::TooMany)
        ));
        create(
            &conn,
            2,
            fields("theirs", "busy", "08:00", "09:00", &["mon"]),
        )
        .unwrap();
    }

    #[test]
    fn update_switches_an_entry_between_recurring_and_one_off() {
        let conn = env();
        let e = school(&conn);
        let one_off = update(
            &conn,
            1,
            e.id,
            Patch {
                on_date: Some("2026-09-19".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            (one_off.days, one_off.on_date.as_deref()),
            (0, Some("2026-09-19"))
        );

        let back = update(
            &conn,
            1,
            e.id,
            Patch {
                days: Some(day_mask(&["sat"]).unwrap()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!((back.days, back.on_date), (32, None));
        assert_eq!(back.day_names, vec!["sat"]);
    }

    #[test]
    fn update_clears_a_validity_bound_with_a_blank_string() {
        let conn = env();
        let e = create(
            &conn,
            1,
            Fields {
                until_date: Some("2026-09-16".into()),
                ..fields("term", "fixed", "08:15", "15:30", &["mon"])
            },
        )
        .unwrap();
        let cleared = update(
            &conn,
            1,
            e.id,
            Patch {
                until_date: Some("".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(cleared.until_date, None);
    }

    #[test]
    fn another_users_entry_is_missing_rather_than_theirs() {
        let conn = env();
        let e = school(&conn);
        assert!(get(&conn, 2, e.id).unwrap().is_none());
        assert!(matches!(
            update(
                &conn,
                2,
                e.id,
                Patch {
                    title: Some("mine now".into()),
                    ..Default::default()
                }
            ),
            Err(CalendarError::NotFound(_))
        ));
        assert!(matches!(
            skip(&conn, 2, e.id, "2026-09-15"),
            Err(CalendarError::NotFound(_))
        ));
        assert!(!delete(&conn, 2, e.id).unwrap());
        assert!(delete(&conn, 1, e.id).unwrap());
        assert!(list(&conn, 1).unwrap().is_empty());
    }

    #[test]
    fn deleting_an_entry_takes_its_exceptions_with_it() {
        let conn = env();
        let e = school(&conn);
        skip(&conn, 1, e.id, "2026-09-15").unwrap();
        assert!(delete(&conn, 1, e.id).unwrap());
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM calendar_exceptions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    fn at(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn quiet_until_reports_the_end_of_the_window_it_is_inside() {
        let conn = env();
        school(&conn);
        // 2026-09-15 is a Tuesday
        assert_eq!(
            quiet_until(&conn, 1, &utc(), at("2026-09-15T10:00:00Z")).unwrap(),
            Some(at("2026-09-15T15:30:00Z"))
        );
        assert_eq!(
            quiet_until(&conn, 1, &utc(), at("2026-09-15T08:14:00Z")).unwrap(),
            None
        );
        assert_eq!(
            quiet_until(&conn, 1, &utc(), at("2026-09-15T15:30:00Z")).unwrap(),
            None,
            "the window is over the moment it ends"
        );
        assert_eq!(
            quiet_until(&conn, 1, &utc(), at("2026-09-19T10:00:00Z")).unwrap(),
            None,
            "saturday"
        );
    }

    #[test]
    fn adjacent_and_overlapping_quiet_windows_merge() {
        let conn = env();
        school(&conn);
        create(
            &conn,
            1,
            fields("club", "fixed", "15:30", "17:00", &["tue"]),
        )
        .unwrap();
        create(
            &conn,
            1,
            fields("ride home", "busy", "16:30", "18:00", &["tue"]),
        )
        .unwrap();
        let w = quiet_window(&conn, 1, &utc(), at("2026-09-15T10:00:00Z"))
            .unwrap()
            .unwrap();
        assert_eq!((w.end.as_str(), w.title.as_str()), ("18:00", "ride home"));
    }

    #[test]
    fn a_window_that_is_not_quiet_never_holds_anything() {
        let conn = env();
        create(
            &conn,
            1,
            Fields {
                quiet: Some(false),
                ..fields("commute", "busy", "08:00", "09:00", &["tue"])
            },
        )
        .unwrap();
        create(
            &conn,
            1,
            fields("bin day", "note", "09:00", "10:00", &["tue"]),
        )
        .unwrap();
        assert_eq!(
            quiet_until(&conn, 1, &utc(), at("2026-09-15T08:30:00Z")).unwrap(),
            None
        );
        assert_eq!(
            quiet_until(&conn, 1, &utc(), at("2026-09-15T09:30:00Z")).unwrap(),
            None
        );
    }

    #[test]
    fn a_skipped_date_is_not_quiet() {
        let conn = env();
        let e = school(&conn);
        skip(&conn, 1, e.id, "2026-09-15").unwrap();
        assert_eq!(
            quiet_until(&conn, 1, &utc(), at("2026-09-15T10:00:00Z")).unwrap(),
            None
        );
        assert!(quiet_until(&conn, 1, &utc(), at("2026-09-16T10:00:00Z"))
            .unwrap()
            .is_some());
    }

    #[test]
    fn quiet_windows_are_read_in_the_users_own_timezone() {
        let conn = env();
        school(&conn);
        let tokyo = jiff::tz::TimeZone::get("Asia/Tokyo").unwrap();
        // 2026-09-15T01:00Z is 10:00 Tuesday in Tokyo
        assert_eq!(
            quiet_until(&conn, 1, &tokyo, at("2026-09-15T01:00:00Z")).unwrap(),
            Some(at("2026-09-15T06:30:00Z"))
        );
        assert_eq!(
            quiet_until(&conn, 1, &utc(), at("2026-09-15T01:00:00Z")).unwrap(),
            None
        );
    }

    #[test]
    fn conflict_names_the_commitment_a_time_lands_in() {
        let conn = env();
        school(&conn);
        create(
            &conn,
            1,
            fields("snack", "busy", "16:00", "16:30", &["tue"]),
        )
        .unwrap();
        let tue = date("2026-09-15");
        assert_eq!(
            conflict(&conn, 1, tue, "10:00", "10:00")
                .unwrap()
                .as_deref(),
            Some("inside school 08:15-15:30")
        );
        assert_eq!(
            conflict(&conn, 1, tue, "07:00", "09:00")
                .unwrap()
                .as_deref(),
            Some("inside school 08:15-15:30"),
            "a range that runs into the window"
        );
        assert_eq!(
            conflict(&conn, 1, tue, "07:00", "08:15").unwrap(),
            None,
            "ends as it starts"
        );
        assert_eq!(conflict(&conn, 1, tue, "15:30", "15:30").unwrap(), None);
        assert_eq!(
            conflict(&conn, 1, tue, "16:10", "16:10").unwrap(),
            None,
            "busy is not fixed"
        );
        assert_eq!(
            conflict(&conn, 1, date("2026-09-19"), "10:00", "10:00").unwrap(),
            None
        );
    }

    #[test]
    fn day_names_and_masks_round_trip() {
        assert_eq!(day_mask(&["mon", "sun"]).unwrap(), 65);
        assert_eq!(day_names(65), vec!["mon", "sun"]);
        assert_eq!(day_names(127).len(), 7);
        assert!(day_names(0).is_empty());
        assert!(day_mask(&["moon"]).is_err());
    }
}
