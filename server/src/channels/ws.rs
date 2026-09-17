use super::{Channel, OutboundMessage};
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
        let mut conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
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
            .unwrap_or_else(|e| e.into_inner())
            .get(&user_id)
            .is_some_and(|v| v.len() >= MAX_PER_USER)
    }

    pub fn unregister(&self, user_id: i64, conn_id: u64) {
        let mut conns = self.conns.lock().unwrap();
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
        let mut conns = self.conns.lock().unwrap();
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
            event_id: Some(7),
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
    }
}
