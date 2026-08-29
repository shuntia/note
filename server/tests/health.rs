use axum::body::Body;
use axum::http::{Request, StatusCode};
use note_server::{api, AppState};
use tower::ServiceExt;

#[tokio::test]
async fn healthz_returns_ok() {
    let app = api::router(AppState::default());
    let res = app
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}
