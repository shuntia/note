use super::{deliver_via, Channel, OutboundMessage};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

/// The web app's side of a call Note places.
pub trait WebCalls: Send + Sync {
    /// Whether the user has the web app open and visible, able to take a call now.
    fn has_live(&self, user_id: i64) -> bool;
    /// Rings that web app; the call joins `conversation_id`, or a new thread
    /// when it is `None`. `false` when it could not be rung.
    fn ring(&self, user_id: i64, conversation_id: Option<i64>) -> bool;
}

pub struct NoWebCalls;

impl WebCalls for NoWebCalls {
    fn has_live(&self, _: i64) -> bool {
        false
    }
    fn ring(&self, _: i64, _: Option<i64>) -> bool {
        false
    }
}

/// Rings the user's linked phone; the message follows down the ladder if the
/// ring goes unanswered.
pub trait PhoneRing {
    fn ring_phone(&self, user_id: i64, msg: &OutboundMessage) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingOutcome {
    Web,
    Phone,
    /// Nothing could ring: the channel that carried the message instead, if any.
    Messaged(Option<&'static str>),
}

impl RingOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            RingOutcome::Web => "web",
            RingOutcome::Phone => "phone",
            RingOutcome::Messaged(Some(_)) => "messaged",
            RingOutcome::Messaged(None) => "undelivered",
        }
    }
}

pub fn ring_with(
    db: &Mutex<Connection>,
    web: &dyn WebCalls,
    phone: Option<&dyn PhoneRing>,
    ladder: &[Arc<dyn Channel>],
    user_id: i64,
    username: &str,
    msg: &OutboundMessage,
) -> RingOutcome {
    if web.has_live(user_id) && web.ring(user_id, msg.conversation_id) {
        return RingOutcome::Web;
    }
    if let Some(phone) = phone {
        match phone.ring_phone(user_id, msg) {
            Ok(()) => return RingOutcome::Phone,
            Err(e) => {
                let conn = crate::db_guard(db);
                let _ = crate::log::record(&conn, Some(user_id), "ring_phone_error", &format!("{e:#}"));
            }
        }
    }
    RingOutcome::Messaged(deliver_via(db, ladder, user_id, username, msg))
}

/// Calls the user the best way open to them: the open web app, else the linked
/// phone, else the message alone down the ladder.
pub fn ring(state: &crate::AppState, user_id: i64, username: &str, msg: &OutboundMessage) -> RingOutcome {
    let phone = state
        .voice
        .as_ref()
        .map(|v| super::voice::VoicePhone::new(v.clone(), state.db.clone()));
    ring_with(
        &state.db,
        state.web_calls.as_ref(),
        phone.as_ref().map(|p| p as &dyn PhoneRing),
        &state.channels,
        user_id,
        username,
        msg,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::mock::MockChannel;
    use crate::channels::Urgency;
    use std::sync::Mutex as StdMutex;

    struct FakeWeb {
        live: bool,
        answers: bool,
        rang: StdMutex<Vec<(i64, Option<i64>)>>,
    }

    impl WebCalls for FakeWeb {
        fn has_live(&self, _: i64) -> bool {
            self.live
        }
        fn ring(&self, user_id: i64, conversation_id: Option<i64>) -> bool {
            self.rang.lock().unwrap().push((user_id, conversation_id));
            self.answers
        }
    }

    fn web(live: bool, answers: bool) -> FakeWeb {
        FakeWeb { live, answers, rang: StdMutex::default() }
    }

    struct FakePhone(bool, StdMutex<usize>);

    impl PhoneRing for FakePhone {
        fn ring_phone(&self, _: i64, _: &OutboundMessage) -> anyhow::Result<()> {
            *self.1.lock().unwrap() += 1;
            anyhow::ensure!(self.0, "no linked Matrix account");
            Ok(())
        }
    }

    fn msg() -> OutboundMessage {
        OutboundMessage {
            title: "Note".into(),
            body: "got a minute?".into(),
            urgency: Urgency::Normal,
            checkin: false,
            event_id: Some(3),
            conversation_id: Some(7),
            actions: Vec::new(),
        }
    }

    fn world() -> (Mutex<Connection>, Arc<MockChannel>, Vec<Arc<dyn Channel>>) {
        let conn = crate::db::open_memory().unwrap();
        let push = Arc::new(MockChannel::new("mock"));
        let ladder: Vec<Arc<dyn Channel>> = vec![push.clone()];
        (Mutex::new(conn), push, ladder)
    }

    #[test]
    fn an_open_web_app_takes_the_call() {
        let (db, push, ladder) = world();
        let (web, phone) = (web(true, true), FakePhone(true, StdMutex::default()));
        let out = ring_with(&db, &web, Some(&phone), &ladder, 1, "aki", &msg());
        assert_eq!(out, RingOutcome::Web);
        assert_eq!(*web.rang.lock().unwrap(), [(1, Some(7))]);
        assert_eq!(*phone.1.lock().unwrap(), 0);
        assert!(push.seen().is_empty());
    }

    #[test]
    fn without_a_web_app_the_phone_rings() {
        let (db, push, ladder) = world();
        for w in [web(false, true), web(true, false)] {
            let phone = FakePhone(true, StdMutex::default());
            assert_eq!(ring_with(&db, &w, Some(&phone), &ladder, 1, "aki", &msg()), RingOutcome::Phone);
        }
        assert!(push.seen().is_empty());
    }

    #[test]
    fn with_nothing_to_ring_the_message_goes_down_the_ladder() {
        let (db, push, ladder) = world();
        let phone = FakePhone(false, StdMutex::default());
        assert_eq!(ring_with(&db, &NoWebCalls, Some(&phone), &ladder, 1, "aki", &msg()), RingOutcome::Messaged(Some("mock")));
        assert_eq!(ring_with(&db, &NoWebCalls, None, &ladder, 1, "aki", &msg()), RingOutcome::Messaged(Some("mock")));
        assert_eq!(push.seen().len(), 2);
        let errors: i64 = db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind = 'ring_phone_error'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(errors, 1);
    }

    #[test]
    fn a_state_rings_no_web_app_until_one_is_plugged_in() {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::AppState::new(crate::db::open_memory().unwrap(), tmp.path().into(), tmp.path().into());
        assert!(!state.web_calls.has_live(1));
        let state = state.with_web_calls(Arc::new(web(true, true)));
        assert!(state.web_calls.has_live(1));
    }
}
