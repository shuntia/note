use super::{Channel, OutboundMessage};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Recording channel for tests; public for integration tests, mirroring
/// `providers::mock`.
pub struct MockChannel {
    name: &'static str,
    fail: AtomicBool,
    companion: AtomicBool,
    seen: Mutex<Vec<(i64, OutboundMessage)>>,
}

impl MockChannel {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            fail: AtomicBool::new(false),
            companion: AtomicBool::new(false),
            seen: Mutex::new(Vec::new()),
        }
    }

    pub fn set_fail(&self, fail: bool) {
        self.fail.store(fail, Ordering::Relaxed);
    }

    pub fn set_companion(&self, on: bool) {
        self.companion.store(on, Ordering::Relaxed);
    }

    pub fn seen(&self) -> Vec<(i64, OutboundMessage)> {
        self.seen.lock().unwrap().clone()
    }
}

impl Channel for MockChannel {
    fn name(&self) -> &'static str {
        self.name
    }

    fn companion(&self) -> bool {
        self.companion.load(Ordering::Relaxed)
    }

    fn deliver(&self, user_id: i64, _username: &str, msg: &OutboundMessage) -> anyhow::Result<()> {
        if self.fail.load(Ordering::Relaxed) {
            anyhow::bail!("mock channel set to fail");
        }
        self.seen.lock().unwrap().push((user_id, msg.clone()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::{Channel, OutboundMessage, Urgency};

    #[test]
    fn mock_records_and_fails_on_demand() {
        let ch = MockChannel::new("mock");
        let m = OutboundMessage {
            title: "t".into(),
            body: "b".into(),
            urgency: Urgency::Low,
            checkin: false,
            event_id: None,
            conversation_id: None,
            actions: Vec::new(),
        };
        ch.deliver(1, "aki", &m).unwrap();
        ch.set_fail(true);
        assert!(ch.deliver(1, "aki", &m).is_err());
        let seen = ch.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, 1);
        assert_eq!(seen[0].1.title, "t");
    }
}
