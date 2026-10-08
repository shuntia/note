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

#[tokio::test(flavor = "multi_thread")]
async fn a_page_reports_whether_it_is_in_view() {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut req = format!("ws://{addr}/api/ws").into_client_request().unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    ws.send(tokio_tungstenite::tungstenite::Message::Text(r#"{"type":"visible","on":true}"#.into())).await.unwrap();
    let hub = state.hub.clone();
    note_voice_proto::testkit::eventually("in view", || hub.has_visible(1)).await;
    ws.send(tokio_tungstenite::tungstenite::Message::Text(r#"{"type":"visible","on":false}"#.into())).await.unwrap();
    note_voice_proto::testkit::eventually("hidden", || !hub.has_visible(1)).await;
}
