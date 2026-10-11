use super::{deliver_via, Channel, OutboundMessage};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

/// The web app's side of a call Note places.
pub trait WebCalls: Send + Sync {
    /// Whether the user has the web app open and visible, able to take a call now.
    fn has_live(&self, user_id: i64) -> bool;
    /// Rings that web app; the call joins the message's conversation, or a new
    /// thread when it has none. `false` when it could not be rung. `phone` is the
    /// phone call ringing alongside: whichever is answered first stands the other
    /// down, and that call owns the message's delivery. Without one, the ringer
    /// owns it if the ring goes unanswered.
    fn ring(&self, user_id: i64, msg: &OutboundMessage, phone: Option<&str>) -> bool;
}

pub struct NoWebCalls;

impl WebCalls for NoWebCalls {
    fn has_live(&self, _: i64) -> bool {
        false
    }
    fn ring(&self, _: i64, _: &OutboundMessage, _: Option<&str>) -> bool {
        false
    }
}

#[derive(Debug, thiserror::Error)]
#[error("no linked Matrix account")]
pub struct NoLinkedPhone;

/// Rings the user's linked phone and returns the call's id; the message follows
/// down the ladder if the ring goes unanswered. Fails with `NoLinkedPhone` when
/// there is none to ring.
pub trait PhoneRing {
    fn ring_phone(&self, user_id: i64, msg: &OutboundMessage) -> anyhow::Result<String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingOutcome {
    Web,
    Phone,
    /// The web app and the phone both ring; the first answered takes the call.
    Both,
    /// Nothing could ring: the channel that carried the message instead, if any.
    Messaged(Option<&'static str>),
}

impl RingOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            RingOutcome::Web => "web",
            RingOutcome::Phone => "phone",
            RingOutcome::Both => "web+phone",
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
    let phone_call = phone.and_then(|phone| match phone.ring_phone(user_id, msg) {
        Ok(call_id) => Some(call_id),
        Err(e) if e.is::<NoLinkedPhone>() => None,
        Err(e) => {
            let subject = msg.event_id.map_or_else(|| "test".to_string(), |id| format!("event {id}"));
            let conn = crate::db_guard(db);
            let _ = crate::log::record(&conn, Some(user_id), "ring_phone_error", &format!("{subject}: {e:#}"));
            None
        }
    });
    let web_rang = web.has_live(user_id) && web.ring(user_id, msg, phone_call.as_deref());
    match (web_rang, phone_call.is_some()) {
        (true, true) => return RingOutcome::Both,
        (true, false) => return RingOutcome::Web,
        (false, true) => return RingOutcome::Phone,
        (false, false) => {}
    }
    let rest: Vec<Arc<dyn Channel>> = ladder.iter().filter(|c| c.name() != "voice").cloned().collect();
    RingOutcome::Messaged(deliver_via(db, &rest, user_id, username, msg))
}

/// Calls the user every way open to them at once — the open web app and the
/// linked phone — else sends the message alone down the ladder.
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reach {
    /// The web app is open and visible.
    pub web: bool,
    /// A linked phone and a voice side up to ring it.
    pub phone: bool,
}

/// What a session needs to call its user.
pub trait Caller {
    fn reach(&self, user_id: i64) -> Reach;
    /// Lands `opening` in the day's check-in thread, then rings with it.
    fn ring(&self, user_id: i64, username: &str, opening: &str, reason: &str) -> RingOutcome;
}

impl Caller for crate::AppState {
    fn reach(&self, user_id: i64) -> Reach {
        let phone = self.voice.as_ref().is_some_and(|v| v.is_up())
            && crate::voice::links::ringable(&self.db(), user_id).ok().flatten().is_some();
        Reach { web: self.web_calls.has_live(user_id), phone }
    }

    fn ring(&self, user_id: i64, username: &str, opening: &str, reason: &str) -> RingOutcome {
        let now = jiff::Timestamp::now();
        let lang = crate::text::Lang::for_user(&self.config_dir, username);
        let local = now.to_zoned(crate::triggers::timezone(&self.config_dir, username));
        let wall = format!("{:02}:{:02}", local.hour(), local.minute());
        let thread = {
            let conn = self.db();
            crate::talk::checkin_thread(&conn, user_id, &local.date().to_string(), &wall, opening, lang, now).ok()
        };
        self.hub.broadcast_changed(user_id);
        let msg = OutboundMessage {
            title: "Note".into(),
            reason: Some(reason.to_string()),
            body: opening.to_string(),
            urgency: super::Urgency::Normal,
            checkin: false,
            event_id: None,
            conversation_id: thread,
            actions: Vec::new(),
        };
        ring(self, user_id, username, &msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::mock::MockChannel;
    use crate::channels::Urgency;
    use std::sync::Mutex as StdMutex;

    /// Each ring's user, conversation and the phone call beside it.
    type Rang = (i64, Option<i64>, Option<String>);

    struct FakeWeb {
        live: bool,
        answers: bool,
        rang: StdMutex<Vec<Rang>>,
    }

    impl WebCalls for FakeWeb {
        fn has_live(&self, _: i64) -> bool {
            self.live
        }
        fn ring(&self, user_id: i64, msg: &OutboundMessage, phone: Option<&str>) -> bool {
            self.rang.lock().unwrap().push((user_id, msg.conversation_id, phone.map(str::to_string)));
            self.answers
        }
    }

    fn web(live: bool, answers: bool) -> FakeWeb {
        FakeWeb { live, answers, rang: StdMutex::default() }
    }

    #[derive(Clone, Copy)]
    enum Phone {
        Answers,
        Unlinked,
        Fails,
    }

    struct FakePhone(Phone, StdMutex<usize>);

    impl PhoneRing for FakePhone {
        fn ring_phone(&self, _: i64, _: &OutboundMessage) -> anyhow::Result<String> {
            *self.1.lock().unwrap() += 1;
            match self.0 {
                Phone::Answers => Ok("call-1".into()),
                Phone::Unlinked => Err(NoLinkedPhone.into()),
                Phone::Fails => anyhow::bail!("the voice service is not connected"),
            }
        }
    }

    fn phone(p: Phone) -> FakePhone {
        FakePhone(p, StdMutex::default())
    }

    fn ring_errors(db: &Mutex<Connection>) -> Vec<String> {
        let conn = db.lock().unwrap();
        let mut stmt = conn.prepare("SELECT detail FROM event_log WHERE kind = 'ring_phone_error'").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    }

    fn msg() -> OutboundMessage {
        OutboundMessage {
            title: "Note".into(),
            reason: None,
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
    fn the_web_app_and_the_phone_ring_together() {
        let (db, push, ladder) = world();
        let (web, phone) = (web(true, true), phone(Phone::Answers));
        let out = ring_with(&db, &web, Some(&phone), &ladder, 1, "aki", &msg());
        assert_eq!(out, RingOutcome::Both);
        assert_eq!(*web.rang.lock().unwrap(), [(1, Some(7), Some("call-1".to_string()))]);
        assert_eq!(*phone.1.lock().unwrap(), 1);
        assert!(push.seen().is_empty());
    }

    #[test]
    fn an_open_web_app_rings_alone_without_a_phone() {
        let (db, push, ladder) = world();
        for phone in [phone(Phone::Unlinked), phone(Phone::Fails)] {
            let web = web(true, true);
            assert_eq!(ring_with(&db, &web, Some(&phone), &ladder, 1, "aki", &msg()), RingOutcome::Web);
            assert_eq!(*web.rang.lock().unwrap(), [(1, Some(7), None)]);
        }
        assert_eq!(ring_with(&db, &web(true, true), None, &ladder, 1, "aki", &msg()), RingOutcome::Web);
        assert!(push.seen().is_empty());
    }

    #[test]
    fn without_a_web_app_the_phone_rings() {
        let (db, push, ladder) = world();
        for w in [web(false, true), web(true, false)] {
            let phone = phone(Phone::Answers);
            assert_eq!(ring_with(&db, &w, Some(&phone), &ladder, 1, "aki", &msg()), RingOutcome::Phone);
        }
        assert!(push.seen().is_empty());
    }

    #[test]
    fn with_nothing_to_ring_the_message_goes_down_the_ladder() {
        let (db, push, ladder) = world();
        let phone = phone(Phone::Fails);
        assert_eq!(ring_with(&db, &NoWebCalls, Some(&phone), &ladder, 1, "aki", &msg()), RingOutcome::Messaged(Some("mock")));
        assert_eq!(ring_with(&db, &NoWebCalls, None, &ladder, 1, "aki", &msg()), RingOutcome::Messaged(Some("mock")));
        assert_eq!(push.seen().len(), 2);
        assert_eq!(ring_errors(&db), ["event 3: the voice service is not connected"]);
    }

    #[test]
    fn a_user_without_a_linked_phone_is_messaged_quietly() {
        let (db, push, ladder) = world();
        let phone = phone(Phone::Unlinked);
        assert_eq!(ring_with(&db, &NoWebCalls, Some(&phone), &ladder, 1, "aki", &msg()), RingOutcome::Messaged(Some("mock")));
        assert_eq!(push.seen().len(), 1);
        assert!(ring_errors(&db).is_empty());
    }

    #[test]
    fn a_failed_phone_ring_is_not_retried_down_the_ladder() {
        let (db, push, _) = world();
        let voice = Arc::new(MockChannel::new("voice"));
        let ladder: Vec<Arc<dyn Channel>> = vec![voice.clone(), push.clone()];
        let phone = phone(Phone::Fails);
        assert_eq!(ring_with(&db, &NoWebCalls, Some(&phone), &ladder, 1, "aki", &msg()), RingOutcome::Messaged(Some("mock")));
        assert_eq!(*phone.1.lock().unwrap(), 1);
        assert!(voice.seen().is_empty());
        assert_eq!(push.seen().len(), 1);
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
