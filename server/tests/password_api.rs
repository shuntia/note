mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::ServiceExt;

async fn change(app: &axum::Router, cookie: &str, body: &str) -> StatusCode {
    app.clone()
        .oneshot(
            Request::post("/api/password")
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn whoami(app: &axum::Router, cookie: &str) -> StatusCode {
    app.clone()
        .oneshot(Request::get("/api/me").header(header::COOKIE, cookie).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

async fn sign_in(app: &axum::Router, password: &str) -> StatusCode {
    app.clone()
        .oneshot(
            Request::post("/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"username":"aki","password":"{password}"}}"#)))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn a_wrong_current_password_changes_nothing() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    assert_eq!(
        change(&app, &cookie, r#"{"current":"nope","new":"a longer one"}"#).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(sign_in(&app, "pw").await, StatusCode::OK, "the old password still works");
}

#[tokio::test]
async fn a_new_password_under_eight_characters_is_refused() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    assert_eq!(
        change(&app, &cookie, r#"{"current":"pw","new":"short7!"}"#).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(sign_in(&app, "pw").await, StatusCode::OK);
}

#[tokio::test]
async fn the_change_takes_and_the_old_password_stops_working() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    assert_eq!(
        change(&app, &cookie, r#"{"current":"pw","new":"a longer one"}"#).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(sign_in(&app, "pw").await, StatusCode::UNAUTHORIZED);
    assert_eq!(sign_in(&app, "a longer one").await, StatusCode::OK);
}

#[tokio::test]
async fn every_other_session_ends_and_the_caller_stays_signed_in() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let elsewhere = common::login(&app, "aki", "pw").await;
    assert_eq!(whoami(&app, &elsewhere).await, StatusCode::OK);

    assert_eq!(
        change(&app, &cookie, r#"{"current":"pw","new":"a longer one"}"#).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(whoami(&app, &cookie).await, StatusCode::OK, "the caller keeps their session");
    assert_eq!(whoami(&app, &elsewhere).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_route_needs_a_session_of_its_own() {
    let (app, _cookie, _cfg) = common::app_with_logged_in_user().await;
    assert_eq!(
        change(&app, "session=nobody", r#"{"current":"pw","new":"a longer one"}"#).await,
        StatusCode::UNAUTHORIZED
    );
}
