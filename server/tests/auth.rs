use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use note_server::{api, auth, db, AppState};
use tower::ServiceExt;

fn state_with_user() -> (AppState, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "hunter2", true).unwrap();
    (AppState::new(conn, tmp.path().to_path_buf(), tmp.path().to_path_buf()), tmp)
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

async fn login_cookie(app: &axum::Router, password: &str) -> String {
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"username":"aki","password":"{password}"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    res.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn logout_invalidates_the_session() {
    let (state, _tmp) = state_with_user();
    let app = api::router(state);
    let cookie = login_cookie(&app, "hunter2").await;

    let res = app
        .clone()
        .oneshot(
            Request::get("/api/me")
                .header(header::COOKIE, cookie.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .clone()
        .oneshot(
            Request::post("/api/logout")
                .header(header::COOKIE, cookie.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .oneshot(
            Request::get("/api/me")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn repeated_login_failures_are_throttled() {
    let (state, _tmp) = state_with_user();
    let app = api::router(state);
    let attempt = || {
        Request::post("/api/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"username":"aki","password":"wrong"}"#))
            .unwrap()
    };
    for _ in 0..10 {
        let res = app.clone().oneshot(attempt()).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
    let res = app.oneshot(attempt()).await.unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
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

#[tokio::test]
async fn correct_password_succeeds_while_the_username_is_rate_limited() {
    let (state, _tmp) = state_with_user();
    let app = api::router(state);
    let wrong = || {
        Request::post("/api/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"username":"aki","password":"wrong"}"#))
            .unwrap()
    };
    for _ in 0..11 {
        app.clone().oneshot(wrong()).await.unwrap();
    }
    let res = app
        .oneshot(
            Request::post("/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"username":"aki","password":"hunter2"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().contains_key(header::SET_COOKIE));
}

#[tokio::test]
async fn login_beyond_the_hash_admission_limit_is_service_unavailable() {
    let (state, _tmp) = state_with_user();
    let held: Vec<_> = (0..note_server::auth::MAX_CONCURRENT_LOGINS)
        .map(|_| state.login_slots.clone().try_acquire_owned().unwrap())
        .collect();
    let app = api::router(state);
    let res = app
        .oneshot(
            Request::post("/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"username":"aki","password":"hunter2"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(res.headers()[header::RETRY_AFTER], "2");
    drop(held);
}
