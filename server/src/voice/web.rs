use crate::channels::OutboundMessage;
use note_voice_proto::{CallBody, LiveState, Media, Outcome};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// The rate of the audio a web call sends the browser.
pub const OUT_RATE: u32 = 48_000;
/// How long a ring offered to the open app can be answered.
pub const RING_FOR: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub enum WebOut {
    Audio(Vec<i16>),
    Flush,
    State(LiveState),
    Caption(String),
    Ended { reason: &'static str, conversation_id: Option<i64> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebRefusal {
    Unavailable,
    Busy,
}

impl WebRefusal {
    pub fn reason(self) -> &'static str {
        match self {
            WebRefusal::Unavailable => "unavailable",
            WebRefusal::Busy => "busy",
        }
    }
}

/// `message` set makes it Note's call (an answered ring), spoken first; unset, Note waits for the caller.
pub struct WebStart {
    pub thread: Option<i64>,
    pub message: Option<OutboundMessage>,
}

pub struct WebCall {
    pub call_id: String,
    pub out: mpsc::UnboundedReceiver<WebOut>,
}

struct Ring {
    user_id: i64,
    conversation_id: Option<i64>,
    until: Instant,
}

/// Each live web call's browser socket by call id, and the rings offered to open apps.
#[derive(Default)]
pub struct Relays {
    calls: Mutex<HashMap<String, (i64, mpsc::UnboundedSender<WebOut>)>>,
    rings: Mutex<HashMap<String, Ring>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Relays {
    pub fn open(&self, call_id: &str, user_id: i64) -> mpsc::UnboundedReceiver<WebOut> {
        let (tx, rx) = mpsc::unbounded_channel();
        lock(&self.calls).insert(call_id.to_string(), (user_id, tx));
        rx
    }

    /// Tells each of the user's open web calls it was replaced and forgets it; returns their ids.
    pub fn replace(&self, user_id: i64) -> Vec<String> {
        let mut calls = lock(&self.calls);
        let ids: Vec<String> = calls.iter().filter(|(_, (u, _))| *u == user_id).map(|(id, _)| id.clone()).collect();
        for id in &ids {
            if let Some((_, tx)) = calls.remove(id) {
                let _ = tx.send(WebOut::Ended { reason: "replaced", conversation_id: None });
            }
        }
        ids
    }

    pub fn is_web(&self, call_id: &str) -> bool {
        lock(&self.calls).contains_key(call_id)
    }

    pub fn forget(&self, call_id: &str) {
        lock(&self.calls).remove(call_id);
    }

    fn send(&self, call_id: &str, out: WebOut) {
        if let Some((_, tx)) = lock(&self.calls).get(call_id) {
            let _ = tx.send(out);
        }
    }

    fn end(&self, call_id: &str, reason: &'static str, conversation_id: Option<i64>) {
        if let Some((_, tx)) = lock(&self.calls).remove(call_id) {
            let _ = tx.send(WebOut::Ended { reason, conversation_id });
        }
    }

    pub fn media(&self, call_id: &str, body: Media) {
        let out = match body {
            Media::AudioOut { pcm, .. } => WebOut::Audio(pcm.0),
            Media::Flush => WebOut::Flush,
            Media::State { state } => WebOut::State(state),
            Media::AudioIn { .. } => return,
        };
        self.send(call_id, out);
    }

    /// Captions the caller's words as Note hears them, and closes the socket with the call.
    pub fn on_frame(&self, call_id: &str, body: &CallBody, conversation_id: impl FnOnce() -> Option<i64>) {
        match body {
            CallBody::Draft { text, .. } | CallBody::Commit { text, .. } if !text.trim().is_empty() => {
                self.send(call_id, WebOut::Caption(text.clone()));
            }
            CallBody::Outcome { outcome: Outcome::Failed { .. } } => self.end(call_id, "failed", None),
            CallBody::Ended if self.is_web(call_id) => self.end(call_id, "ended", conversation_id()),
            _ => {}
        }
    }

    pub fn offer_ring_at(&self, user_id: i64, conversation_id: Option<i64>, now: Instant) -> String {
        let token = uuid::Uuid::new_v4().to_string();
        let mut rings = lock(&self.rings);
        rings.retain(|_, r| r.until >= now);
        rings.insert(token.clone(), Ring { user_id, conversation_id, until: now + RING_FOR });
        token
    }

    /// The thread the ring was about, for its own user, once, while it lasts.
    pub fn take_ring_at(&self, token: &str, user_id: i64, now: Instant) -> Option<Option<i64>> {
        let mut rings = lock(&self.rings);
        let ring = rings.get(token).filter(|r| r.user_id == user_id && r.until >= now)?;
        let conversation_id = ring.conversation_id;
        rings.remove(token);
        Some(conversation_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use note_voice_proto::Pcm;

    #[test]
    fn media_and_the_callers_words_reach_only_their_calls_socket() {
        let relays = Relays::default();
        let mut a = relays.open("a", 1);
        let mut b = relays.open("b", 2);
        relays.media("a", Media::AudioOut { pcm: Pcm(vec![3; 480]), rate: OUT_RATE });
        relays.media("a", Media::State { state: LiveState::Thinking });
        relays.media("a", Media::Flush);
        relays.media("a", Media::AudioIn { pcm: Pcm(vec![1; 320]) });
        relays.on_frame("a", &CallBody::Draft { turn: 1, text: "move my run".into(), language: None }, || None);
        relays.on_frame("a", &CallBody::Commit { turn: 1, text: "  ".into(), language: None }, || None);
        assert_eq!(a.try_recv().unwrap(), WebOut::Audio(vec![3; 480]));
        assert_eq!(a.try_recv().unwrap(), WebOut::State(LiveState::Thinking));
        assert_eq!(a.try_recv().unwrap(), WebOut::Flush);
        assert_eq!(a.try_recv().unwrap(), WebOut::Caption("move my run".into()));
        assert!(a.try_recv().is_err(), "the caller's own audio and a blank commit are not sent back");
        assert!(b.try_recv().is_err());
    }

    #[test]
    fn the_end_of_a_call_closes_its_socket_once_with_its_thread() {
        let relays = Relays::default();
        let mut a = relays.open("a", 1);
        relays.on_frame("a", &CallBody::Ended, || Some(9));
        relays.on_frame("a", &CallBody::Ended, || panic!("asked for the thread twice"));
        assert_eq!(a.try_recv().unwrap(), WebOut::Ended { reason: "ended", conversation_id: Some(9) });
        assert!(a.try_recv().is_err());
        assert!(!relays.is_web("a"));
    }

    #[test]
    fn a_failed_call_ends_as_failed() {
        let relays = Relays::default();
        let mut a = relays.open("a", 1);
        relays.on_frame("a", &CallBody::Outcome { outcome: Outcome::Failed { reason: "late".into() } }, || None);
        assert_eq!(a.try_recv().unwrap(), WebOut::Ended { reason: "failed", conversation_id: None });
    }

    #[test]
    fn a_new_call_replaces_only_that_users_open_calls() {
        let relays = Relays::default();
        let mut a = relays.open("a", 1);
        let mut b = relays.open("b", 2);
        assert_eq!(relays.replace(1), vec!["a".to_string()]);
        assert_eq!(a.try_recv().unwrap(), WebOut::Ended { reason: "replaced", conversation_id: None });
        assert!(b.try_recv().is_err());
        assert!(relays.is_web("b") && !relays.is_web("a"));
    }

    #[test]
    fn a_ring_is_answered_once_by_its_user_while_it_lasts() {
        let relays = Relays::default();
        let at = Instant::now();
        let token = relays.offer_ring_at(1, Some(4), at);
        assert_eq!(relays.take_ring_at(&token, 2, at), None, "another user's ring");
        assert_eq!(relays.take_ring_at(&token, 1, at), Some(Some(4)));
        assert_eq!(relays.take_ring_at(&token, 1, at), None, "answered once");
        let late = relays.offer_ring_at(1, None, at);
        assert_eq!(relays.take_ring_at(&late, 1, at + RING_FOR + Duration::from_secs(1)), None);
    }
}
