mod common;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use note_server::auth;
use tower::ServiceExt;

#[tokio::test]
async fn member_cannot_use_admin_routes() {
    let (app, _admin_cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    {
        let conn = state.db.lock().unwrap();
        auth::create_user(&conn, "kid", "pw", false).unwrap();
    }
    let kid_cookie = common::login(&app, "kid", "pw").await;

    let res = app.oneshot(
        Request::get("/api/admin/log")
            .header(header::COOKIE, &kid_cookie)
            .body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn member_creation_is_not_exposed_over_http() {
    let (app, admin_cookie, _cfg) = common::app_with_logged_in_user().await;
    let res = app.oneshot(
        Request::post("/api/admin/users")
            .header(header::COOKIE, &admin_cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"username":"kid","password":"pw","admin":false}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
