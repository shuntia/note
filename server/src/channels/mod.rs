pub mod mock;
pub mod ntfy;
pub mod voice;
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
    /// The conversation the message opened or joined; a check-in carries one,
    /// so the client can land on the thread where its question waits.
    pub conversation_id: Option<i64>,
}

/// The web client's route for a conversation, relative to the app's origin.
pub fn conversation_path(id: i64) -> String {
    format!("/#/chat/{id}")
}

pub fn is_checkin(kind: &str) -> bool {
    kind.contains("checkin")
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
    let (title, mut body, urgency) = if is_checkin(&ev.kind) {
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
    OutboundMessage { title, body, urgency, event_id: Some(ev.event_id), conversation_id: None }
}

/// A check-in's question becomes the newest message of the day's check-in
/// thread before any channel carries it, so the thread exists whether or not
/// delivery succeeds. A failure to open the thread is logged and the message
/// still goes out, without an id.
fn open_checkin_thread(conn: &Connection, ev: &crate::runner::FiredEvent, msg: &mut OutboundMessage) {
    if !is_checkin(&ev.kind) {
        return;
    }
    let now = jiff::Timestamp::now();
    match crate::talk::checkin_thread(conn, ev.user_id, &ev.date, &msg.body, now) {
        Ok(id) => msg.conversation_id = Some(id),
        Err(e) => {
            let _ = crate::log::record(
                conn,
                Some(ev.user_id),
                "checkin_thread_error",
                &format!("event {}: {e}", ev.event_id),
            );
        }
    }
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

/// A `voice` event rings the phone before anything else; the push ladder is
/// what it falls back to, logged as `voice_fallback`, so a refused call is still
/// a nudge that lands.
pub fn deliver_event(
    db: &Mutex<Connection>,
    ladder: &[Arc<dyn Channel>],
    voice: Option<&voice::VoiceChannel>,
    ev: &crate::runner::FiredEvent,
) {
    let msg = {
        let conn = crate::db_guard(db);
        let mut msg = render(&conn, ev);
        open_checkin_thread(&conn, ev, &mut msg);
        msg
    };
    if ev.channel == "voice" {
        let refusal = match voice {
            Some(ch) => match ch.deliver(ev.user_id, &ev.username, &msg) {
                Ok(()) => {
                    let conn = crate::db_guard(db);
                    let _ = crate::log::record(
                        &conn,
                        Some(ev.user_id),
                        "delivery_ok",
                        &format!("event {} via voice", ev.event_id),
                    );
                    return;
                }
                Err(e) => e.to_string(),
            },
            None => "no voice channel configured".to_string(),
        };
        let conn = crate::db_guard(db);
        let _ = crate::log::record(
            &conn,
            Some(ev.user_id),
            "voice_fallback",
            &format!("event {}: {refusal}", ev.event_id),
        );
    }
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

        deliver_event(&db, &ladder, None, &ev("nudge", "push", ""));
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
            conversation_id: None,
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
        deliver_event(&db, &ladder, None, &ev("nudge", "push", ""));
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
        deliver_event(&db, &[], None, &ev("nudge", "push", ""));
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
        deliver_event(&db, &ladder, None, &ev("nudge", "push", ""));
        let conn = db.lock().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='delivery_ok'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    fn thread_rows(conn: &rusqlite::Connection) -> Vec<(i64, Option<String>, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT c.id, c.checkin_date, m.content FROM conversations c
                 JOIN talk_messages m ON m.conversation_id = c.id ORDER BY m.id",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn a_checkin_opens_the_days_thread_and_the_message_names_it() {
        let (db, _uid) = env();
        let push = Arc::new(MockChannel::new("push"));
        let ladder: Vec<Arc<dyn Channel>> = vec![push.clone()];

        deliver_event(&db, &ladder, None, &ev("checkin", "push", ""));
        let mut later = ev("checkin_call", "push", "Afternoon. Where did the morning go?");
        later.event_id = 12;
        later.wall_time = "15:30".into();
        deliver_event(&db, &ladder, None, &later);

        let seen = push.seen();
        assert_eq!(seen.len(), 2);
        let id = seen[0].1.conversation_id.expect("the first check-in opened a thread");
        assert_eq!(seen[1].1.conversation_id, Some(id), "the same day joins the same thread");

        let conn = db.lock().unwrap();
        let rows = thread_rows(&conn);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|(c, date, _)| *c == id && date.as_deref() == Some("2026-08-31")));
        assert!(rows[0].2.contains("09:00"), "the opener is the rendered question: {}", rows[0].2);
        assert_eq!(rows[1].2, "Afternoon. Where did the morning go?");
        let title: String =
            conn.query_row("SELECT title FROM conversations WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert!(title.contains("09:00"), "{title}");
    }

    #[test]
    fn the_thread_exists_even_when_no_channel_takes_the_checkin() {
        let (db, _uid) = env();
        deliver_event(&db, &[], None, &ev("checkin", "push", ""));
        let conn = db.lock().unwrap();
        assert_eq!(thread_rows(&conn).len(), 1);
    }

    #[test]
    fn a_nudge_opens_no_thread() {
        let (db, _uid) = env();
        let push = Arc::new(MockChannel::new("push"));
        let ladder: Vec<Arc<dyn Channel>> = vec![push.clone()];
        deliver_event(&db, &ladder, None, &ev("nudge", "push", "stretch"));
        assert_eq!(push.seen()[0].1.conversation_id, None);
        let conn = db.lock().unwrap();
        assert!(thread_rows(&conn).is_empty());
    }

    #[test]
    fn a_voice_event_with_no_channel_falls_back_to_the_push_ladder() {
        let (db, _uid) = env();
        let push = Arc::new(MockChannel::new("push"));
        let ladder: Vec<Arc<dyn Channel>> = vec![push.clone()];
        deliver_event(&db, &ladder, None, &ev("checkin_call", "voice", ""));
        assert_eq!(push.seen().len(), 1);
        let conn = db.lock().unwrap();
        let detail: String = conn
            .query_row("SELECT detail FROM event_log WHERE kind='voice_fallback'", [], |r| r.get(0))
            .unwrap();
        assert!(detail.contains("no voice channel configured"), "unexpected detail: {detail}");
    }
}
