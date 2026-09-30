mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn call(
    app: &axum::Router,
    cookie: &str,
    method: Method,
    path: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(path).header(header::COOKIE, cookie);
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let res = req.body(Body::from(body.unwrap_or_default().to_string())).unwrap();
    let res = app.clone().oneshot(res).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

async fn texts(app: &axum::Router, cookie: &str) -> Vec<String> {
    let (status, v) = call(app, cookie, Method::GET, "/api/notes", None).await;
    assert_eq!(status, StatusCode::OK);
    v.as_array().unwrap().iter().map(|n| n["text"].as_str().unwrap().to_string()).collect()
}

#[tokio::test]
async fn a_note_is_added_pinned_done_undone_and_deleted() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    assert!(texts(&app, &cookie).await.is_empty());

    let (status, made) =
        call(&app, &cookie, Method::POST, "/api/notes", Some(r#"{"text":"call the bank"}"#)).await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let id = made["id"].as_i64().unwrap();
    assert_eq!(made["pinned"], false);
    assert!(made["done_at"].is_null() && made["last_nudged_at"].is_null());
    call(&app, &cookie, Method::POST, "/api/notes", Some(r#"{"text":"milk"}"#)).await;

    let path = format!("/api/notes/{id}");
    let (status, v) = call(&app, &cookie, Method::PATCH, &path, Some(r#"{"pinned":true}"#)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["pinned"], true);

    let (_, v) = call(&app, &cookie, Method::PATCH, &path, Some(r#"{"done":true}"#)).await;
    assert!(v["done_at"].is_string());
    assert_eq!(texts(&app, &cookie).await, ["milk", "call the bank"], "done ones come last");

    let (_, v) = call(&app, &cookie, Method::PATCH, &path, Some(r#"{"done":false}"#)).await;
    assert!(v["done_at"].is_null());
    assert_eq!(texts(&app, &cookie).await, ["call the bank", "milk"], "pinned first");

    let (status, _) = call(&app, &cookie, Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&app, &cookie, Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, &cookie, Method::PATCH, &path, Some(r#"{"done":true}"#)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn text_rules_hold_on_the_route() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let post = |body: String| {
        let app = app.clone();
        let cookie = cookie.clone();
        async move { call(&app, &cookie, Method::POST, "/api/notes", Some(&body)).await }
    };
    let (status, v) = post(r#"{"text":"  two\nlines  "}"#.into()).await;
    assert_eq!((status, v["text"].as_str()), (StatusCode::CREATED, Some("two lines")));
    let (status, _) = post(format!(r#"{{"text":"{}"}}"#, "あ".repeat(200))).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, v) = post(format!(r#"{{"text":"{}"}}"#, "あ".repeat(201))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(v["error"].as_str().unwrap().contains("200"));
    let (status, _) = post(r#"{"text":"   "}"#.into()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = post(r#"{"text":"x","pinned":true}"#.into()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "unknown fields are refused");
    assert_eq!(texts(&app, &cookie).await.len(), 2);
}

#[tokio::test]
async fn another_users_note_is_not_found_and_not_listed() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    {
        let conn = state.db.lock().unwrap();
        note_server::auth::create_user(&conn, "bo", "pw", false).unwrap();
    }
    let bo = common::login(&app, "bo", "pw").await;
    let (_, theirs) = call(&app, &bo, Method::POST, "/api/notes", Some(r#"{"text":"theirs"}"#)).await;
    let path = format!("/api/notes/{}", theirs["id"]);

    assert!(texts(&app, &cookie).await.is_empty());
    let (status, _) = call(&app, &cookie, Method::PATCH, &path, Some(r#"{"done":true}"#)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, &cookie, Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, &cookie, Method::PATCH, "/api/notes/abc", Some("{}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(texts(&app, &bo).await, ["theirs"]);
}
