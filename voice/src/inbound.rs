use crate::audio::playout::{Clip, Playout};
use crate::matrix::RoomEvent;
use crate::media::MediaIo;
use std::collections::HashMap;
use std::time::Duration;

/// An older event is a call that has likely ended.
const MAX_AGE_MS: i64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detect {
    Answer { room: String, mxid: String, key: String },
    Ignore,
}

/// Picks out the linked users' own calls from the room events. A device's
/// call membership starts a call only on its edge from inactive to active,
/// since Element X re-sends it while in a call; a ring counts once per event.
/// Events over 30 s old are recorded but never answered, which also covers
/// the state a cold start's first sync carries.
#[derive(Default)]
pub struct Detector {
    /// Whether each `(room, user, device)`'s call membership is set.
    active: HashMap<(String, String, String), bool>,
    answered_rings: HashMap<String, String>,
}

impl Detector {
    pub fn new(_cold_start: bool, _now_ms: i64) -> Self {
        Detector::default()
    }

    /// `links` are `(room_id, mxid)` pairs; `busy` reports a room with a call starting, ringing or live.
    /// A ringing outbound call counts as busy: its own ring sees the user's call member as the answer.
    pub fn on_event(
        &mut self,
        ev: &RoomEvent,
        links: &[(String, String)],
        busy: &dyn Fn(&str) -> bool,
        now_ms: i64,
    ) -> Detect {
        let (room, caller, key, ts_ms) = match ev {
            RoomEvent::CallMember { room, user, device, active, event_id, ts_ms } => {
                let at = (room.clone(), user.clone(), device.clone());
                let was_active = self.active.insert(at, *active).unwrap_or(false);
                if !*active || was_active {
                    return Detect::Ignore;
                }
                (room, user, event_id, *ts_ms)
            }
            RoomEvent::RingForBot { room, sender, event_id, ts_ms } => {
                if self.answered_rings.get(room) == Some(event_id) {
                    return Detect::Ignore;
                }
                (room, sender, event_id, *ts_ms)
            }
            _ => return Detect::Ignore,
        };
        if !links.iter().any(|(r, m)| r == room && m == caller) {
            return Detect::Ignore;
        }
        if ts_ms < now_ms - MAX_AGE_MS {
            return Detect::Ignore;
        }
        if busy(room) {
            return Detect::Ignore;
        }
        if matches!(ev, RoomEvent::RingForBot { .. }) {
            self.answered_rings.insert(room.clone(), key.clone());
        }
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

    fn left(event_id: &str, ts_ms: i64) -> RoomEvent {
        left_from("PHONE", event_id, ts_ms)
    }

    fn joined_from(device: &str, event_id: &str, ts_ms: i64) -> RoomEvent {
        RoomEvent::CallMember {
            room: "!dm:t".into(),
            user: "@aki:t".into(),
            device: device.into(),
            active: true,
            event_id: event_id.into(),
            ts_ms,
        }
    }

    fn left_from(device: &str, event_id: &str, ts_ms: i64) -> RoomEvent {
        RoomEvent::CallMember {
            room: "!dm:t".into(),
            user: "@aki:t".into(),
            device: device.into(),
            active: false,
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
        assert_eq!(d.on_event(&left("$3", NOW), &links(), &idle, NOW), Detect::Ignore);
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
        let mut fresh = Detector::new(true, NOW);
        assert_eq!(fresh.on_event(&member("!dm:t", "@aki:t", "$2", NOW - 5_000), &links(), &idle, NOW), answer("$2"));
        let mut warm = Detector::new(false, NOW);
        assert_eq!(
            warm.on_event(&member("!dm:t", "@aki:t", "$1", NOW - 60_000), &links(), &idle, NOW),
            Detect::Ignore,
            "a resumed sync's old call has likely ended too"
        );
        let old_ring =
            RoomEvent::RingForBot { room: "!dm:t".into(), sender: "@aki:t".into(), event_id: "$r".into(), ts_ms: NOW - 60_000 };
        assert_eq!(warm.on_event(&old_ring, &links(), &idle, NOW), Detect::Ignore);
    }

    #[test]
    fn the_same_attempt_is_answered_once() {
        let mut d = Detector::new(false, NOW);
        assert_eq!(d.on_event(&member("!dm:t", "@aki:t", "$1", NOW), &links(), &idle, NOW), answer("$1"));
        assert_eq!(d.on_event(&member("!dm:t", "@aki:t", "$1", NOW), &links(), &idle, NOW), Detect::Ignore);
        let ring = RoomEvent::RingForBot { room: "!dm:t".into(), sender: "@aki:t".into(), event_id: "$r".into(), ts_ms: NOW };
        assert_eq!(d.on_event(&ring, &links(), &idle, NOW), answer("$r"));
        assert_eq!(d.on_event(&ring, &links(), &idle, NOW), Detect::Ignore);
    }

    #[test]
    fn a_refresh_while_in_the_call_is_not_a_new_call() {
        let mut d = Detector::new(false, NOW);
        assert_eq!(d.on_event(&member("!dm:t", "@aki:t", "$1", NOW), &links(), &idle, NOW), answer("$1"));
        assert_eq!(d.on_event(&member("!dm:t", "@aki:t", "$2", NOW + 1_000), &links(), &idle, NOW), Detect::Ignore);
    }

    #[test]
    fn leaving_and_calling_again_is_answered_again() {
        let mut d = Detector::new(false, NOW);
        assert_eq!(d.on_event(&member("!dm:t", "@aki:t", "$1", NOW), &links(), &idle, NOW), answer("$1"));
        assert_eq!(d.on_event(&left("$2", NOW + 1_000), &links(), &idle, NOW), Detect::Ignore);
        assert_eq!(d.on_event(&member("!dm:t", "@aki:t", "$3", NOW + 2_000), &links(), &idle, NOW), answer("$3"));
    }

    #[test]
    fn a_user_already_in_a_call_at_a_cold_start_is_not_answered() {
        let mut d = Detector::new(true, NOW);
        assert_eq!(d.on_event(&member("!dm:t", "@aki:t", "$1", NOW - 120_000), &links(), &idle, NOW), Detect::Ignore);
        assert_eq!(
            d.on_event(&member("!dm:t", "@aki:t", "$2", NOW + 1_000), &links(), &idle, NOW),
            Detect::Ignore,
            "a refresh of the call already up"
        );
        assert_eq!(d.on_event(&left("$3", NOW + 2_000), &links(), &idle, NOW), Detect::Ignore);
        assert_eq!(d.on_event(&member("!dm:t", "@aki:t", "$4", NOW + 3_000), &links(), &idle, NOW), answer("$4"));
    }

    #[test]
    fn one_device_leaving_leaves_the_others_call_as_it_was() {
        let mut d = Detector::new(false, NOW);
        let busy = |room: &str| room == "!dm:t";
        assert_eq!(d.on_event(&joined_from("PHONE", "$1", NOW), &links(), &idle, NOW), answer("$1"));
        assert_eq!(d.on_event(&joined_from("LAPTOP", "$2", NOW), &links(), &busy, NOW), Detect::Ignore);
        assert_eq!(d.on_event(&left_from("PHONE", "$3", NOW), &links(), &idle, NOW), Detect::Ignore);
        assert_eq!(d.on_event(&joined_from("LAPTOP", "$4", NOW), &links(), &idle, NOW), Detect::Ignore, "a refresh");
        assert_eq!(d.on_event(&joined_from("PHONE", "$5", NOW), &links(), &idle, NOW), answer("$5"));
    }

    #[test]
    fn a_busy_room_is_not_answered_again() {
        let mut d = Detector::new(false, NOW);
        let busy = |room: &str| room == "!dm:t";
        assert_eq!(d.on_event(&member("!dm:t", "@aki:t", "$1", NOW), &links(), &busy, NOW), Detect::Ignore);
        assert_eq!(
            d.on_event(&member("!dm:t", "@aki:t", "$2", NOW + 1_000), &links(), &idle, NOW),
            Detect::Ignore,
            "the call that began while busy is still the same call"
        );
    }
}
