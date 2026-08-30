mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use note_server::channels::mock::MockChannel;
use note_server::channels::webpush::{public_key_b64, WebPushChannel};
use note_server::channels::Channel;
use note_server::{db, AppState};
use std::sync::Arc;
use tower::ServiceExt;

// Throwaway P-256 key generated for these tests only — never a deployment key.
const TEST_PEM: &[u8] = b"-----BEGIN EC PRIVATE KEY-----
MHcCAQEEIHmZ5O6AfVwy/vYIs4KDabU6mZnBmFw1RV7wfeQ0LB7goAoGCCqGSM49
AwEHoUQDQgAEA7nqkgOVsRzMWh/T0AnwWx4Nep2dfQAns3Mn1OkO/t/+V/Voqszi
v5mC8db8ZSK9ruR2mEgvMEvePYwohpr98g==
-----END EC PRIVATE KEY-----
";

#[tokio::test]
async fn ws_route_requires_a_session() {
    let (app, _cookie, _cfg) = common::app_with_logged_in_user().await;
    let res = app
        .oneshot(Request::get("/api/ws").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn default_ladder_is_ws_only_and_with_channels_replaces_it() {
    let cfg = common::config_dir();
    let dir = cfg.path().to_path_buf();
    let state = AppState::new(db::open_memory().unwrap(), dir.clone(), dir);
    assert_eq!(state.channels.len(), 1);
    assert_eq!(state.channels[0].name(), "ws");

    let mock: Arc<dyn Channel> = Arc::new(MockChannel::new("mock"));
    let state = state.with_channels(vec![mock]);
    assert_eq!(state.channels.len(), 1);
    assert_eq!(state.channels[0].name(), "mock");
}

#[test]
fn with_webpush_appends_below_ws_and_sets_the_vapid_key() {
    let cfg = common::config_dir();
    let dir = cfg.path().to_path_buf();
    let state = AppState::new(db::open_memory().unwrap(), dir.clone(), dir);
    let ch = WebPushChannel::new(
        state.db.clone(),
        TEST_PEM.to_vec(),
        "mailto:admin@example.com".into(),
    )
    .unwrap();
    let key = public_key_b64(TEST_PEM).unwrap();

    let state = state.with_webpush(ch, key.clone());
    assert_eq!(state.channels.len(), 2);
    assert_eq!(state.channels[0].name(), "ws");
    assert_eq!(state.channels[1].name(), "webpush");
    assert_eq!(state.vapid_public_key.as_deref(), Some(key.as_str()));
}
