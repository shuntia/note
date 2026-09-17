mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use note_server::channels::mock::MockChannel;
use note_server::channels::ntfy::NtfyChannel;
use note_server::channels::webpush::{public_key_b64, WebPushChannel};
use note_server::config::NtfySettings;
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

/// The headers a browser sends to open a socket; without them the upgrade
/// extractor answers before any of our own checks run.
fn upgrade(uri: &str) -> axum::http::request::Builder {
    Request::get(uri)
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
}

#[tokio::test]
async fn a_cross_site_upgrade_is_refused() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    for site in ["cross-site", "same-site"] {
        let res = app
            .clone()
            .oneshot(
                upgrade("/api/ws")
                    .header("cookie", cookie.clone())
                    .header("sec-fetch-site", site)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{site}");
    }
}

#[tokio::test]
async fn a_user_at_the_socket_cap_is_refused_before_the_upgrade() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let _held: Vec<_> = (0..note_server::channels::ws::MAX_PER_USER)
        .map(|_| state.hub.register(1).unwrap())
        .collect();
    let res = app
        .oneshot(
            upgrade("/api/ws")
                .header("cookie", cookie)
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
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

#[test]
fn with_ntfy_appends_below_web_push() {
    let cfg = common::config_dir();
    let dir = cfg.path().to_path_buf();
    let state = AppState::new(db::open_memory().unwrap(), dir.clone(), dir.clone());
    let wp = WebPushChannel::new(
        state.db.clone(),
        TEST_PEM.to_vec(),
        "mailto:admin@example.com".into(),
    )
    .unwrap();
    let settings = NtfySettings {
        base_url: "http://127.0.0.1:2586".into(),
        token_file: std::path::PathBuf::new(),
        topic_prefix: "note-".into(),
    };
    let ntfy = NtfyChannel::new(dir, &settings, "http://localhost:3271").unwrap();

    let state = state
        .with_webpush(wp, public_key_b64(TEST_PEM).unwrap())
        .with_ntfy(ntfy, settings.topic_prefix.clone());
    let names: Vec<&str> = state.channels.iter().map(|c| c.name()).collect();
    assert_eq!(names, ["ws", "webpush", "ntfy"]);
    assert_eq!(state.ntfy_topic_prefix.as_deref(), Some("note-"));
}
