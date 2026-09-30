use crate::frame::CallBody;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::{Arc, Mutex};

/// Durable store of the call frames this side sent and the peer has not yet
/// acknowledged. `append` must be durable before it returns.
pub trait Outbox: Send {
    /// Stores `body` as the next frame of `call_id` and returns its seq; the
    /// first is 1 and numbering never restarts while the call is known.
    fn append(&mut self, call_id: &str, body: &CallBody) -> io::Result<u64>;
    fn unacked(&self, call_id: &str, after: u64) -> io::Result<Vec<(u64, CallBody)>>;
    /// Drops every frame of `call_id` up to and including `upto`.
    fn ack(&mut self, call_id: &str, upto: u64) -> io::Result<()>;
    fn pending_calls(&self) -> io::Result<Vec<String>>;
    fn forget(&mut self, call_id: &str) -> io::Result<()>;
}

#[derive(Default)]
struct MemCall {
    last: u64,
    frames: BTreeMap<u64, CallBody>,
}

#[derive(Default)]
pub struct MemOutbox {
    calls: HashMap<String, MemCall>,
}

impl Outbox for MemOutbox {
    fn append(&mut self, call_id: &str, body: &CallBody) -> io::Result<u64> {
        let call = self.calls.entry(call_id.to_string()).or_default();
        call.last += 1;
        call.frames.insert(call.last, body.clone());
        Ok(call.last)
    }

    fn unacked(&self, call_id: &str, after: u64) -> io::Result<Vec<(u64, CallBody)>> {
        Ok(self
            .calls
            .get(call_id)
            .map(|c| c.frames.range(after.saturating_add(1)..).map(|(s, b)| (*s, b.clone())).collect())
            .unwrap_or_default())
    }

    fn ack(&mut self, call_id: &str, upto: u64) -> io::Result<()> {
        if let Some(c) = self.calls.get_mut(call_id) {
            c.frames.retain(|s, _| *s > upto);
        }
        Ok(())
    }

    fn pending_calls(&self) -> io::Result<Vec<String>> {
        let mut ids: Vec<String> =
            self.calls.iter().filter(|(_, c)| !c.frames.is_empty()).map(|(id, _)| id.clone()).collect();
        ids.sort();
        Ok(ids)
    }

    fn forget(&mut self, call_id: &str) -> io::Result<()> {
        self.calls.remove(call_id);
        Ok(())
    }
}

/// Lets tests keep a handle on an outbox a `Peer` owns, the way a restarted
/// process reopens the same journal.
impl Outbox for Arc<Mutex<MemOutbox>> {
    fn append(&mut self, call_id: &str, body: &CallBody) -> io::Result<u64> {
        crate::lock(self).append(call_id, body)
    }
    fn unacked(&self, call_id: &str, after: u64) -> io::Result<Vec<(u64, CallBody)>> {
        crate::lock(self).unacked(call_id, after)
    }
    fn ack(&mut self, call_id: &str, upto: u64) -> io::Result<()> {
        crate::lock(self).ack(call_id, upto)
    }
    fn pending_calls(&self) -> io::Result<Vec<String>> {
        crate::lock(self).pending_calls()
    }
    fn forget(&mut self, call_id: &str) -> io::Result<()> {
        crate::lock(self).forget(call_id)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Arrival {
    Apply,
    Duplicate,
    Gap,
}

pub fn classify(applied: u64, seq: u64) -> Arrival {
    if seq <= applied {
        Arrival::Duplicate
    } else if seq == applied + 1 {
        Arrival::Apply
    } else {
        Arrival::Gap
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn seqs_start_at_one_and_survive_acks() {
        let mut o = MemOutbox::default();
        assert_eq!(o.append("c", &CallBody::Ringing).unwrap(), 1);
        assert_eq!(o.append("c", &CallBody::Ended).unwrap(), 2);
        o.ack("c", 2).unwrap();
        assert!(o.unacked("c", 0).unwrap().is_empty());
        assert!(o.pending_calls().unwrap().is_empty());
        assert_eq!(o.append("c", &CallBody::HangUp).unwrap(), 3, "numbering never restarts");
    }

    #[test]
    fn unacked_is_ordered_and_respects_after() {
        let mut o = MemOutbox::default();
        for _ in 0..5 {
            o.append("c", &CallBody::Ringing).unwrap();
        }
        o.ack("c", 2).unwrap();
        let seqs: Vec<u64> = o.unacked("c", 0).unwrap().into_iter().map(|(s, _)| s).collect();
        assert_eq!(seqs, vec![3, 4, 5]);
        let seqs: Vec<u64> = o.unacked("c", 4).unwrap().into_iter().map(|(s, _)| s).collect();
        assert_eq!(seqs, vec![5]);
    }

    #[test]
    fn unacked_after_the_last_possible_seq_is_empty() {
        let mut o = MemOutbox::default();
        o.append("c", &CallBody::Ringing).unwrap();
        assert!(o.unacked("c", u64::MAX).unwrap().is_empty());
    }

    #[test]
    fn forget_drops_the_call_entirely() {
        let mut o = MemOutbox::default();
        o.append("c", &CallBody::Ringing).unwrap();
        o.forget("c").unwrap();
        assert!(o.pending_calls().unwrap().is_empty());
        assert_eq!(o.append("c", &CallBody::Ringing).unwrap(), 1);
    }

    #[test]
    fn classify_names_every_arrival() {
        assert_eq!(classify(0, 1), Arrival::Apply);
        assert_eq!(classify(3, 4), Arrival::Apply);
        assert_eq!(classify(3, 3), Arrival::Duplicate);
        assert_eq!(classify(3, 1), Arrival::Duplicate);
        assert_eq!(classify(3, 5), Arrival::Gap);
    }

    proptest! {
        /// Any mix of duplicates and replays, fed through `classify`, applies
        /// each seq exactly once and in order.
        #[test]
        fn classify_applies_each_seq_once(order in prop::collection::vec(1u64..20, 1..200)) {
            let mut applied = 0u64;
            let mut seen = Vec::new();
            for seq in order {
                if classify(applied, seq) == Arrival::Apply {
                    applied = seq;
                    seen.push(seq);
                }
            }
            let expected: Vec<u64> = (1..=applied).collect();
            prop_assert_eq!(seen, expected);
        }
    }
}
