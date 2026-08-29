use axum::body::Body;
use axum::http::{Request, StatusCode};
use note_server::{api, db, AppState};
use tower::ServiceExt;

#[tokio::test]
async fn healthz_returns_ok() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let app = api::router(AppState::new(db::open_memory().unwrap(), dir.clone(), dir));
    let res = app
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}
