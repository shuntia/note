mod common;
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn json(res: axum::response::Response) -> serde_json::Value {
    let body = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn req(method: Method, path: &str, cookies: &str, body: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(path).header(header::COOKIE, cookies);
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    b.body(body.map_or_else(Body::empty, |s| Body::from(s.to_string()))).unwrap()
}

fn cookie_of(res: &axum::response::Response) -> String {
    res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string()
}

async fn elevated() -> (axum::Router, String, note_server::AppState, tempfile::TempDir) {
    let (app, session, state, cfg) = common::app_with_password_only_admin().await;
    let res = app
        .clone()
        .oneshot(req(Method::POST, "/api/admin/elevate", &session, Some(r#"{"password":"pw"}"#)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookies = format!("{session}; {}", cookie_of(&res));
    (app, cookies, state, cfg)
}

async fn invite(app: &axum::Router, cookies: &str, body: &str) -> serde_json::Value {
    let res = app.clone().oneshot(req(Method::POST, "/api/admin/invites", cookies, Some(body))).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    json(res).await
}

fn token_of(made: &serde_json::Value) -> String {
    made["token"].as_str().unwrap().to_string()
}

async fn join(app: &axum::Router, token: &str, username: &str) -> axum::response::Response {
    let body = format!(r#"{{"username":"{username}","password":"longenough"}}"#);
    app.clone().oneshot(req(Method::POST, &format!("/api/join/{token}"), "", Some(&body))).await.unwrap()
}

#[tokio::test]
async fn an_admin_issues_lists_and_the_link_carries_the_public_url() {
    let (app, cookies, _state, _cfg) = elevated().await;
    let made = invite(&app, &cookies, r#"{"admin":true,"username":"mika","days":3}"#).await;
    let token = token_of(&made);
    assert!(token.starts_with("join_"));
    assert_eq!(made["url"], format!("http://localhost:3271/join/{token}"));
    assert_eq!(made["admin"], true);
    assert_eq!(made["username"], "mika");

    let res = app.clone().oneshot(req(Method::GET, "/api/admin/invites", &cookies, None)).await.unwrap();
    let list = json(res).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert!(list[0].get("token").is_none(), "the list never carries a secret");

    let res = app.clone().oneshot(req(Method::GET, &format!("/api/join/{token}"), "", None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(json(res).await["username"], "mika");
}

#[tokio::test]
async fn an_unelevated_admin_cannot_issue_invites() {
    let (app, session, _state, _cfg) = common::app_with_password_only_admin().await;
    let res = app
        .clone()
        .oneshot(req(Method::POST, "/api/admin/invites", &session, Some("{}")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn joining_creates_a_signed_in_account_that_starts_onboarding() {
    let (app, cookies, _state, _cfg) = elevated().await;
    let token = token_of(&invite(&app, &cookies, "{}").await);
    let res = join(&app, &token, "mika").await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let session = cookie_of(&res);

    let me = json(app.clone().oneshot(req(Method::GET, "/api/me", &session, None)).await.unwrap()).await;
    assert_eq!(me["username"], "mika");
    assert_eq!(me["admin"], false);
    assert_eq!(me["onboarding"], true);

    let res = app.clone().oneshot(req(Method::POST, "/api/onboarding/done", &session, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let me = json(app.clone().oneshot(req(Method::GET, "/api/me", &session, None)).await.unwrap()).await;
    assert_eq!(me["onboarding"], false);

    let list = json(app.clone().oneshot(req(Method::GET, "/api/admin/invites", &cookies, None)).await.unwrap()).await;
    assert!(list.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_used_invite_is_refused() {
    let (app, cookies, _state, _cfg) = elevated().await;
    let token = token_of(&invite(&app, &cookies, "{}").await);
    assert_eq!(join(&app, &token, "mika").await.status(), StatusCode::CREATED);
    assert_eq!(join(&app, &token, "mika2").await.status(), StatusCode::NOT_FOUND);
    let res = app.clone().oneshot(req(Method::GET, &format!("/api/join/{token}"), "", None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_expired_invite_is_refused() {
    let (app, cookies, state, _cfg) = elevated().await;
    let token = token_of(&invite(&app, &cookies, "{}").await);
    state.db().execute("UPDATE invites SET expires_at = 0", []).unwrap();
    assert_eq!(join(&app, &token, "mika").await.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_revoked_invite_is_refused() {
    let (app, cookies, _state, _cfg) = elevated().await;
    let made = invite(&app, &cookies, "{}").await;
    let path = format!("/api/admin/invites/{}", made["id"]);
    let res = app.clone().oneshot(req(Method::DELETE, &path, &cookies, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let res = app.clone().oneshot(req(Method::DELETE, &path, &cookies, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(join(&app, &token_of(&made), "mika").await.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn concurrent_joins_on_one_token_create_one_account() {
    let (app, cookies, state, _cfg) = elevated().await;
    let token = token_of(&invite(&app, &cookies, "{}").await);
    let tries: Vec<_> = (0..4)
        .map(|i| {
            let (app, token) = (app.clone(), token.clone());
            tokio::spawn(async move { join(&app, &token, &format!("racer{i}")).await.status() })
        })
        .collect();
    let mut created = 0;
    for t in tries {
        let status = t.await.unwrap();
        if status == StatusCode::CREATED {
            created += 1;
        } else {
            assert!(
                matches!(status, StatusCode::NOT_FOUND | StatusCode::SERVICE_UNAVAILABLE),
                "{status}"
            );
        }
    }
    assert_eq!(created, 1);
    let racers: i64 = state
        .db()
        .query_row("SELECT COUNT(*) FROM users WHERE username LIKE 'racer%'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(racers, 1);
}

#[tokio::test]
async fn a_taken_username_keeps_the_invite_and_a_bad_one_is_refused_unhashed() {
    let (app, cookies, _state, _cfg) = elevated().await;
    let token = token_of(&invite(&app, &cookies, "{}").await);
    assert_eq!(join(&app, &token, "aki").await.status(), StatusCode::CONFLICT);
    assert_eq!(join(&app, &token, "../x").await.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let short = app
        .clone()
        .oneshot(req(
            Method::POST,
            &format!("/api/join/{token}"),
            "",
            Some(r#"{"username":"mika","password":"short"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(short.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(join(&app, &token, "mika").await.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn unknown_tokens_are_404_until_the_address_is_throttled_and_foreign_pages_are_refused() {
    let (app, _cookies, _state, _cfg) = elevated().await;
    let mut last = StatusCode::OK;
    for _ in 0..=note_server::invites::ADDRESS_ATTEMPTS {
        let res = app.clone().oneshot(req(Method::GET, "/api/join/join_nope", "", None)).await.unwrap();
        last = res.status();
        if last != StatusCode::NOT_FOUND {
            break;
        }
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);

    let foreign = Request::post("/api/join/join_nope")
        .header(header::CONTENT_TYPE, "application/json")
        .header("sec-fetch-site", "cross-site")
        .body(Body::from(r#"{"username":"mika","password":"longenough"}"#))
        .unwrap();
    assert_eq!(app.clone().oneshot(foreign).await.unwrap().status(), StatusCode::FORBIDDEN);
}
