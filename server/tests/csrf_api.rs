mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

const TASK: &str = r#"{"title":"call dentist"}"#;
const SCHOOL: &str = r#"{"title":"school","kind":"fixed","start_time":"08:15","end_time":"15:30","days":31}"#;

async fn send(app: &axum::Router, req: axum::http::request::Builder, body: &str) -> (StatusCode, serde_json::Value) {
    let res = app.clone().oneshot(req.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

fn post(path: &str, cookie: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json")
}

#[tokio::test]
async fn a_cross_site_write_riding_on_the_cookie_is_refused() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    for req in [
        post("/api/tasks", &cookie).header("sec-fetch-site", "cross-site").header(header::ORIGIN, "https://evil.example"),
        post("/api/tasks", &cookie).header("sec-fetch-site", "same-site").header(header::ORIGIN, "https://evil.note.example"),
        post("/api/tasks", &cookie).header(header::ORIGIN, "https://evil.example"),
        post("/api/tasks", &cookie).header(header::ORIGIN, "null"),
        post("/api/tasks", &cookie).header("sec-fetch-site", "cross-site"),
    ] {
        let (status, v) = send(&app, req, TASK).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
        assert_eq!(v["error"], "cross-site request refused");
    }
    let (status, v) = send(
        &app,
        Request::builder()
            .method(Method::DELETE)
            .uri("/api/tasks/1")
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, "https://evil.example"),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    let (_, tasks) = send(&app, Request::get("/api/tasks").header(header::COOKIE, &cookie), "").await;
    assert!(tasks.as_array().unwrap().is_empty(), "nothing was written");
}

#[tokio::test]
async fn notes_own_page_and_non_browser_clients_still_write() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    for req in [
        post("/api/tasks", &cookie).header("sec-fetch-site", "same-origin").header(header::ORIGIN, "https://note.example.net"),
        post("/api/tasks", &cookie).header("sec-fetch-site", "none"),
        post("/api/tasks", &cookie).header(header::ORIGIN, "http://localhost:3271"),
        post("/api/tasks", &cookie).header(header::ORIGIN, "https://note.example.net").header(header::HOST, "note.example.net"),
        post("/api/tasks", &cookie),
    ] {
        let (status, v) = send(&app, req, TASK).await;
        assert_eq!(status, StatusCode::OK, "{v}");
    }
    let (status, _) = send(&app, Request::get("/api/tasks").header(header::COOKIE, &cookie).header("sec-fetch-site", "cross-site"), "").await;
    assert_eq!(status, StatusCode::OK, "a read is never refused");
}

#[tokio::test]
async fn a_bearer_token_writes_from_anywhere() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (status, made) = send(&app, post("/api/tokens", &cookie), r#"{"name":"script"}"#).await;
    assert_eq!(status, StatusCode::OK, "{made}");
    let token = made["token"].as_str().unwrap();
    let bearer = || {
        Request::builder()
            .method(Method::POST)
            .uri("/api/tasks")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("sec-fetch-site", "cross-site")
            .header(header::ORIGIN, "https://elsewhere.example")
    };
    let (status, v) = send(&app, bearer(), TASK).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, _) = send(&app, bearer().header(header::COOKIE, &cookie), TASK).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a session cookie alongside a bearer header still needs the page's own origin");
}

#[tokio::test]
async fn a_raw_body_route_wants_json_from_a_cookie() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let plain = Request::builder()
        .method(Method::POST)
        .uri("/api/calendar")
        .header(header::COOKIE, &cookie)
        .header(header::CONTENT_TYPE, "text/plain");
    let (status, v) = send(&app, plain, SCHOOL).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{v}");
    let bare = Request::builder().method(Method::POST).uri("/api/calendar").header(header::COOKIE, &cookie);
    let (status, _) = send(&app, bare, SCHOOL).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    for path in ["/api/agent/inbox", "/api/tasks/1/agent"] {
        let form = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        let (status, v) = send(&app, form, "{}").await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{path}: {v}");
    }
    let json = post("/api/calendar", &cookie).header(header::CONTENT_TYPE, "application/json; charset=utf-8");
    let (status, v) = send(&app, json, SCHOOL).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
}

#[tokio::test]
async fn a_signed_in_visitor_still_reaches_a_share_from_its_page() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let expires = (jiff::Timestamp::now() + jiff::Span::new().hours(24)).to_string();
    let (status, made) = send(&app, post("/api/shares", &cookie), &format!(r#"{{"name":"Mom","expires_at":"{expires}"}}"#)).await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let url = made["url"].as_str().unwrap();
    let token = url.rsplit('/').next().unwrap();
    let (status, v) = send(
        &app,
        post(&format!("/api/share/{token}/messages"), &cookie)
            .header("sec-fetch-site", "same-origin")
            .header(header::ORIGIN, "http://localhost:3271"),
        r#"{"message":"hi"}"#,
    )
    .await;
    assert_ne!(status, StatusCode::FORBIDDEN, "{v}");
    let (status, _) = send(&app, Request::get(format!("/api/share/{token}")), "").await;
    assert_eq!(status, StatusCode::OK);
}
