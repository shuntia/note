use note_voice_proto::peer::{dial_forever, listen_forever, Peer};
use note_voice_proto::testkit::{eventually, fast, FaultProxy, Recording};
use note_voice_proto::*;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, Mutex};
use tokio::net::UnixListener;

struct Side {
    peer: Peer,
    rec: Arc<Recording>,
    outbox: Arc<Mutex<MemOutbox>>,
}

fn side(role: Role, out: Dir, outbox: Arc<Mutex<MemOutbox>>, rec: Arc<Recording>) -> Side {
    let peer = Peer::new(fast(role), out, rec.clone(), Box::new(outbox.clone()));
    Side { peer, rec, outbox }
}

struct Rig {
    _dir: tempfile::TempDir,
    proxy: FaultProxy,
    proxy_path: std::path::PathBuf,
    note: Side,
    voice: Side,
    voice_task: tokio::task::JoinHandle<()>,
    _note_task: tokio::task::JoinHandle<()>,
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let note_path = dir.path().join("note.sock");
    let proxy_path = dir.path().join("proxy.sock");
    let note = side(Role::Note, Dir::ToVoice, Arc::default(), Arc::default());
    let voice = side(Role::Voice, Dir::ToNote, Arc::default(), Arc::default());
    let listener = UnixListener::bind(&note_path).unwrap();
    let note_task = tokio::spawn(listen_forever(note.peer.clone(), listener));
    let proxy = FaultProxy::start(&proxy_path, note_path).unwrap();
    let voice_task = tokio::spawn(dial_forever(voice.peer.clone(), proxy_path.clone()));
    let (n, v) = (note.peer.clone(), voice.peer.clone());
    eventually("both sides up", || n.is_up() && v.is_up()).await;
    Rig { _dir: dir, proxy, proxy_path, note, voice, voice_task, _note_task: note_task }
}

fn one_to(n: u64) -> Vec<u64> {
    (1..=n).collect()
}

#[tokio::test]
async fn frames_arrive_once_and_in_order() {
    let r = rig().await;
    for _ in 0..200 {
        r.voice.peer.send_call("c1", CallBody::Ringing).unwrap();
    }
    let rec = r.note.rec.clone();
    eventually("200 applied", || rec.seqs("c1").len() == 200).await;
    assert_eq!(r.note.rec.seqs("c1"), one_to(200));
    let v = r.voice.peer.clone();
    eventually("all acknowledged", || v.pending_calls().is_empty()).await;
}

#[tokio::test]
async fn a_cut_mid_stream_resumes_where_it_stopped() {
    let r = rig().await;
    r.proxy.faults.cut_after_calls(50);
    for _ in 0..200 {
        r.voice.peer.send_call("c1", CallBody::Ringing).unwrap();
    }
    let rec = r.note.rec.clone();
    eventually("200 applied after the cut", || rec.seqs("c1").len() >= 200).await;
    assert_eq!(r.note.rec.seqs("c1"), one_to(200));
}

#[tokio::test]
async fn duplicated_frames_apply_once() {
    let r = rig().await;
    r.proxy.faults.duplicate_calls.store(true, SeqCst);
    for _ in 0..100 {
        r.note.peer.send_call("c2", CallBody::HangUp).unwrap();
    }
    let rec = r.voice.rec.clone();
    eventually("100 applied", || rec.seqs("c2").len() >= 100).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(r.voice.rec.seqs("c2"), one_to(100));
}

#[tokio::test]
async fn lost_acks_are_recovered_by_replay_without_reapplying() {
    let r = rig().await;
    r.proxy.faults.drop_acks.store(true, SeqCst);
    for _ in 0..50 {
        r.voice.peer.send_call("c3", CallBody::Ringing).unwrap();
    }
    let rec = r.note.rec.clone();
    eventually("50 applied", || rec.seqs("c3").len() == 50).await;
    assert_eq!(r.voice.peer.pending_calls(), vec!["c3".to_string()]);
    r.proxy.faults.drop_acks.store(false, SeqCst);
    r.proxy.faults.cut();
    let v = r.voice.peer.clone();
    eventually("acks after the reconnect", || v.pending_calls().is_empty()).await;
    assert_eq!(r.note.rec.seqs("c3"), one_to(50));
}

#[tokio::test]
async fn a_restarted_sender_replays_its_durable_outbox() {
    let r = rig().await;
    r.proxy.faults.hold.store(true, SeqCst);
    for _ in 0..30 {
        r.voice.peer.send_call("c4", CallBody::Ringing).unwrap();
    }
    r.voice_task.abort();
    r.proxy.faults.hold.store(false, SeqCst);
    r.proxy.faults.cut();
    let reborn = side(Role::Voice, Dir::ToNote, r.voice.outbox.clone(), r.voice.rec.clone());
    tokio::spawn(dial_forever(reborn.peer.clone(), r.proxy_path.clone()));
    let rec = r.note.rec.clone();
    eventually("30 applied after the restart", || rec.seqs("c4").len() >= 30).await;
    assert_eq!(r.note.rec.seqs("c4"), one_to(30));
}

#[tokio::test]
async fn a_silent_link_is_declared_down_and_comes_back() {
    let r = rig().await;
    r.proxy.faults.hold.store(true, SeqCst);
    let v = r.voice.peer.clone();
    eventually("voice notices the dead link", || !v.is_up()).await;
    r.proxy.faults.hold.store(false, SeqCst);
    let (n, v) = (r.note.peer.clone(), r.voice.peer.clone());
    eventually("both back up", || n.is_up() && v.is_up()).await;
}

#[tokio::test]
async fn requests_round_trip_and_fail_cleanly() {
    let r = rig().await;
    r.voice.rec.answer_with(|req| match req {
        Request::OpenDm { link_id, .. } => Ok(Reply::Dm { room_id: format!("!room{link_id}:t") }),
        Request::DmJoined { .. } => Err(Refusal::new(RefusalCode::BadRequest, "wrong way")),
    });
    let got = r.note.peer.request(Request::OpenDm { link_id: 4, mxid: "@a:t".into() }).await;
    assert_eq!(got, Ok(Reply::Dm { room_id: "!room4:t".into() }));

    r.proxy.faults.hold.store(true, SeqCst);
    let got = r.note.peer.request(Request::OpenDm { link_id: 5, mxid: "@a:t".into() }).await;
    assert!(
        matches!(got, Err(Refusal { code: RefusalCode::Timeout | RefusalCode::LinkDown, .. })),
        "{got:?}"
    );
    let n = r.note.peer.clone();
    eventually("note sees the link down", || !n.is_up()).await;
    let got = r.note.peer.request(Request::OpenDm { link_id: 6, mxid: "@a:t".into() }).await;
    assert_eq!(got.unwrap_err().code, RefusalCode::LinkDown);
}

#[tokio::test]
async fn a_version_mismatch_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.sock");
    let note = side(Role::Note, Dir::ToVoice, Arc::default(), Arc::default());
    tokio::spawn(listen_forever(note.peer.clone(), UnixListener::bind(&path).unwrap()));
    let mut s = tokio::net::UnixStream::connect(&path).await.unwrap();
    codec::write_frame(&mut s, &Frame::Hello { proto: 99, role: Role::Voice, instance: "x".into() })
        .await
        .unwrap();
    let first = codec::read_frame(&mut s).await.unwrap();
    assert!(matches!(first, Some(Frame::Hello { .. })));
    let next = codec::read_frame(&mut s).await;
    assert!(matches!(next, Ok(None) | Err(_)), "the connection closes: {next:?}");
    assert!(!note.peer.is_up());
}

async fn raw_hello(path: &std::path::Path, proto: u32) -> tokio::net::UnixStream {
    let mut s = tokio::net::UnixStream::connect(path).await.unwrap();
    codec::write_frame(&mut s, &Frame::Hello { proto, role: Role::Voice, instance: "raw".into() })
        .await
        .unwrap();
    assert!(matches!(codec::read_frame(&mut s).await.unwrap(), Some(Frame::Hello { .. })));
    s
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replacement_that_fails_its_hello_leaves_the_link_down() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.sock");
    let note = side(Role::Note, Dir::ToVoice, Arc::default(), Arc::default());
    tokio::spawn(listen_forever(note.peer.clone(), UnixListener::bind(&path).unwrap()));
    let _first = raw_hello(&path, PROTO_VERSION).await;
    let n = note.peer.clone();
    eventually("note up with the first connection", || n.is_up()).await;
    let _second = raw_hello(&path, 99).await;
    eventually("note down once the replacement is refused", || !n.is_up()).await;
}

#[tokio::test]
async fn an_unsolicited_pong_does_not_stop_the_dialer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let voice = side(Role::Voice, Dir::ToNote, Arc::default(), Arc::default());
    tokio::spawn(dial_forever(voice.peer.clone(), path.clone()));
    let (mut s, _) = listener.accept().await.unwrap();
    assert!(matches!(codec::read_frame(&mut s).await.unwrap(), Some(Frame::Hello { .. })));
    codec::write_frame(&mut s, &Frame::Hello { proto: PROTO_VERSION, role: Role::Note, instance: "raw".into() })
        .await
        .unwrap();
    codec::write_frame(&mut s, &Frame::Pong { n: 1000 }).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    drop(s);
    let redial = tokio::time::timeout(std::time::Duration::from_secs(3), listener.accept()).await;
    assert!(redial.is_ok(), "the dialer reconnects");
}

/// Applies slowly and remembers every call to `apply`, so a double apply
/// shows up.
#[derive(Default)]
struct Slow {
    applied: Mutex<std::collections::HashMap<String, u64>>,
    calls: Mutex<Vec<u64>>,
    started: std::sync::atomic::AtomicU64,
    downs: std::sync::atomic::AtomicU64,
}

impl Handler for Slow {
    fn applied(&self, call_id: &str) -> u64 {
        self.applied.lock().unwrap().get(call_id).copied().unwrap_or(0)
    }

    fn apply(&self, call_id: &str, seq: u64, _body: CallBody) -> Result<(), String> {
        self.started.fetch_add(1, SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(300));
        self.calls.lock().unwrap().push(seq);
        self.applied.lock().unwrap().insert(call_id.to_string(), seq);
        Ok(())
    }

    fn request(&self, _body: Request) -> BoxFuture<Result<Reply, Refusal>> {
        Box::pin(async { Ok(Reply::Done) })
    }

    fn link_changed(&self, up: bool) {
        if !up {
            self.downs.fetch_add(1, SeqCst);
        }
    }
}

struct SlowRig {
    _dir: tempfile::TempDir,
    proxy: FaultProxy,
    slow: Arc<Slow>,
    voice: Peer,
}

async fn slow_rig() -> SlowRig {
    let dir = tempfile::tempdir().unwrap();
    let note_path = dir.path().join("note.sock");
    let proxy_path = dir.path().join("proxy.sock");
    let slow = Arc::new(Slow::default());
    let note = Peer::new(fast(Role::Note), Dir::ToVoice, slow.clone(), Box::new(MemOutbox::default()));
    let voice = side(Role::Voice, Dir::ToNote, Arc::default(), Arc::default()).peer;
    tokio::spawn(listen_forever(note.clone(), UnixListener::bind(&note_path).unwrap()));
    let proxy = FaultProxy::start(&proxy_path, note_path).unwrap();
    tokio::spawn(dial_forever(voice.clone(), proxy_path));
    let (n, v) = (note.clone(), voice.clone());
    eventually("both sides up", || n.is_up() && v.is_up()).await;
    SlowRig { _dir: dir, proxy, slow, voice }
}

#[tokio::test]
async fn a_slow_apply_keeps_the_link_up() {
    let r = slow_rig().await;
    for _ in 0..3 {
        r.voice.send_call("c5", CallBody::Ringing).unwrap();
    }
    let v = r.voice.clone();
    eventually("all acknowledged", || v.pending_calls().is_empty()).await;
    assert_eq!(*r.slow.calls.lock().unwrap(), one_to(3));
    assert_eq!(r.slow.downs.load(SeqCst), 0, "the link never dropped");
}

#[tokio::test]
async fn a_cut_mid_apply_applies_each_seq_once() {
    let r = slow_rig().await;
    for _ in 0..3 {
        r.voice.send_call("c6", CallBody::Ringing).unwrap();
    }
    let slow = r.slow.clone();
    eventually("the first apply is under way", || slow.started.load(SeqCst) >= 1).await;
    r.proxy.faults.cut();
    let v = r.voice.clone();
    eventually("all acknowledged after the cut", || v.pending_calls().is_empty()).await;
    assert_eq!(*r.slow.calls.lock().unwrap(), one_to(3));
}
