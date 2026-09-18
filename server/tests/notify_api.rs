mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::channels::mock::MockChannel;
use note_server::channels::Channel;
use note_server::{api, auth, db, AppState};
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

async fn app_with(
    cfg: &TempDir,
    build: impl FnOnce(AppState) -> AppState,
) -> (axum::Router, String, AppState) {
    let dir = cfg.path().to_path_buf();
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", true).unwrap();
    let state = build(AppState::new(conn, dir.clone(), dir));
    let app = api::router(state.clone());
    let cookie = common::login(&app, "aki", "pw").await;
    (app, cookie, state)
}

fn test_request(cookie: Option<&str>) -> Request<Body> {
    let req = Request::post("/api/notify/test");
    match cookie {
        Some(c) => req.header(header::COOKIE, c),
        None => req,
    }
    .body(Body::empty())
    .unwrap()
}

async fn json(res: axum::response::Response) -> serde_json::Value {
    let body = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn logged(state: &AppState, kind: &str) -> Option<String> {
    let conn = state.db.lock().unwrap();
    conn.query_row("SELECT detail FROM event_log WHERE kind = ?1", [kind], |r| r.get(0)).ok()
}

#[tokio::test]
async fn the_test_notification_names_the_channel_that_took_it() {
    let cfg = common::config_dir();
    let ws = Arc::new(MockChannel::new("ws"));
    ws.set_fail(true);
    let webpush = Arc::new(MockChannel::new("webpush"));
    let ladder: Vec<Arc<dyn Channel>> = vec![ws, webpush.clone()];
    let (app, cookie, state) = app_with(&cfg, |s| s.with_channels(ladder)).await;

    let res = app.oneshot(test_request(Some(&cookie))).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(json(res).await["via"], "webpush");

    let seen = webpush.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1.title, "Note");
    assert_eq!(seen[0].1.body, "Test notification");
    assert_eq!(seen[0].1.event_id, None);
    let detail = logged(&state, "delivery_ok").unwrap();
    assert!(detail.contains("via webpush"), "unexpected detail: {detail}");
}

#[tokio::test]
async fn a_test_no_channel_can_take_is_a_502() {
    let cfg = common::config_dir();
    let (app, cookie, state) = app_with(&cfg, |s| s.with_channels(Vec::new())).await;
    let res = app.oneshot(test_request(Some(&cookie))).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    assert!(json(res).await["error"].is_string());
    assert!(logged(&state, "delivery_degraded").is_some());
}

#[tokio::test]
async fn the_test_route_needs_a_session() {
    let cfg = common::config_dir();
    let (app, _cookie, _state) = app_with(&cfg, |s| s).await;
    let res = app.oneshot(test_request(None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
