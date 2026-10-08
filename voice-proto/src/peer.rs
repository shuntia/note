use crate::codec::{read_frame, write_frame, CodecError};
use crate::frame::{CallBody, Request, Reply, Refusal, Role, Frame, Dir, RefusalCode, PROTO_VERSION, valid_call_id, Media};
use crate::stream::{classify, Arrival, Outbox};
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, watch};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// What each side does with what arrives. `apply` must record `seq` as
/// applied in the same durable step as the frame's effect. An `Err` from it
/// drops the connection so the frame is redelivered, so it is for storage
/// failures only: a frame whose content makes no sense is recorded as applied
/// and ignored. `apply` is only called with `seq == applied(call_id) + 1`, one
/// call at a time across every connection of the peer.
pub trait Handler: Send + Sync + 'static {
    fn applied(&self, call_id: &str) -> u64;
    fn apply(&self, call_id: &str, seq: u64, body: CallBody) -> Result<(), String>;
    fn request(&self, body: Request) -> BoxFuture<Result<Reply, Refusal>>;
    fn acked(&self, _call_id: &str, _upto: u64) {}
    /// Reports the link's level, not an edge: `true` repeats when a
    /// connection is replaced.
    fn link_changed(&self, _up: bool) {}
    /// Called on the connection's read loop, so it hands off and never blocks.
    fn media(&self, _call_id: &str, _body: Media) {}
}

#[derive(Debug, Clone)]
pub struct PeerConfig {
    pub role: Role,
    pub heartbeat: Duration,
    pub missed_pongs: u32,
    pub hello_timeout: Duration,
    pub request_timeout: Duration,
}

impl PeerConfig {
    pub fn new(role: Role) -> Self {
        Self {
            role,
            heartbeat: Duration::from_secs(1),
            missed_pongs: 3,
            hello_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug)]
pub enum Disconnect {
    Eof,
    Codec(String),
    HelloRefused(String),
    HeartbeatLost,
    Protocol(String),
}

struct Shared {
    outbox: Box<dyn Outbox>,
    current: Option<mpsc::UnboundedSender<Frame>>,
}

struct Inner {
    cfg: PeerConfig,
    instance: String,
    out_dir: Dir,
    handler: Arc<dyn Handler>,
    shared: Mutex<Shared>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Reply, Refusal>>>>,
    /// Held across `applied` and `apply`, so an apply still running for a
    /// dropped connection commits before the next connection checks its seq.
    apply_gate: Mutex<()>,
    next_request: AtomicU64,
    generation: AtomicU64,
    up: watch::Sender<bool>,
}

#[derive(Clone)]
pub struct Peer {
    inner: Arc<Inner>,
}

fn instance_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{}-{nanos}", std::process::id())
}

impl Peer {
    pub fn new(cfg: PeerConfig, out_dir: Dir, handler: Arc<dyn Handler>, outbox: Box<dyn Outbox>) -> Peer {
        Peer {
            inner: Arc::new(Inner {
                cfg,
                instance: instance_id(),
                out_dir,
                handler,
                shared: Mutex::new(Shared { outbox, current: None }),
                pending: Mutex::new(HashMap::new()),
                apply_gate: Mutex::new(()),
                next_request: AtomicU64::new(0),
                generation: AtomicU64::new(0),
                up: watch::channel(false).0,
            }),
        }
    }

    pub fn is_up(&self) -> bool {
        *self.inner.up.borrow()
    }

    pub fn up_watch(&self) -> watch::Receiver<bool> {
        self.inner.up.subscribe()
    }

    pub fn pending_calls(&self) -> Vec<String> {
        crate::lock(&self.inner.shared).outbox.pending_calls().unwrap_or_default()
    }

    pub fn forget(&self, call_id: &str) -> io::Result<()> {
        crate::lock(&self.inner.shared).outbox.forget(call_id)
    }

    /// Stores the frame durably and sends it if the link is up. It is resent
    /// on every reconnect until acknowledged. Never call while holding a lock
    /// the outbox takes (Note's DB guard): an arriving ack holds this peer's
    /// lock and then enters the outbox.
    pub fn send_call(&self, call_id: &str, body: CallBody) -> io::Result<u64> {
        let mut sh = crate::lock(&self.inner.shared);
        let seq = sh.outbox.append(call_id, &body)?;
        if let Some(tx) = &sh.current {
            let _ = tx.send(Frame::Call { call_id: call_id.to_string(), dir: self.inner.out_dir, seq, body });
        }
        Ok(seq)
    }

    /// Sends now if the link is up; true when it was handed to the connection.
    pub fn send_media(&self, call_id: &str, body: Media) -> bool {
        let sh = crate::lock(&self.inner.shared);
        sh.current.as_ref().is_some_and(|tx| {
            tx.send(Frame::Media { call_id: call_id.to_string(), dir: self.inner.out_dir, body }).is_ok()
        })
    }

    pub async fn request(&self, body: Request) -> Result<Reply, Refusal> {
        let down = || Refusal::new(RefusalCode::LinkDown, "the voice link is down");
        let Some(tx) = crate::lock(&self.inner.shared).current.clone() else {
            return Err(down());
        };
        let id = self.inner.next_request.fetch_add(1, SeqCst) + 1;
        let (reply_tx, reply_rx) = oneshot::channel();
        crate::lock(&self.inner.pending).insert(id, reply_tx);
        if tx.send(Frame::Request { id, body }).is_err() {
            crate::lock(&self.inner.pending).remove(&id);
            return Err(down());
        }
        match tokio::time::timeout(self.inner.cfg.request_timeout, reply_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(down()),
            Err(_) => {
                crate::lock(&self.inner.pending).remove(&id);
                Err(Refusal::new(RefusalCode::Timeout, "no answer in time"))
            }
        }
    }

    /// Serves one connection until it drops and says why.
    pub async fn serve(&self, stream: UnixStream) -> Disconnect {
        let generation = self.inner.generation.fetch_add(1, SeqCst) + 1;
        // A guard, not a trailing statement: whether the hello fails or the
        // serve future is dropped by an abort, the link is marked down.
        let _teardown = Teardown { inner: self.inner.clone(), generation };
        let (mut rd, mut wr) = stream.into_split();
        let hello = Frame::Hello {
            proto: PROTO_VERSION,
            role: self.inner.cfg.role,
            instance: self.inner.instance.clone(),
        };
        if let Err(e) = write_frame(&mut wr, &hello).await {
            return Disconnect::Codec(e.to_string());
        }
        match tokio::time::timeout(self.inner.cfg.hello_timeout, read_frame(&mut rd)).await {
            Err(_) => return Disconnect::HelloRefused("no hello in time".into()),
            Ok(Ok(Some(Frame::Hello { proto, role, .. }))) => {
                if proto != PROTO_VERSION {
                    return Disconnect::HelloRefused(format!(
                        "peer speaks protocol {proto}, this side {PROTO_VERSION}"
                    ));
                }
                if role == self.inner.cfg.role {
                    return Disconnect::HelloRefused("peer claims this side's role".into());
                }
            }
            Ok(Ok(Some(other))) => return Disconnect::Protocol(format!("expected hello, got {other:?}")),
            Ok(Ok(None)) => return Disconnect::Eof,
            Ok(Err(e)) => return Disconnect::Codec(e.to_string()),
        }

        let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();
        if let Err(d) = self.install(generation, &tx) {
            return d;
        }
        self.inner.up.send_replace(true);
        self.inner.handler.link_changed(true);

        let _writer = AbortOnDrop(Some(tokio::spawn(async move {
            while let Some(f) = rx.recv().await {
                if write_frame(&mut wr, &f).await.is_err() {
                    break;
                }
            }
        })));
        let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Result<Option<Frame>, CodecError>>();
        let _reader = AbortOnDrop(Some(tokio::spawn(async move {
            loop {
                let got = read_frame(&mut rd).await;
                let stop = !matches!(got, Ok(Some(_)));
                if in_tx.send(got).is_err() || stop {
                    break;
                }
            }
        })));

        let (calls_tx, calls_rx) = mpsc::unbounded_channel::<Arriving>();
        let (failed_tx, failed_rx) = oneshot::channel::<Disconnect>();
        let _applier = AbortOnDrop(Some(tokio::spawn(apply_in_order(
            self.inner.clone(),
            calls_rx,
            tx.clone(),
            failed_tx,
        ))));

        let conn = Conn { tx, calls: calls_tx };
        self.run(&mut in_rx, failed_rx, &conn).await
    }

    /// Queues every unacknowledged frame on `tx` and makes it the live sender,
    /// under one lock, so a frame appended meanwhile is neither missed nor sent
    /// ahead of an older one.
    fn install(&self, generation: u64, tx: &mpsc::UnboundedSender<Frame>) -> Result<(), Disconnect> {
        let mut sh = crate::lock(&self.inner.shared);
        if self.inner.generation.load(SeqCst) != generation {
            return Err(Disconnect::Protocol("superseded".into()));
        }
        let unreadable = |e: io::Error| Disconnect::Protocol(format!("outbox unreadable: {e}"));
        for call_id in sh.outbox.pending_calls().map_err(unreadable)? {
            for (seq, body) in sh.outbox.unacked(&call_id, 0).map_err(unreadable)? {
                let _ = tx.send(Frame::Call { call_id: call_id.clone(), dir: self.inner.out_dir, seq, body });
            }
        }
        sh.current = Some(tx.clone());
        Ok(())
    }

    async fn run(
        &self,
        incoming: &mut mpsc::UnboundedReceiver<Result<Option<Frame>, CodecError>>,
        mut failed: oneshot::Receiver<Disconnect>,
        conn: &Conn,
    ) -> Disconnect {
        let mut tick = tokio::time::interval(self.inner.cfg.heartbeat);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let (mut sent, mut answered) = (0u64, 0u64);
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if sent.saturating_sub(answered) >= u64::from(self.inner.cfg.missed_pongs) {
                        return Disconnect::HeartbeatLost;
                    }
                    sent += 1;
                    let _ = conn.tx.send(Frame::Ping { n: sent });
                }
                d = &mut failed => {
                    return d.unwrap_or_else(|_| Disconnect::Protocol("the applier stopped".into()));
                }
                got = incoming.recv() => match got {
                    None | Some(Ok(None)) => return Disconnect::Eof,
                    Some(Err(e)) => return Disconnect::Codec(e.to_string()),
                    Some(Ok(Some(frame))) => {
                        if let Frame::Pong { n } = frame {
                            answered = answered.max(n.min(sent));
                            continue;
                        }
                        if let Err(d) = self.on_frame(frame, conn) {
                            return d;
                        }
                    }
                }
            }
        }
    }

    fn on_frame(&self, frame: Frame, conn: &Conn) -> Result<(), Disconnect> {
        let tx = &conn.tx;
        let wrong_way = |what: &str| Disconnect::Protocol(format!("{what} travelling the wrong way"));
        match frame {
            Frame::Ping { n } => {
                let _ = tx.send(Frame::Pong { n });
            }
            Frame::Pong { .. } => {}
            Frame::Hello { .. } => return Err(Disconnect::Protocol("a second hello".into())),
            Frame::Request { id, body } => {
                let handler = self.inner.handler.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let result = handler.request(body).await;
                    let _ = tx.send(Frame::Response { id, result });
                });
            }
            Frame::Response { id, result } => {
                if let Some(waiter) = crate::lock(&self.inner.pending).remove(&id) {
                    let _ = waiter.send(result);
                }
            }
            Frame::Call { call_id, dir, seq, body } => {
                if dir == self.inner.out_dir {
                    return Err(wrong_way("a call frame"));
                }
                if !valid_call_id(&call_id) {
                    return Err(Disconnect::Protocol(format!("bad call id {call_id:?}")));
                }
                let _ = conn.calls.send(Arriving { call_id, dir, seq, body });
            }
            Frame::Ack { call_id, dir, seq } => {
                if dir != self.inner.out_dir {
                    return Err(wrong_way("an ack"));
                }
                crate::lock(&self.inner.shared)
                    .outbox
                    .ack(&call_id, seq)
                    .map_err(|e| Disconnect::Protocol(format!("outbox: {e}")))?;
                self.inner.handler.acked(&call_id, seq);
            }
            Frame::Resume { call_id, dir, after } => {
                if dir != self.inner.out_dir {
                    return Err(wrong_way("a resume"));
                }
                let sh = crate::lock(&self.inner.shared);
                let frames = sh
                    .outbox
                    .unacked(&call_id, after)
                    .map_err(|e| Disconnect::Protocol(format!("outbox: {e}")))?;
                for (seq, body) in frames {
                    let _ = tx.send(Frame::Call { call_id: call_id.clone(), dir, seq, body });
                }
            }
            Frame::Media { call_id, dir, body } => {
                if dir == self.inner.out_dir {
                    return Err(wrong_way("a media frame"));
                }
                if !valid_call_id(&call_id) {
                    return Err(Disconnect::Protocol(format!("bad call id {call_id:?}")));
                }
                self.inner.handler.media(&call_id, body);
            }
        }
        Ok(())
    }
}

struct Arriving {
    call_id: String,
    dir: Dir,
    seq: u64,
    body: CallBody,
}

struct Conn {
    tx: mpsc::UnboundedSender<Frame>,
    calls: mpsc::UnboundedSender<Arriving>,
}

/// Applies one connection's call frames in arrival order, off the run loop so
/// heartbeats keep flowing, and answers each with an ack or a resume.
async fn apply_in_order(
    inner: Arc<Inner>,
    mut calls: mpsc::UnboundedReceiver<Arriving>,
    tx: mpsc::UnboundedSender<Frame>,
    failed: oneshot::Sender<Disconnect>,
) {
    while let Some(Arriving { call_id, dir, seq, body }) = calls.recv().await {
        let inner = inner.clone();
        let answered = tokio::task::spawn_blocking(move || {
            let _gate = crate::lock(&inner.apply_gate);
            let applied = inner.handler.applied(&call_id);
            match classify(applied, seq) {
                Arrival::Apply => match inner.handler.apply(&call_id, seq, body) {
                    Ok(()) => Ok(Frame::Ack { call_id, dir, seq }),
                    Err(e) => Err(format!("applying {call_id}#{seq}: {e}")),
                },
                Arrival::Duplicate => Ok(Frame::Ack { call_id, dir, seq: applied }),
                Arrival::Gap => Ok(Frame::Resume { call_id, dir, after: applied }),
            }
        })
        .await;
        match answered {
            Ok(Ok(frame)) => {
                let _ = tx.send(frame);
            }
            Ok(Err(e)) => {
                let _ = failed.send(Disconnect::Protocol(e));
                return;
            }
            Err(e) => {
                let _ = failed.send(Disconnect::Protocol(e.to_string()));
                return;
            }
        }
    }
}

/// The voice side: connects, serves, and reconnects forever with jittered
/// backoff capped at 2 s.
pub async fn dial_forever(peer: Peer, path: PathBuf) {
    let mut delay = Duration::from_millis(100);
    loop {
        match UnixStream::connect(&path).await {
            Ok(stream) => {
                delay = Duration::from_millis(100);
                let why = peer.serve(stream).await;
                eprintln!("voice link dropped: {why:?}");
            }
            Err(e) => eprintln!("voice link: cannot reach {}: {e}", path.display()),
        }
        let jitter = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_millis() % 100);
        tokio::time::sleep(delay + Duration::from_millis(u64::from(jitter))).await;
        delay = (delay * 2).min(Duration::from_secs(2));
    }
}

struct AbortOnDrop(Option<tokio::task::JoinHandle<()>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            h.abort();
        }
    }
}

/// Marks the link down when its connection ends, unless a newer connection
/// has already taken over.
struct Teardown {
    inner: Arc<Inner>,
    generation: u64,
}

impl Drop for Teardown {
    fn drop(&mut self) {
        let was_up = {
            let mut sh = crate::lock(&self.inner.shared);
            if self.inner.generation.load(SeqCst) != self.generation {
                return;
            }
            sh.current = None;
            self.inner.up.send_replace(false)
        };
        if was_up {
            self.inner.handler.link_changed(false);
        }
        for (_, waiter) in crate::lock(&self.inner.pending).drain() {
            let _ = waiter.send(Err(Refusal::new(RefusalCode::LinkDown, "the voice link dropped")));
        }
    }
}

/// Note's side: a new connection replaces the one before it.
pub async fn listen_forever(peer: Peer, listener: UnixListener) {
    let mut current = AbortOnDrop(None);
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                if let Some(old) = current.0.take() {
                    old.abort();
                }
                let p = peer.clone();
                current.0 = Some(tokio::spawn(async move {
                    let why = p.serve(stream).await;
                    eprintln!("voice link dropped: {why:?}");
                }));
            }
            Err(e) => {
                eprintln!("voice link: accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::MemOutbox;
    use crate::testkit::Recording;

    #[test]
    fn a_superseded_connection_does_not_install_over_a_newer_one() {
        let peer = Peer::new(
            PeerConfig::new(Role::Note),
            Dir::ToVoice,
            Arc::new(Recording::default()),
            Box::new(MemOutbox::default()),
        );
        peer.inner.generation.store(2, SeqCst);
        let (newer, _rx_newer) = mpsc::unbounded_channel();
        let (older, _rx_older) = mpsc::unbounded_channel();
        assert!(peer.install(2, &newer).is_ok());
        assert!(peer.install(1, &older).is_err());
        let sh = crate::lock(&peer.inner.shared);
        assert!(sh.current.as_ref().unwrap().same_channel(&newer));
    }
}
