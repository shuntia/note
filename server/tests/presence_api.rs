mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use note_server::AppState;
use tower::ServiceExt;

fn seen(state: &AppState) -> Option<String> {
    let conn = state.db();
    conn.query_row("SELECT last_active_at FROM users WHERE id = 1", [], |r| r.get(0)).unwrap()
}

#[tokio::test]
async fn a_presence_ping_stamps_the_user_and_a_read_does_not() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;

    let res = app
        .clone()
        .oneshot(Request::get("/api/me").header(header::COOKIE, &cookie).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(seen(&state).is_none(), "an open page polls; a read is not the user");

    let res = app
        .oneshot(
            Request::post("/api/presence")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let at: jiff::Timestamp = seen(&state).expect("a stamp").parse().unwrap();
    assert!((jiff::Timestamp::now().as_second() - at.as_second()).abs() <= 2);
}

#[tokio::test]
async fn an_api_token_write_is_not_presence() {
    let (app, _cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let secret = {
        let conn = state.db();
        note_server::tokens::create(&conn, 1, "script").unwrap().token
    };
    let res = app
        .oneshot(
            Request::post("/api/tasks")
                .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"title":"from the script"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(res.status().is_success(), "{}", res.status());
    assert!(seen(&state).is_none());
}

#[tokio::test]
async fn presence_needs_a_session() {
    let (app, _cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let res = app
        .oneshot(Request::post("/api/presence").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(seen(&state).is_none());
}
