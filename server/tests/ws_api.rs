mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use note_server::channels::mock::MockChannel;
use note_server::channels::Channel;
use note_server::{db, AppState};
use std::sync::Arc;
use tower::ServiceExt;

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
