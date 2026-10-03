#[allow(dead_code)]
mod common;

use common::*;
use note_voice_proto::testkit::{eventually, fast};
use note_voice_proto::{CallBody, Direction, Outcome, Reply, Request, Role, VoiceProfile};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;

const ROOM: &str = "!r:t";
const USER: &str = "@aki:t";
const USER_KEY: &str = "_@aki:t_PHONE_m.call";
const BOT_KEY: &str = "_@note:t_DEV_m.call";

struct Rig {
    _dir: tempfile::TempDir,
    hs: SharedHs,
    note: Option<FakeNote>,
    media: Arc<MediaProbe>,
}

impl Rig {
    fn note(&self) -> &FakeNote {
        self.note.as_ref().expect("this rig has Note")
    }
}

/// The service with `@aki:t` linked in `!r:t`, a 100 ms ready cue, and Note up if `with_note`.
async fn rig(with_note: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let (base, hs) = homeserver().await;
    let mut cfg = voice_config(dir.path(), base);
    let cue = dir.path().join("ready.pcm");
    std::fs::write(&cue, [0x10u8, 0x20].repeat(4800)).unwrap();
    cfg.ready_cue_file = Some(cue);
    std::fs::create_dir_all(&cfg.state_dir).unwrap();
    let state = json!({
        "links": { "1": { "mxid": USER, "room_id": ROOM, "reported": true } },
        "calls": {},
        "since": "s9",
    });
    std::fs::write(cfg.state_dir.join("state.json"), state.to_string()).unwrap();
    let note = with_note.then(|| fake_note(&cfg.socket));
    let (backends, media) = probed_backends(true, Join::Quiet);
    tokio::spawn(async move { note_voice::service::run_with(cfg, fast(Role::Voice), backends).await.unwrap() });
    if let Some(note) = &note {
        let p = note.peer.clone();
        eventually("voice connects", || p.is_up()).await;
    } else {
        let h = hs.clone();
        eventually("the sync loop runs", || !h.lock().unwrap().sync_sinces.is_empty()).await;
    }
    Rig { _dir: dir, hs, note, media }
}

fn ring_for_bot(sender: &str) -> Value {
    json!({
        "type": "org.matrix.msc4075.rtc.notification",
        "sender": sender,
        "event_id": "$ring-for-bot",
        "origin_server_ts": now_ms(),
        "content": { "notification_type": "ring", "m.mentions": { "user_ids": ["@note:t"] } },
    })
}

/// The user starts a call: their membership is set, and the sync delivers it and the ring that follows.
fn user_calls(r: &Rig, still_calling: bool) {
    let mut hs = r.hs.lock().unwrap();
    hs.member_state.insert((ROOM.into(), USER_KEY.into()), member_content(still_calling));
    hs.syncs.push_back(joined_room(ROOM, &[member_event(USER, true), ring_for_bot(USER)]));
}

fn note_opens(r: &Rig, call_id: &'static str) {
    r.note().rec.answer_with(move |q| match q {
        Request::IncomingCall { .. } => Ok(Reply::Call { call_id: call_id.into() }),
        _ => Ok(Reply::Done),
    });
}

fn incoming_calls(r: &Rig) -> Vec<Request> {
    let requests = r.note().rec.requests.lock().unwrap();
    requests.iter().filter(|q| matches!(q, Request::IncomingCall { .. })).cloned().collect()
}

fn inbound_start() -> CallBody {
    CallBody::Start {
        user_id: 1,
        room_id: ROOM.into(),
        mxid: USER.into(),
        title: "Call".into(),
        ring_secs: 0,
        ring_by_ms: 0,
        voice: VoiceProfile::default(),
        direction: Direction::Inbound,
    }
}

fn outcomes(r: &Rig, call: &str) -> Vec<Outcome> {
    r.note()
        .rec
        .bodies(call)
        .into_iter()
        .filter_map(|b| match b {
            CallBody::Outcome { outcome } => Some(outcome),
            _ => None,
        })
        .collect()
}

/// The bot's own membership puts, in order; `true` for one that is set.
fn bot_memberships(r: &Rig) -> Vec<bool> {
    let hs = r.hs.lock().unwrap();
    hs.state_puts.iter().filter(|(_, _, key, _)| key == BOT_KEY).map(|(_, _, _, body)| body != &json!({})).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_incoming_call_is_prepared_by_note_and_answered() {
    let r = rig(true).await;
    note_opens(&r, "c9");
    user_calls(&r, true);
    eventually("Note is asked to open the call", || !incoming_calls(&r).is_empty()).await;
    let Request::IncomingCall { room_id, mxid, key } = incoming_calls(&r).remove(0) else { unreachable!() };
    assert_eq!((room_id.as_str(), mxid.as_str()), (ROOM, USER));
    assert!(key.starts_with("$ev"), "the member event answers, not the ring that follows: {key}");

    r.note().peer.send_call("c9", inbound_start()).unwrap();
    eventually("answered", || !outcomes(&r, "c9").is_empty()).await;
    assert_eq!(outcomes(&r, "c9"), vec![Outcome::Answered]);
    assert_eq!(bot_memberships(&r), vec![true], "the bot's membership answers the call");
    assert_eq!(r.media.joins.load(Ordering::SeqCst), 1);
    assert!(r.hs.lock().unwrap().sends.is_empty(), "nothing rings");
    assert_eq!(incoming_calls(&r).len(), 1, "the ring for the bot is the same call");

    r.note().peer.send_call("c9", CallBody::HangUp).unwrap();
    let rec = r.note().rec.clone();
    eventually("Ended", || rec.bodies("c9").contains(&CallBody::Ended)).await;
    assert_eq!(bot_memberships(&r), vec![true, false]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_incoming_call_with_note_down_says_it_cant_reach() {
    let r = rig(false).await;
    user_calls(&r, true);
    let media = r.media.clone();
    eventually("the bot leaves the call", || media.left.load(Ordering::SeqCst)).await;
    assert_eq!(r.media.joins.load(Ordering::SeqCst), 1);
    assert!(r.media.frames_sent.load(Ordering::SeqCst) >= 10, "the ready cue plays");
    eventually("the membership is cleared", || bot_memberships(&r) == vec![true, false]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_abandoned_before_join_is_not_joined() {
    let r = rig(true).await;
    note_opens(&r, "c9");
    user_calls(&r, false);
    eventually("Note is asked to open the call", || !incoming_calls(&r).is_empty()).await;
    r.note().peer.send_call("c9", inbound_start()).unwrap();
    let rec = r.note().rec.clone();
    eventually("Ended", || rec.bodies("c9").contains(&CallBody::Ended)).await;
    assert_eq!(outcomes(&r, "c9"), vec![Outcome::Failed { reason: "the caller hung up".into() }]);
    assert_eq!(r.media.joins.load(Ordering::SeqCst), 0);
    assert!(bot_memberships(&r).iter().all(|set| !set), "the bot never answers");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_while_ringing_answers_instead() {
    let r = rig(true).await;
    let start = CallBody::Start {
        user_id: 1,
        room_id: ROOM.into(),
        mxid: USER.into(),
        title: "Check-in".into(),
        ring_secs: 30,
        ring_by_ms: now_ms() + 10_000,
        voice: VoiceProfile::default(),
        direction: Direction::Outbound,
    };
    r.note().peer.send_call("c1", start).unwrap();
    let hs = r.hs.clone();
    eventually("the ring is sent", || !hs.lock().unwrap().sends.is_empty()).await;
    user_calls(&r, true);
    eventually("answered", || !outcomes(&r, "c1").is_empty()).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(outcomes(&r, "c1"), vec![Outcome::Answered]);
    assert!(incoming_calls(&r).is_empty(), "no second call");
    assert_eq!(r.media.joins.load(Ordering::SeqCst), 1);
}
