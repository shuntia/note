mod common;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn member_cannot_use_admin_routes() {
    let (app, admin_cookie, _cfg) = common::app_with_logged_in_user().await;
    // create a member via admin route, then log in as them
    let res = app.clone().oneshot(
        Request::post("/api/admin/users")
            .header(header::COOKIE, &admin_cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"username":"kid","password":"pw","admin":false}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.clone().oneshot(
        Request::post("/api/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"username":"kid","password":"pw"}"#)).unwrap(),
    ).await.unwrap();
    let kid_cookie = res.headers()[header::SET_COOKIE]
        .to_str().unwrap().split(';').next().unwrap().to_string();

    let res = app.oneshot(
        Request::get("/api/admin/log")
            .header(header::COOKIE, &kid_cookie)
            .body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}
