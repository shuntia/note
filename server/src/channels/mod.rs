pub mod mock;
pub mod ntfy;
pub mod webpush;
pub mod ws;

use rusqlite::Connection;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Urgency {
    Low,
    Normal,
    High,
}

impl Urgency {
    /// Wire value shared by the WS frame field and the Web Push `Urgency` header.
    pub fn as_str(&self) -> &'static str {
        match self {
            Urgency::Low => "low",
            Urgency::Normal => "normal",
            Urgency::High => "high",
        }
    }
}

#[derive(Clone, Debug)]
pub struct OutboundMessage {
    pub title: String,
    pub body: String,
    pub urgency: Urgency,
    pub event_id: Option<i64>,
}

/// A delivery mechanism. `deliver` returns `Err` when this channel cannot
/// currently reach the user, so the dispatcher can fall through the ladder.
pub trait Channel: Send + Sync {
    fn name(&self) -> &'static str;
    fn deliver(&self, user_id: i64, username: &str, msg: &OutboundMessage) -> anyhow::Result<()>;
}

/// Pure rendering of a fired event into a user-facing message; a non-empty
/// per-event `message` (set by `notify_send`) overrides the generic body.
pub fn render(conn: &Connection, ev: &crate::runner::FiredEvent) -> OutboundMessage {
    let (title, mut body, urgency) = if ev.kind.contains("checkin") {
        (
            "Check-in".to_string(),
            format!("Time for your {} check-in — how is the day going?", ev.wall_time),
            Urgency::High,
        )
    } else if ev.kind == "debrief" {
        let content: String = conn
            .query_row(
                "SELECT content FROM debriefs WHERE user_id = ?1 AND date = ?2",
                (ev.user_id, &ev.date),
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "(no debrief yet)".into());
        ("Good morning".to_string(), content, Urgency::Normal)
    } else {
        (ev.kind.clone(), format!("scheduled for {}", ev.wall_time), Urgency::Normal)
    };
    if !ev.message.is_empty() {
        body = ev.message.clone();
    }
    OutboundMessage { title, body, urgency, event_id: Some(ev.event_id) }
}

/// Walks the ladder until one channel delivers, returning its name; every
/// outcome is logged and none propagates — a failed delivery is a plainer day,
/// never an error. The DB guard is never held across a channel's `deliver`.
pub fn deliver_via(
    db: &Mutex<Connection>,
    ladder: &[Arc<dyn Channel>],
    user_id: i64,
    username: &str,
    msg: &OutboundMessage,
) -> Option<&'static str> {
    let subject = match msg.event_id {
        Some(id) => format!("event {id}"),
        None => "test".to_string(),
    };
    let mut errors: Vec<String> = Vec::new();
    for ch in ladder {
        match ch.deliver(user_id, username, msg) {
            Ok(()) => {
                let conn = crate::db_guard(db);
                let _ = crate::log::record(
                    &conn,
                    Some(user_id),
                    "delivery_ok",
                    &format!("{subject} via {}", ch.name()),
                );
                return Some(ch.name());
            }
            Err(e) => errors.push(format!("{}: {e}", ch.name())),
        }
    }
    let detail = if errors.is_empty() {
        format!("{subject}: no channels configured")
    } else {
        format!("{subject}: {}", errors.join("; "))
    };
    let conn = crate::db_guard(db);
    let _ = crate::log::record(&conn, Some(user_id), "delivery_degraded", &detail);
    None
}

pub fn deliver_event(
    db: &Mutex<Connection>,
    ladder: &[Arc<dyn Channel>],
    ev: &crate::runner::FiredEvent,
) {
    let msg = {
        let conn = crate::db_guard(db);
        if ev.channel == "voice" {
            let _ = crate::log::record(
                &conn,
                Some(ev.user_id),
                "voice_unavailable",
                &format!("event {}: voice not implemented, using push ladder", ev.event_id),
            );
        }
        render(&conn, ev)
    };
    deliver_via(db, ladder, ev.user_id, &ev.username, &msg);
}

#[cfg(test)]
mod tests {
    use super::mock::MockChannel;
    use super::*;
    use crate::runner::FiredEvent;
    use std::sync::{Arc, Mutex};

    fn env() -> (Mutex<rusqlite::Connection>, i64) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        (Mutex::new(conn), 1)
    }

    fn ev(kind: &str, channel: &str, message: &str) -> FiredEvent {
        FiredEvent {
            event_id: 11,
            user_id: 1,
            username: "aki".into(),
            kind: kind.into(),
            wall_time: "09:00".into(),
            date: "2026-08-31".into(),
            channel: channel.into(),
            message: message.into(),
        }
    }

    #[test]
    fn render_uses_debrief_content_and_message_override() {
        let (db, _uid) = env();
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO debriefs (user_id, date, content, created_at)
             VALUES (1, '2026-08-31', 'slept well, one task rolled', 'now')",
            [],
        )
        .unwrap();
        let m = render(&conn, &ev("debrief", "push", ""));
        assert_eq!(m.title, "Good morning");
        assert!(m.body.contains("slept well"));

        let m = render(&conn, &ev("checkin_call", "push", ""));
        assert_eq!(m.title, "Check-in");
        assert_eq!(m.urgency, Urgency::High);
        assert!(m.body.contains("09:00"), "body: {}", m.body);
        assert!(!m.body.contains("checkin_call"), "body leaks the kind: {}", m.body);

        let m = render(&conn, &ev("nudge", "push", "you wanted a stretch break"));
        assert_eq!(m.body, "you wanted a stretch break");
    }

    #[test]
    fn render_debrief_without_a_row_says_so() {
        let (db, _uid) = env();
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO debriefs (user_id, date, content, created_at)
             VALUES (1, '2026-08-30', 'yesterday', 'now')",
            [],
        )
        .unwrap();
        let m = render(&conn, &ev("debrief", "push", ""));
        assert_eq!(m.body, "(no debrief yet)");
    }

    #[test]
    fn dispatcher_falls_through_ladder_and_logs() {
        let (db, _uid) = env();
        let first = Arc::new(MockChannel::new("first"));
        let second = Arc::new(MockChannel::new("second"));
        first.set_fail(true);
        let ladder: Vec<Arc<dyn Channel>> = vec![first.clone(), second.clone()];

        deliver_event(&db, &ladder, &ev("nudge", "push", ""));
        assert!(first.seen().is_empty());
        assert_eq!(second.seen().len(), 1);
        let conn = db.lock().unwrap();
        let detail: String = conn
            .query_row("SELECT detail FROM event_log WHERE kind='delivery_ok'", [], |r| r.get(0))
            .unwrap();
        assert!(detail.contains("second"), "unexpected detail: {detail}");
    }

    #[test]
    fn the_walk_names_the_channel_that_delivered() {
        let (db, _uid) = env();
        let ws = Arc::new(MockChannel::new("ws"));
        let webpush = Arc::new(MockChannel::new("webpush"));
        let ntfy = Arc::new(MockChannel::new("ntfy"));
        ws.set_fail(true);
        webpush.set_fail(true);
        let ladder: Vec<Arc<dyn Channel>> = vec![ws, webpush, ntfy.clone()];
        let msg = OutboundMessage {
            title: "Note".into(),
            body: "Test notification".into(),
            urgency: Urgency::Normal,
            event_id: None,
        };
        assert_eq!(deliver_via(&db, &ladder, 1, "aki", &msg), Some("ntfy"));
        assert_eq!(ntfy.seen().len(), 1);

        ntfy.set_fail(true);
        assert_eq!(deliver_via(&db, &ladder, 1, "aki", &msg), None);
        let conn = db.lock().unwrap();
        let detail: String = conn
            .query_row("SELECT detail FROM event_log WHERE kind='delivery_degraded'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(detail.contains("ntfy"), "unexpected detail: {detail}");
    }

    #[test]
    fn all_channels_failing_logs_degraded_not_error() {
        let (db, _uid) = env();
        let only = Arc::new(MockChannel::new("only"));
        only.set_fail(true);
        let ladder: Vec<Arc<dyn Channel>> = vec![only];
        deliver_event(&db, &ladder, &ev("nudge", "push", ""));
        let conn = db.lock().unwrap();
        let detail: String = conn
            .query_row("SELECT detail FROM event_log WHERE kind='delivery_degraded'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(detail.contains("only"), "unexpected detail: {detail}");
        assert!(detail.contains("set to fail"), "unexpected detail: {detail}");
    }

    #[test]
    fn empty_ladder_logs_a_named_reason() {
        let (db, _uid) = env();
        deliver_event(&db, &[], &ev("nudge", "push", ""));
        let conn = db.lock().unwrap();
        let detail: String = conn
            .query_row("SELECT detail FROM event_log WHERE kind='delivery_degraded'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(detail.contains("no channels configured"), "unexpected detail: {detail}");
    }

    /// Errors unless the dispatcher has released the DB guard before calling it;
    /// a regression would deadlock the runner, since real channels re-lock the
    /// same mutex to read their subscriptions.
    struct LockProbe(Arc<Mutex<rusqlite::Connection>>);

    impl Channel for LockProbe {
        fn name(&self) -> &'static str {
            "probe"
        }

        fn deliver(
            &self,
            _user_id: i64,
            _username: &str,
            _msg: &OutboundMessage,
        ) -> anyhow::Result<()> {
            match self.0.try_lock() {
                Ok(_) => Ok(()),
                Err(_) => anyhow::bail!("db guard held across deliver"),
            }
        }
    }

    #[test]
    fn dispatcher_releases_the_db_guard_before_delivering() {
        let (db, _uid) = env();
        let db = Arc::new(db);
        let ladder: Vec<Arc<dyn Channel>> = vec![Arc::new(LockProbe(db.clone()))];
        deliver_event(&db, &ladder, &ev("nudge", "push", ""));
        let conn = db.lock().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='delivery_ok'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn voice_channel_logs_unavailable_then_falls_back() {
        let (db, _uid) = env();
        let push = Arc::new(MockChannel::new("push"));
        let ladder: Vec<Arc<dyn Channel>> = vec![push.clone()];
        deliver_event(&db, &ladder, &ev("checkin_call", "voice", ""));
        assert_eq!(push.seen().len(), 1);
        let conn = db.lock().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='voice_unavailable'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1);
    }
}
