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

fn sign_in(username: &str, password: &str, client: &str) -> Request<Body> {
    Request::post("/api/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header("cf-connecting-ip", client)
        .body(Body::from(format!(r#"{{"username":"{username}","password":"{password}"}}"#)))
        .unwrap()
}

#[tokio::test]
async fn a_locked_username_refuses_even_the_right_password_without_hashing() {
    let (state, _tmp) = state_with_user();
    let app = api::router(state.clone());
    for _ in 0..auth::ACCOUNT_FAILURES {
        let res = app.clone().oneshot(sign_in("aki", "wrong", "198.51.100.1")).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
    let held: Vec<_> = (0..auth::MAX_CONCURRENT_LOGINS)
        .map(|_| state.login_slots.clone().try_acquire_owned().unwrap())
        .collect();
    let res = app.clone().oneshot(sign_in("aki", "hunter2", "198.51.100.2")).await.unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS, "refused before a slot is even asked for");
    let wait: i64 = res.headers()[header::RETRY_AFTER].to_str().unwrap().parse().unwrap();
    assert!((1..=auth::BACKOFF_BASE_SECS).contains(&wait));
    assert!(!res.headers().contains_key(header::SET_COOKIE));
    drop(held);

    state.login_limiter.clear("aki");
    let res = app.oneshot(sign_in("aki", "hunter2", "198.51.100.2")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_successful_sign_in_forgets_the_usernames_failures() {
    let (state, _tmp) = state_with_user();
    let app = api::router(state);
    for _ in 0..auth::ACCOUNT_FAILURES - 1 {
        app.clone().oneshot(sign_in("aki", "wrong", "198.51.100.1")).await.unwrap();
    }
    let res = app.clone().oneshot(sign_in("aki", "hunter2", "198.51.100.1")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    for _ in 0..auth::ACCOUNT_FAILURES - 1 {
        let res = app.clone().oneshot(sign_in("aki", "wrong", "198.51.100.1")).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn a_signed_in_password_recheck_does_not_wait_on_sign_in_slots() {
    let (state, _tmp) = state_with_user();
    let app = api::router(state.clone());
    let res = app.clone().oneshot(sign_in("aki", "hunter2", "198.51.100.1")).await.unwrap();
    let cookie = res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string();
    let held: Vec<_> = (0..auth::MAX_CONCURRENT_LOGINS)
        .map(|_| state.login_slots.clone().try_acquire_owned().unwrap())
        .collect();
    let res = app
        .oneshot(
            Request::post("/api/password")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"current":"hunter2","new":"correct horse"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    drop(held);
}

#[tokio::test]
async fn one_address_guessing_across_usernames_is_locked_out_alone() {
    let (state, _tmp) = state_with_user();
    let app = api::router(state);
    for i in 0..auth::CLIENT_FAILURES {
        let res = app.clone().oneshot(sign_in(&format!("guess{i}"), "wrong", "203.0.113.7")).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
    let res = app.clone().oneshot(sign_in("aki", "hunter2", "203.0.113.7")).await.unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(res.headers().contains_key(header::RETRY_AFTER));
    let res = app.oneshot(sign_in("aki", "hunter2", "203.0.113.8")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "another address is unaffected");
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
