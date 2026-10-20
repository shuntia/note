mod common;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::providers::{mock::MockLLM, ChatResponse, Effort};
use note_server::{auth, talk};
use std::sync::Arc;
use tower::ServiceExt;

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null))
}

fn json_req(method: &str, path: &str, cookie: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(path: &str, cookie: &str) -> Request<Body> {
    Request::get(path).header(header::COOKIE, cookie).body(Body::empty()).unwrap()
}

fn offering() -> Arc<MockLLM> {
    let replies = (0..4).map(|_| ChatResponse { text: "ok".into(), tool_calls: vec![] }).collect();
    Arc::new(MockLLM::scripted(replies).offering(&["base/model", "deepseek/deepseek-v4.1-flash"]))
}

fn seed(state: &note_server::AppState, user_id: i64) -> i64 {
    let conn = state.db();
    let at = jiff::Timestamp::now();
    let id = talk::create(&conn, user_id, "t", at).unwrap();
    talk::append_text(&conn, id, "user", "hi", at).unwrap();
    id
}

#[tokio::test]
async fn models_lists_the_default_and_the_extras() {
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_llm_and_state(offering()).await;
    let (status, v) = send(&app, get("/api/models", &cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["default"], "base/model");
    assert_eq!(v["models"], serde_json::json!(["base/model", "deepseek/deepseek-v4.1-flash"]));
    assert_eq!(v["efforts"], serde_json::json!(["off", "low", "medium", "high"]));
    assert!(v["effort_default"].is_null());
    assert_eq!(send(&app, Request::get("/api/models").body(Body::empty()).unwrap()).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_conversation_pick_is_validated_owned_and_cleared_by_null() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_llm_and_state(offering()).await;
    let mine = seed(&state, 1);
    let other_user = auth::create_user(&state.db(), "aitest", "aitest-pw-2026", false).unwrap();
    let theirs = seed(&state, other_user);
    let path = |id: i64| format!("/api/conversations/{id}/model");
    let pick = serde_json::json!({"model": "deepseek/deepseek-v4.1-flash", "effort": "high"});

    let (status, v) = send(&app, json_req("PUT", &path(mine), &cookie, &pick)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v, pick);
    let (_, list) = send(&app, get("/api/conversations", &cookie)).await;
    assert_eq!(list[0]["model"], "deepseek/deepseek-v4.1-flash");
    assert_eq!(list[0]["effort"], "high");

    let unknown = serde_json::json!({"model": "someone/else", "effort": null});
    assert_eq!(send(&app, json_req("PUT", &path(mine), &cookie, &unknown)).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let junk = serde_json::json!({"model": null, "effort": "max"});
    assert_eq!(send(&app, json_req("PUT", &path(mine), &cookie, &junk)).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(send(&app, json_req("PUT", &path(theirs), &cookie, &pick)).await.0, StatusCode::NOT_FOUND);
    assert_eq!(send(&app, json_req("PUT", &path(9999), &cookie, &pick)).await.0, StatusCode::NOT_FOUND);

    let clear = serde_json::json!({"model": null, "effort": null});
    assert_eq!(send(&app, json_req("PUT", &path(mine), &cookie, &clear)).await.0, StatusCode::OK);
    let (_, list) = send(&app, get("/api/conversations", &cookie)).await;
    assert!(list[0]["model"].is_null() && list[0]["effort"].is_null());
}

#[tokio::test]
async fn settings_hold_a_default_pick_that_null_clears() {
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_llm_and_state(offering()).await;
    let (_, v) = send(&app, get("/api/settings", &cookie)).await;
    assert!(v["model"].is_null() && v["effort"].is_null());
    let set = serde_json::json!({"model": "deepseek/deepseek-v4.1-flash", "effort": "low"});
    let (status, v) = send(&app, json_req("PUT", "/api/settings", &cookie, &set)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((v["model"].as_str(), v["effort"].as_str()), (Some("deepseek/deepseek-v4.1-flash"), Some("low")));
    let bad = serde_json::json!({"model": "nope/nope"});
    assert_eq!(send(&app, json_req("PUT", "/api/settings", &cookie, &bad)).await.0, StatusCode::BAD_REQUEST);
    let (_, v) = send(&app, json_req("PUT", "/api/settings", &cookie, &serde_json::json!({"display_name": "Y"}))).await;
    assert_eq!(v["model"], "deepseek/deepseek-v4.1-flash", "an absent field leaves the pick alone");
    let (_, v) = send(&app, json_req("PUT", "/api/settings", &cookie, &serde_json::json!({"model": null, "effort": null}))).await;
    assert!(v["model"].is_null() && v["effort"].is_null());
}

#[tokio::test]
async fn a_talk_turn_asks_for_the_thread_pick_over_the_user_default() {
    let llm = offering();
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm.clone()).await;
    let set = serde_json::json!({"model": "base/model", "effort": "low"});
    send(&app, json_req("PUT", "/api/settings", &cookie, &set)).await;

    let first = serde_json::json!({"message": "hi", "model": "deepseek/deepseek-v4.1-flash", "effort": "off"});
    let (status, v) = send(&app, json_req("POST", "/api/talk", &cookie, &first)).await;
    assert_eq!(status, StatusCode::OK);
    let id = v["conversation_id"].as_i64().unwrap();
    let seen = llm.seen();
    let round = seen.iter().find(|c| !c.plain).unwrap();
    assert_eq!((round.model.as_deref(), round.effort), (Some("deepseek/deepseek-v4.1-flash"), Some(Effort::Off)));

    let clear = serde_json::json!({"model": null, "effort": null});
    send(&app, json_req("PUT", &format!("/api/conversations/{id}/model"), &cookie, &clear)).await;
    let next = serde_json::json!({"message": "again", "conversation_id": id});
    assert_eq!(send(&app, json_req("POST", "/api/talk", &cookie, &next)).await.0, StatusCode::OK);
    let last = llm.seen().into_iter().rev().find(|c| !c.plain).unwrap();
    assert_eq!((last.model.as_deref(), last.effort), (Some("base/model"), Some(Effort::Low)));
}
