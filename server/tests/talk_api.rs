mod common;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::providers::{mock::MockLLM, ChatResponse, ToolCall};
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn talk_runs_a_session_and_returns_the_reply() {
    let llm = Arc::new(MockLLM::scripted(vec![
        ChatResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "task_create".into(),
                args: r#"{"title":"call mom"}"#.into(),
            }],
        },
        ChatResponse { text: "done — added call mom".into(), tool_calls: vec![] },
        ChatResponse { text: "anything else?".into(), tool_calls: vec![] },
    ]));
    let (app, cookie, _cfg) = common::app_with_logged_in_user_and_llm(llm).await;

    let res = app
        .clone()
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"remind me to call mom"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["reply"], "done — added call mom");

    // the finished session must have released its gate slot
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"thanks"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .clone()
        .oneshot(
            Request::get("/api/tasks")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v[0]["title"], "call mom");
}

#[tokio::test]
async fn empty_message_is_400_and_no_cookie_is_401() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"   "}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["error"].as_str().unwrap().contains("16384"));

    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn concurrent_talk_for_same_user_is_conflict() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let user_id: i64 = {
        let conn = state.db.lock().unwrap();
        conn.query_row("SELECT id FROM users LIMIT 1", [], |r| r.get(0)).unwrap()
    };
    let _permit = state.talk_gate.clone().try_enter(user_id).unwrap();
    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"], "a reply is already in progress");
}

#[tokio::test]
async fn talk_at_global_capacity_is_service_unavailable() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let permits: Vec<_> = (0..note_server::MAX_CONCURRENT_TALKS)
        .map(|i| state.talk_gate.try_enter(-1 - i as i64).unwrap())
        .collect();
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(res.headers()[header::RETRY_AFTER], "5");

    // the rejected user was rolled back out of the active set
    drop(permits);
    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

struct FailingLLM;
impl note_server::providers::LLMProvider for FailingLLM {
    fn chat(
        &self,
        _: &note_server::providers::ChatRequest,
    ) -> anyhow::Result<note_server::providers::ChatResponse> {
        anyhow::bail!("provider down")
    }
}

#[tokio::test]
async fn provider_failure_is_bad_gateway_and_logged() {
    let (app, cookie, state, _cfg) =
        common::app_with_logged_in_user_llm_and_state(Arc::new(FailingLLM)).await;
    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["error"].as_str().unwrap().contains("unavailable"));

    let logged: i64 = {
        let conn = state.db.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM event_log WHERE kind='talk_error'", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(logged, 1);
}

#[tokio::test]
async fn oversized_message_is_bad_request_with_body() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let big = "a".repeat(16 * 1024 + 1);
    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({ "message": big }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["error"].as_str().unwrap().contains("16384"));
}

#[tokio::test]
async fn blank_reply_is_replaced_with_the_canned_line() {
    let llm =
        Arc::new(MockLLM::scripted(vec![ChatResponse { text: "   ".into(), tool_calls: vec![] }]));
    let (app, cookie, _cfg) = common::app_with_logged_in_user_and_llm(llm).await;
    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["reply"], note_server::EMPTY_REPLY_FALLBACK);
}
