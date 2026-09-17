mod common;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::channels::mock::MockChannel;
use note_server::channels::{self, Channel};
use note_server::providers::{mock::MockLLM, ChatResponse, Message};
use note_server::{plan, runner, templates, AppState};
use std::sync::Arc;
use tower::ServiceExt;

async fn json(res: axum::response::Response) -> serde_json::Value {
    let body = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

async fn get(app: &axum::Router, path: &str, cookie: &str) -> axum::response::Response {
    app.clone()
        .oneshot(Request::get(path).header(header::COOKIE, cookie).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn talk(app: &axum::Router, cookie: &str, body: serde_json::Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// A plan for 2026-09-17 with one 09:00 check-in and one 15:30 check-in, both
/// already due at `now`, delivered through a mock channel below the socket.
fn fire_checkins(state: &AppState, mock: &Arc<MockChannel>) -> Vec<runner::FiredEvent> {
    let date: jiff::civil::Date = "2026-09-17".parse().unwrap();
    let all = || vec!["mon".into(), "tue".into(), "wed".into(), "thu".into(), "fri".into(), "sat".into(), "sun".into()];
    let template = templates::Template {
        events: vec![
            templates::TemplateEvent {
                kind: "checkin".into(), time: "09:00".into(), days: all(),
                flexibility: Some("fixed".into()), channel: "push".into(), ..Default::default()
            },
            templates::TemplateEvent {
                kind: "checkin_call".into(), time: "15:30".into(), days: all(),
                flexibility: Some("fixed".into()), channel: "voice".into(), ..Default::default()
            },
        ],
    };
    let fired = {
        let conn = state.db.lock().unwrap();
        plan::generate(&conn, 1, &template, date).unwrap();
        conn.execute(
            "UPDATE events SET message = 'Afternoon. Where did the morning go?' WHERE wall_time = '15:30'",
            [],
        )
        .unwrap();
        let now: jiff::Timestamp = "2026-09-17T16:00:00Z".parse().unwrap();
        runner::fire_due(&conn, &state.config_dir, now).unwrap()
    };
    let ladder: Vec<Arc<dyn Channel>> = vec![mock.clone()];
    for ev in &fired {
        channels::deliver_event(&state.db, &ladder, ev);
    }
    fired
}

#[tokio::test]
async fn the_days_checkins_land_in_one_thread_the_api_lists() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let mock = Arc::new(MockChannel::new("mock"));
    let fired = fire_checkins(&state, &mock);
    assert_eq!(fired.len(), 2);

    let seen = mock.seen();
    let id = seen[0].1.conversation_id.expect("the morning check-in opened a thread");
    assert_eq!(seen[1].1.conversation_id, Some(id));

    let list = json(get(&app, "/api/conversations", &cookie).await).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], id);
    assert!(list[0]["title"].as_str().unwrap().contains("09:00"), "{}", list[0]["title"]);

    let rows = json(get(&app, &format!("/api/conversations/{id}/messages"), &cookie).await).await;
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r["role"] == "assistant"));
    assert!(rows[0]["content"].as_str().unwrap().contains("09:00"));
    assert_eq!(rows[1]["content"], "Afternoon. Where did the morning go?");
}

#[tokio::test]
async fn a_reply_continues_the_thread_and_the_session_knows_it_was_a_checkin() {
    let llm = Arc::new(MockLLM::scripted(vec![ChatResponse {
        text: "Glad the morning went. What is next?".into(),
        tool_calls: vec![],
    }]));
    let (app, cookie, state, _cfg) =
        common::app_with_logged_in_user_llm_and_state(llm.clone()).await;
    let mock = Arc::new(MockChannel::new("mock"));
    fire_checkins(&state, &mock);
    let id = mock.seen()[0].1.conversation_id.unwrap();

    let res = talk(
        &app,
        &cookie,
        serde_json::json!({ "message": "went fine, finished the essay", "conversation_id": id }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = json(res).await;
    assert_eq!(v["conversation_id"], id);
    assert_eq!(v["reply"], "Glad the morning went. What is next?");

    let seen = llm.seen();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].system.contains("# This conversation"), "{}", seen[0].system);
    assert!(seen[0].system.contains("check-in on 2026-09-17"), "{}", seen[0].system);
    assert!(seen[0].system.contains("# Today's plan"), "the standing context still comes first");
    assert_eq!(seen[0].n_messages, 3, "both questions precede the reply");
    assert!(matches!(&seen[0].messages[0], Message::Assistant { text, .. } if text.contains("09:00")));
    assert!(matches!(&seen[0].messages[2], Message::User(t) if t == "went fine, finished the essay"));

    let rows = json(get(&app, &format!("/api/conversations/{id}/messages"), &cookie).await).await;
    let roles: Vec<&str> = rows.as_array().unwrap().iter().map(|r| r["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["assistant", "assistant", "user", "assistant"]);
}

#[tokio::test]
async fn a_thread_the_user_started_carries_no_checkin_note() {
    let llm = Arc::new(MockLLM::scripted(vec![
        ChatResponse { text: "hi".into(), tool_calls: vec![] },
        ChatResponse { text: "still here".into(), tool_calls: vec![] },
    ]));
    let (app, cookie, _cfg) = common::app_with_logged_in_user_and_llm(llm.clone()).await;
    let first = json(talk(&app, &cookie, serde_json::json!({ "message": "hello" })).await).await;
    let id = first["conversation_id"].as_i64().unwrap();
    talk(&app, &cookie, serde_json::json!({ "message": "again", "conversation_id": id })).await;
    for chat in llm.seen() {
        assert!(!chat.system.contains("# This conversation"), "{}", chat.system);
    }
}
