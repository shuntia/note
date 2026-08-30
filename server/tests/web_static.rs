mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use note_server::{api, db, AppState};
use tower::ServiceExt;

fn state(cfg: &tempfile::TempDir) -> AppState {
    let conn = db::open_memory().unwrap();
    AppState::new(conn, cfg.path().to_path_buf(), cfg.path().to_path_buf())
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, String) {
    let res = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn serves_the_web_build_with_spa_fallback() {
    let cfg = common::config_dir();
    let web = tempfile::tempdir().unwrap();
    std::fs::write(web.path().join("index.html"), "<title>Note</title>").unwrap();
    std::fs::create_dir(web.path().join("assets")).unwrap();
    std::fs::write(web.path().join("assets/app.js"), "console.log(1)").unwrap();
    let app = api::router_with_web(state(&cfg), web.path());

    let (status, body) = get(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<title>Note</title>"));

    let (status, body) = get(&app, "/assets/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("console.log"));

    // an SPA route the filesystem doesn't know still serves the shell
    let (status, body) = get(&app, "/today").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<title>Note</title>"));

    // API misses stay API-shaped, and real API routes still work
    let (status, _) = get(&app, "/api/definitely-not-a-route").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get(&app, "/api").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get(&app, "/api/").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // 401 rather than 404 proves the real handler beat the catch-all
    let (status, _) = get(&app, "/api/me").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = get(&app, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn missing_web_dir_leaves_the_api_router_untouched() {
    let cfg = common::config_dir();
    let app = api::router_with_web(state(&cfg), std::path::Path::new("/nonexistent/web"));
    let (status, _) = get(&app, "/").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = get(&app, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok");
}
