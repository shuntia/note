use super::render::Item;
use note_voice_proto::Floor;
use std::time::Duration;

pub struct WakeConfig {
    pub settle: Duration,
    pub max_wakes: u32,
}

pub struct Queue {
    cfg: WakeConfig,
    items: Vec<Item>,
    user_speaking: bool,
    note_playing: bool,
    floor_free_since: Option<Duration>,
    wakes_in_a_row: u32,
}

pub enum Start {
    Heard,
    Wake,
}

impl Queue {
    pub fn new(cfg: WakeConfig, now: Duration) -> Self {
        Self {
            cfg,
            items: Vec::new(),
            user_speaking: false,
            note_playing: false,
            floor_free_since: Some(now),
            wakes_in_a_row: 0,
        }
    }

    pub fn push(&mut self, item: Item, _now: Duration) {
        self.items.push(item);
    }

    pub fn floor(&mut self, floor: Floor, now: Duration) {
        match floor {
            Floor::UserSpeaking => self.user_speaking = true,
            Floor::UserQuiet => self.user_speaking = false,
            Floor::Drained => self.note_playing = false,
        }
        self.refresh_floor(now);
    }

    /// Note's own reply started or stopped playing; playing holds the floor like the user speaking.
    pub fn note_playing(&mut self, playing: bool, now: Duration) {
        self.note_playing = playing;
        self.refresh_floor(now);
    }

    /// Called when no turn is in flight; if a turn should start, drains and returns its items.
    pub fn take_turn(&mut self, now: Duration) -> Option<(Start, Vec<Item>)> {
        if self.has_heard() {
            self.wakes_in_a_row = 0;
            return Some((Start::Heard, std::mem::take(&mut self.items)));
        }
        let settled = self
            .floor_free_since
            .is_some_and(|since| now.saturating_sub(since) >= self.cfg.settle);
        if self.items.is_empty() || !settled || self.wakes_in_a_row >= self.cfg.max_wakes {
            return None;
        }
        self.wakes_in_a_row += 1;
        Some((Start::Wake, std::mem::take(&mut self.items)))
    }

    /// The user's words started a turn outside `take_turn` (a promoted draft); resets the wake cap.
    pub fn heard(&mut self) {
        self.wakes_in_a_row = 0;
    }

    pub fn has_heard(&self) -> bool {
        self.items.iter().any(|item| matches!(item, Item::Heard(_)))
    }

    /// Drains the queued completions for a draft turn, leaving any `Heard` queued.
    pub fn snapshot(&mut self) -> Vec<Item> {
        let (heard, rest) = std::mem::take(&mut self.items)
            .into_iter()
            .partition(|item| matches!(item, Item::Heard(_)));
        self.items = heard;
        rest
    }

    /// Returns the drained items to the front (a retracted draft gives its snapshot back).
    pub fn give_back(&mut self, items: Vec<Item>) {
        self.items.splice(0..0, items);
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn refresh_floor(&mut self, now: Duration) {
        if !self.user_speaking && !self.note_playing {
            self.floor_free_since.get_or_insert(now);
        } else {
            self.floor_free_since = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::call::render::JobOutcome;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn cfg() -> WakeConfig {
        WakeConfig {
            settle: ms(600),
            max_wakes: 4,
        }
    }

    fn job_done(job: u32) -> Item {
        Item::Job {
            job,
            tool: "web_search".into(),
            outcome: JobOutcome::Done("ok".into()),
        }
    }

    fn job_ids(items: &[Item]) -> Vec<u32> {
        items
            .iter()
            .map(|item| match item {
                Item::Job { job, .. } => *job,
                other => panic!("expected a job, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn heard_starts_at_once_and_carries_completions() {
        let mut q = Queue::new(cfg(), ms(0));
        q.push(job_done(3), ms(10));
        q.floor(Floor::UserSpeaking, ms(20));
        q.push(Item::Heard("hi".into()), ms(900));
        let (start, items) = q.take_turn(ms(900)).unwrap();
        assert!(matches!(start, Start::Heard));
        assert_eq!(items.len(), 2);
        assert!(
            matches!(items[0], Item::Job { job: 3, .. }),
            "arrival order"
        );
        assert!(q.is_empty());
    }

    #[test]
    fn completions_wait_for_a_free_floor() {
        let mut q = Queue::new(cfg(), ms(0));
        q.floor(Floor::UserSpeaking, ms(0));
        q.push(job_done(1), ms(100));
        assert!(q.take_turn(ms(5000)).is_none(), "user still talking");
        q.floor(Floor::UserQuiet, ms(5000));
        assert!(q.take_turn(ms(5599)).is_none(), "settling");
        assert!(matches!(q.take_turn(ms(5600)), Some((Start::Wake, _))));
    }

    #[test]
    fn speech_during_the_settle_defers_to_the_users_turn() {
        let mut q = Queue::new(cfg(), ms(0));
        q.push(job_done(1), ms(0));
        q.floor(Floor::UserSpeaking, ms(300));
        assert!(q.take_turn(ms(700)).is_none());
        q.push(Item::Heard("ok".into()), ms(1500));
        assert!(matches!(q.take_turn(ms(1500)), Some((Start::Heard, items)) if items.len() == 2));
    }

    #[test]
    fn note_playing_holds_the_floor() {
        let mut q = Queue::new(cfg(), ms(0));
        q.note_playing(true, ms(0));
        q.push(job_done(1), ms(10));
        assert!(q.take_turn(ms(2000)).is_none(), "Note is still talking");
        q.note_playing(false, ms(2000));
        assert!(q.take_turn(ms(2599)).is_none(), "settling");
        assert!(matches!(q.take_turn(ms(2600)), Some((Start::Wake, _))));
    }

    #[test]
    fn a_barge_in_holds_the_floor_until_the_user_is_quiet() {
        let mut q = Queue::new(cfg(), ms(0));
        q.floor(Floor::UserSpeaking, ms(100));
        q.floor(Floor::Drained, ms(200));
        q.push(job_done(1), ms(300));
        assert!(q.take_turn(ms(5000)).is_none(), "the user is still talking");
        q.floor(Floor::UserQuiet, ms(5000));
        assert!(q.take_turn(ms(5599)).is_none(), "settling");
        assert!(matches!(q.take_turn(ms(5600)), Some((Start::Wake, _))));
    }

    #[test]
    fn a_drained_playout_frees_the_floor_like_note_stopping() {
        let mut q = Queue::new(cfg(), ms(0));
        q.note_playing(true, ms(0));
        q.push(job_done(1), ms(10));
        q.floor(Floor::Drained, ms(1000));
        assert!(q.take_turn(ms(1599)).is_none(), "settling");
        assert!(matches!(q.take_turn(ms(1600)), Some((Start::Wake, _))));
    }

    #[test]
    fn an_empty_queue_never_wakes() {
        let mut q = Queue::new(cfg(), ms(0));
        assert!(q.is_empty());
        assert!(q.take_turn(ms(10_000)).is_none());
    }

    #[test]
    fn wakes_are_capped_until_the_user_speaks() {
        let mut q = Queue::new(cfg(), ms(0));
        for i in 0..4 {
            q.push(job_done(i), ms(i as u64 * 1000));
            assert!(q.take_turn(ms(i as u64 * 1000 + 600)).is_some());
        }
        q.push(job_done(9), ms(10_000));
        assert!(q.take_turn(ms(20_000)).is_none(), "fifth wake refused");
        q.push(Item::Heard("yes".into()), ms(21_000));
        let (_, items) = q.take_turn(ms(21_000)).unwrap();
        assert_eq!(items.len(), 2, "the held completion rides along");
        q.push(job_done(10), ms(22_000));
        assert!(q.take_turn(ms(22_600)).is_some(), "the cap resets");
    }

    #[test]
    fn a_draft_snapshot_leaves_the_heard_queued() {
        let mut q = Queue::new(cfg(), ms(0));
        q.push(job_done(1), ms(0));
        q.push(Item::Heard("hm".into()), ms(10));
        q.push(job_done(2), ms(20));
        assert_eq!(job_ids(&q.snapshot()), vec![1, 2]);
        assert!(!q.is_empty());
        assert!(matches!(q.take_turn(ms(30)), Some((Start::Heard, items)) if items.len() == 1));
    }

    #[test]
    fn a_retracted_draft_gives_its_snapshot_back_in_front() {
        let mut q = Queue::new(cfg(), ms(0));
        q.push(job_done(1), ms(0));
        let snapshot = q.snapshot();
        assert_eq!(job_ids(&snapshot), vec![1]);
        assert!(q.is_empty());
        q.push(job_done(2), ms(100));
        q.give_back(snapshot);
        let (start, items) = q.take_turn(ms(1000)).unwrap();
        assert!(matches!(start, Start::Wake));
        assert_eq!(job_ids(&items), vec![1, 2]);
    }
}
