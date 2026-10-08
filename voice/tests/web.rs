#[allow(dead_code)]
mod common;

use common::*;
use note_voice_proto::testkit::{eventually, fast};
use note_voice_proto::{CallBody, Direction, LiveState, Media, Origin, Outcome, Pcm, Role, VoiceProfile};
use std::sync::atomic::Ordering;

struct Rig {
    _dir: tempfile::TempDir,
    note: FakeNote,
}

async fn rig(loaded: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let (base, _hs) = homeserver().await;
    let cfg = voice_config(dir.path(), base);
    let note = fake_note(&cfg.socket);
    std::fs::create_dir_all(&cfg.state_dir).unwrap();
    let backends = backends(loaded, Join::Quiet);
    tokio::spawn(async move { note_voice::service::run_with(cfg, fast(Role::Voice), backends).await.unwrap() });
    let p = note.peer.clone();
    eventually("voice connects", || p.is_up()).await;
    Rig { _dir: dir, note }
}

fn web_start(ring_by_ms: i64) -> CallBody {
    CallBody::Start {
        user_id: 1,
        room_id: String::new(),
        mxid: String::new(),
        title: "Call".into(),
        ring_secs: 0,
        ring_by_ms,
        voice: VoiceProfile::default(),
        direction: Direction::Inbound,
        origin: Origin::Web,
    }
}

fn media_of(r: &Rig, call: &str) -> Vec<Media> {
    r.note.rec.media.lock().unwrap().iter().filter(|(c, _)| c == call).map(|(_, m)| m.clone()).collect()
}

#[tokio::test]
async fn a_web_call_answers_at_once_hears_the_caller_and_speaks_through_media() {
    let r = rig(true).await;
    r.note.peer.send_call("w-1", web_start(now_ms() + 10_000)).unwrap();
    let rec = r.note.rec.clone();
    eventually("answered", || rec.bodies("w-1").contains(&CallBody::Outcome { outcome: Outcome::Answered })).await;
    eventually("listening shown", || media_of(&r, "w-1").contains(&Media::State { state: LiveState::Listening })).await;

    let before = HEARD_SAMPLES.load(Ordering::SeqCst);
    for _ in 0..60 {
        assert!(r.note.peer.send_media("w-1", Media::AudioIn { pcm: Pcm(vec![100; 320]) }));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    eventually("the session hears the caller", || HEARD_SAMPLES.load(Ordering::SeqCst) - before >= 512 * 20).await;

    r.note.peer.send_call("w-1", CallBody::Speak { reply: 1, idx: 0, text: "hi".into() }).unwrap();
    r.note.peer.send_call("w-1", CallBody::SpeakDone { reply: 1 }).unwrap();
    r.note.peer.send_call("w-1", CallBody::Play { reply: 1 }).unwrap();
    eventually("Note's voice reaches the browser", || {
        media_of(&r, "w-1").iter().any(|m| matches!(m, Media::AudioOut { pcm, rate: 48_000 } if pcm.0.first() == Some(&spoken_marker("hi"))))
    })
    .await;
    eventually("played", || rec.bodies("w-1").contains(&CallBody::Played { reply: 1 })).await;

    r.note.peer.send_call("w-1", CallBody::HangUp).unwrap();
    eventually("ended", || rec.bodies("w-1").contains(&CallBody::Ended)).await;
}

#[tokio::test]
async fn a_late_web_start_fails_without_a_session() {
    let r = rig(true).await;
    r.note.peer.send_call("w-2", web_start(now_ms() - 1)).unwrap();
    let rec = r.note.rec.clone();
    eventually("ended", || rec.bodies("w-2").contains(&CallBody::Ended)).await;
    assert!(rec.bodies("w-2").contains(&CallBody::Outcome { outcome: Outcome::Failed { reason: "late".into() } }));
    assert!(media_of(&r, "w-2").is_empty());
}

#[tokio::test]
async fn a_web_call_without_voice_models_fails_and_ends() {
    let r = rig(false).await;
    r.note.peer.send_call("w-3", web_start(now_ms() + 10_000)).unwrap();
    let rec = r.note.rec.clone();
    eventually("ended", || rec.bodies("w-3").contains(&CallBody::Ended)).await;
    assert!(rec.bodies("w-3").contains(&CallBody::Outcome { outcome: Outcome::Failed { reason: "no voice models".into() } }));
}

#[tokio::test]
async fn a_hang_up_right_behind_the_web_start_ends_the_call() {
    let r = rig(true).await;
    r.note.peer.send_call("w-4", web_start(now_ms() + 10_000)).unwrap();
    r.note.peer.send_call("w-4", CallBody::HangUp).unwrap();
    let rec = r.note.rec.clone();
    eventually("ended", || rec.bodies("w-4").contains(&CallBody::Ended)).await;
}
