use note_server::channels::mock::MockChannel;
use note_server::channels::{deliver_via, OutboundMessage, Urgency};
use note_server::voice::{links, Voice};
use note_server::{auth, db, AppState};
use note_voice_proto::testkit::{eventually, fast, Recording};
use note_voice_proto::{dial_forever, CallBody, Dir, MemOutbox, Outcome, Peer, Role};
use std::sync::{Arc, Mutex};

fn urgent() -> OutboundMessage {
    OutboundMessage {
        title: "Check-in".into(),
        body: "how is the essay going?".into(),
        urgency: Urgency::High,
        event_id: Some(1),
        conversation_id: None,
        actions: Vec::new(),
    }
}

struct Rig {
    dir: tempfile::TempDir,
    state: AppState,
    voice: Arc<Voice>,
    listen: tokio::task::JoinHandle<()>,
    push: Arc<MockChannel>,
    fake: Peer,
    fake_rec: Arc<Recording>,
    _fake_outbox: Arc<Mutex<MemOutbox>>,
}

fn socket(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("voice.sock")
}

async fn rig(linked: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("defaults")).unwrap();
    std::fs::write(
        dir.path().join("defaults/user.toml"),
        "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n",
    )
    .unwrap();
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", false).unwrap();
    if linked {
        let id = links::begin(&conn, 1, "@aki:t", jiff::Timestamp::now()).unwrap();
        links::mark_joined(&conn, id, "!r:t", jiff::Timestamp::now()).unwrap();
    }
    let push = Arc::new(MockChannel::new("push"));
    let state = AppState::new(conn, dir.path().to_path_buf(), dir.path().to_path_buf())
        .with_channels(vec![push.clone()]);
    let voice = Voice::with_config(state.db.clone(), fast(Role::Note));
    let state = state.with_voice(voice.clone());
    let listen = voice.listen(&socket(&dir)).unwrap();
    let fake_rec = Arc::new(Recording::default());
    let fake_outbox: Arc<Mutex<MemOutbox>> = Arc::default();
    let fake = Peer::new(fast(Role::Voice), Dir::ToNote, fake_rec.clone(), Box::new(fake_outbox.clone()));
    tokio::spawn(dial_forever(fake.clone(), socket(&dir)));
    let (v, f) = (voice.clone(), fake.clone());
    eventually("link up", || v.is_up() && f.is_up()).await;
    Rig { dir, state, voice, listen, push, fake, fake_rec, _fake_outbox: fake_outbox }
}

fn deliver(r: &Rig, msg: &OutboundMessage) -> Option<&'static str> {
    deliver_via(&r.state.db, &r.state.channels, 1, "aki", msg)
}

fn started_call(r: &Rig) -> String {
    let seen = r.fake_rec.seen.lock().unwrap();
    let (id, _, body) = seen.first().expect("a Start frame").clone();
    assert!(matches!(body, CallBody::Start { ring_secs: 30, .. }), "{body:?}");
    id
}

#[tokio::test(flavor = "multi_thread")]
async fn an_urgent_message_rings_then_falls_through_on_missed() {
    let r = rig(true).await;
    assert_eq!(r.state.channels[0].name(), "voice");
    assert_eq!(tokio::task::block_in_place(|| deliver(&r, &urgent())), Some("voice"));
    let rec = r.fake_rec.clone();
    eventually("Start reaches the voice side", || !rec.seen.lock().unwrap().is_empty()).await;
    let id = started_call(&r);
    r.fake.send_call(&id, CallBody::Ringing).unwrap();
    r.fake.send_call(&id, CallBody::Outcome { outcome: Outcome::Missed }).unwrap();
    r.fake.send_call(&id, CallBody::Ended).unwrap();
    let push = r.push.clone();
    eventually("the message falls through", || push.seen().len() == 1).await;
    assert_eq!(r.push.seen()[0].1.body, "how is the essay going?");
    let f = r.fake.clone();
    eventually("the voice side's frames are acknowledged", || f.pending_calls().is_empty()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_normal_message_is_not_rung() {
    let r = rig(true).await;
    let mut m = urgent();
    m.urgency = Urgency::Normal;
    assert_eq!(tokio::task::block_in_place(|| deliver(&r, &m)), Some("push"));
    assert!(r.fake_rec.seen.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn invited_but_not_joined_is_not_rung() {
    let r = rig(false).await;
    {
        let conn = r.state.db();
        links::begin(&conn, 1, "@aki:t", jiff::Timestamp::now()).unwrap();
    }
    assert_eq!(tokio::task::block_in_place(|| deliver(&r, &urgent())), Some("push"));
}

#[tokio::test(flavor = "multi_thread")]
async fn down_link_falls_through_at_once() {
    let r = rig(true).await;
    r.listen.abort();
    let v = r.voice.clone();
    eventually("Note sees the link down", || !v.is_up()).await;
    assert_eq!(tokio::task::block_in_place(|| deliver(&r, &urgent())), Some("push"));
    let open: i64 = r.state.db().query_row("SELECT COUNT(*) FROM voice_calls", [], |x| x.get(0)).unwrap();
    assert_eq!(open, 0, "no call is recorded when the link is down");
}

#[tokio::test(flavor = "multi_thread")]
async fn note_restart_mid_ring_applies_the_outcome_once() {
    let r = rig(true).await;
    tokio::task::block_in_place(|| deliver(&r, &urgent()));
    let rec = r.fake_rec.clone();
    eventually("Start arrives", || !rec.seen.lock().unwrap().is_empty()).await;
    let id = started_call(&r);
    r.fake.send_call(&id, CallBody::Ringing).unwrap();

    r.listen.abort();
    let f = r.fake.clone();
    eventually("the voice side sees Note gone", || !f.is_up()).await;
    r.fake.send_call(&id, CallBody::Outcome { outcome: Outcome::Declined }).unwrap();
    r.fake.send_call(&id, CallBody::Ended).unwrap();

    let reborn = Voice::with_config(r.state.db.clone(), fast(Role::Note));
    reborn.set_fallback(vec![r.push.clone()]);
    let _listen = reborn.listen(&socket(&r.dir)).unwrap();
    let f = r.fake.clone();
    eventually("the outbox drains into the new Note", || f.pending_calls().is_empty()).await;
    let push = r.push.clone();
    eventually("one fallthrough", || push.seen().len() == 1).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(r.push.seen().len(), 1);
    let outcome: String = r
        .state
        .db()
        .query_row("SELECT outcome FROM voice_calls WHERE id = ?1", [&id], |x| x.get(0))
        .unwrap();
    assert_eq!(outcome, "declined");
}
