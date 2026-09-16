mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::channels::mock::MockChannel;
use note_server::channels::ntfy::NtfyChannel;
use note_server::channels::Channel;
use note_server::config::NtfySettings;
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
    let ntfy = Arc::new(MockChannel::new("ntfy"));
    let ladder: Vec<Arc<dyn Channel>> = vec![ws, ntfy.clone()];
    let (app, cookie, state) = app_with(&cfg, |s| s.with_channels(ladder)).await;

    let res = app.oneshot(test_request(Some(&cookie))).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(json(res).await["via"], "ntfy");

    let seen = ntfy.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1.title, "Note");
    assert_eq!(seen[0].1.body, "Test notification");
    assert_eq!(seen[0].1.event_id, None);
    let detail = logged(&state, "delivery_ok").unwrap();
    assert!(detail.contains("via ntfy"), "unexpected detail: {detail}");
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

#[tokio::test]
async fn settings_report_the_configured_prefix_as_the_default_topic() {
    let cfg = common::config_dir();
    let settings = NtfySettings {
        base_url: "http://127.0.0.1:1".into(),
        token_file: std::path::PathBuf::new(),
        topic_prefix: "plan-".into(),
    };
    let dir = cfg.path().to_path_buf();
    let (app, cookie, _state) = app_with(&cfg, move |s| {
        let ch = NtfyChannel::new(dir, &settings, "http://localhost:3271").unwrap();
        s.with_ntfy(ch, settings.topic_prefix.clone())
    })
    .await;

    let res = app
        .oneshot(
            Request::get("/api/settings")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = json(res).await;
    assert_eq!(v["ntfy_enabled"], true);
    assert_eq!(v["ntfy_topic"], "plan-aki");
}
