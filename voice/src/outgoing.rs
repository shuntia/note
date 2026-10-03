use note_voice_proto::CallBody;
use std::sync::mpsc;
use std::thread::JoinHandle;

/// Writes one call's outgoing frames in order on a thread of its own, so the
/// audio loop that hands them over never waits on the journal.
pub struct CallWriter {
    tx: mpsc::Sender<CallBody>,
    thread: JoinHandle<()>,
}

impl CallWriter {
    pub fn spawn(write: impl Fn(CallBody) + Send + 'static) -> std::io::Result<CallWriter> {
        let (tx, rx) = mpsc::channel::<CallBody>();
        let thread = std::thread::Builder::new().name("note-voice-out".into()).spawn(move || {
            for body in rx {
                write(body);
            }
        })?;
        Ok(CallWriter { tx, thread })
    }

    pub fn sender(&self) -> impl Fn(CallBody) + Send + Sync + 'static {
        let tx = self.tx.clone();
        move |body| {
            let _ = tx.send(body);
        }
    }

    /// Returns once every frame handed over is written; every sender must be dropped first.
    pub async fn close(self) {
        let CallWriter { tx, thread } = self;
        drop(tx);
        let _ = tokio::task::spawn_blocking(move || thread.join()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn frames_are_handed_over_at_once_and_written_in_order() {
        let written: Arc<Mutex<Vec<u64>>> = Arc::default();
        let log = written.clone();
        let writer = CallWriter::spawn(move |body| {
            std::thread::sleep(Duration::from_millis(20));
            if let CallBody::Played { reply } = body {
                log.lock().unwrap().push(reply);
            }
        })
        .unwrap();
        let send = writer.sender();
        let started = Instant::now();
        for reply in 1..=10 {
            send(CallBody::Played { reply });
        }
        assert!(started.elapsed() < Duration::from_millis(20), "handing over waited on the writes");
        drop(send);
        writer.close().await;
        assert_eq!(*written.lock().unwrap(), (1..=10).collect::<Vec<_>>());
    }
}
