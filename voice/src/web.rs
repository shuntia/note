use crate::media::{Gone, MediaIo, Rechunker, OUT_FRAME};
use note_voice_proto::{LiveState, Media, Pcm};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::task::JoinHandle;

pub const OUT_RATE: u32 = 48_000;
/// Outbound frames held ahead of real time, as `LiveKit`'s source holds 100 ms.
const AHEAD: usize = 10;
const FRAME_TIME: Duration = Duration::from_millis(10);

/// Runs under `WebMedia`'s queue lock on a runtime worker: it must not block or call back into `WebMedia`.
pub type Emit = Arc<dyn Fn(Media) + Send + Sync>;

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Outbound {
    queue: std::sync::Mutex<VecDeque<[i16; OUT_FRAME]>>,
    space: Notify,
}

/// A web call's media: the caller's audio comes in through its `WebFeed`, Note's voice leaves
/// through `emit` one 10 ms frame every 10 ms.
pub struct WebMedia {
    frames: Mutex<mpsc::Receiver<Vec<f32>>>,
    out: Arc<Outbound>,
    emit: Emit,
    pacer: JoinHandle<()>,
}

pub struct WebFeed {
    tx: mpsc::Sender<Vec<f32>>,
    chunks: std::sync::Mutex<Rechunker>,
}

impl WebFeed {
    /// Audio a session is not keeping up with is dropped.
    pub fn push(&self, pcm: &[i16]) {
        let chunks = lock(&self.chunks).push(pcm);
        for chunk in chunks {
            let _ = self.tx.try_send(chunk);
        }
    }
}

pub fn web_media(emit: Emit) -> (WebMedia, WebFeed) {
    let (tx, rx) = mpsc::channel(50);
    let out = Arc::new(Outbound { queue: std::sync::Mutex::default(), space: Notify::new() });
    let pacer = tokio::spawn(pace(out.clone(), emit.clone()));
    (
        WebMedia { frames: Mutex::new(rx), out, emit, pacer },
        WebFeed { tx, chunks: std::sync::Mutex::default() },
    )
}

/// Emits under the queue's lock, so a `clear` never lets a dropped frame out after its `Flush`.
async fn pace(out: Arc<Outbound>, emit: Emit) {
    let mut tick = tokio::time::interval(FRAME_TIME);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
    loop {
        tick.tick().await;
        let mut queue = lock(&out.queue);
        if let Some(frame) = queue.pop_front() {
            emit(Media::AudioOut { pcm: Pcm(frame.to_vec()), rate: OUT_RATE });
            out.space.notify_waiters();
        }
    }
}

impl Drop for WebMedia {
    fn drop(&mut self) {
        self.pacer.abort();
    }
}

#[async_trait::async_trait]
impl MediaIo for WebMedia {
    async fn recv(&self) -> Option<Vec<f32>> {
        self.frames.lock().await.recv().await
    }

    async fn send(&self, frame: &[i16; OUT_FRAME]) -> anyhow::Result<()> {
        loop {
            let space = self.out.space.notified();
            {
                let mut queue = lock(&self.out.queue);
                if queue.len() < AHEAD {
                    queue.push_back(*frame);
                    return Ok(());
                }
            }
            space.await;
        }
    }

    fn clear(&self) {
        let mut queue = lock(&self.out.queue);
        queue.clear();
        (self.emit)(Media::Flush);
        self.out.space.notify_waiters();
    }

    /// The browser's going is Note's to tell, with a `HangUp`.
    async fn left(&self) -> Gone {
        std::future::pending().await
    }

    async fn leave(&self) {
        self.pacer.abort();
    }

    fn show(&self, state: LiveState) {
        (self.emit)(Media::State { state });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recording() -> (Emit, Arc<std::sync::Mutex<Vec<Media>>>) {
        let log: Arc<std::sync::Mutex<Vec<Media>>> = Arc::default();
        let sink = log.clone();
        (Arc::new(move |m| sink.lock().unwrap().push(m)), log)
    }

    fn firsts_out(log: &std::sync::Mutex<Vec<Media>>) -> Vec<i16> {
        log.lock()
            .unwrap()
            .iter()
            .filter_map(|m| match m {
                Media::AudioOut { pcm, .. } => Some(pcm.0[0]),
                _ => None,
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn the_callers_audio_reaches_recv_as_10_ms_frames() {
        let (emit, _) = recording();
        let (media, feed) = web_media(emit);
        feed.push(&(0..320).map(|i| i as i16).collect::<Vec<_>>());
        let a = media.recv().await.unwrap();
        let b = media.recv().await.unwrap();
        assert_eq!((a.len(), b.len()), (160, 160));
        assert_eq!(b[0], 160.0 / 32768.0);
    }

    #[tokio::test(start_paused = true)]
    async fn notes_voice_leaves_in_real_time_and_in_order() {
        let (emit, log) = recording();
        let (media, _feed) = web_media(emit);
        let media = Arc::new(media);
        let sender = {
            let m = media.clone();
            tokio::spawn(async move {
                for i in 0..30i16 {
                    m.send(&[i; OUT_FRAME]).await.unwrap();
                }
            })
        };
        tokio::time::sleep(Duration::from_millis(105)).await;
        let early = firsts_out(&log).len();
        assert!((9..=12).contains(&early), "{early} frames left in 105 ms");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(sender.is_finished());
        assert_eq!(firsts_out(&log), (0..30).collect::<Vec<i16>>());
        assert!(log
            .lock()
            .unwrap()
            .iter()
            .all(|m| matches!(m, Media::AudioOut { pcm, rate: OUT_RATE } if pcm.0.len() == OUT_FRAME)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_buffer_holds_the_sender_until_a_frame_leaves() {
        let (emit, log) = recording();
        let (media, _feed) = web_media(emit);
        for _ in 0..AHEAD {
            media.send(&[0; OUT_FRAME]).await.unwrap();
        }
        assert!(firsts_out(&log).is_empty());
        media.send(&[0; OUT_FRAME]).await.unwrap();
        assert!(!firsts_out(&log).is_empty(), "the frame past the buffer waited for one to leave");
    }

    #[tokio::test(start_paused = true)]
    async fn clear_drops_what_is_queued_and_tells_the_browser() {
        let (emit, log) = recording();
        let (media, _feed) = web_media(emit);
        for i in 0..AHEAD as i16 {
            media.send(&[i; OUT_FRAME]).await.unwrap();
        }
        media.clear();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(log.lock().unwrap().contains(&Media::Flush));
        assert!(firsts_out(&log).len() <= 1, "at most the frame already leaving");
    }

    #[tokio::test(start_paused = true)]
    async fn the_calls_state_goes_to_the_browser() {
        let (emit, log) = recording();
        let (media, _feed) = web_media(emit);
        media.show(LiveState::Thinking);
        assert_eq!(*log.lock().unwrap(), vec![Media::State { state: LiveState::Thinking }]);
    }
}
