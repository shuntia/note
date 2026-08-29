use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use note_server::{api, auth, db, AppState};
use tower::ServiceExt;

fn state_with_user() -> (AppState, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "hunter2", true).unwrap();
    (AppState::new(conn, tmp.path().to_path_buf()), tmp)
}

#[tokio::test]
async fn login_sets_cookie_and_me_works() {
    let (state, _tmp) = state_with_user();
    let app = api::router(state);
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"username":"aki","password":"hunter2"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = res.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_string();

    let res = app
        .oneshot(
            Request::get("/api/me")
                .header(header::COOKIE, cookie.split(';').next().unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn wrong_password_is_401_and_me_without_cookie_is_401() {
    let (state, _tmp) = state_with_user();
    let app = api::router(state);
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"username":"aki","password":"nope"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let res = app
        .oneshot(Request::get("/api/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
