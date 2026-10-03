use std::collections::{HashMap, VecDeque};

pub const FRAME: usize = 480;

/// `reply` is `None` for a cue or a line; `chars` is the length of the text `pcm` speaks.
pub struct Clip {
    pub reply: Option<u64>,
    pub chars: u32,
    pub pcm: Vec<i16>,
}

#[derive(Default)]
pub struct Playout {
    queue: VecDeque<Clip>,
    cursor: usize,
    paused: bool,
    heard: HashMap<u64, u32>,
    finished: Vec<u64>,
}

impl Playout {
    pub fn push(&mut self, clip: Clip) {
        self.queue.push_back(clip);
    }

    /// Pushes ahead of everything not yet started (the heard cue goes before the reply).
    pub fn push_front(&mut self, clip: Clip) {
        let at = usize::from(self.cursor > 0).min(self.queue.len());
        self.queue.insert(at, clip);
    }

    /// The next 10 ms frame, or None when paused or empty. Pads the last frame of a clip with silence.
    pub fn next_frame(&mut self) -> Option<[i16; FRAME]> {
        if self.paused {
            return None;
        }
        while self.queue.front().is_some_and(|c| c.pcm.is_empty()) {
            self.finish_front();
        }
        let clip = self.queue.front()?;
        let end = (self.cursor + FRAME).min(clip.pcm.len());
        let mut frame = [0; FRAME];
        frame[..end - self.cursor].copy_from_slice(&clip.pcm[self.cursor..end]);
        self.cursor = end;
        if end == clip.pcm.len() {
            self.finish_front();
        }
        Some(frame)
    }

    pub fn pause(&mut self) {
        self.paused = true;
    }

    pub fn resume(&mut self) {
        self.paused = false;
    }

    /// Drops everything queued; returns, per reply cut, the characters actually played.
    pub fn flush(&mut self) -> Vec<(u64, u32)> {
        let mut cut: Vec<(u64, u32)> = Vec::new();
        for (i, clip) in self.queue.iter().enumerate() {
            let Some(reply) = clip.reply else { continue };
            if cut.iter().any(|&(r, _)| r == reply) {
                continue;
            }
            let mut heard = self.heard.get(&reply).copied().unwrap_or(0);
            if i == 0 {
                heard += (u64::from(clip.chars) * self.cursor as u64 / clip.pcm.len() as u64) as u32;
            }
            cut.push((reply, heard));
        }
        self.queue.clear();
        self.cursor = 0;
        self.heard.clear();
        cut
    }

    /// Not paused and not empty.
    pub fn is_playing(&self) -> bool {
        !self.paused && !self.queue.is_empty()
    }

    /// Replies whose every clip has finished playing since the last call.
    pub fn take_finished(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.finished)
    }

    /// Characters of `reply`'s finished clips since the last flush.
    pub fn heard(&self, reply: u64) -> u32 {
        self.heard.get(&reply).copied().unwrap_or(0)
    }

    pub fn holds(&self, reply: u64) -> bool {
        self.queue.iter().any(|c| c.reply == Some(reply))
    }

    fn finish_front(&mut self) {
        let Some(clip) = self.queue.pop_front() else { return };
        self.cursor = 0;
        let Some(reply) = clip.reply else { return };
        *self.heard.entry(reply).or_default() += clip.chars;
        if !self.holds(reply) && !self.finished.contains(&reply) {
            self.finished.push(reply);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flush_reports_what_was_heard() {
        let mut p = Playout::default();
        p.push(Clip { reply: Some(4), chars: 10, pcm: vec![1; FRAME * 10] });
        p.push(Clip { reply: Some(4), chars: 20, pcm: vec![1; FRAME * 10] });
        for _ in 0..15 {
            p.next_frame().unwrap();
        }
        assert_eq!(p.flush(), vec![(4, 20)]);
        assert!(p.next_frame().is_none());
    }

    #[test]
    fn pause_holds_the_position_and_resume_continues() {
        let mut p = Playout::default();
        p.push(Clip { reply: Some(1), chars: 5, pcm: (0..FRAME as i16 * 3).collect() });
        let first = p.next_frame().unwrap();
        p.pause();
        assert!(p.next_frame().is_none());
        p.resume();
        let second = p.next_frame().unwrap();
        assert_eq!(second[0], first[FRAME - 1] + 1);
    }

    #[test]
    fn the_heard_cue_goes_ahead_of_a_reply_not_yet_started() {
        let mut p = Playout::default();
        p.push(Clip { reply: Some(2), chars: 3, pcm: vec![7; FRAME] });
        p.push_front(Clip { reply: None, chars: 0, pcm: vec![9; FRAME] });
        assert_eq!(p.next_frame().unwrap()[0], 9);
        assert_eq!(p.next_frame().unwrap()[0], 7);
        assert_eq!(p.take_finished(), vec![2]);
    }
}
