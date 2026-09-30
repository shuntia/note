mod common;

use common::*;
use note_voice::config::VoiceServiceConfig;
use note_voice_proto::testkit::{eventually, fast};
use note_voice_proto::{CallBody, Outcome, Role};

struct Rig {
    dir: tempfile::TempDir,
    hs: SharedHs,
    note: FakeNote,
}

async fn rig() -> Rig {
    rig_with(|_, _, _| {}).await
}

/// `seed` gets the state dir, the homeserver and Note before the service starts.
async fn rig_with(seed: impl FnOnce(&std::path::Path, &SharedHs, &FakeNote)) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let (base, hs) = homeserver().await;
    let token = dir.path().join("token");
    std::fs::write(&token, "secret\n").unwrap();
    let socket = dir.path().join("voice.sock");
    let note = fake_note(&socket);
    std::fs::create_dir_all(dir.path().join("state")).unwrap();
    seed(&dir.path().join("state"), &hs, &note);
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
    Rig { dir, hs, note }
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
    r.hs.lock().unwrap().syncs.push_back(joined_room("!r:t", &[member_event("@aki:t", true)]));
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
    r.hs.lock().unwrap().syncs.push_back(joined_room("!r:t", &[decline_event("@aki:t", &notification)]));
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
    r.hs.lock().unwrap().syncs.push_back(joined_room(&room_id, &[join_event("@aki:t")]));
    let rec = r.note.rec.clone();
    eventually("DmJoined reaches Note", || {
        rec.requests.lock().unwrap().iter().any(|q| matches!(q, note_voice_proto::Request::DmJoined { link_id: 7, .. }))
    })
    .await;
}

fn seed_state(state_dir: &std::path::Path, state: &serde_json::Value) {
    std::fs::write(state_dir.join("state.json"), state.to_string()).unwrap();
}

fn saved_state(r: &Rig) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(r.dir.path().join("state/state.json")).unwrap()).unwrap()
}

fn dm_joined(r: &Rig, link: i64) -> usize {
    r.note
        .rec
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|q| matches!(q, note_voice_proto::Request::DmJoined { link_id, .. } if *link_id == link))
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_join_missed_before_a_restart_is_reported_at_startup() {
    let r = rig_with(|dir, hs, _| {
        seed_state(
            dir,
            &serde_json::json!({
                "links": { "3": { "mxid": "@aki:t", "room_id": "!dm:t", "reported": false } },
                "calls": {},
                "since": "s9",
            }),
        );
        hs.lock().unwrap().joined.insert("!dm:t".into(), vec!["@note:t".into(), "@aki:t".into()]);
    })
    .await;
    eventually("DmJoined reaches Note", || dm_joined(&r, 3) == 1).await;
    let rec = r.note.rec.clone();
    assert!(rec.requests.lock().unwrap().iter().any(|q| matches!(
        q,
        note_voice_proto::Request::DmJoined { link_id: 3, room_id } if room_id == "!dm:t"
    )));
    eventually("the link is marked reported", || saved_state(&r)["links"]["3"]["reported"] == true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn open_dm_for_a_known_link_reports_a_join_it_missed() {
    let r = rig_with(|dir, _, _| {
        seed_state(
            dir,
            &serde_json::json!({
                "links": { "4": { "mxid": "@aki:t", "room_id": "!dm:t", "reported": false } },
                "calls": {},
                "since": "s9",
            }),
        );
    })
    .await;
    r.hs.lock().unwrap().joined.insert("!dm:t".into(), vec!["@note:t".into(), "@aki:t".into()]);
    let got = r.note.peer.request(note_voice_proto::Request::OpenDm { link_id: 4, mxid: "@aki:t".into() }).await;
    assert_eq!(got, Ok(note_voice_proto::Reply::Dm { room_id: "!dm:t".into() }));
    eventually("DmJoined reaches Note", || dm_joined(&r, 4) == 1).await;
    assert!(r.hs.lock().unwrap().created.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn one_join_report_runs_per_link_at_a_time() {
    let r = rig().await;
    r.note.rec.answer_with(|q| match q {
        note_voice_proto::Request::DmJoined { .. } => {
            Err(note_voice_proto::Refusal::new(note_voice_proto::RefusalCode::Failed, "not yet"))
        }
        note_voice_proto::Request::OpenDm { .. } => Ok(note_voice_proto::Reply::Done),
    });
    let Ok(note_voice_proto::Reply::Dm { room_id }) =
        r.note.peer.request(note_voice_proto::Request::OpenDm { link_id: 5, mxid: "@aki:t".into() }).await
    else {
        panic!()
    };
    r.hs.lock().unwrap().syncs.push_back(joined_room(&room_id, &[join_event("@aki:t")]));
    r.hs.lock().unwrap().syncs.push_back(joined_room(&room_id, &[join_event("@aki:t")]));
    eventually("a DmJoined reaches Note", || dm_joined(&r, 5) >= 1).await;
    let hs = r.hs.clone();
    eventually("both syncs are consumed", || hs.lock().unwrap().syncs.is_empty()).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(dm_joined(&r, 5), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replayed_start_after_recovery_neither_rings_nor_reopens_the_call() {
    let r = rig_with(|dir, _, note| {
        note.hold.set(true);
        seed_state(
            dir,
            &serde_json::json!({
                "links": {},
                "calls": { "c9": { "room_id": "!r:t", "done": false, "ended_seq": null } },
                "since": "s9",
            }),
        );
    })
    .await;
    r.note.peer.send_call("c9", start(30, soon())).unwrap();
    let rec = r.note.rec.clone();
    eventually("the replayed Start is acked", || rec.acks.lock().unwrap().get("c9") == Some(&1)).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    {
        let hs = r.hs.lock().unwrap();
        assert!(hs.sends.is_empty(), "no ring");
        assert!(hs.state_puts.iter().all(|(_, _, _, body)| body == &serde_json::json!({})), "no membership");
    }
    assert_eq!(saved_state(&r)["calls"]["c9"]["done"], true);
    r.note.hold.set(false);
    eventually("Ended reaches Note", || rec.bodies("c9").contains(&CallBody::Ended)).await;
    assert_eq!(rec.bodies("c9").iter().filter(|b| **b == CallBody::Ended).count(), 1);
    assert_eq!(outcome_of(&r, "c9"), Some(Outcome::Failed { reason: "the voice service restarted".into() }));
}

#[tokio::test(flavor = "multi_thread")]
async fn finished_calls_with_nothing_pending_are_dropped_at_startup() {
    let r = rig_with(|dir, _, _| {
        seed_state(
            dir,
            &serde_json::json!({
                "links": {},
                "calls": { "c8": { "room_id": "!r:t", "done": true, "ended_seq": 2 } },
                "since": "s9",
            }),
        );
    })
    .await;
    assert!(saved_state(&r)["calls"].get("c8").is_none());
    assert!(r.note.rec.bodies("c8").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_m_direct_is_left_alone() {
    let r = rig().await;
    r.hs.lock().unwrap().direct_get_fails = true;
    let got = r.note.peer.request(note_voice_proto::Request::OpenDm { link_id: 6, mxid: "@aki:t".into() }).await;
    assert!(matches!(got, Ok(note_voice_proto::Reply::Dm { .. })));
    assert!(r.hs.lock().unwrap().direct_puts.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_m_direct_write_still_links_one_room() {
    let r = rig().await;
    r.hs.lock().unwrap().direct_put_fails = true;
    let first = r.note.peer.request(note_voice_proto::Request::OpenDm { link_id: 7, mxid: "@aki:t".into() }).await;
    let second = r.note.peer.request(note_voice_proto::Request::OpenDm { link_id: 7, mxid: "@aki:t".into() }).await;
    assert!(matches!(first, Ok(note_voice_proto::Reply::Dm { .. })));
    assert_eq!(first, second);
    assert_eq!(r.hs.lock().unwrap().created.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_voice_restart_mid_ring_closes_the_call_and_reports_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let (base, hs) = homeserver().await;
    let token = dir.path().join("token");
    std::fs::write(&token, "secret").unwrap();
    let socket = dir.path().join("voice.sock");
    let note = fake_note(&socket);
    let cfg = VoiceServiceConfig {
        homeserver: base,
        token_file: token,
        livekit_service_url: "https://rtc.t".into(),
        socket,
        state_dir: dir.path().join("state"),
    };
    let first = {
        let cfg = cfg.clone();
        tokio::spawn(async move { note_voice::service::run_with(cfg, fast(Role::Voice)).await.unwrap() })
    };
    let p = note.peer.clone();
    eventually("voice connects", || p.is_up()).await;
    note.peer.send_call("c9", start(30, soon())).unwrap();
    let rec = note.rec.clone();
    eventually("ringing", || rec.bodies("c9").contains(&CallBody::Ringing)).await;

    first.abort();
    let p = note.peer.clone();
    eventually("Note sees the voice side gone", || !p.is_up()).await;
    tokio::spawn(async move { note_voice::service::run_with(cfg, fast(Role::Voice)).await.unwrap() });

    let rec = note.rec.clone();
    eventually("the call is closed after the restart", || rec.bodies("c9").contains(&CallBody::Ended)).await;
    let outcomes: Vec<Outcome> = note
        .rec
        .bodies("c9")
        .into_iter()
        .filter_map(|b| match b {
            CallBody::Outcome { outcome } => Some(outcome),
            _ => None,
        })
        .collect();
    assert_eq!(outcomes, vec![Outcome::Failed { reason: "the voice service restarted".into() }]);
    assert_eq!(note.rec.seqs("c9"), (1..=note.rec.seqs("c9").len() as u64).collect::<Vec<_>>());
    let cleared = hs.lock().unwrap().state_puts.iter().filter(|(_, _, _, b)| b == &serde_json::json!({})).count();
    assert!(cleared >= 1, "the orphaned membership is cleared");
}

#[tokio::test(flavor = "multi_thread")]
async fn relinking_the_same_account_reports_the_join_again() {
    let r = rig_with(|dir, hs, _| {
        seed_state(
            dir,
            &serde_json::json!({
                "links": { "4": { "mxid": "@aki:t", "room_id": "!dm:t", "reported": true } },
                "calls": {},
                "since": "s9",
            }),
        );
        hs.lock().unwrap().joined.insert("!dm:t".into(), vec!["@note:t".into(), "@aki:t".into()]);
    })
    .await;
    let got = r.note.peer.request(note_voice_proto::Request::OpenDm { link_id: 4, mxid: "@aki:t".into() }).await;
    assert_eq!(got, Ok(note_voice_proto::Reply::Dm { room_id: "!dm:t".into() }));
    eventually("DmJoined reaches Note", || dm_joined(&r, 4) == 1).await;
    eventually("the link is marked reported", || saved_state(&r)["links"]["4"]["reported"] == true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_since_starts_the_sync_over() {
    let r = rig_with(|dir, hs, _| {
        seed_state(dir, &serde_json::json!({ "links": {}, "calls": {}, "since": "s9" }));
        hs.lock().unwrap().reject_since = Some("s9".into());
    })
    .await;
    let hs = r.hs.clone();
    eventually("a sync after the refusal", || hs.lock().unwrap().sync_sinces.len() >= 2).await;
    assert_eq!(r.hs.lock().unwrap().sync_sinces[..2], [Some("s9".to_string()), None]);
}
