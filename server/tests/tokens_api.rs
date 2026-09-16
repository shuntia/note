mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::auth;
use tower::ServiceExt;

async fn read(res: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

async fn with_cookie(
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
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.unwrap_or("").to_string())).unwrap())
        .await
        .unwrap();
    read(res).await
}

async fn with_bearer(
    app: &axum::Router,
    token: &str,
    method: Method,
    path: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.unwrap_or("").to_string())).unwrap())
        .await
        .unwrap();
    read(res).await
}

async fn mint(app: &axum::Router, cookie: &str, name: &str) -> (StatusCode, serde_json::Value) {
    with_cookie(app, cookie, Method::POST, "/api/tokens", Some(&format!(r#"{{"name":"{name}"}}"#)))
        .await
}

async fn minted(app: &axum::Router, cookie: &str, name: &str) -> (i64, String) {
    let (status, v) = mint(app, cookie, name).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    (v["id"].as_i64().unwrap(), v["token"].as_str().unwrap().to_string())
}

#[tokio::test]
async fn mint_list_revoke_round_trip() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let (status, made) = mint(&app, &cookie, "laptop").await;
    assert_eq!(status, StatusCode::OK, "{made}");
    assert_eq!(made["name"], "laptop");
    assert!(made["token"].as_str().unwrap().starts_with("note_"));
    assert!(made["last_used_at"].is_null());
    let id = made["id"].as_i64().unwrap();

    let (status, list) = with_cookie(&app, &cookie, Method::GET, "/api/tokens", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], id);
    assert!(list[0].get("token").is_none());

    let (status, _) =
        with_cookie(&app, &cookie, Method::DELETE, &format!("/api/tokens/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) =
        with_cookie(&app, &cookie, Method::DELETE, &format!("/api/tokens/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, list) = with_cookie(&app, &cookie, Method::GET, "/api/tokens", None).await;
    assert!(list.as_array().unwrap().is_empty());

    let kinds: Vec<String> = {
        let conn = state.db.lock().unwrap();
        let mut stmt = conn.prepare("SELECT kind FROM event_log ORDER BY id").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
    };
    assert!(kinds.contains(&"token_created".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"token_revoked".to_string()), "{kinds:?}");
}

#[tokio::test]
async fn name_and_cap_are_enforced() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (status, v) = mint(&app, &cookie, "   ").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(v["error"].is_string());
    let (status, _) = mint(&app, &cookie, &"x".repeat(65)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    for i in 0..20 {
        let (status, _) = mint(&app, &cookie, &format!("t{i}")).await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, v) = mint(&app, &cookie, "one more").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(v["error"].is_string());
}

#[tokio::test]
async fn another_users_token_id_is_404() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    {
        let conn = state.db.lock().unwrap();
        auth::create_user(&conn, "bo", "pw", false).unwrap();
    }
    let bo = common::login(&app, "bo", "pw").await;
    let (id, _) = minted(&app, &bo, "bos").await;
    let (status, _) =
        with_cookie(&app, &cookie, Method::DELETE, &format!("/api/tokens/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, list) = with_cookie(&app, &bo, Method::GET, "/api/tokens", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn token_routes_need_the_cookie_not_a_bearer() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (_, token) = minted(&app, &cookie, "cli").await;
    let (status, _) = with_bearer(&app, &token, Method::GET, "/api/tokens", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
