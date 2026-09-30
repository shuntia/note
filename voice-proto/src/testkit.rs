use crate::codec::{read_frame, write_frame};
use crate::frame::*;
use crate::peer::{BoxFuture, Handler, PeerConfig};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

/// Short timings so the suites run in well under a second per case.
pub fn fast(role: Role) -> PeerConfig {
    PeerConfig {
        role,
        heartbeat: Duration::from_millis(50),
        missed_pongs: 3,
        hello_timeout: Duration::from_millis(500),
        request_timeout: Duration::from_millis(400),
    }
}

pub async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for: {what}");
}

type Answer = Box<dyn Fn(Request) -> Result<Reply, Refusal> + Send + Sync>;

/// A handler that keeps every applied frame, standing in for either side.
pub struct Recording {
    applied: Mutex<HashMap<String, u64>>,
    pub seen: Mutex<Vec<(String, u64, CallBody)>>,
    pub requests: Mutex<Vec<Request>>,
    pub acks: Mutex<HashMap<String, u64>>,
    answer: Mutex<Answer>,
}

impl Default for Recording {
    fn default() -> Self {
        Self {
            applied: Mutex::default(),
            seen: Mutex::default(),
            requests: Mutex::default(),
            acks: Mutex::default(),
            answer: Mutex::new(Box::new(|_| Ok(Reply::Done))),
        }
    }
}

impl Recording {
    pub fn answer_with(&self, f: impl Fn(Request) -> Result<Reply, Refusal> + Send + Sync + 'static) {
        *crate::lock(&self.answer) = Box::new(f);
    }

    pub fn seqs(&self, call_id: &str) -> Vec<u64> {
        crate::lock(&self.seen).iter().filter(|(c, _, _)| c == call_id).map(|(_, s, _)| *s).collect()
    }

    pub fn bodies(&self, call_id: &str) -> Vec<CallBody> {
        crate::lock(&self.seen).iter().filter(|(c, _, _)| c == call_id).map(|(_, _, b)| b.clone()).collect()
    }
}

impl Handler for Recording {
    fn applied(&self, call_id: &str) -> u64 {
        crate::lock(&self.applied).get(call_id).copied().unwrap_or(0)
    }

    fn apply(&self, call_id: &str, seq: u64, body: CallBody) -> Result<(), String> {
        crate::lock(&self.seen).push((call_id.to_string(), seq, body));
        crate::lock(&self.applied).insert(call_id.to_string(), seq);
        Ok(())
    }

    fn request(&self, body: Request) -> BoxFuture<Result<Reply, Refusal>> {
        crate::lock(&self.requests).push(body.clone());
        let result = (crate::lock(&self.answer))(body);
        Box::pin(async move { result })
    }

    fn acked(&self, call_id: &str, upto: u64) {
        crate::lock(&self.acks).insert(call_id.to_string(), upto);
    }
}

pub struct Faults {
    pub duplicate_calls: AtomicBool,
    pub drop_acks: AtomicBool,
    /// Swallows every frame in both directions without closing anything.
    pub hold: AtomicBool,
    cut_after: AtomicU64,
    kick: watch::Sender<u64>,
}

impl Faults {
    /// Closes the live connection once `n` more call frames have passed.
    pub fn cut_after_calls(&self, n: u64) {
        self.cut_after.store(n, SeqCst);
    }

    pub fn cut(&self) {
        self.kick.send_modify(|g| *g += 1);
    }
}

/// Sits between the two sides, forwarding decoded frames and breaking the
/// link on demand.
pub struct FaultProxy {
    pub faults: Arc<Faults>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FaultProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FaultProxy {
    pub async fn start(listen: PathBuf, target: PathBuf) -> std::io::Result<FaultProxy> {
        let listener = UnixListener::bind(&listen)?;
        let faults = Arc::new(Faults {
            duplicate_calls: AtomicBool::new(false),
            drop_acks: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            cut_after: AtomicU64::new(0),
            kick: watch::channel(0).0,
        });
        let f = faults.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else { continue };
                let Ok(server) = UnixStream::connect(&target).await else { continue };
                tokio::spawn(pump_pair(client, server, f.clone()));
            }
        });
        Ok(FaultProxy { faults, task })
    }
}

async fn pump_pair(a: UnixStream, b: UnixStream, f: Arc<Faults>) {
    let mut kick = f.kick.subscribe();
    kick.borrow_and_update();
    let (ar, aw) = a.into_split();
    let (br, bw) = b.into_split();
    let calls = Arc::new(AtomicU64::new(0));
    tokio::select! {
        _ = pump(ar, bw, f.clone(), calls.clone()) => {}
        _ = pump(br, aw, f.clone(), calls) => {}
        _ = kick.changed() => {}
    }
}

async fn pump(mut r: OwnedReadHalf, mut w: OwnedWriteHalf, f: Arc<Faults>, calls: Arc<AtomicU64>) {
    while let Ok(Some(frame)) = read_frame(&mut r).await {
        if f.hold.load(SeqCst) {
            continue;
        }
        match &frame {
            Frame::Ack { .. } if f.drop_acks.load(SeqCst) => continue,
            Frame::Call { .. } => {
                let n = calls.fetch_add(1, SeqCst) + 1;
                let limit = f.cut_after.load(SeqCst);
                if limit != 0 && n > limit {
                    f.cut_after.store(0, SeqCst);
                    return;
                }
                if f.duplicate_calls.load(SeqCst) && write_frame(&mut w, &frame).await.is_err() {
                    return;
                }
            }
            _ => {}
        }
        if write_frame(&mut w, &frame).await.is_err() {
            return;
        }
    }
}
