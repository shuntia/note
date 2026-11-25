mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::own_keys::OwnKeys;
use note_server::providers::{mock::MockLLM, ChatResponse, LLMProvider};
use std::sync::Arc;
use tower::ServiceExt;

async fn send(app: &axum::Router, method: Method, path: &str, cookie: &str, body: Option<&str>) -> (StatusCode, String) {
    let mut req = Request::builder().method(method).uri(path).header(header::COOKIE, cookie);
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let res = app.clone().oneshot(req.body(Body::from(body.unwrap_or("").to_string())).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

fn json(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or(serde_json::Value::Null)
}

/// A provider that answers its one request with `status`.
fn refusing(status: &'static str) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else { return };
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf);
        let body = r#"{"error":{"message":"invalid api key"}}"#;
        let _ = sock.write_all(
            format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                .as_bytes(),
        );
    });
    base
}

/// The app with every user on their own key, against a provider at `base`.
async fn strict(llm: Arc<dyn LLMProvider>, base: &str) -> (axum::Router, String, note_server::AppState, tempfile::TempDir) {
    let (_, _, mut state, cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    state.own_keys = Arc::new(OwnKeys::ephemeral().with_bases(base, base));
    let app = note_server::api::router(state.clone());
    let cookie = common::login(&app, "aki", "pw").await;
    (app, cookie, state, cfg)
}

fn server_llm() -> Arc<dyn LLMProvider> {
    Arc::new(MockLLM::scripted(vec![ChatResponse { text: "server".into(), tool_calls: vec![] }]).offering(&["server/model", "fast/model"]))
}

#[tokio::test]
async fn a_saved_key_is_shown_by_its_last_four_and_never_given_back() {
    let fake = common::fake_http();
    let (app, cookie, state, _cfg) = strict(server_llm(), &fake.base).await;
    assert_eq!(json(&send(&app, Method::GET, "/api/settings/llm-key", &cookie, None).await.1), serde_json::Value::Null);

    fake.answer(r#"{"data":{"label":"k"}}"#);
    fake.answer(r#"{"data":[{"id":"mine/model"}]}"#);
    let put = r#"{"kind":"openrouter","key":"sk-or-v1-TOPSECRET-wxyz","model":"mine/model"}"#;
    let (status, body) = send(&app, Method::PUT, "/api/settings/llm-key", &cookie, Some(put)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(fake.took().contains("Bearer sk-or-v1-TOPSECRET-wxyz"), "the key is checked with the provider");
    assert_eq!(json(&body), serde_json::json!({"kind": "openrouter", "model": "mine/model", "key_hint": "wxyz", "base_url": null}));

    for path in ["/api/settings/llm-key", "/api/settings", "/api/me", "/api/models"] {
        let (_, body) = send(&app, Method::GET, path, &cookie, None).await;
        assert!(!body.contains("TOPSECRET"), "{path} leaks the key: {body}");
    }
    let sealed: Vec<u8> = state.db().query_row("SELECT sealed FROM llm_keys WHERE user_id = 1", [], |r| r.get(0)).unwrap();
    assert!(!String::from_utf8_lossy(&sealed).contains("TOPSECRET"), "the key is sealed at rest");

    let (status, _) = send(&app, Method::DELETE, "/api/settings/llm-key", &cookie, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(json(&send(&app, Method::GET, "/api/settings/llm-key", &cookie, None).await.1), serde_json::Value::Null);
}

#[tokio::test]
async fn a_key_the_provider_refuses_is_not_saved() {
    let base = refusing("401 Unauthorized");
    let (app, cookie, _state, _cfg) = strict(server_llm(), &base).await;
    let put = r#"{"kind":"openai","key":"sk-revoked","model":"gpt-5"}"#;
    let (status, body) = send(&app, Method::PUT, "/api/settings/llm-key", &cookie, Some(put)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(json(&body)["error"].as_str().unwrap().starts_with("key "));
    assert_eq!(json(&send(&app, Method::GET, "/api/settings/llm-key", &cookie, None).await.1), serde_json::Value::Null);
    let (status, _) = send(&app, Method::PUT, "/api/settings/llm-key", &cookie, Some(r#"{"kind":"anthropic","key":"k","model":"m"}"#)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_typed_endpoint_must_be_public_https_and_the_server_exemption_cannot_be_claimed() {
    let (app, cookie, state, _cfg) = strict(server_llm(), "https://openrouter.ai/api/v1").await;
    for endpoint in ["http://llm.example.com/v1", "https://192.168.1.10/v1", "https://localhost/v1"] {
        let put = format!(r#"{{"kind":"openai","key":"sk","model":"m","base_url":"{endpoint}"}}"#);
        let (status, body) = send(&app, Method::PUT, "/api/settings/llm-key", &cookie, Some(&put)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{endpoint}: {body}");
        assert!(json(&body)["error"].as_str().unwrap().starts_with("base_url "), "{body}");
    }
    let forged = r#"{"kind":"openai","key":"sk","model":"m","base_url":"http://127.0.0.1:1","seeded":true}"#;
    let (status, _) = send(&app, Method::PUT, "/api/settings/llm-key", &cookie, Some(forged)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let rows: i64 = state.db().query_row("SELECT COUNT(*) FROM llm_keys", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn models_and_picks_follow_the_users_own_provider() {
    let fake = common::fake_http();
    let (app, cookie, _state, _cfg) = strict(server_llm(), &fake.base).await;
    let (_, none) = send(&app, Method::GET, "/api/models", &cookie, None).await;
    assert_eq!(json(&none)["models"], serde_json::json!([]), "a user with no key has nothing to pick");

    let catalogue = r#"{"data":[{"id":"gpt-5","created":3},{"id":"gpt-5-mini","created":2},{"id":"whisper-1","created":9}]}"#;
    fake.answer(catalogue);
    let put = r#"{"kind":"openai","key":"sk-proj-abcd","model":"gpt-5-mini"}"#;
    assert_eq!(send(&app, Method::PUT, "/api/settings/llm-key", &cookie, Some(put)).await.0, StatusCode::OK);
    let (_, models) = send(&app, Method::GET, "/api/models", &cookie, None).await;
    let models = json(&models);
    assert_eq!(models["default"], "gpt-5-mini");
    let ids: Vec<&str> = models["models"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["gpt-5-mini", "gpt-5"]);

    let (status, _) = send(&app, Method::PUT, "/api/settings", &cookie, Some(r#"{"model":"gpt-5"}"#)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&app, Method::PUT, "/api/settings", &cookie, Some(r#"{"model":"server/model"}"#)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "the server's catalogue is not this key's");
}

#[tokio::test]
async fn without_a_key_a_turn_asks_for_one_and_a_share_is_unavailable() {
    let fake = common::fake_http();
    let llm = Arc::new(MockLLM::scripted(vec![ChatResponse { text: "should not be asked".into(), tool_calls: vec![] }]));
    let (app, cookie, _state, _cfg) = strict(llm.clone(), &fake.base).await;
    let (status, body) = send(&app, Method::POST, "/api/talk", &cookie, Some(r#"{"message":"hello"}"#)).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let v = json(&body);
    assert_eq!(v["reason"], "no_key");
    assert_eq!(v["error"], "Add your AI key in Settings to use Note.");

    let expires = (jiff::Timestamp::now() + jiff::Span::new().hours(24 * 30)).to_string();
    let share = format!(r#"{{"name":"Mom","brief":"be warm","scope":{{}},"expires_at":"{expires}"}}"#);
    let (status, made) = send(&app, Method::POST, "/api/shares", &cookie, Some(&share)).await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let token = json(&made)["url"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    let (status, body) = send(&app, Method::POST, &format!("/api/share/{token}/messages"), "", Some(r#"{"message":"hi"}"#)).await;
    assert_eq!((status, json(&body)["error"].as_str()), (StatusCode::BAD_GATEWAY, Some("Note could not answer")));
    assert!(llm.seen().is_empty(), "the server's key is never used");
}

#[tokio::test]
async fn onboarding_cannot_finish_until_a_verified_key_is_saved() {
    let refused = refusing("403 Forbidden");
    let (app, cookie, state, _cfg) = strict(server_llm(), &refused).await;
    state.db().execute("UPDATE users SET onboarding = 1 WHERE id = 1", []).unwrap();
    assert_eq!(json(&send(&app, Method::GET, "/api/me", &cookie, None).await.1)["needs_key"], true);
    let put = r#"{"kind":"openrouter","key":"sk-or-bad","model":"mine/model"}"#;
    assert_eq!(send(&app, Method::PUT, "/api/settings/llm-key", &cookie, Some(put)).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(send(&app, Method::POST, "/api/onboarding/done", &cookie, None).await.0, StatusCode::CONFLICT);

    let fake = common::fake_http();
    let mut good = state.clone();
    good.own_keys = Arc::new(OwnKeys::ephemeral().with_bases(&fake.base, &fake.base));
    let app = note_server::api::router(good);
    fake.answer(r#"{"data":{"label":"k"}}"#);
    fake.answer(r#"{"data":[{"id":"mine/model"}]}"#);
    let put = r#"{"kind":"openrouter","key":"sk-or-good","model":"mine/model"}"#;
    assert_eq!(send(&app, Method::PUT, "/api/settings/llm-key", &cookie, Some(put)).await.0, StatusCode::OK);
    assert_eq!(json(&send(&app, Method::GET, "/api/me", &cookie, None).await.1)["needs_key"], false);
    assert_eq!(send(&app, Method::POST, "/api/onboarding/done", &cookie, None).await.0, StatusCode::NO_CONTENT);
}
