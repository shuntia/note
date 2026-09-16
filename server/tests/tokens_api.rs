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

#[tokio::test]
async fn bearer_token_reaches_every_task_route() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (_, token) = minted(&app, &cookie, "cli").await;

    let (status, t) =
        with_bearer(&app, &token, Method::POST, "/api/tasks", Some(r#"{"title":"via token"}"#)).await;
    assert_eq!(status, StatusCode::OK, "{t}");
    let id = t["id"].as_i64().unwrap();

    let (status, list) = with_bearer(&app, &token, Method::GET, "/api/tasks", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["title"], "via token");

    let (status, t) = with_bearer(
        &app,
        &token,
        Method::PATCH,
        &format!("/api/tasks/{id}"),
        Some(r#"{"duration_min":20}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{t}");
    assert_eq!(t["duration_source"], "user");

    let (status, _) = with_bearer(
        &app,
        &token,
        Method::POST,
        &format!("/api/tasks/{id}/split"),
        Some(r#"{"steps":[{"title":"a","duration_min":5},{"title":"b","duration_min":5}]}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) =
        with_bearer(&app, &token, Method::POST, &format!("/api/tasks/{id}/flatten"), None).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = with_cookie(&app, &cookie, Method::GET, "/api/tasks", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn bearer_token_is_refused_outside_tasks() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (_, token) = minted(&app, &cookie, "cli").await;
    for path in ["/api/me", "/api/settings", "/api/plan/today", "/api/conversations"] {
        let (status, _) = with_bearer(&app, &token, Method::GET, path, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
    }
}

#[tokio::test]
async fn bad_bearer_is_401_even_with_a_valid_cookie() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    for auth_value in ["Bearer note_not_a_real_token", "Basic abc", "Bearer "] {
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/tasks")
                    .header(header::COOKIE, cookie.as_str())
                    .header(header::AUTHORIZATION, auth_value)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{auth_value:?}");
    }
}

#[tokio::test]
async fn disabled_user_token_stops_resolving() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let (_, token) = minted(&app, &cookie, "cli").await;
    state
        .db
        .lock()
        .unwrap()
        .execute("UPDATE users SET disabled = 1 WHERE username = 'aki'", [])
        .unwrap();
    let (status, _) = with_bearer(&app, &token, Method::GET, "/api/tasks", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoked_token_is_401_on_next_use() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (id, token) = minted(&app, &cookie, "cli").await;
    let (status, _) = with_bearer(&app, &token, Method::GET, "/api/tasks", None).await;
    assert_eq!(status, StatusCode::OK);
    with_cookie(&app, &cookie, Method::DELETE, &format!("/api/tokens/{id}"), None).await;
    let (status, _) = with_bearer(&app, &token, Method::GET, "/api/tasks", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn token_writes_are_user_actor() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (_, token) = minted(&app, &cookie, "cli").await;
    let (_, t) = with_bearer(
        &app,
        &token,
        Method::POST,
        "/api/tasks",
        Some(r#"{"title":"timed","duration_min":15}"#),
    )
    .await;
    assert_eq!(t["duration_source"], "user");
}

#[tokio::test]
async fn bearer_token_can_delete_a_task() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (_, token) = minted(&app, &cookie, "cli").await;
    let (_, t) =
        with_bearer(&app, &token, Method::POST, "/api/tasks", Some(r#"{"title":"gone"}"#)).await;
    let id = t["id"].as_i64().unwrap();
    let (status, _) =
        with_bearer(&app, &token, Method::DELETE, &format!("/api/tasks/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) =
        with_bearer(&app, &token, Method::DELETE, &format!("/api/tasks/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
