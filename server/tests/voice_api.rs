mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::channels::mock::MockChannel;
use note_server::channels::voice::{self, VoiceChannel};
use note_server::channels::Channel;
use note_server::{api, auth, db, AppState};
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

const EVENT_ID: i64 = 1;

async fn app_with(
    cfg: &TempDir,
    twilio: &str,
    ladder: Vec<Arc<dyn Channel>>,
) -> (axum::Router, String, AppState) {
    let dir = cfg.path().to_path_buf();
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", true).unwrap();
    conn.execute("INSERT INTO plans (user_id, date, created_at) VALUES (1, '2026-09-17', 'now')", [])
        .unwrap();
    conn.execute(
        "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, channel)
         VALUES (1, 'checkin_call', '09:00', '09:00', 'voice')",
        [],
    )
    .unwrap();
    let mut state = AppState::new(conn, dir.clone(), dir.clone()).with_channels(ladder);
    let ch = VoiceChannel::new(
        dir.clone(),
        state.db.clone(),
        &common::voice_settings(&dir, twilio),
        common::VOICE_PUBLIC_BASE,
    )
    .unwrap();
    state = state.with_voice(ch);
    let app = api::router(state.clone());
    let cookie = common::login(&app, "aki", "pw").await;
    (app, cookie, state)
}

async fn plain(cfg: &TempDir) -> (axum::Router, String) {
    let dir = cfg.path().to_path_buf();
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", true).unwrap();
    let state = AppState::new(conn, dir.clone(), dir);
    let app = api::router(state);
    let cookie = common::login(&app, "aki", "pw").await;
    (app, cookie)
}

fn write_user(dir: &std::path::Path, content: &str) {
    let p = dir.join("users/aki/user.toml");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn seed_call(state: &AppState, token: &str, event_id: Option<i64>, message: &str) {
    let conn = state.db.lock().unwrap();
    conn.execute(
        "INSERT INTO voice_calls (token, user_id, event_id, message, created_at, updated_at)
         VALUES (?1, 1, ?2, ?3, ?4, ?4)",
        (token, event_id, message, jiff::Timestamp::now().to_string()),
    )
    .unwrap();
}

fn callback(path: &str, body: &str) -> Request<Body> {
    let params = voice::parse_form(body);
    let url = format!("{}{path}", common::VOICE_PUBLIC_BASE);
    signed(path, body, &voice::sign(common::TWILIO_TOKEN, &url, &params))
}

fn signed(path: &str, body: &str, signature: &str) -> Request<Body> {
    Request::post(path)
        .header("X-Twilio-Signature", signature)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn text(res: axum::response::Response) -> String {
    let body = res.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(body.to_vec()).unwrap()
}

async fn json(res: axum::response::Response) -> serde_json::Value {
    let body = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn event_status(state: &AppState) -> (String, String) {
    let conn = state.db.lock().unwrap();
    conn.query_row("SELECT status, wall_time FROM events WHERE id = ?1", [EVENT_ID], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
    .unwrap()
}

fn call_row(state: &AppState) -> (String, Option<String>) {
    let conn = state.db.lock().unwrap();
    conn.query_row("SELECT status, digit FROM voice_calls WHERE token = 'tok-1'", [], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
    .unwrap()
}

fn logged(state: &AppState, kind: &str) -> Option<String> {
    let conn = state.db.lock().unwrap();
    conn.query_row("SELECT detail FROM event_log WHERE kind = ?1", [kind], |r| r.get(0)).ok()
}

#[tokio::test]
async fn the_twiml_asks_the_question_and_escapes_what_it_says() {
    let cfg = common::config_dir();
    write_user(cfg.path(), "display_name = \"Aki & Co\"\n");
    let (app, _cookie, state) = app_with(&cfg, "http://127.0.0.1:1", Vec::new()).await;
    seed_call(&state, "tok-1", Some(EVENT_ID), "Ship the <draft> by 5.");

    let res = app.oneshot(callback("/api/voice/twiml/tok-1", "CallSid=CA1")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CONTENT_TYPE], "application/xml");
    let xml = text(res).await;
    assert!(xml.contains("action=\"/api/voice/gather/tok-1\""), "{xml}");
    assert!(xml.contains("Hi Aki &amp; Co."), "{xml}");
    assert!(xml.contains("Ship the &lt;draft&gt; by 5."), "{xml}");
    assert!(xml.contains("Press 1 if it is done"), "{xml}");
}

#[tokio::test]
async fn a_call_with_nothing_to_decide_just_speaks() {
    let cfg = common::config_dir();
    let (app, _cookie, state) = app_with(&cfg, "http://127.0.0.1:1", Vec::new()).await;
    seed_call(&state, "tok-1", None, "This is Note. Your phone is set up.");
    let xml = text(app.oneshot(callback("/api/voice/twiml/tok-1", "CallSid=CA1")).await.unwrap()).await;
    assert!(!xml.contains("<Gather"), "{xml}");
    assert!(xml.contains("Your phone is set up."), "{xml}");
}

#[tokio::test]
async fn one_finishes_the_event_and_says_so() {
    let cfg = common::config_dir();
    let (app, _cookie, state) = app_with(&cfg, "http://127.0.0.1:1", Vec::new()).await;
    seed_call(&state, "tok-1", Some(EVENT_ID), "Time for your check-in.");
    let res = app.oneshot(callback("/api/voice/gather/tok-1", "CallSid=CA1&Digits=1")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let xml = text(res).await;
    assert!(xml.contains("<Say>Got it.</Say>") && xml.contains("<Hangup/>"), "{xml}");
    assert_eq!(event_status(&state).0, "done");
    assert_eq!(call_row(&state), ("answered".into(), Some("1".into())));
    let detail = logged(&state, "voice_decision").unwrap();
    assert_eq!(detail, format!("event {EVENT_ID}: done by phone"));
}

#[tokio::test]
async fn two_snoozes_a_quarter_hour_and_three_drops_it() {
    let cfg = common::config_dir();
    let (app, _cookie, state) = app_with(&cfg, "http://127.0.0.1:1", Vec::new()).await;
    seed_call(&state, "tok-1", Some(EVENT_ID), "Time for your check-in.");
    app.clone()
        .oneshot(callback("/api/voice/gather/tok-1", "Digits=2"))
        .await
        .unwrap();
    assert_eq!(event_status(&state), ("snoozed".into(), "09:15".into()));

    seed_call(&state, "tok-2", Some(EVENT_ID), "Time for your check-in.");
    app.oneshot(callback("/api/voice/gather/tok-2", "Digits=3")).await.unwrap();
    assert_eq!(event_status(&state).0, "dropped");
}

#[tokio::test]
async fn an_unrecognised_digit_asks_once_more_and_then_lets_go() {
    let cfg = common::config_dir();
    let (app, _cookie, state) = app_with(&cfg, "http://127.0.0.1:1", Vec::new()).await;
    seed_call(&state, "tok-1", Some(EVENT_ID), "Time for your check-in.");

    let xml = text(
        app.clone().oneshot(callback("/api/voice/gather/tok-1", "Digits=9")).await.unwrap(),
    )
    .await;
    assert!(xml.contains("<Gather"), "the first stray press replays the question: {xml}");
    assert_eq!(event_status(&state).0, "pending");

    let xml = text(
        app.oneshot(callback("/api/voice/gather/tok-1", "Digits=8")).await.unwrap(),
    )
    .await;
    assert!(!xml.contains("<Gather"), "{xml}");
    assert!(xml.contains("I will check in again later."), "{xml}");
    assert_eq!(event_status(&state).0, "pending");
    assert!(logged(&state, "voice_decision").is_none());
}

#[tokio::test]
async fn a_keypad_decision_tells_the_open_clients_to_reload() {
    let cfg = common::config_dir();
    let (app, _cookie, state) = app_with(&cfg, "http://127.0.0.1:1", Vec::new()).await;
    seed_call(&state, "tok-1", Some(EVENT_ID), "Time for your check-in.");
    let (_id, mut rx) = state.hub.register(1).unwrap();
    app.oneshot(callback("/api/voice/gather/tok-1", "Digits=1")).await.unwrap();
    let frame: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
    assert_eq!(frame["type"], "changed");
}

#[tokio::test]
async fn a_request_twilio_did_not_sign_is_refused() {
    let cfg = common::config_dir();
    let (app, _cookie, state) = app_with(&cfg, "http://127.0.0.1:1", Vec::new()).await;
    seed_call(&state, "tok-1", Some(EVENT_ID), "Time for your check-in.");

    let res = app
        .clone()
        .oneshot(signed("/api/voice/gather/tok-1", "Digits=1", "0/KCTR6DLpKmkAf8muzZqo1nDgQ="))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // the signature covers the body, so the same one cannot carry another digit
    let honest = callback("/api/voice/gather/tok-1", "Digits=1");
    let stolen = honest.headers()["X-Twilio-Signature"].to_str().unwrap().to_string();
    let res = app
        .clone()
        .oneshot(signed("/api/voice/gather/tok-1", "Digits=3", &stolen))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(event_status(&state).0, "pending");

    let res = app.oneshot(Request::post("/api/voice/gather/tok-1").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_token_that_names_no_live_call_is_a_404() {
    let cfg = common::config_dir();
    let (app, _cookie, _state) = app_with(&cfg, "http://127.0.0.1:1", Vec::new()).await;
    let res = app.oneshot(callback("/api/voice/twiml/never-issued", "CallSid=CA1")).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_callbacks_are_not_mounted_without_a_voice_channel() {
    let cfg = common::config_dir();
    let (app, _cookie) = plain(&cfg).await;
    let res = app.oneshot(callback("/api/voice/twiml/tok-1", "CallSid=CA1")).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_phone_that_never_picked_up_sends_the_nudge_down_the_ladder() {
    let cfg = common::config_dir();
    let push = Arc::new(MockChannel::new("push"));
    let ladder: Vec<Arc<dyn Channel>> = vec![push.clone()];
    let (app, _cookie, state) = app_with(&cfg, "http://127.0.0.1:1", ladder).await;
    seed_call(&state, "tok-1", Some(EVENT_ID), "Time for your 09:00 check-in.");

    let res = app
        .oneshot(callback("/api/voice/status/tok-1", "CallSid=CA1&CallStatus=no-answer"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(call_row(&state).0, "no_answer");
    let seen = push.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1.title, "Check-in");
    assert_eq!(seen[0].1.body, "Time for your 09:00 check-in.");
    assert_eq!(seen[0].1.event_id, Some(EVENT_ID));
    assert!(logged(&state, "voice_unanswered").unwrap().contains("no-answer"));
}

#[tokio::test]
async fn a_call_that_ran_its_course_only_moves_the_row() {
    let cfg = common::config_dir();
    let push = Arc::new(MockChannel::new("push"));
    let ladder: Vec<Arc<dyn Channel>> = vec![push.clone()];
    let (app, _cookie, state) = app_with(&cfg, "http://127.0.0.1:1", ladder).await;
    seed_call(&state, "tok-1", Some(EVENT_ID), "Time for your check-in.");

    for (reported, stored) in [("ringing", "ringing"), ("in-progress", "answered"), ("completed", "completed")] {
        let res = app
            .clone()
            .oneshot(callback("/api/voice/status/tok-1", &format!("CallStatus={reported}")))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert_eq!(call_row(&state).0, stored);
    }
    assert!(push.seen().is_empty());
    assert!(logged(&state, "voice_unanswered").is_none());
}

#[tokio::test]
async fn the_test_call_rings_the_users_number() {
    let cfg = common::config_dir();
    write_user(cfg.path(), "phone_number = \"+819012345678\"\n");
    let (twilio, rx) = common::one_shot("201 Created", r#"{"sid":"CA7"}"#);
    let (app, cookie, state) = app_with(&cfg, &twilio, Vec::new()).await;

    let res = app
        .oneshot(
            Request::post("/api/notify/call")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let call_id = json(res).await["call_id"].as_i64().unwrap();
    let raw = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(raw.contains("To=%2B819012345678"), "call request: {raw}");
    let conn = state.db.lock().unwrap();
    let (sid, event): (Option<String>, Option<i64>) = conn
        .query_row("SELECT call_sid, event_id FROM voice_calls WHERE id = ?1", [call_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(sid.as_deref(), Some("CA7"));
    assert!(event.is_none(), "a test call decides nothing");
}

#[tokio::test]
async fn a_test_call_needs_a_number_and_reports_a_refusal() {
    let cfg = common::config_dir();
    let (app, cookie, _state) = app_with(&cfg, "http://127.0.0.1:1", Vec::new()).await;
    let request = || {
        Request::post("/api/notify/call").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()
    };
    let res = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert!(json(res).await["error"].is_string());

    write_user(cfg.path(), "phone_number = \"+819012345678\"\n");
    let res = app.oneshot(request()).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    assert!(json(res).await["error"].is_string());
}

#[tokio::test]
async fn the_test_call_route_needs_a_session_and_a_channel() {
    let cfg = common::config_dir();
    let (app, _cookie, _state) = app_with(&cfg, "http://127.0.0.1:1", Vec::new()).await;
    let res = app.oneshot(Request::post("/api/notify/call").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let cfg = common::config_dir();
    let (app, cookie) = plain(&cfg).await;
    let res = app
        .oneshot(
            Request::post("/api/notify/call")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
