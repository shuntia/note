mod common;

use common::*;
use note_voice::config::VoiceServiceConfig;
use note_voice_proto::testkit::{eventually, fast};
use note_voice_proto::{CallBody, Outcome, Role};

struct Rig {
    _dir: tempfile::TempDir,
    hs: SharedHs,
    note: FakeNote,
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let (base, hs) = homeserver().await;
    let token = dir.path().join("token");
    std::fs::write(&token, "secret\n").unwrap();
    let socket = dir.path().join("voice.sock");
    let note = fake_note(&socket);
    let cfg = VoiceServiceConfig {
        homeserver: base,
        token_file: token,
        livekit_service_url: "https://rtc.t".into(),
        socket,
        state_dir: dir.path().join("state"),
    };
    tokio::spawn(async move { note_voice::service::run_with(cfg, fast(Role::Voice)).await.unwrap() });
    let p = note.peer.clone();
    eventually("voice connects", || p.is_up()).await;
    Rig { _dir: dir, hs, note }
}

fn start(ring_secs: u32, ring_by_ms: i64) -> CallBody {
    CallBody::Start {
        user_id: 1,
        room_id: "!r:t".into(),
        mxid: "@aki:t".into(),
        title: "Check-in".into(),
        ring_secs,
        ring_by_ms,
    }
}

fn soon() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64 + 10_000
}

fn outcome_of(r: &Rig, call: &str) -> Option<Outcome> {
    r.note.rec.bodies(call).into_iter().find_map(|b| match b {
        CallBody::Outcome { outcome } => Some(outcome),
        _ => None,
    })
}

/// The ring's event id, once it is out.
async fn wait_for_ring(r: &Rig) -> String {
    let hs = r.hs.clone();
    eventually("the ring is sent", || {
        hs.lock().unwrap().sends.iter().any(|(_, k, _, _)| k == "m.rtc.notification")
    })
    .await;
    let hs = r.hs.lock().unwrap();
    let (_, _, body, event_id) = hs.sends.iter().find(|(_, k, _, _)| k == "m.rtc.notification").unwrap();
    assert_eq!(body["notification_type"], "ring");
    assert_eq!(body["m.mentions"]["user_ids"][0], "@aki:t");
    assert_eq!(body["m.relates_to"]["rel_type"], "m.reference");
    event_id.clone()
}

fn cleared(r: &Rig) -> bool {
    r.hs.lock().unwrap().state_puts.iter().any(|(_, k, key, body)| {
        k == "org.matrix.msc3401.call.member" && key == "_@note:t_DEV_m.call" && body == &serde_json::json!({})
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_ends_the_ring_as_answered() {
    let r = rig().await;
    r.note.peer.send_call("c1", start(30, soon())).unwrap();
    wait_for_ring(&r).await;
    r.hs.lock().unwrap().syncs.push_back(joined_room("!r:t", vec![member_event("@aki:t", true)]));
    let rec = r.note.rec.clone();
    eventually("an outcome and Ended", || rec.bodies("c1").contains(&CallBody::Ended)).await;
    assert_eq!(outcome_of(&r, "c1"), Some(Outcome::Answered));
    assert_eq!(r.note.rec.bodies("c1")[0], CallBody::Ringing);
    assert!(cleared(&r), "the bot leaves the call");
}

#[tokio::test(flavor = "multi_thread")]
async fn decline_ends_the_ring() {
    let r = rig().await;
    r.note.peer.send_call("c2", start(30, soon())).unwrap();
    let notification = wait_for_ring(&r).await;
    r.hs.lock().unwrap().syncs.push_back(joined_room("!r:t", vec![decline_event("@aki:t", &notification)]));
    let rec = r.note.rec.clone();
    eventually("declined", || rec.bodies("c2").contains(&CallBody::Ended)).await;
    assert_eq!(outcome_of(&r, "c2"), Some(Outcome::Declined));
}

#[tokio::test(flavor = "multi_thread")]
async fn no_answer_is_missed() {
    let r = rig().await;
    r.note.peer.send_call("c3", start(1, soon())).unwrap();
    let rec = r.note.rec.clone();
    eventually("missed", || rec.bodies("c3").contains(&CallBody::Ended)).await;
    assert_eq!(outcome_of(&r, "c3"), Some(Outcome::Missed));
    assert!(cleared(&r));
}

#[tokio::test(flavor = "multi_thread")]
async fn late_start_is_refused_without_touching_matrix() {
    let r = rig().await;
    r.note.peer.send_call("c4", start(30, soon() - 60_000)).unwrap();
    let rec = r.note.rec.clone();
    eventually("refused", || rec.bodies("c4").contains(&CallBody::Ended)).await;
    assert_eq!(outcome_of(&r, "c4"), Some(Outcome::Failed { reason: "late".into() }));
    let hs = r.hs.lock().unwrap();
    assert!(hs.sends.is_empty() && hs.state_puts.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn homeserver_error_fails_the_call_and_reports_it() {
    let r = rig().await;
    r.hs.lock().unwrap().fail_sends = true;
    r.note.peer.send_call("c5", start(30, soon())).unwrap();
    let rec = r.note.rec.clone();
    eventually("failed", || rec.bodies("c5").contains(&CallBody::Ended)).await;
    assert!(matches!(outcome_of(&r, "c5"), Some(Outcome::Failed { .. })));
    assert!(cleared(&r), "the membership is cleared even when the ring failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn open_dm_is_idempotent_and_the_join_is_reported() {
    let r = rig().await;
    let first = r.note.peer.request(note_voice_proto::Request::OpenDm { link_id: 7, mxid: "@aki:t".into() }).await;
    let second = r.note.peer.request(note_voice_proto::Request::OpenDm { link_id: 7, mxid: "@aki:t".into() }).await;
    assert_eq!(first, second);
    let note_voice_proto::Reply::Dm { room_id } = first.unwrap() else { panic!() };
    {
        let hs = r.hs.lock().unwrap();
        assert_eq!(hs.created.len(), 1, "one room per link");
        assert_eq!(hs.created[0]["preset"], "trusted_private_chat");
        assert_eq!(hs.created[0]["is_direct"], true);
    }
    r.hs.lock().unwrap().syncs.push_back(joined_room(&room_id, vec![join_event("@aki:t")]));
    let rec = r.note.rec.clone();
    eventually("DmJoined reaches Note", || {
        rec.requests.lock().unwrap().iter().any(|q| matches!(q, note_voice_proto::Request::DmJoined { link_id: 7, .. }))
    })
    .await;
}
