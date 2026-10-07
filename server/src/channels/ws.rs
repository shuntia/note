use super::{Channel, OutboundMessage};
use crate::agent::AgentEvent;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

type Conns = HashMap<i64, Vec<(u64, UnboundedSender<String>)>>;

/// Sockets one account may hold at once. Every delivery fans out to all of
/// them, and each pins a task plus an unbounded queue.
pub const MAX_PER_USER: usize = 8;

#[derive(Default)]
pub struct ClientHub {
    next_id: AtomicU64,
    conns: Mutex<Conns>,
}

impl ClientHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// `None` once the user already holds `MAX_PER_USER` sockets.
    pub fn register(&self, user_id: i64) -> Option<(u64, UnboundedReceiver<String>)> {
        let mut conns = self.conns.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let held = conns.entry(user_id).or_default();
        if held.len() >= MAX_PER_USER {
            return None;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = unbounded_channel();
        held.push((id, tx));
        Some((id, rx))
    }

    pub fn at_capacity(&self, user_id: i64) -> bool {
        self.conns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&user_id)
            .is_some_and(|v| v.len() >= MAX_PER_USER)
    }

    pub fn unregister(&self, user_id: i64, conn_id: u64) {
        let mut conns = self.conns.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(v) = conns.get_mut(&user_id) {
            v.retain(|(id, _)| *id != conn_id);
            if v.is_empty() {
                conns.remove(&user_id);
            }
        }
    }

    /// Sends to every live connection of `user_id`, pruning closed ones, and
    /// returns how many actually received it.
    pub fn send(&self, user_id: i64, text: &str) -> usize {
        let mut conns = self.conns.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(v) = conns.get_mut(&user_id) else {
            return 0;
        };
        v.retain(|(_, tx)| tx.send(text.to_string()).is_ok());
        let n = v.len();
        if v.is_empty() {
            conns.remove(&user_id);
        }
        n
    }
}

impl ClientHub {
    /// Tells every socket of `user_id` that something it is showing changed;
    /// the client reloads rather than patching, so the frame carries no detail.
    pub fn broadcast_changed(&self, user_id: i64) {
        self.send(user_id, "{\"type\":\"changed\"}");
    }
}

/// A frame the client chose to send. Ping and pong are answered by the
/// browser whether or not anyone is there.
pub fn is_presence(msg: &axum::extract::ws::Message) -> bool {
    use axum::extract::ws::Message;
    matches!(msg, Message::Text(_) | Message::Binary(_))
}

/// Cap on one agent frame's variable text, so a large tool payload cannot
/// flood a socket; what is left ends in an ellipsis.
pub const MAX_FRAME_TEXT: usize = 4 * 1024;

fn clip(text: &str) -> String {
    if text.len() <= MAX_FRAME_TEXT {
        return text.to_string();
    }
    let mut end = MAX_FRAME_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// One session event as the client reads it. `seq` counts from zero within a
/// session, so a client can order and dedupe frames from parallel sessions.
pub fn agent_frame(conversation_id: Option<i64>, seq: u64, event: &AgentEvent) -> String {
    let event = match *event {
        AgentEvent::Thinking { text } => {
            serde_json::json!({"kind": "thinking", "text": clip(text)})
        }
        AgentEvent::ToolCall { index, name, args } => {
            serde_json::json!({"kind": "tool_call", "index": index, "name": name, "args": clip(args)})
        }
        AgentEvent::ToolResult { index, name, result, is_error } => serde_json::json!({
            "kind": "tool_result", "index": index, "name": name,
            "result": clip(result), "is_error": is_error,
        }),
        AgentEvent::Reply { text } => serde_json::json!({"kind": "reply", "text": clip(text)}),
        AgentEvent::Error { reason } => serde_json::json!({"kind": "error", "reason": reason}),
    };
    serde_json::json!({
        "type": "agent",
        "conversation_id": conversation_id,
        "seq": seq,
        "event": event,
    })
    .to_string()
}

pub struct WsChannel {
    hub: Arc<ClientHub>,
}

impl WsChannel {
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self { hub }
    }
}

impl Channel for WsChannel {
    fn name(&self) -> &'static str {
        "ws"
    }

    fn deliver(&self, user_id: i64, _username: &str, msg: &OutboundMessage) -> anyhow::Result<()> {
        let text = serde_json::json!({
            "type": "event",
            "title": msg.title,
            "body": msg.body,
            "urgency": msg.urgency.as_str(),
            "event_id": msg.event_id,
            "conversation_id": msg.conversation_id,
        })
        .to_string();
        if self.hub.send(user_id, &text) == 0 {
            anyhow::bail!("no connected clients");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::{Channel, OutboundMessage, Urgency};
    use std::sync::Arc;

    fn msg() -> OutboundMessage {
        OutboundMessage {
            title: "Check-in".into(),
            body: "at 09:00".into(),
            urgency: Urgency::High,
            checkin: false,
            event_id: Some(7),
            conversation_id: Some(3),
            actions: Vec::new(),
        }
    }

    #[test]
    fn a_user_cannot_hold_more_than_the_socket_cap() {
        let hub = ClientHub::new();
        let held: Vec<_> = (0..MAX_PER_USER).map(|_| hub.register(1).unwrap()).collect();
        assert!(hub.at_capacity(1));
        assert!(hub.register(1).is_none());
        assert!(!hub.at_capacity(2));
        assert!(hub.register(2).is_some());
        drop(held);
        // a closed socket must free its slot
        hub.send(1, "x");
        assert!(!hub.at_capacity(1));
        assert!(hub.register(1).is_some());
    }

    #[test]
    fn hub_delivers_to_all_connections_of_the_user_only() {
        let hub = ClientHub::new();
        let (_id1, mut rx1) = hub.register(1).unwrap();
        let (_id2, mut rx2) = hub.register(1).unwrap();
        let (_id3, mut rx3) = hub.register(2).unwrap();
        assert_eq!(hub.send(1, "hello"), 2);
        assert_eq!(rx1.try_recv().unwrap(), "hello");
        assert_eq!(rx2.try_recv().unwrap(), "hello");
        assert!(rx3.try_recv().is_err());
    }

    #[test]
    fn unregister_drops_only_that_connection() {
        let hub = ClientHub::new();
        let (id1, mut rx1) = hub.register(1).unwrap();
        let (_id2, mut rx2) = hub.register(1).unwrap();
        hub.unregister(1, id1);
        assert_eq!(hub.send(1, "x"), 1);
        assert!(rx1.try_recv().is_err());
        assert_eq!(rx2.try_recv().unwrap(), "x");
    }

    #[test]
    fn dropped_receivers_stop_counting() {
        let hub = ClientHub::new();
        let (_id1, rx1) = hub.register(1).unwrap();
        let (_id2, rx2) = hub.register(1).unwrap();
        drop(rx1);
        drop(rx2);
        assert_eq!(hub.send(1, "x"), 0);
    }

    #[test]
    fn agent_frames_reach_the_users_socket_in_sequence() {
        let hub = ClientHub::new();
        let (_id, mut rx) = hub.register(1).unwrap();
        let (_other, mut theirs) = hub.register(2).unwrap();
        let events = [
            AgentEvent::Thinking { text: "a task, then" },
            AgentEvent::ToolCall { index: 0, name: "task_create", args: "{\"title\":\"milk\"}" },
            AgentEvent::ToolResult { index: 0, name: "task_create", result: "{}", is_error: false },
            AgentEvent::Reply { text: "added it" },
        ];
        for (seq, ev) in events.iter().enumerate() {
            hub.send(1, &agent_frame(Some(7), seq as u64, ev));
        }
        let frames: Vec<serde_json::Value> = (0..4)
            .map(|_| serde_json::from_str(&rx.try_recv().unwrap()).unwrap())
            .collect();
        assert!(frames.iter().all(|f| f["type"] == "agent" && f["conversation_id"] == 7));
        assert_eq!(
            frames.iter().map(|f| f["seq"].as_u64().unwrap()).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(frames[0]["event"]["kind"], "thinking");
        assert_eq!(frames[1]["event"]["args"], "{\"title\":\"milk\"}");
        assert_eq!(frames[2]["event"]["is_error"], false);
        assert_eq!(frames[3]["event"]["text"], "added it");
        assert!(theirs.try_recv().is_err());
    }

    #[test]
    fn a_new_conversation_frame_carries_a_null_id_and_clips_long_text() {
        let long = "x".repeat(MAX_FRAME_TEXT * 2);
        let frame: serde_json::Value = serde_json::from_str(&agent_frame(
            None,
            0,
            &AgentEvent::ToolResult { index: 1, name: "memory_query", result: &long, is_error: true },
        ))
        .unwrap();
        assert!(frame["conversation_id"].is_null());
        let result = frame["event"]["result"].as_str().unwrap();
        assert_eq!(result.len(), MAX_FRAME_TEXT + "…".len());
        assert!(result.ends_with('…'));
    }

    #[test]
    fn a_changed_frame_reaches_only_that_users_sockets() {
        let hub = ClientHub::new();
        let (_id, mut rx) = hub.register(1).unwrap();
        let (_other, mut theirs) = hub.register(2).unwrap();
        hub.broadcast_changed(1);
        let v: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(v["type"], "changed");
        assert!(theirs.try_recv().is_err());
    }

    #[test]
    fn ws_channel_errors_when_nobody_is_connected() {
        let hub = Arc::new(ClientHub::new());
        let ch = WsChannel::new(hub.clone());
        assert!(ch.deliver(1, "aki", &msg()).is_err());
        let (_id, mut rx) = hub.register(1).unwrap();
        ch.deliver(1, "aki", &msg()).unwrap();
        let text = rx.try_recv().unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["type"], "event");
        assert_eq!(v["title"], "Check-in");
        assert_eq!(v["urgency"], "high");
        assert_eq!(v["event_id"], 7);
        assert_eq!(v["conversation_id"], 3);

        let mut plain = msg();
        plain.conversation_id = None;
        ch.deliver(1, "aki", &plain).unwrap();
        let v: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert!(v["conversation_id"].is_null());
    }
    #[test]
    fn only_a_frame_the_client_chose_to_send_is_presence() {
        use axum::extract::ws::Message;
        assert!(is_presence(&Message::Text("hi".into())));
        assert!(is_presence(&Message::Binary(Vec::new().into())));
        assert!(!is_presence(&Message::Pong(Vec::new().into())));
        assert!(!is_presence(&Message::Ping(Vec::new().into())));
    }
}
