use crate::audio::playout::{Clip, Playout};
use crate::matrix::RoomEvent;
use crate::media::MediaIo;
use std::collections::HashMap;
use std::time::Duration;

/// On a cold start, older events are a call that has likely ended.
const COLD_START_MAX_AGE_MS: i64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detect {
    Answer { room: String, mxid: String, key: String },
    Ignore,
}

/// Picks out the linked users' own calls from the room events.
pub struct Detector {
    /// Events from before this are ignored.
    cutoff_ms: Option<i64>,
    answered: HashMap<String, String>,
}

impl Detector {
    pub fn new(cold_start: bool, now_ms: i64) -> Self {
        Detector { cutoff_ms: cold_start.then(|| now_ms - COLD_START_MAX_AGE_MS), answered: HashMap::new() }
    }

    /// `links` are `(room_id, mxid)` pairs; `busy` reports a room with a call starting, ringing or live.
    /// A ringing outbound call counts as busy: its own ring sees the user's call member as the answer.
    pub fn on_event(
        &mut self,
        ev: &RoomEvent,
        links: &[(String, String)],
        busy: &dyn Fn(&str) -> bool,
        _now_ms: i64,
    ) -> Detect {
        let (room, caller, key, ts_ms) = match ev {
            RoomEvent::CallMember { room, user, active: true, event_id, ts_ms, .. } => (room, user, event_id, *ts_ms),
            RoomEvent::RingForBot { room, sender, event_id, ts_ms } => (room, sender, event_id, *ts_ms),
            _ => return Detect::Ignore,
        };
        if !links.iter().any(|(r, m)| r == room && m == caller) {
            return Detect::Ignore;
        }
        if self.cutoff_ms.is_some_and(|cutoff| ts_ms < cutoff) {
            return Detect::Ignore;
        }
        if self.answered.get(room) == Some(key) || busy(room) {
            return Detect::Ignore;
        }
        self.answered.insert(room.clone(), key.clone());
        Detect::Answer { room: room.clone(), mxid: caller.clone(), key: key.clone() }
    }
}

/// Plays `clips` in real time, then leaves; stops early if the user leaves or the media fails.
pub async fn say_and_leave(media: &dyn MediaIo, clips: Vec<Vec<i16>>) {
    let mut playout = Playout::default();
    for pcm in clips {
        playout.push(Clip { reply: None, chars: 0, pcm });
    }
    let play = async {
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        while let Some(frame) = playout.next_frame() {
            tick.tick().await;
            if let Err(e) = media.send(&frame).await {
                eprintln!("voice: playing to the caller failed: {e:#}");
                return;
            }
        }
    };
    tokio::select! {
        () = play => {}
        _ = media.left() => {}
    }
    media.leave().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    fn links() -> Vec<(String, String)> {
        vec![("!dm:t".into(), "@aki:t".into())]
    }

    fn member(room: &str, user: &str, event_id: &str, ts_ms: i64) -> RoomEvent {
        RoomEvent::CallMember {
            room: room.into(),
            user: user.into(),
            device: "PHONE".into(),
            active: true,
            event_id: event_id.into(),
            ts_ms,
        }
    }

    fn idle(_: &str) -> bool {
        false
    }

    fn answer(event_id: &str) -> Detect {
        Detect::Answer { room: "!dm:t".into(), mxid: "@aki:t".into(), key: event_id.into() }
    }

    #[test]
    fn only_the_linked_user_in_the_linked_room_is_answered() {
        let mut d = Detector::new(false, NOW);
        assert_eq!(d.on_event(&member("!dm:t", "@eve:t", "$1", NOW), &links(), &idle, NOW), Detect::Ignore);
        assert_eq!(d.on_event(&member("!other:t", "@aki:t", "$2", NOW), &links(), &idle, NOW), Detect::Ignore);
        let left = RoomEvent::CallMember {
            room: "!dm:t".into(),
            user: "@aki:t".into(),
            device: "PHONE".into(),
            active: false,
            event_id: "$3".into(),
            ts_ms: NOW,
        };
        assert_eq!(d.on_event(&left, &links(), &idle, NOW), Detect::Ignore);
        assert_eq!(d.on_event(&member("!dm:t", "@aki:t", "$4", NOW), &links(), &idle, NOW), answer("$4"));
        let ring = RoomEvent::RingForBot { room: "!dm:t".into(), sender: "@aki:t".into(), event_id: "$5".into(), ts_ms: NOW };
        assert_eq!(d.on_event(&ring, &links(), &idle, NOW), answer("$5"));
        let stranger = RoomEvent::RingForBot { room: "!dm:t".into(), sender: "@eve:t".into(), event_id: "$6".into(), ts_ms: NOW };
        assert_eq!(d.on_event(&stranger, &links(), &idle, NOW), Detect::Ignore);
    }

    #[test]
    fn stale_call_members_after_a_restart_are_ignored() {
        let mut d = Detector::new(true, NOW);
        assert_eq!(d.on_event(&member("!dm:t", "@aki:t", "$1", NOW - 60_000), &links(), &idle, NOW), Detect::Ignore);
        assert_eq!(d.on_event(&member("!dm:t", "@aki:t", "$2", NOW - 5_000), &links(), &idle, NOW), answer("$2"));
        let mut warm = Detector::new(false, NOW);
        assert_eq!(
            warm.on_event(&member("!dm:t", "@aki:t", "$1", NOW - 60_000), &links(), &idle, NOW),
            answer("$1"),
            "a resumed sync has not seen the event before"
        );
    }

    #[test]
    fn the_same_attempt_is_answered_once() {
        let mut d = Detector::new(false, NOW);
        assert_eq!(d.on_event(&member("!dm:t", "@aki:t", "$1", NOW), &links(), &idle, NOW), answer("$1"));
        assert_eq!(d.on_event(&member("!dm:t", "@aki:t", "$1", NOW), &links(), &idle, NOW), Detect::Ignore);
    }

    #[test]
    fn a_busy_room_is_not_answered_again() {
        let mut d = Detector::new(false, NOW);
        let busy = |room: &str| room == "!dm:t";
        assert_eq!(d.on_event(&member("!dm:t", "@aki:t", "$1", NOW), &links(), &busy, NOW), Detect::Ignore);
        assert_eq!(
            d.on_event(&member("!dm:t", "@aki:t", "$1", NOW), &links(), &idle, NOW),
            answer("$1"),
            "an event ignored while busy is not counted as answered"
        );
    }
}
