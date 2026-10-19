//! Standing problems with what Note leans on outside itself: each is told to
//! its user once and shown in the app until it clears.

use crate::failure::Reason;
use rusqlite::Connection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outage {
    Ai(Reason),
    DailyCap,
    Matrix,
    Voice,
}

const AI: [Reason; 5] =
    [Reason::ModelUnavailable, Reason::AuthInvalid, Reason::OutOfCredits, Reason::RateLimited, Reason::ProviderDown];

impl Outage {
    pub fn code(self) -> &'static str {
        match self {
            Outage::Ai(r) => r.code(),
            Outage::DailyCap => "daily_cap",
            Outage::Matrix => "matrix_down",
            Outage::Voice => "voice_down",
        }
    }

    /// The outage a failed AI request stands for; a refusal, a long thread or a
    /// fault inside Note is that one request's, not the service's.
    pub fn of_failure(reason: Reason) -> Option<Self> {
        AI.contains(&reason).then_some(Outage::Ai(reason))
    }

    fn from_code(code: &str) -> Option<Self> {
        match code {
            "daily_cap" => Some(Outage::DailyCap),
            "matrix_down" => Some(Outage::Matrix),
            "voice_down" => Some(Outage::Voice),
            _ => AI.into_iter().find(|r| r.code() == code).map(Outage::Ai),
        }
    }
}

/// Records the outage; `true` when it was not already standing.
pub fn raise(conn: &Connection, user_id: i64, outage: Outage, now: jiff::Timestamp) -> rusqlite::Result<bool> {
    conn.execute(
        "INSERT OR IGNORE INTO outages (user_id, code, since) VALUES (?1, ?2, ?3)",
        (user_id, outage.code(), now.to_string()),
    )
    .map(|n| n > 0)
}

pub fn clear(conn: &Connection, user_id: i64, outage: Outage) -> rusqlite::Result<usize> {
    conn.execute("DELETE FROM outages WHERE user_id = ?1 AND code = ?2", (user_id, outage.code()))
}

/// A session that got its answers: the AI service and the day's sessions are
/// both there again.
pub fn clear_ai(conn: &Connection, user_id: i64) -> rusqlite::Result<usize> {
    let mut n = clear(conn, user_id, Outage::DailyCap)?;
    for r in AI {
        n += clear(conn, user_id, Outage::Ai(r))?;
    }
    Ok(n)
}

/// What stands for the user now, oldest first, with when each began. The
/// day's cap is read live, so it shows the moment it is reached and leaves the
/// moment it lifts.
pub fn standing(
    conn: &Connection,
    user_id: i64,
    capped: bool,
    now: jiff::Timestamp,
) -> rusqlite::Result<Vec<(Outage, String)>> {
    if !capped {
        clear(conn, user_id, Outage::DailyCap)?;
    }
    let mut stmt = conn.prepare("SELECT code, since FROM outages WHERE user_id = ?1 ORDER BY since, code")?;
    let mut out: Vec<(Outage, String)> = stmt
        .query_map([user_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .filter_map(|row| row.ok().and_then(|(code, since)| Outage::from_code(&code).map(|o| (o, since))))
        .collect();
    if capped && !out.iter().any(|(o, _)| *o == Outage::DailyCap) {
        out.push((Outage::DailyCap, now.to_string()));
    }
    Ok(out)
}

/// Background work that failed tells its user once per reason in this window,
/// however often the outage clears and comes back.
pub const NOTICE_WINDOW_MINS: i64 = 6 * 60;

/// Tells the user, once, of each outage raised since they were last told: a
/// message down the ladder, and the open app looks again for its banner.
pub fn tell_pending(state: &crate::AppState, user_id: i64, username: &str, now: jiff::Timestamp) {
    let fresh: Vec<Outage> = {
        let conn = state.db();
        let codes: Vec<String> = conn
            .prepare("SELECT code FROM outages WHERE user_id = ?1 AND told_at IS NULL ORDER BY since, code")
            .and_then(|mut stmt| stmt.query_map([user_id], |r| r.get(0))?.collect())
            .unwrap_or_default();
        let _ = conn.execute(
            "UPDATE outages SET told_at = ?1 WHERE user_id = ?2 AND told_at IS NULL",
            (now.to_string(), user_id),
        );
        codes
            .iter()
            .filter_map(|c| Outage::from_code(c))
            .filter(|o| {
                crate::log::record_throttled(&conn, Some(user_id), "failure_notice", o.code(), now, NOTICE_WINDOW_MINS)
                    .unwrap_or(false)
            })
            .collect()
    };
    if fresh.is_empty() {
        return;
    }
    let lang = crate::text::Lang::for_user(&state.config_dir, username);
    for outage in fresh {
        let msg = crate::channels::OutboundMessage {
            title: crate::text::failure_title(lang),
            reason: None,
            brief: None,
            body: crate::text::outage(lang, outage),
            urgency: crate::channels::Urgency::Low,
            checkin: false,
            event_id: None,
            conversation_id: None,
            actions: Vec::new(),
        };
        crate::channels::deliver_via(&state.db, &state.channels, user_id, username, &msg);
    }
    state.hub.broadcast_changed(user_id);
}

/// `tell_pending` for every user with an outage not yet told.
pub fn tell_all_pending(state: &crate::AppState, now: jiff::Timestamp) {
    let users: Vec<(i64, String)> = {
        let conn = state.db();
        conn.prepare(
            "SELECT DISTINCT o.user_id, u.username FROM outages o JOIN users u ON u.id = o.user_id
             WHERE o.told_at IS NULL",
        )
        .and_then(|mut stmt| stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
        .unwrap_or_default()
    };
    for (user_id, username) in users {
        tell_pending(state, user_id, &username, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::mock::MockChannel;
    use std::sync::Arc;

    fn state() -> (crate::AppState, Arc<MockChannel>, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        crate::auth::create_user(&conn, "aitest", "pw", false).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let ch = Arc::new(MockChannel::new("mock"));
        let state = crate::AppState::new(conn, tmp.path().into(), tmp.path().into()).with_channels(vec![ch.clone()]);
        (state, ch, tmp)
    }

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    #[test]
    fn only_a_failure_of_the_service_is_an_outage() {
        assert_eq!(Outage::of_failure(Reason::ProviderDown), Some(Outage::Ai(Reason::ProviderDown)));
        for r in [Reason::ContextTooLong, Reason::Refused, Reason::Internal] {
            assert_eq!(Outage::of_failure(r), None, "{r:?}");
        }
        for o in [Outage::Ai(Reason::OutOfCredits), Outage::DailyCap, Outage::Matrix, Outage::Voice] {
            assert_eq!(Outage::from_code(o.code()), Some(o));
        }
    }

    #[test]
    fn an_outage_is_told_once_and_stands_until_it_clears() {
        let (state, ch, _tmp) = state();
        let t0 = at("2026-10-18T09:00:00Z");
        {
            let conn = state.db();
            assert!(raise(&conn, 1, Outage::Ai(Reason::ProviderDown), t0).unwrap());
            assert!(!raise(&conn, 1, Outage::Ai(Reason::ProviderDown), t0).unwrap());
        }
        tell_all_pending(&state, t0);
        tell_all_pending(&state, t0 + jiff::Span::new().minutes(1));
        assert_eq!(ch.seen().len(), 1);
        assert_eq!(ch.seen()[0].1.body, crate::text::failure_reason(crate::text::Lang::En, Reason::ProviderDown));
        let conn = state.db();
        assert_eq!(standing(&conn, 1, false, t0).unwrap(), vec![(Outage::Ai(Reason::ProviderDown), t0.to_string())]);
        clear_ai(&conn, 1).unwrap();
        assert!(standing(&conn, 1, false, t0).unwrap().is_empty());
    }

    #[test]
    fn a_flapping_outage_is_told_once_a_window() {
        let (state, ch, _tmp) = state();
        let t0 = at("2026-10-18T09:00:00Z");
        for hours in [0, 1, 7] {
            let t = t0 + jiff::Span::new().hours(hours);
            raise(&state.db(), 1, Outage::Matrix, t).unwrap();
            tell_pending(&state, 1, "aitest", t);
            clear(&state.db(), 1, Outage::Matrix).unwrap();
        }
        assert_eq!(ch.seen().len(), 2);
    }

    #[test]
    fn the_days_cap_stands_while_it_is_reached() {
        let (state, _ch, _tmp) = state();
        let t0 = at("2026-10-18T09:00:00Z");
        let conn = state.db();
        assert_eq!(standing(&conn, 1, true, t0).unwrap(), vec![(Outage::DailyCap, t0.to_string())]);
        raise(&conn, 1, Outage::DailyCap, t0).unwrap();
        assert!(standing(&conn, 1, false, t0).unwrap().is_empty());
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM outages", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 0, "a lifted cap leaves no row behind");
    }
}
