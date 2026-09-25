mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn read(res: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

async fn owner(app: &axum::Router, cookie: &str, method: Method, path: &str, body: Option<&str>) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(path).header(header::COOKIE, cookie).header("sec-fetch-site", "same-origin");
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    read(app.clone().oneshot(req.body(Body::from(body.unwrap_or("").to_string())).unwrap()).await.unwrap()).await
}

fn in_days(days: i64) -> String {
    (jiff::Timestamp::now() + jiff::Span::new().hours(24 * days)).to_string()
}

async fn mint(app: &axum::Router, cookie: &str, name: &str, scope: &str) -> serde_json::Value {
    let (status, v) = owner(
        app,
        cookie,
        Method::POST,
        "/api/shares",
        Some(&format!(r#"{{"name":"{name}","brief":"be warm","scope":{scope},"expires_at":"{}"}}"#, in_days(30))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    v
}

fn token_of(v: &serde_json::Value) -> String {
    v["url"].as_str().unwrap().rsplit('/').next().unwrap().to_string()
}

#[tokio::test]
async fn owner_mints_lists_patches_and_revokes() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let made = mint(&app, &cookie, "Mom", "{}").await;
    assert!(made["url"].as_str().unwrap().contains("/s/share_"), "{made}");
    assert!(token_of(&made).starts_with("share_"), "{made}");
    assert_eq!(made["scope"]["horizon_days"], 3);
    assert_eq!(made["messages_today"], 0);

    let (status, list) = owner(&app, &cookie, Method::GET, "/api/shares", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["url"], made["url"], "the url is listed for the link's whole life");

    let id = made["id"].as_i64().unwrap();
    let (status, patched) = owner(&app, &cookie, Method::PATCH, &format!("/api/shares/{id}"), Some(r#"{"name":"Mother","scope":{"categories":["school"],"details":true}}"#)).await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["name"], "Mother");
    assert_eq!(patched["scope"]["categories"][0], "school");
    assert_eq!(patched["url"], made["url"]);

    let (status, _) = owner(&app, &cookie, Method::DELETE, &format!("/api/shares/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = owner(&app, &cookie, Method::DELETE, &format!("/api/shares/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_far_expiry_is_clamped_and_bad_input_is_422() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (status, v) = owner(&app, &cookie, Method::POST, "/api/shares", Some(&format!(r#"{{"name":"Far","expires_at":"{}"}}"#, in_days(400)))).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let exp: jiff::Timestamp = v["expires_at"].as_str().unwrap().parse().unwrap();
    let ceiling = jiff::Timestamp::now() + jiff::Span::new().hours(24 * 120);
    assert!(exp <= ceiling && exp > ceiling - jiff::Span::new().minutes(5));
    let (status, _) = owner(&app, &cookie, Method::POST, "/api/shares", Some(&format!(r#"{{"name":"","expires_at":"{}"}}"#, in_days(1)))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = owner(&app, &cookie, Method::POST, "/api/shares", Some(&format!(r#"{{"name":"x","scope":{{"horizon_days":0}},"expires_at":"{}"}}"#, in_days(1)))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = owner(&app, &cookie, Method::POST, "/api/shares", Some(r#"{"name":"x","expires_at":"2000-01-01T00:00:00Z"}"#)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_cross_site_write_is_refused_and_a_stranger_gets_nothing() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let req = Request::post("/api/shares")
        .header(header::COOKIE, &cookie)
        .header("sec-fetch-site", "cross-site")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!(r#"{{"name":"x","expires_at":"{}"}}"#, in_days(1))))
        .unwrap();
    let (status, _) = read(app.clone().oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = read(app.clone().oneshot(Request::get("/api/shares").body(Body::empty()).unwrap()).await.unwrap()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
