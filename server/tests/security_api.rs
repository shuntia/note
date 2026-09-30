mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::{auth, security, totp, AppState};
use tower::ServiceExt;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_rs::prelude::{CreationChallengeResponse, RequestChallengeResponse, Url};

async fn read(res: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

async fn call(
    app: &axum::Router,
    cookie: &str,
    method: Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, cookie);
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    read(
        app.clone()
            .oneshot(req.body(Body::from(payload)).unwrap())
            .await
            .unwrap(),
    )
    .await
}

fn password(pw: &str) -> serde_json::Value {
    serde_json::json!({ "password": pw })
}

fn authenticator() -> WebauthnAuthenticator<SoftPasskey> {
    // the relying party asks for user verification, which a soft token can only claim
    WebauthnAuthenticator::new(SoftPasskey::new(true))
}

/// Registers one passkey the way the browser would, and keeps the authenticator
/// so the same credential can assert later.
async fn enrol_passkey(
    app: &axum::Router,
    cookie: &str,
    name: &str,
) -> (WebauthnAuthenticator<SoftPasskey>, serde_json::Value) {
    let mut soft = authenticator();
    let (status, challenge) = call(
        app,
        cookie,
        Method::POST,
        "/api/security/passkeys/challenge",
        Some(password("pw")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{challenge}");
    let ccr: CreationChallengeResponse = serde_json::from_value(challenge).unwrap();
    let made = soft
        .do_registration(Url::parse(common::ORIGIN).unwrap(), ccr)
        .unwrap();
    let (status, info) = call(
        app,
        cookie,
        Method::POST,
        "/api/security/passkeys",
        Some(serde_json::json!({ "name": name, "credential": made })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{info}");
    (soft, info)
}

async fn assertion(
    app: &axum::Router,
    cookie: &str,
    soft: &mut WebauthnAuthenticator<SoftPasskey>,
) -> serde_json::Value {
    let (status, challenge) = call(
        app,
        cookie,
        Method::POST,
        "/api/admin/elevate/challenge",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{challenge}");
    let rcr: RequestChallengeResponse = serde_json::from_value(challenge).unwrap();
    let signed = soft
        .do_authentication(Url::parse(common::ORIGIN).unwrap(), rcr)
        .unwrap();
    serde_json::to_value(signed).unwrap()
}

fn kinds(state: &AppState) -> Vec<String> {
    let conn = state.db.lock().unwrap();
    let mut stmt = conn
        .prepare("SELECT kind FROM event_log ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

async fn gate(app: &axum::Router, cookie: &str) -> serde_json::Value {
    let (status, v) = call(app, cookie, Method::GET, "/api/admin/gate", None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v
}

#[tokio::test]
async fn the_overview_reports_no_factors_on_a_fresh_account() {
    let (app, cookie, _state, _cfg) = common::app_with_passkeys().await;
    let (status, v) = call(&app, &cookie, Method::GET, "/api/security", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(v["passkeys"].as_array().unwrap().is_empty());
    assert_eq!(
        v["totp"],
        serde_json::json!({ "enabled": false, "pending": false })
    );
    assert_eq!(v["webauthn_available"], true);
}

#[tokio::test]
async fn an_http_origin_reports_passkeys_unavailable() {
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let (_, v) = call(&app, &cookie, Method::GET, "/api/security", None).await;
    assert_eq!(v["webauthn_available"], false);
    let (status, v) = call(
        &app,
        &cookie,
        Method::POST,
        "/api/security/passkeys/challenge",
        Some(password("pw")),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(v["error"].as_str().unwrap().contains("https"));
}

#[tokio::test]
async fn every_mutating_call_re_checks_the_password() {
    let (app, cookie, state, _cfg) = common::app_with_passkeys().await;
    for (method, path) in [
        (Method::POST, "/api/security/passkeys/challenge"),
        (Method::POST, "/api/security/totp/start"),
        (Method::DELETE, "/api/security/totp"),
        (Method::DELETE, "/api/security/passkeys/1"),
    ] {
        let (status, v) = call(&app, &cookie, method, path, Some(password("wrong"))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}: {v}");
        assert!(v["error"].is_string(), "{path}");
    }
    let (_, v) = call(&app, &cookie, Method::GET, "/api/security", None).await;
    assert!(v["passkeys"].as_array().unwrap().is_empty());
    assert_eq!(
        v["totp"]["pending"], false,
        "a refused start must not leave a secret"
    );
    assert!(security::totp_seed(&state.db.lock().unwrap(), 1)
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn a_passkey_is_registered_renamed_and_removed() {
    let (app, cookie, state, _cfg) = common::app_with_passkeys().await;
    let (_, info) = enrol_passkey(&app, &cookie, "  laptop  ").await;
    assert_eq!(info["name"], "laptop");
    assert!(info["last_used_at"].is_null());
    let id = info["id"].as_i64().unwrap();

    let (status, v) = call(&app, &cookie, Method::GET, "/api/security", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["passkeys"].as_array().unwrap().len(), 1);
    assert_eq!(v["passkeys"][0]["name"], "laptop");

    let (status, v) = call(
        &app,
        &cookie,
        Method::PATCH,
        &format!("/api/security/passkeys/{id}"),
        Some(serde_json::json!({ "name": "phone" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["name"], "phone");

    let (status, _) = call(
        &app,
        &cookie,
        Method::PATCH,
        &format!("/api/security/passkeys/{id}"),
        Some(serde_json::json!({ "name": "   " })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(
        &app,
        &cookie,
        Method::PATCH,
        "/api/security/passkeys/9999",
        Some(serde_json::json!({ "name": "nope" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = call(
        &app,
        &cookie,
        Method::DELETE,
        &format!("/api/security/passkeys/{id}"),
        Some(password("pw")),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, v) = call(&app, &cookie, Method::GET, "/api/security", None).await;
    assert!(v["passkeys"].as_array().unwrap().is_empty());

    let logged = kinds(&state);
    assert!(logged.contains(&"passkey_added".to_string()), "{logged:?}");
    assert!(
        logged.contains(&"passkey_removed".to_string()),
        "{logged:?}"
    );
}

#[tokio::test]
async fn one_credential_cannot_be_registered_twice_and_the_count_is_capped() {
    let (app, cookie, state, _cfg) = common::app_with_passkeys().await;
    let mut soft = authenticator();
    let (_, challenge) = call(
        &app,
        &cookie,
        Method::POST,
        "/api/security/passkeys/challenge",
        Some(password("pw")),
    )
    .await;
    let ccr: CreationChallengeResponse = serde_json::from_value(challenge).unwrap();
    let made = soft
        .do_registration(Url::parse(common::ORIGIN).unwrap(), ccr)
        .unwrap();
    let body = serde_json::json!({ "name": "one", "credential": made });
    let (status, _) = call(
        &app,
        &cookie,
        Method::POST,
        "/api/security/passkeys",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, v) = call(
        &app,
        &cookie,
        Method::POST,
        "/api/security/passkeys",
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert!(
        v["error"].is_string(),
        "a spent challenge is not a second registration"
    );

    {
        let conn = state.db.lock().unwrap();
        for i in 1..security::MAX_PASSKEYS {
            conn.execute(
                "INSERT INTO passkeys (user_id, name, credential, cred_id, created_at)
                 VALUES (1, ?1, '{}', ?2, 'now')",
                (format!("k{i}"), vec![200 + u8::try_from(i).unwrap()]),
            )
            .unwrap();
        }
    }
    let (status, v) = call(
        &app,
        &cookie,
        Method::POST,
        "/api/security/passkeys/challenge",
        Some(password("pw")),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert!(v["error"].as_str().unwrap().contains("10"));
}

#[tokio::test]
async fn a_member_enrols_an_authenticator_app() {
    let (app, admin_cookie, state, _cfg) = common::app_with_passkeys().await;
    {
        let conn = state.db.lock().unwrap();
        auth::create_user(&conn, "bo", "pw", false).unwrap();
    }
    let bo = common::login(&app, "bo", "pw").await;

    let (status, start) = call(
        &app,
        &bo,
        Method::POST,
        "/api/security/totp/start",
        Some(password("pw")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let secret = start["secret_base32"].as_str().unwrap().to_string();
    assert_eq!(start["issuer"], "Note");
    assert_eq!(start["account"], "bo");
    assert!(start["otpauth_uri"]
        .as_str()
        .unwrap()
        .starts_with("otpauth://totp/Note:bo?secret="));
    assert!(start["otpauth_uri"].as_str().unwrap().contains(&secret));

    let (_, v) = call(&app, &bo, Method::GET, "/api/security", None).await;
    assert_eq!(
        v["totp"],
        serde_json::json!({ "enabled": false, "pending": true })
    );

    let (status, v) = call(
        &app,
        &bo,
        Method::POST,
        "/api/security/totp/confirm",
        Some(serde_json::json!({ "code": "000000" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");

    let seed = totp::parse_seed(&secret).unwrap();
    let code = totp::code(&seed, totp::step_at(jiff::Timestamp::now()));
    let confirm = serde_json::json!({ "code": code });
    let (status, _) = call(
        &app,
        &bo,
        Method::POST,
        "/api/security/totp/confirm",
        Some(confirm.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, v) = call(&app, &bo, Method::GET, "/api/security", None).await;
    assert_eq!(
        v["totp"],
        serde_json::json!({ "enabled": true, "pending": false })
    );
    let (status, _) = call(
        &app,
        &bo,
        Method::POST,
        "/api/security/totp/confirm",
        Some(confirm),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a used code cannot be replayed"
    );

    // the member's enrolment is theirs alone
    let (_, mine) = call(&app, &admin_cookie, Method::GET, "/api/security", None).await;
    assert_eq!(mine["totp"]["enabled"], false);

    let (status, _) = call(
        &app,
        &bo,
        Method::DELETE,
        "/api/security/totp",
        Some(password("pw")),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, v) = call(&app, &bo, Method::GET, "/api/security", None).await;
    assert_eq!(
        v["totp"],
        serde_json::json!({ "enabled": false, "pending": false })
    );

    let logged = kinds(&state);
    assert!(logged.contains(&"totp_enrolled".to_string()), "{logged:?}");
    assert!(logged.contains(&"totp_removed".to_string()), "{logged:?}");
}

#[tokio::test]
async fn the_gate_reports_what_this_admin_has_enrolled() {
    let (app, cookie, _state, _cfg) = common::app_with_passkeys().await;
    let v = gate(&app, &cookie).await;
    assert_eq!(
        v["methods"],
        serde_json::json!({ "passkey": false, "totp": false })
    );
    assert_eq!(v["second_factor"], "none");
    assert_eq!(v["require_second_factor"], !note_server::admin::INSPECT);

    enrol_totp(&app, &cookie).await;
    let v = gate(&app, &cookie).await;
    assert_eq!(
        v["methods"],
        serde_json::json!({ "passkey": false, "totp": true })
    );
    assert_eq!(v["second_factor"], "totp");
    assert_eq!(v["require_second_factor"], true);
    assert_eq!(v["totp"], "required");

    enrol_passkey(&app, &cookie, "phone").await;
    let v = gate(&app, &cookie).await;
    assert_eq!(
        v["methods"],
        serde_json::json!({ "passkey": true, "totp": true })
    );
    assert_eq!(v["second_factor"], "passkey");
}

/// Enrols an authenticator app and returns its seed.
async fn enrol_totp(app: &axum::Router, cookie: &str) -> Vec<u8> {
    enrol_totp_at(app, cookie, totp::step_at(jiff::Timestamp::now())).await
}

/// Confirms with the code for one particular step, which a test picks when an
/// earlier code of its own already spent the current one.
async fn enrol_totp_at(app: &axum::Router, cookie: &str, step: i64) -> Vec<u8> {
    let (status, start) = call(
        app,
        cookie,
        Method::POST,
        "/api/security/totp/start",
        Some(password("pw")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let seed = totp::parse_seed(start["secret_base32"].as_str().unwrap()).unwrap();
    let code = totp::code(&seed, step);
    let (status, _) = call(
        app,
        cookie,
        Method::POST,
        "/api/security/totp/confirm",
        Some(serde_json::json!({ "code": code })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    seed
}

async fn elevate(
    app: &axum::Router,
    cookie: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    call(app, cookie, Method::POST, "/api/admin/elevate", Some(body)).await
}

#[tokio::test]
async fn a_passkey_assertion_elevates_and_stamps_the_credential() {
    let (app, cookie, state, _cfg) = common::app_with_passkeys().await;
    let (mut soft, info) = enrol_passkey(&app, &cookie, "phone").await;
    let id = info["id"].as_i64().unwrap();

    let signed = assertion(&app, &cookie, &mut soft).await;
    let (status, v) = elevate(
        &app,
        &cookie,
        serde_json::json!({ "password": "nope", "assertion": signed }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");

    let signed = assertion(&app, &cookie, &mut soft).await;
    let (status, v) = elevate(
        &app,
        &cookie,
        serde_json::json!({ "password": "pw", "assertion": signed.clone() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(v["expires_at"].is_string());

    let (status, _) = elevate(
        &app,
        &cookie,
        serde_json::json!({ "password": "pw", "assertion": signed }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "an assertion is single use"
    );

    let last: Option<String> = state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT last_used_at FROM passkeys WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(last.is_some(), "the credential that elevated is stamped");
    assert!(kinds(&state).contains(&"admin_elevate".to_string()));
}

#[tokio::test]
async fn a_per_user_code_elevates_and_a_missing_factor_does_not() {
    let (app, cookie, state, _cfg) = common::app_with_passkeys().await;
    let seed = enrol_totp(&app, &cookie).await;

    let (status, _) = elevate(&app, &cookie, serde_json::json!({ "password": "pw" })).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the password alone is not enough"
    );
    let (status, _) = elevate(
        &app,
        &cookie,
        serde_json::json!({ "password": "pw", "code": "000000" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // the step enrolment spent is gone; the next code is the one that elevates
    let code = totp::code(&seed, totp::step_at(jiff::Timestamp::now()) + 1);
    let (status, v) = elevate(
        &app,
        &cookie,
        serde_json::json!({ "password": "pw", "code": code.clone() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, _) = elevate(
        &app,
        &cookie,
        serde_json::json!({ "password": "pw", "code": code }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a used code cannot be replayed"
    );
    assert!(kinds(&state).contains(&"admin_elevate_denied".to_string()));
}

#[tokio::test]
async fn a_per_user_secret_takes_over_from_the_shared_seed() {
    const SEED: &[u8] = b"12345678901234567890";
    let (app, cookie, _state, _cfg) = common::app_with_seed_and_passkeys(SEED.to_vec()).await;
    // each accepted code spends its time step, so the three below walk forward
    let step = totp::step_at(jiff::Timestamp::now());

    let v = gate(&app, &cookie).await;
    assert_eq!(
        v["methods"],
        serde_json::json!({ "passkey": false, "totp": true })
    );

    let shared = totp::code(SEED, step - 1);
    let (status, v) = elevate(
        &app,
        &cookie,
        serde_json::json!({ "password": "pw", "code": shared }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the shared seed still elevates: {v}"
    );

    let seed = enrol_totp_at(&app, &cookie, step).await;
    let shared = totp::code(SEED, step + 1);
    let (status, _) = elevate(
        &app,
        &cookie,
        serde_json::json!({ "password": "pw", "code": shared }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the shared seed stops once the user has one"
    );
    let mine = totp::code(&seed, step + 1);
    let (status, v) = elevate(
        &app,
        &cookie,
        serde_json::json!({ "password": "pw", "code": mine }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn an_account_with_no_factor_cannot_elevate_on_a_release_build() {
    let (app, cookie, _state, _cfg) = common::app_with_passkeys().await;
    let v = gate(&app, &cookie).await;
    let (status, body) = elevate(&app, &cookie, serde_json::json!({ "password": "pw" })).await;
    if note_server::admin::INSPECT {
        assert_eq!(v["totp"], "password_only");
        assert_eq!(status, StatusCode::OK, "{body}");
    } else {
        assert_eq!(v["totp"], "missing");
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert!(body["error"].is_string());
    }
}

#[tokio::test]
async fn a_challenge_needs_a_passkey_and_the_admin_role() {
    let (app, cookie, state, _cfg) = common::app_with_passkeys().await;
    let (status, v) = call(
        &app,
        &cookie,
        Method::POST,
        "/api/admin/elevate/challenge",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert!(v["error"].is_string());

    {
        let conn = state.db.lock().unwrap();
        auth::create_user(&conn, "bo", "pw", false).unwrap();
    }
    let bo = common::login(&app, "bo", "pw").await;
    let (status, _) = call(
        &app,
        &bo,
        Method::POST,
        "/api/admin/elevate/challenge",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn removing_a_passkey_leaves_it_unable_to_elevate() {
    let (app, cookie, _state, _cfg) = common::app_with_passkeys().await;
    enrol_totp(&app, &cookie).await;
    let (mut soft, info) = enrol_passkey(&app, &cookie, "phone").await;
    let signed = assertion(&app, &cookie, &mut soft).await;
    let id = info["id"].as_i64().unwrap();
    let (status, _) = call(
        &app,
        &cookie,
        Method::DELETE,
        &format!("/api/security/passkeys/{id}"),
        Some(password("pw")),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = elevate(
        &app,
        &cookie,
        serde_json::json!({ "password": "pw", "assertion": signed }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_password_only_server_ignores_enrolled_factors() {
    let (app, cookie, _state, _cfg) = common::app_with_password_only_admin().await;
    let v = gate(&app, &cookie).await;
    assert_eq!(v["require_second_factor"], false);
    assert_eq!(v["totp"], "password_only");
    let (status, v) = elevate(&app, &cookie, serde_json::json!({ "password": "pw" })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn the_enrolment_routes_need_a_session() {
    let (app, _cookie, _state, _cfg) = common::app_with_passkeys().await;
    let (status, _) = call(&app, "session=nobody", Method::GET, "/api/security", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        &app,
        "session=nobody",
        Method::POST,
        "/api/security/totp/start",
        Some(password("pw")),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
