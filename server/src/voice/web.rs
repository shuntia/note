use super::Voice;
use crate::channels::OutboundMessage;
use axum::extract::ws::{Message, WebSocket};
use note_voice_proto::{CallBody, LiveState, Media, Outcome};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// The rate of the audio a web call sends the browser.
pub const OUT_RATE: u32 = 48_000;
/// How long a ring offered to the open app can be answered.
pub const RING_FOR: Duration = Duration::from_secs(30);
/// Audio frames a call's socket may hold unsent before further audio is dropped; control always gets through.
pub const AUDIO_BACKLOG: usize = 50;

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
    pub out: WebRx,
}

/// What a web call sends its browser, in order.
pub struct WebRx {
    rx: mpsc::UnboundedReceiver<WebOut>,
    audio: Arc<AtomicUsize>,
}

impl WebRx {
    pub async fn recv(&mut self) -> Option<WebOut> {
        let out = self.rx.recv().await;
        self.took(out.as_ref());
        out
    }

    pub fn try_recv(&mut self) -> Result<WebOut, mpsc::error::TryRecvError> {
        let out = self.rx.try_recv();
        self.took(out.as_ref().ok());
        out
    }

    fn took(&self, out: Option<&WebOut>) {
        if let Some(WebOut::Audio(_)) = out {
            self.audio.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

struct Socket {
    user_id: i64,
    tx: mpsc::UnboundedSender<WebOut>,
    audio: Arc<AtomicUsize>,
}

impl Socket {
    fn send(&self, out: WebOut) {
        if let WebOut::Audio(_) = out {
            if self.audio.load(Ordering::Relaxed) >= AUDIO_BACKLOG {
                return;
            }
            self.audio.fetch_add(1, Ordering::Relaxed);
        }
        let _ = self.tx.send(out);
    }
}

struct Ring {
    user_id: i64,
    conversation_id: Option<i64>,
    until: Instant,
}

/// Each live web call's browser socket by call id, and the rings offered to open apps.
#[derive(Default)]
pub struct Relays {
    calls: Mutex<HashMap<String, Socket>>,
    rings: Mutex<HashMap<String, Ring>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Relays {
    pub fn open(&self, call_id: &str, user_id: i64) -> WebRx {
        let (tx, rx) = mpsc::unbounded_channel();
        let audio = Arc::new(AtomicUsize::new(0));
        lock(&self.calls).insert(call_id.to_string(), Socket { user_id, tx, audio: audio.clone() });
        WebRx { rx, audio }
    }

    /// Tells each of the user's open web calls it was replaced and forgets it; returns their ids.
    pub fn replace(&self, user_id: i64) -> Vec<String> {
        let mut calls = lock(&self.calls);
        let ids: Vec<String> = calls.iter().filter(|(_, s)| s.user_id == user_id).map(|(id, _)| id.clone()).collect();
        for id in &ids {
            if let Some(socket) = calls.remove(id) {
                socket.send(WebOut::Ended { reason: "replaced", conversation_id: None });
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
        if let Some(socket) = lock(&self.calls).get(call_id) {
            socket.send(out);
        }
    }

    /// Closes the call's socket with `reason`; does nothing once it is closed.
    pub fn end(&self, call_id: &str, reason: &'static str, conversation_id: Option<i64>) {
        if let Some(socket) = lock(&self.calls).remove(call_id) {
            socket.send(WebOut::Ended { reason, conversation_id });
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

/// A browser that sends no audio this long is taken to be gone.
pub const SILENCE_LIMIT: Duration = Duration::from_secs(5);
/// A voice link down this long ends the call for the browser.
pub const LINK_LIMIT: Duration = Duration::from_secs(10);
/// 100 ms of 16 kHz s16; anything larger is not a capture frame.
pub(crate) const MAX_IN_BYTES: usize = 3200;
/// A browser that takes longer than this to accept a frame is taken to be gone.
const SEND_LIMIT: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Control {
    Mute { on: bool },
    Hangup,
}

/// Opens the call the socket asked for and relays it until it ends.
pub async fn serve(
    socket: WebSocket,
    state: crate::AppState,
    user_id: i64,
    conversation_id: Option<i64>,
    ring: Option<String>,
) {
    let Some(voice) = state.voice.clone().filter(|v| v.is_up()) else {
        return refuse(socket, "unavailable").await;
    };
    let start = match &ring {
        Some(token) => {
            let Some(start) = voice.answer_ring(token, user_id) else {
                return refuse(socket, "missed").await;
            };
            start
        }
        None => WebStart { thread: conversation_id, message: None },
    };
    match voice.start_web_call(user_id, start, jiff::Timestamp::now()) {
        Ok(call) => {
            if let Some(token) = ring {
                state.hub.send(user_id, &serde_json::json!({ "type": "ring_taken", "ring": token }).to_string());
            }
            relay(socket, &voice, call).await;
        }
        Err(refusal) => refuse(socket, refusal.reason()).await,
    }
}

async fn refuse(mut socket: WebSocket, reason: &'static str) {
    send(&mut socket, to_message(WebOut::Ended { reason, conversation_id: None })).await;
    send(&mut socket, Message::Close(None)).await;
}

async fn send(socket: &mut WebSocket, message: Message) -> bool {
    matches!(tokio::time::timeout(SEND_LIMIT, socket.send(message)).await, Ok(Ok(())))
}

/// After the browser hangs up, Note's last words keep flowing until the voice side's `Ended`;
/// a browser that goes away hangs the call up and leaves its relay to that `Ended`.
async fn relay(mut socket: WebSocket, voice: &Voice, call: WebCall) {
    let WebCall { call_id, mut out } = call;
    let opened = serde_json::json!({ "type": "open", "rate": OUT_RATE }).to_string();
    if !send(&mut socket, Message::Text(opened.into())).await {
        return voice.hang_up(&call_id);
    }
    let (mut muted, mut hung_up) = (false, false);
    let mut heard = tokio::time::Instant::now();
    let mut down_since: Option<tokio::time::Instant> = None;
    let mut watch = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            got = out.recv() => {
                let Some(o) = got else { break };
                let ended = matches!(o, WebOut::Ended { .. });
                let sent = send(&mut socket, to_message(o)).await;
                if ended {
                    send(&mut socket, Message::Close(None)).await;
                    return;
                }
                if !sent {
                    break;
                }
            }
            got = socket.recv() => match got {
                Some(Ok(Message::Binary(bytes))) if (2..=MAX_IN_BYTES).contains(&bytes.len()) => {
                    heard = tokio::time::Instant::now();
                    if !hung_up {
                        voice.web_audio(&call_id, pcm_in(&bytes, muted));
                    }
                }
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<Control>(text.as_str()) {
                    Ok(Control::Mute { on }) => muted = on,
                    Ok(Control::Hangup) if !hung_up => {
                        hung_up = true;
                        voice.hang_up(&call_id);
                    }
                    _ => {}
                },
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            _ = watch.tick() => {
                if !hung_up && heard.elapsed() >= SILENCE_LIMIT {
                    hung_up = true;
                    voice.hang_up(&call_id);
                }
                if voice.is_up() {
                    down_since = None;
                } else if down_since.get_or_insert_with(tokio::time::Instant::now).elapsed() >= LINK_LIMIT {
                    send(&mut socket, to_message(WebOut::Ended { reason: "unavailable", conversation_id: None })).await;
                    send(&mut socket, Message::Close(None)).await;
                    break;
                }
            }
        }
    }
    if !hung_up {
        voice.hang_up(&call_id);
    }
}

fn pcm_in(bytes: &[u8], muted: bool) -> Vec<i16> {
    bytes.as_chunks::<2>().0.iter().map(|&b| if muted { 0 } else { i16::from_le_bytes(b) }).collect()
}

fn to_message(out: WebOut) -> Message {
    let json = |v: serde_json::Value| Message::Text(v.to_string().into());
    match out {
        WebOut::Audio(pcm) => Message::Binary(pcm.iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<u8>>().into()),
        WebOut::Flush => json(serde_json::json!({ "type": "flush" })),
        WebOut::State(state) => json(serde_json::json!({ "type": "state", "state": state })),
        WebOut::Caption(text) => json(serde_json::json!({ "type": "caption", "text": text })),
        WebOut::Ended { reason, conversation_id } => {
            json(serde_json::json!({ "type": "ended", "reason": reason, "conversation_id": conversation_id }))
        }
    }
}

/// Rings the user's app when a page of it is in view and the voice side is up.
pub struct WebRinger {
    hub: std::sync::Arc<crate::channels::ws::ClientHub>,
    voice: std::sync::Arc<Voice>,
}

impl WebRinger {
    pub fn new(hub: std::sync::Arc<crate::channels::ws::ClientHub>, voice: std::sync::Arc<Voice>) -> Self {
        Self { hub, voice }
    }
}

impl crate::channels::WebCalls for WebRinger {
    fn has_live(&self, user_id: i64) -> bool {
        self.voice.is_up() && self.hub.has_visible(user_id)
    }

    /// True when a page in view took the ring; it then opens the call view, ringing.
    fn ring(&self, user_id: i64, conversation_id: Option<i64>) -> bool {
        if !self.has_live(user_id) {
            return false;
        }
        let token = self.voice.offer_ring(user_id, conversation_id);
        let frame = serde_json::json!({ "type": "incoming", "ring": token, "conversation_id": conversation_id });
        self.hub.send_visible(user_id, &frame.to_string()) > 0
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
    fn a_backed_up_socket_drops_audio_but_never_control() {
        let relays = Relays::default();
        let mut a = relays.open("a", 1);
        for _ in 0..AUDIO_BACKLOG + 5 {
            relays.media("a", Media::AudioOut { pcm: Pcm(vec![3; 960]), rate: OUT_RATE });
        }
        relays.media("a", Media::Flush);
        relays.media("a", Media::State { state: LiveState::Listening });
        relays.on_frame("a", &CallBody::Ended, || Some(2));
        for _ in 0..AUDIO_BACKLOG {
            assert_eq!(a.try_recv().unwrap(), WebOut::Audio(vec![3; 960]));
        }
        assert_eq!(a.try_recv().unwrap(), WebOut::Flush);
        assert_eq!(a.try_recv().unwrap(), WebOut::State(LiveState::Listening));
        assert_eq!(a.try_recv().unwrap(), WebOut::Ended { reason: "ended", conversation_id: Some(2) });
        assert!(a.try_recv().is_err());
    }

    #[test]
    fn audio_flows_again_once_the_socket_catches_up() {
        let relays = Relays::default();
        let mut a = relays.open("a", 1);
        for _ in 0..AUDIO_BACKLOG + 1 {
            relays.media("a", Media::AudioOut { pcm: Pcm(vec![3; 960]), rate: OUT_RATE });
        }
        while a.try_recv().is_ok() {}
        relays.media("a", Media::AudioOut { pcm: Pcm(vec![4; 960]), rate: OUT_RATE });
        assert_eq!(a.try_recv().unwrap(), WebOut::Audio(vec![4; 960]));
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
