pub mod calling;
pub mod matrix;
pub mod mock;
pub mod voice;
pub mod webpush;
pub mod ws;

pub use calling::{ring, NoWebCalls, RingOutcome, WebCalls};

use crate::text::{self, Lang};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// An error line without the request URL, which for Web Push is a per-device
/// bearer capability; delivery errors are written to the event log.
pub(crate) fn describe_http_error(err: &ureq::Error) -> String {
    match err {
        ureq::Error::StatusCode(code) => format!("status {code}"),
        ureq::Error::Io(e) => format!("transport error: {}", e.kind()),
        ureq::Error::BadUri(_) | ureq::Error::RequireHttpsOnly(_) | ureq::Error::Http(_) => {
            "transport error: invalid url".to_string()
        }
        e => format!("transport error: {e}"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutboundMessage {
    pub title: String,
    /// Why Note is calling, for the call's own prompt when this message rings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What the session that rang gathered for the call to talk from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    pub body: String,
    pub urgency: Urgency,
    /// Note asking how the user is doing, as a fired check-in or one of its own trigger points.
    #[serde(default)]
    pub checkin: bool,
    pub event_id: Option<i64>,
    /// The conversation the message opened or joined; a check-in carries one,
    /// so the client can land on the thread where its question waits.
    pub conversation_id: Option<i64>,
    /// One answer the user can give without typing. Only the channels that
    /// carry buttons look at these; the rest deliver the text alone.
    pub actions: Vec<Action>,
    /// The parked wake-up a call with this message resumes once its outcome is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<i64>,
}

/// A button on a message. `data` travels back verbatim when it is pressed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub label: String,
    pub data: String,
}

pub const MAX_ACTION_DATA: usize = 64;

/// The event channel of something laid to ring: it rings through
/// `calling::ring_if_free` instead of walking the ladder.
pub const RING_CHANNEL: &str = "call";

/// What can be said about an event that has just asked for something.
pub fn event_actions(event_id: i64, lang: Lang) -> Vec<Action> {
    vec![
        Action { label: text::action_done(lang), data: format!("ev:done:{event_id}") },
        Action { label: text::action_snooze_15(lang), data: format!("ev:snooze:{event_id}:15") },
        Action { label: text::action_drop(lang), data: format!("ev:drop:{event_id}") },
    ]
}

/// What can be said about a trigger that has spoken: it has settled, so only
/// asking again later is left.
pub fn trigger_actions(event_id: i64, lang: Lang) -> Vec<Action> {
    vec![Action { label: text::action_snooze_15(lang), data: format!("ev:snooze:{event_id}:15") }]
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

    /// A companion keeps a copy: the walk posts to it and always goes on.
    fn companion(&self) -> bool {
        false
    }
}

/// Pure rendering of a fired event into a user-facing message; a non-empty
/// per-event `message` overrides the generic body.
pub fn render(conn: &Connection, ev: &crate::runner::FiredEvent, lang: Lang) -> OutboundMessage {
    let (title, mut body, urgency) = if is_checkin(&ev.kind) {
        (text::checkin_title(lang), text::checkin_body(lang, &ev.wall_time), Urgency::High)
    } else if ev.kind == "debrief" {
        let content: String = conn
            .query_row(
                "SELECT content FROM debriefs WHERE user_id = ?1 AND date = ?2",
                (ev.user_id, &ev.date),
                |r| r.get(0),
            )
            .unwrap_or_else(|_| text::no_debrief(lang));
        (text::good_morning(lang), content, Urgency::Normal)
    } else {
        (ev.kind.clone(), text::scheduled_for(lang, &ev.wall_time), Urgency::Normal)
    };
    if !ev.message.is_empty() {
        body.clone_from(&ev.message);
    }
    let actions =
        if is_checkin(&ev.kind) { event_actions(ev.event_id, lang) } else { Vec::new() };
    OutboundMessage {
        title,
        reason: None,
        brief: None,
        body,
        urgency,
        checkin: is_checkin(&ev.kind),
        event_id: Some(ev.event_id),
        conversation_id: None,
        actions,
        wait: None,
    }
}

/// A check-in's question becomes the newest message of the day's check-in
/// thread before any channel carries it, so the thread exists whether or not
/// delivery succeeds. A failure to open the thread is logged and the message
/// still goes out, without an id.
fn open_checkin_thread(conn: &Connection, ev: &crate::runner::FiredEvent, lang: Lang, msg: &mut OutboundMessage) {
    if !is_checkin(&ev.kind) {
        return;
    }
    let now = jiff::Timestamp::now();
    match crate::talk::checkin_thread(conn, ev.user_id, &ev.date, &ev.wall_time, &msg.body, lang, now) {
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

/// Walks the ladder until one channel delivers, returning its name, and posts
/// to each companion it passes on the way. Every outcome is logged and none
/// propagates — a failed delivery is a plainer day, never an error. The DB guard is never held across a channel's `deliver`.
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
        if ch.companion() {
            let sent = ch.deliver(user_id, username, msg);
            let conn = crate::db_guard(db);
            let outage = (ch.name() == "matrix").then_some(crate::outage::Outage::Matrix);
            match sent {
                Ok(()) => {
                    if let Some(o) = outage {
                        let _ = crate::outage::clear(&conn, user_id, o);
                    }
                }
                Err(e) => {
                    let _ = crate::log::record(
                        &conn,
                        Some(user_id),
                        &format!("{}_send_error", ch.name()),
                        &format!("{subject}: {e}"),
                    );
                    if let Some(o) = outage {
                        let _ = crate::outage::raise(&conn, user_id, o, jiff::Timestamp::now());
                    }
                }
            }
            continue;
        }
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

/// Renders a fired event and opens its check-in thread where it has one. A
/// call it rings with is told the event's title as its reason.
pub fn prepare_event(db: &Mutex<Connection>, ev: &crate::runner::FiredEvent, lang: Lang) -> OutboundMessage {
    let conn = crate::db_guard(db);
    let mut msg = render(&conn, ev, lang);
    open_checkin_thread(&conn, ev, lang, &mut msg);
    if ev.channel == RING_CHANNEL {
        msg.reason = Some(msg.title.clone());
    }
    msg
}

/// `prepare_event`, then a walk down the ladder with the result.
pub fn deliver_event(
    db: &Mutex<Connection>,
    ladder: &[Arc<dyn Channel>],
    ev: &crate::runner::FiredEvent,
    lang: Lang,
) {
    let msg = prepare_event(db, ev, lang);
    deliver_via(db, ladder, ev.user_id, &ev.username, &msg);
}

/// What a starting block says, and where: a notification through the ladder, or
/// a line in the day's thread that never leaves the app. Either way the day has
/// changed, so the open clients are told to look again.
pub fn deliver_block_start(
    db: &Mutex<Connection>,
    ladder: &[Arc<dyn Channel>],
    hub: &ws::ClientHub,
    start: &crate::runner::BlockStart,
    lang: Lang,
) {
    if start.notify == "chat" {
        let conn = crate::db_guard(db);
        let line = text::block_start_line(lang, &start.task, &start.end_wall_time);
        if let Err(e) = crate::talk::checkin_thread(
            &conn,
            start.user_id,
            &start.date,
            &start.wall_time,
            &line,
            lang,
            jiff::Timestamp::now(),
        ) {
            let _ = crate::log::record(
                &conn,
                Some(start.user_id),
                "checkin_thread_error",
                &format!("event {}: {e}", start.event_id),
            );
        }
        drop(conn);
        hub.broadcast_changed(start.user_id);
        return;
    }
    deliver_via(
        db,
        ladder,
        start.user_id,
        &start.username,
        &OutboundMessage {
            title: text::starting_now(lang),
            reason: None,
            brief: None,
            body: text::block_start_body(lang, &start.task, &start.end_wall_time),
            urgency: Urgency::Normal,
            checkin: false,
            event_id: Some(start.event_id),
            conversation_id: None,
            actions: vec![Action {
                label: text::action_start_session(lang),
                data: format!("block:start:{}", start.event_id),
            }],
            wait: None,
        },
    );
    hub.broadcast_changed(start.user_id);
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
            folded: Vec::new(),
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
        let m = render(&conn, &ev("debrief", "push", ""), Lang::En);
        assert_eq!(m.title, "Good morning");
        assert!(m.body.contains("slept well"));

        let m = render(&conn, &ev("checkin_call", "push", ""), Lang::En);
        assert_eq!(m.title, "Check-in");
        assert_eq!(m.urgency, Urgency::High);
        assert!(m.body.contains("09:00"), "body: {}", m.body);
        assert!(!m.body.contains("checkin_call"), "body leaks the kind: {}", m.body);

        let m = render(&conn, &ev("nudge", "push", "you wanted a stretch break"), Lang::En);
        assert_eq!(m.body, "you wanted a stretch break");
    }

    #[test]
    fn only_a_checkin_is_rendered_with_something_to_press() {
        let (db, _uid) = env();
        let conn = db.lock().unwrap();
        let m = render(&conn, &ev("checkin_call", "push", ""), Lang::En);
        assert_eq!(
            m.actions,
            vec![
                Action { label: "Done".into(), data: "ev:done:11".into() },
                Action { label: "Snooze 15".into(), data: "ev:snooze:11:15".into() },
                Action { label: "Drop".into(), data: "ev:drop:11".into() },
            ]
        );
        assert!(m.actions.iter().all(|a| a.data.len() <= MAX_ACTION_DATA));
        assert!(render(&conn, &ev("nudge", "push", "stretch"), Lang::En).actions.is_empty());
        assert!(render(&conn, &ev("debrief", "push", ""), Lang::En).actions.is_empty());
    }

    #[test]
    fn a_checkin_speaks_the_users_language_and_keeps_its_data() {
        let (db, _uid) = env();
        let conn = db.lock().unwrap();
        let m = render(&conn, &ev("checkin_call", "push", ""), Lang::Ja);
        assert_eq!(m.title, "チェックイン");
        assert!(m.body.starts_with("09:00のチェックイン"), "body: {}", m.body);
        assert_eq!(m.actions[0], Action { label: "完了".into(), data: "ev:done:11".into() });
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
        let m = render(&conn, &ev("debrief", "push", ""), Lang::En);
        assert_eq!(m.body, "(no debrief yet)");
    }

    fn plain(body: &str) -> OutboundMessage {
        OutboundMessage {
            title: "Note".into(),
            reason: None,
            brief: None,
            body: body.into(),
            urgency: Urgency::Normal,
            checkin: false,
            event_id: None,
            conversation_id: None,
            actions: Vec::new(),
            wait: None,
        }
    }

    #[test]
    fn a_companion_is_posted_to_and_the_walk_goes_on() {
        let (db, _uid) = env();
        let matrix = Arc::new(MockChannel::new("matrix"));
        matrix.set_companion(true);
        let push = Arc::new(MockChannel::new("webpush"));
        let ladder: Vec<Arc<dyn Channel>> = vec![matrix.clone(), push.clone()];
        assert_eq!(deliver_via(&db, &ladder, 1, "aki", &plain("hi")), Some("webpush"));
        assert_eq!((matrix.seen().len(), push.seen().len()), (1, 1));
    }

    #[test]
    fn a_failing_companion_is_logged_and_delivers_nothing() {
        let (db, _uid) = env();
        let matrix = Arc::new(MockChannel::new("matrix"));
        matrix.set_companion(true);
        matrix.set_fail(true);
        let ladder: Vec<Arc<dyn Channel>> = vec![matrix];
        assert_eq!(deliver_via(&db, &ladder, 1, "aki", &plain("hi")), None);
        let conn = db.lock().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind = 'matrix_send_error'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn a_failing_matrix_stands_as_an_outage_until_a_post_gets_through() {
        let (db, _uid) = env();
        let matrix = Arc::new(MockChannel::new("matrix"));
        matrix.set_companion(true);
        matrix.set_fail(true);
        let ladder: Vec<Arc<dyn Channel>> = vec![matrix.clone()];
        let standing = |db: &Mutex<rusqlite::Connection>| {
            let conn = db.lock().unwrap();
            crate::outage::standing(&conn, 1, false, jiff::Timestamp::now()).unwrap().len()
        };
        deliver_via(&db, &ladder, 1, "aki", &plain("hi"));
        assert_eq!(standing(&db), 1);
        matrix.set_fail(false);
        deliver_via(&db, &ladder, 1, "aki", &plain("hi"));
        assert_eq!(standing(&db), 0);
    }

    #[test]
    fn dispatcher_falls_through_ladder_and_logs() {
        let (db, _uid) = env();
        let first = Arc::new(MockChannel::new("first"));
        let second = Arc::new(MockChannel::new("second"));
        first.set_fail(true);
        let ladder: Vec<Arc<dyn Channel>> = vec![first.clone(), second.clone()];

        deliver_event(&db, &ladder, &ev("nudge", "push", ""), Lang::En);
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
        ws.set_fail(true);
        let ladder: Vec<Arc<dyn Channel>> = vec![ws, webpush.clone()];
        let msg = OutboundMessage {
            title: "Note".into(),
            reason: None,
            brief: None,
            body: "Test notification".into(),
            urgency: Urgency::Normal,
            checkin: false,
            event_id: None,
            conversation_id: None,
            actions: Vec::new(),
            wait: None,
        };
        assert_eq!(deliver_via(&db, &ladder, 1, "aki", &msg), Some("webpush"));
        assert_eq!(webpush.seen().len(), 1);

        webpush.set_fail(true);
        assert_eq!(deliver_via(&db, &ladder, 1, "aki", &msg), None);
        let conn = db.lock().unwrap();
        let detail: String = conn
            .query_row("SELECT detail FROM event_log WHERE kind='delivery_degraded'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(detail.contains("webpush"), "unexpected detail: {detail}");
    }

    #[test]
    fn all_channels_failing_logs_degraded_not_error() {
        let (db, _uid) = env();
        let only = Arc::new(MockChannel::new("only"));
        only.set_fail(true);
        let ladder: Vec<Arc<dyn Channel>> = vec![only];
        deliver_event(&db, &ladder, &ev("nudge", "push", ""), Lang::En);
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
        deliver_event(&db, &[], &ev("nudge", "push", ""), Lang::En);
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
        deliver_event(&db, &ladder, &ev("nudge", "push", ""), Lang::En);
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

        deliver_event(&db, &ladder, &ev("checkin", "push", ""), Lang::En);
        let mut later = ev("checkin_call", "push", "Afternoon. Where did the morning go?");
        later.event_id = 12;
        later.wall_time = "15:30".into();
        deliver_event(&db, &ladder, &later, Lang::En);

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
        deliver_event(&db, &[], &ev("checkin", "push", ""), Lang::En);
        let conn = db.lock().unwrap();
        assert_eq!(thread_rows(&conn).len(), 1);
    }

    #[test]
    fn a_nudge_opens_no_thread() {
        let (db, _uid) = env();
        let push = Arc::new(MockChannel::new("push"));
        let ladder: Vec<Arc<dyn Channel>> = vec![push.clone()];
        deliver_event(&db, &ladder, &ev("nudge", "push", "stretch"), Lang::En);
        assert_eq!(push.seen()[0].1.conversation_id, None);
        let conn = db.lock().unwrap();
        assert!(thread_rows(&conn).is_empty());
    }
}
