mod common;

use axum::body::Body;
use axum::http::{header, Request as HttpRequest, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
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
        checkin: false,
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
        links::set_room(&conn, id, "!r:t").unwrap();
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

async fn api_rig() -> (axum::Router, String, Rig) {
    let r = rig(false).await;
    let app = note_server::api::router(r.state.clone());
    let cookie = common::login(&app, "aki", "pw").await;
    (app, cookie, r)
}

async fn call(app: &axum::Router, cookie: &str, method: &str, uri: &str, body: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method(method)
                .uri(uri)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

#[tokio::test(flavor = "multi_thread")]
async fn linking_opens_a_dm_and_settings_show_it() {
    let (app, cookie, r) = api_rig().await;
    r.fake_rec.answer_with(|req| match req {
        note_voice_proto::Request::OpenDm { link_id, .. } => {
            Ok(note_voice_proto::Reply::Dm { room_id: format!("!dm{link_id}:t") })
        }
        _ => {
            Err(note_voice_proto::Refusal::new(note_voice_proto::RefusalCode::BadRequest, "no"))
        }
    });
    let (status, body) = call(&app, &cookie, "POST", "/api/voice/link", r#"{"mxid":"@aki:t"}"#).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "invited");
    let (_, s) = call(&app, &cookie, "GET", "/api/settings", "").await;
    assert_eq!(s["voice_enabled"], true);
    assert_eq!(s["voice_link"]["mxid"], "@aki:t");
    assert_eq!(s["ring_for"], "urgent");

    let (status, _) = call(&app, &cookie, "PUT", "/api/settings", r#"{"ring_for":"never"}"#).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, &cookie, "PUT", "/api/settings", r#"{"ring_for":"sometimes"}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = call(&app, &cookie, "DELETE", "/api/voice/link", "").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, s) = call(&app, &cookie, "GET", "/api/settings", "").await;
    assert!(s["voice_link"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_matrix_id_is_refused_before_anything_is_stored() {
    let (app, cookie, r) = api_rig().await;
    for bad in [r#"{"mxid":"aki"}"#, r#"{"mxid":"@aki"}"#, r#"{"mxid":"@a ki:t"}"#] {
        let (status, _) = call(&app, &cookie, "POST", "/api/voice/link", bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert!(links::get(&r.state.db(), 1).unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_test_ring_needs_a_joined_link() {
    let (app, cookie, r) = api_rig().await;
    let (status, _) = call(&app, &cookie, "POST", "/api/voice/test", "").await;
    assert_eq!(status, StatusCode::CONFLICT);
    {
        let conn = r.state.db();
        let id = links::begin(&conn, 1, "@aki:t", jiff::Timestamp::now()).unwrap();
        links::set_room(&conn, id, "!r:t").unwrap();
        links::mark_joined(&conn, id, "!r:t", jiff::Timestamp::now()).unwrap();
    }
    let (status, body) = call(&app, &cookie, "POST", "/api/voice/test", "").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let rec = r.fake_rec.clone();
    eventually("the test ring starts", || !rec.seen.lock().unwrap().is_empty()).await;
    let (status, body) = call(&app, &cookie, "POST", "/api/voice/test", "").await;
    assert_eq!(status, StatusCode::CONFLICT, "a second ring while the first is out");
    assert_eq!(body["error"], "Already ringing");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_invite_leaves_no_link_behind() {
    let (app, cookie, r) = api_rig().await;
    r.fake_rec.answer_with(|_| {
        Err(note_voice_proto::Refusal::new(note_voice_proto::RefusalCode::Failed, "homeserver down"))
    });
    let (status, _) = call(&app, &cookie, "POST", "/api/voice/link", r#"{"mxid":"@aki:t"}"#).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(links::get(&r.state.db(), 1).unwrap().is_none());
    let (_, s) = call(&app, &cookie, "GET", "/api/settings", "").await;
    assert!(s["voice_link"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_removed_while_the_invite_is_out_is_not_reported_invited() {
    let (app, cookie, r) = api_rig().await;
    let db = r.state.db.clone();
    r.fake_rec.answer_with(move |req| match req {
        note_voice_proto::Request::OpenDm { .. } => {
            links::remove(&db.lock().unwrap(), 1).unwrap();
            Ok(note_voice_proto::Reply::Dm { room_id: "!dm:t".into() })
        }
        _ => {
            Err(note_voice_proto::Refusal::new(note_voice_proto::RefusalCode::BadRequest, "no"))
        }
    });
    let (status, body) = call(&app, &cookie, "POST", "/api/voice/link", r#"{"mxid":"@aki:t"}"#).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(links::get(&r.state.db(), 1).unwrap().is_none());
}

fn offers_voices(r: &Rig) {
    r.fake_rec.answer_with(|req| match req {
        note_voice_proto::Request::ListVoices { language } => Ok(note_voice_proto::Reply::Voices {
            voices: ["af_heart", "bm_george"]
                .map(|id| note_voice_proto::VoiceOption { id: id.into(), label: id.into(), language: language.clone() })
                .to_vec(),
        }),
        note_voice_proto::Request::Preview { voice, .. } => {
            use base64::Engine as _;
            let wav = format!("RIFF{voice}");
            Ok(note_voice_proto::Reply::Audio { wav_base64: base64::engine::general_purpose::STANDARD.encode(wav) })
        }
        _ => Err(note_voice_proto::Refusal::new(note_voice_proto::RefusalCode::BadRequest, "no")),
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn ring_for_checkins_is_saved() {
    let (app, cookie, _r) = api_rig().await;
    let (status, body) = call(&app, &cookie, "PUT", "/api/settings", r#"{"ring_for":"checkins"}"#).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ring_for"], "checkins");
    let (_, s) = call(&app, &cookie, "GET", "/api/settings", "").await;
    assert_eq!(s["ring_for"], "checkins");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_an_offered_voice_is_saved_and_a_ring_carries_it() {
    let (app, cookie, r) = api_rig().await;
    offers_voices(&r);
    let (_, s) = call(&app, &cookie, "GET", "/api/settings", "").await;
    assert_eq!((&s["voice_voice"], &s["voice_cue"]), (&serde_json::json!(""), &serde_json::json!(true)));
    let (status, _) = call(&app, &cookie, "PUT", "/api/settings", r#"{"voice_voice":"nope"}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) =
        call(&app, &cookie, "PUT", "/api/settings", r#"{"voice_voice":"bm_george","voice_cue":false}"#).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!((&body["voice_voice"], &body["voice_cue"]), (&serde_json::json!("bm_george"), &serde_json::json!(false)));
    let (_, voices) = call(&app, &cookie, "GET", "/api/voice/voices", "").await;
    assert_eq!(voices["voices"][1], serde_json::json!({ "id": "bm_george", "label": "bm_george" }));

    {
        let conn = r.state.db();
        let id = links::begin(&conn, 1, "@aki:t", jiff::Timestamp::now()).unwrap();
        links::set_room(&conn, id, "!r:t").unwrap();
        links::mark_joined(&conn, id, "!r:t", jiff::Timestamp::now()).unwrap();
    }
    let (status, _) = call(&app, &cookie, "POST", "/api/voice/test", "").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let rec = r.fake_rec.clone();
    eventually("the ring starts", || !rec.seen.lock().unwrap().is_empty()).await;
    let body = r.fake_rec.seen.lock().unwrap()[0].2.clone();
    let CallBody::Start { voice, .. } = body else { panic!("{body:?}") };
    assert_eq!((voice.voice.as_str(), voice.cue, voice.language.as_str()), ("bm_george", false, "en"));

    let (status, body) = call(&app, &cookie, "PUT", "/api/settings", r#"{"voice_voice":""}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["voice_voice"], "");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_preview_is_served_as_wav() {
    let (app, cookie, r) = api_rig().await;
    offers_voices(&r);
    let res = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .uri("/api/voice/preview?voice=af_heart")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CONTENT_TYPE], "audio/wav");
    assert_eq!(res.headers()[header::CACHE_CONTROL], "private, max-age=86400");
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"RIFFaf_heart");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answered_incoming_call_carries_the_users_voice() {
    let (app, cookie, r) = api_rig().await;
    offers_voices(&r);
    let (status, _) = call(&app, &cookie, "PUT", "/api/settings", r#"{"voice_voice":"af_heart"}"#).await;
    assert_eq!(status, StatusCode::OK);
    {
        let conn = r.state.db();
        let id = links::begin(&conn, 1, "@aki:t", jiff::Timestamp::now()).unwrap();
        links::set_room(&conn, id, "!r:t").unwrap();
        links::mark_joined(&conn, id, "!r:t", jiff::Timestamp::now()).unwrap();
    }
    let incoming = note_voice_proto::Request::IncomingCall { room_id: "!r:t".into(), mxid: "@aki:t".into(), key: "$ev".into() };
    let reply = r.fake.request(incoming).await;
    assert!(matches!(reply, Ok(note_voice_proto::Reply::Call { .. })), "{reply:?}");
    let rec = r.fake_rec.clone();
    eventually("the Start arrives", || !rec.seen.lock().unwrap().is_empty()).await;
    let body = r.fake_rec.seen.lock().unwrap()[0].2.clone();
    let CallBody::Start { voice, .. } = body else { panic!("{body:?}") };
    assert_eq!(voice.voice, "af_heart");
}
