use crate::tasks::UpdateError;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::Timestamp;

/// When a whole-day due passes, on the user's clock.
pub const DAY_END: &str = "23:59";

/// When a task or goal falls due: some time on a day, or a set moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Due {
    Day(Date),
    At(Timestamp),
}

impl Due {
    /// The stored pair: `due_at`, the deadline every comparison reads, and
    /// `due_on`, set only for a whole day.
    pub fn columns(self, tz: Option<&TimeZone>) -> Result<(String, Option<String>), UpdateError> {
        match self {
            Self::At(at) => Ok((at.to_string(), None)),
            Self::Day(day) => {
                let tz = tz.ok_or_else(|| {
                    UpdateError::Invalid("a whole-day due needs the user's time zone".into())
                })?;
                Ok((day_end(tz, day).to_string(), Some(day.to_string())))
            }
        }
    }
}

/// jiff reads a date-time as its date; a day here is the date alone.
fn bare_day(raw: &str) -> Option<Date> {
    (raw.len() == 10).then(|| raw.parse().ok()).flatten()
}

pub fn day_end(tz: &TimeZone, day: Date) -> Timestamp {
    crate::zone::instant(tz, day, DAY_END).expect("DAY_END is a valid wall time")
}

/// The wire pair `due_at` / `due_on` as one change: `None` leaves the due
/// alone, `Some(None)` clears it.
pub fn from_wire(
    at: &Option<Option<String>>,
    on: &Option<Option<String>>,
) -> Result<Option<Option<Due>>, UpdateError> {
    match (at, on) {
        (Some(Some(_)), Some(Some(_))) => {
            Err(UpdateError::Invalid("name either due_at or due_on, not both".into()))
        }
        (Some(Some(raw)), _) => raw
            .parse::<Timestamp>()
            .map(|t| Some(Some(Due::At(t))))
            .map_err(|_| UpdateError::Invalid(format!("due_at must be an RFC 3339 instant, got {raw:?}"))),
        (_, Some(Some(raw))) => bare_day(raw)
            .map(|d| Some(Some(Due::Day(d))))
            .ok_or_else(|| UpdateError::Invalid(format!("due_on must be a YYYY-MM-DD day, got {raw:?}"))),
        (None, None) => Ok(None),
        _ => Ok(Some(None)),
    }
}

/// What a person or the model types: `YYYY-MM-DD` is a whole day, a
/// `YYYY-MM-DDTHH:MM` without an offset is that wall time in `tz`, and an RFC
/// 3339 instant is itself.
pub fn parse_loose(raw: &str, tz: &TimeZone) -> Option<Due> {
    let raw = raw.trim();
    if let Ok(at) = raw.parse::<Timestamp>() {
        return Some(Due::At(at));
    }
    if let Some(day) = bare_day(raw) {
        return Some(Due::Day(day));
    }
    let local: jiff::civil::DateTime = raw.parse().ok()?;
    tz.to_ambiguous_zoned(local).compatible().ok().map(|z| Due::At(z.timestamp()))
}

/// `YYYY-MM-DD` for a whole day, `YYYY-MM-DD HH:MM` on the user's clock for a
/// moment.
pub fn local_label(due_at: &str, due_on: Option<&str>, tz: &TimeZone) -> Option<String> {
    if let Some(day) = due_on {
        return Some(day.to_owned());
    }
    let at = due_at.parse::<Timestamp>().ok()?;
    Some(at.to_zoned(tz.clone()).strftime("%Y-%m-%d %H:%M").to_string())
}

/// Restates every whole-day deadline of the user as the end of its day in `tz`.
pub fn rezone(conn: &rusqlite::Connection, user_id: i64, tz: &TimeZone) -> rusqlite::Result<usize> {
    let mut moved = 0;
    for table in ["tasks", "goals"] {
        let days: Vec<(i64, String)> = conn
            .prepare(&format!("SELECT id, due_on FROM {table} WHERE user_id = ?1 AND due_on IS NOT NULL"))?
            .query_map([user_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, day) in days {
            let Ok(day) = day.parse::<Date>() else { continue };
            moved += conn.execute(
                &format!("UPDATE {table} SET due_at = ?1 WHERE id = ?2"),
                (day_end(tz, day).to_string(), id),
            )?;
        }
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokyo() -> TimeZone {
        TimeZone::get("Asia/Tokyo").unwrap()
    }

    #[test]
    fn a_day_ends_at_day_end_where_the_user_lives() {
        let day = Due::Day("2026-10-16".parse().unwrap());
        let (at, on) = day.columns(Some(&tokyo())).unwrap();
        assert_eq!(at, "2026-10-16T14:59:00Z");
        assert_eq!(on.as_deref(), Some("2026-10-16"));
        assert!(day.columns(None).is_err());
    }

    #[test]
    fn the_wire_pair_names_one_or_clears() {
        let s = |v: &str| Some(Some(v.to_owned()));
        assert!(from_wire(&s("2026-10-16T08:00:00Z"), &s("2026-10-16")).is_err());
        assert_eq!(from_wire(&None, &None).unwrap(), None);
        assert_eq!(from_wire(&Some(None), &None).unwrap(), Some(None));
        assert_eq!(from_wire(&None, &Some(None)).unwrap(), Some(None));
        assert_eq!(
            from_wire(&None, &s("2026-10-16")).unwrap(),
            Some(Some(Due::Day("2026-10-16".parse().unwrap())))
        );
        assert!(from_wire(&s("2026-10-16"), &None).is_err());
        assert!(from_wire(&None, &s("2026-10-16T08:00:00Z")).is_err());
    }

    #[test]
    fn typed_dues_read_as_a_day_or_a_moment() {
        let tz = tokyo();
        assert_eq!(parse_loose("2026-10-16", &tz), Some(Due::Day("2026-10-16".parse().unwrap())));
        assert_eq!(
            parse_loose("2026-10-16T17:30", &tz),
            Some(Due::At("2026-10-16T08:30:00Z".parse().unwrap()))
        );
        assert_eq!(
            parse_loose("2026-10-16T17:30:00+09:00", &tz),
            Some(Due::At("2026-10-16T08:30:00Z".parse().unwrap()))
        );
        assert_eq!(parse_loose("friday", &tz), None);
    }

    #[test]
    fn labels_keep_a_day_bare() {
        let tz = tokyo();
        assert_eq!(local_label("2026-10-16T14:59:00Z", Some("2026-10-16"), &tz).unwrap(), "2026-10-16");
        assert_eq!(local_label("2026-10-16T08:30:00Z", None, &tz).unwrap(), "2026-10-16 17:30");
    }
}
