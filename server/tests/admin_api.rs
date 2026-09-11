mod common;
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::{auth, totp, AppState};
use tower::ServiceExt;

const SEED: &[u8] = b"12345678901234567890";

async fn json(res: axum::response::Response) -> serde_json::Value {
    let body = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn req(method: Method, path: &str, cookies: &str, body: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, cookies);
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    b.body(body.map(|s| Body::from(s.to_string())).unwrap_or_else(Body::empty))
        .unwrap()
}

fn code_now() -> String {
    totp::code(SEED, totp::step_at(jiff::Timestamp::now()))
}

/// Elevates the session and returns the combined cookie header value.
async fn elevate(app: &axum::Router, session: &str, code: &str) -> Option<String> {
    let body = format!(r#"{{"password":"pw","code":"{code}"}}"#);
    let res = app
        .clone()
        .oneshot(req(Method::POST, "/api/admin/elevate", session, Some(&body)))
        .await
        .unwrap();
    if res.status() != StatusCode::OK {
        return None;
    }
    let admin = res.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    Some(format!("{session}; {admin}"))
}

async fn elevated_app() -> (axum::Router, String, String, AppState, tempfile::TempDir) {
    let (app, session, state, cfg) = common::app_with_admin_seed(SEED.to_vec()).await;
    let cookies = elevate(&app, &session, &code_now()).await.expect("elevation");
    (app, session, cookies, state, cfg)
}

#[tokio::test]
async fn members_get_403_on_every_admin_route() {
    let (app, _admin, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    {
        let conn = state.db.lock().unwrap();
        auth::create_user(&conn, "kid", "pw", false).unwrap();
    }
    let kid = common::login(&app, "kid", "pw").await;
    for (m, p, body) in [
        (Method::GET, "/api/admin/gate", None),
        (Method::POST, "/api/admin/elevate", Some(r#"{"password":"pw"}"#)),
        (Method::POST, "/api/admin/drop", None),
        (Method::GET, "/api/admin/status", None),
        (Method::GET, "/api/admin/users", None),
        (Method::POST, "/api/admin/users", Some(r#"{"username":"x","password":"y"}"#)),
        (Method::PATCH, "/api/admin/users/1", Some(r#"{"disabled":true}"#)),
        (Method::POST, "/api/admin/users/1/revoke_sessions", None),
        (Method::GET, "/api/admin/log", None),
    ] {
        let res = app.clone().oneshot(req(m.clone(), p, &kid, body)).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{m} {p}");
    }
    let res = app.oneshot(Request::get("/api/admin/status").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_without_a_grant_sees_the_gate_and_nothing_else() {
    let (app, session, _state, _cfg) = common::app_with_admin_seed(SEED.to_vec()).await;
    let res = app.clone().oneshot(req(Method::GET, "/api/admin/gate", &session, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    let v = json(res).await;
    assert_eq!(v["elevated"], false);
    assert_eq!(v["totp"], "required");
    assert_eq!(v["inspect"], cfg!(feature = "dev-inspect"));
    for p in ["/api/admin/status", "/api/admin/users", "/api/admin/log"] {
        let res = app.clone().oneshot(req(Method::GET, p, &session, None)).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{p}");
    }
}

#[tokio::test]
async fn elevation_needs_both_factors_and_refuses_replay() {
    let (app, session, _state, _cfg) = common::app_with_admin_seed(SEED.to_vec()).await;
    let code = code_now();
    let wrong_pw = format!(r#"{{"password":"nope","code":"{code}"}}"#);
    let res = app.clone().oneshot(req(Method::POST, "/api/admin/elevate", &session, Some(&wrong_pw))).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(elevate(&app, &session, "000000").await.is_none() || code == "000000");
    let res = app.clone().oneshot(req(Method::POST, "/api/admin/elevate", &session, Some(r#"{"password":"pw"}"#))).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let cookies = elevate(&app, &session, &code).await.expect("both factors right");
    assert!(elevate(&app, &session, &code).await.is_none(), "a used code must not work twice");

    let res = app.clone().oneshot(req(Method::GET, "/api/admin/gate", &cookies, None)).await.unwrap();
    let v = json(res).await;
    assert_eq!(v["elevated"], true);
    assert!(v["expires_at"].as_str().unwrap().starts_with("20"));
    let res = app.clone().oneshot(req(Method::GET, "/api/admin/status", &cookies, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json(res).await;
    assert_eq!(v["users"], 1);
    assert_eq!(v["sessions"], 1);
    assert_eq!(v["secrets"]["admin_totp"], true);
    assert_eq!(v["build"], if cfg!(feature = "dev-inspect") { "dev-inspect" } else { "release" });
}

#[tokio::test]
async fn elevate_cookie_is_scoped_strict_and_short_lived() {
    let (app, session, _state, _cfg) = common::app_with_admin_seed(SEED.to_vec()).await;
    let body = format!(r#"{{"password":"pw","code":"{}"}}"#, code_now());
    let res = app.oneshot(req(Method::POST, "/api/admin/elevate", &session, Some(&body))).await.unwrap();
    let cookie = res.headers()[header::SET_COOKIE].to_str().unwrap().to_string();
    assert!(cookie.starts_with("admin="));
    for attr in ["HttpOnly", "Path=/api/admin", "SameSite=Strict", "Max-Age=900"] {
        assert!(cookie.contains(attr), "{cookie} lacks {attr}");
    }
}

#[tokio::test]
async fn a_grant_is_bound_to_its_session_and_dies_with_it() {
    let (app, session_a, cookies_a, _state, _cfg) = elevated_app().await;
    let session_b = common::login(&app, "aki", "pw").await;
    let admin_cookie = cookies_a.split("; ").nth(1).unwrap();
    let mixed = format!("{session_b}; {admin_cookie}");
    let res = app.clone().oneshot(req(Method::GET, "/api/admin/status", &mixed, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "another session cannot borrow the grant");

    let res = app.clone().oneshot(req(Method::POST, "/api/logout", &session_a, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let session_c = common::login(&app, "aki", "pw").await;
    let after = format!("{session_c}; {admin_cookie}");
    let res = app.clone().oneshot(req(Method::GET, "/api/admin/status", &after, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "logout removes the grant");
}

#[tokio::test]
async fn expired_grants_stop_working_and_drop_clears_them() {
    let (app, _session, cookies, state, _cfg) = elevated_app().await;
    {
        let conn = state.db.lock().unwrap();
        conn.execute("UPDATE admin_grants SET expires_at = expires_at - 1000", []).unwrap();
    }
    let res = app.clone().oneshot(req(Method::GET, "/api/admin/users", &cookies, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let (app, _session, cookies, state, _cfg) = elevated_app().await;
    let res = app.clone().oneshot(req(Method::POST, "/api/admin/drop", &cookies, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers()[header::SET_COOKIE].to_str().unwrap().contains("Max-Age=0"));
    let n: i64 = state.db.lock().unwrap().query_row("SELECT COUNT(*) FROM admin_grants", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
    let res = app.oneshot(req(Method::GET, "/api/admin/users", &cookies, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn cross_site_requests_are_refused_before_anything_else() {
    let (app, _session, cookies, _state, _cfg) = elevated_app().await;
    let mut r = req(Method::GET, "/api/admin/status", &cookies, None);
    r.headers_mut().insert("sec-fetch-site", "cross-site".parse().unwrap());
    let res = app.clone().oneshot(r).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let mut r = req(Method::GET, "/api/admin/status", &cookies, None);
    r.headers_mut().insert("sec-fetch-site", "same-origin".parse().unwrap());
    let res = app.oneshot(r).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn elevation_attempts_are_rate_limited() {
    let (app, session, _state, _cfg) = common::app_with_admin_seed(SEED.to_vec()).await;
    for _ in 0..auth::MAX_ATTEMPTS {
        let res = app.clone().oneshot(req(Method::POST, "/api/admin/elevate", &session, Some(r#"{"password":"nope","code":"000000"}"#))).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
    let body = format!(r#"{{"password":"pw","code":"{}"}}"#, code_now());
    let res = app.oneshot(req(Method::POST, "/api/admin/elevate", &session, Some(&body))).await.unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[cfg(not(feature = "dev-inspect"))]
#[tokio::test]
async fn a_release_server_without_a_seed_refuses_elevation() {
    let (app, session, _state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let res = app.clone().oneshot(req(Method::GET, "/api/admin/gate", &session, None)).await.unwrap();
    assert_eq!(json(res).await["totp"], "missing");
    let res = app.oneshot(req(Method::POST, "/api/admin/elevate", &session, Some(r#"{"password":"pw"}"#))).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[cfg(feature = "dev-inspect")]
#[tokio::test]
async fn a_dev_server_without_a_seed_elevates_on_the_password_alone() {
    let (app, session, _state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let res = app.clone().oneshot(req(Method::GET, "/api/admin/gate", &session, None)).await.unwrap();
    assert_eq!(json(res).await["totp"], "password_only");
    let res = app.clone().oneshot(req(Method::POST, "/api/admin/elevate", &session, Some(r#"{"password":"nope"}"#))).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = app.oneshot(req(Method::POST, "/api/admin/elevate", &session, Some(r#"{"password":"pw"}"#))).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn user_management_round_trip() {
    let (app, _session, cookies, state, _cfg) = elevated_app().await;
    let res = app.clone().oneshot(req(Method::POST, "/api/admin/users", &cookies, Some(r#"{"username":"kid","password":"pw"}"#))).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let kid_id = json(res).await["id"].as_i64().unwrap();
    let res = app.clone().oneshot(req(Method::POST, "/api/admin/users", &cookies, Some(r#"{"username":"kid","password":"pw"}"#))).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let res = app.clone().oneshot(req(Method::POST, "/api/admin/users", &cookies, Some(r#"{"username":"../x","password":"pw"}"#))).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let res = app.clone().oneshot(req(Method::POST, "/api/admin/users", &cookies, Some(r#"{"username":"z","password":""}"#))).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let kid = common::login(&app, "kid", "pw").await;
    let res = app.clone().oneshot(req(Method::GET, "/api/admin/users", &cookies, None)).await.unwrap();
    let v = json(res).await;
    assert_eq!(v[1]["username"], "kid");
    assert_eq!(v[1]["role"], "member");
    assert_eq!(v[1]["sessions"], 1);

    let res = app.clone().oneshot(req(Method::PATCH, "/api/admin/users/1", &cookies, Some(r#"{"role":"member"}"#))).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT, "not your own role");
    let res = app.clone().oneshot(req(Method::PATCH, &format!("/api/admin/users/{kid_id}"), &cookies, Some(r#"{"password":"new"}"#))).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let res = app.clone().oneshot(req(Method::GET, "/api/me", &kid, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "password reset ends the old sessions");
    let kid = common::login(&app, "kid", "new").await;

    let res = app.clone().oneshot(req(Method::PATCH, &format!("/api/admin/users/{kid_id}"), &cookies, Some(r#"{"disabled":true}"#))).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let res = app.clone().oneshot(req(Method::GET, "/api/me", &kid, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "disabled accounts lose their sessions");
    let res = app.clone().oneshot(Request::post("/api/login").header(header::CONTENT_TYPE, "application/json").body(Body::from(r#"{"username":"kid","password":"new"}"#)).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "disabled accounts cannot sign in");

    let res = app.clone().oneshot(req(Method::PATCH, &format!("/api/admin/users/{kid_id}"), &cookies, Some(r#"{"disabled":false,"role":"admin"}"#))).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let _ = common::login(&app, "kid", "new").await;
    let _ = common::login(&app, "kid", "new").await;
    let res = app.clone().oneshot(req(Method::POST, &format!("/api/admin/users/{kid_id}/revoke_sessions"), &cookies, None)).await.unwrap();
    assert_eq!(json(res).await["revoked"], 2);

    let res = app.clone().oneshot(req(Method::GET, "/api/admin/log?kind=admin_user_update", &cookies, None)).await.unwrap();
    let v = json(res).await;
    let rows = v["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r["kind"] == "admin_user_update" && r["user_id"] == 1));
    assert!(v["kinds"].as_array().unwrap().iter().any(|k| k == "admin_elevate"));
    let first_id = rows[0]["id"].as_i64().unwrap();
    let res = app.clone().oneshot(req(Method::GET, &format!("/api/admin/log?before_id={first_id}&limit=1"), &cookies, None)).await.unwrap();
    let v = json(res).await;
    assert_eq!(v["rows"].as_array().unwrap().len(), 1);
    assert!(v["rows"][0]["id"].as_i64().unwrap() < first_id);

    let n: i64 = state.db.lock().unwrap().query_row("SELECT COUNT(*) FROM event_log WHERE kind LIKE 'admin_%'", [], |r| r.get(0)).unwrap();
    assert!(n >= 6, "every admin action is audited, got {n}");
}

#[cfg(not(feature = "dev-inspect"))]
#[tokio::test]
async fn inspection_routes_do_not_exist_in_a_release_build() {
    let (app, _session, cookies, _state, _cfg) = elevated_app().await;
    for (m, p) in [
        (Method::GET, "/api/admin/inspect/users/1"),
        (Method::POST, "/api/admin/inspect/sql"),
    ] {
        let res = app.clone().oneshot(req(m, p, &cookies, Some("{}"))).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{p}");
    }
}

#[cfg(feature = "dev-inspect")]
#[tokio::test]
async fn inspection_reads_and_edits_user_data() {
    let (app, _session, cookies, state, cfg) = elevated_app().await;
    {
        let conn = state.db.lock().unwrap();
        note_server::tasks::create(&conn, 1, note_server::tasks::NewTask { title: "walk".into(), ..Default::default() }, "manual", note_server::tasks::Actor::User).unwrap();
        let cid = note_server::talk::create(&conn, 1, "hello", jiff::Timestamp::now()).unwrap();
        note_server::talk::append_text(&conn, cid, "user", "hi", jiff::Timestamp::now()).unwrap();
        note_server::memory::add(&conn, cfg.path(), "aki", "semantic", "likes tea", "green tea", None).unwrap();
    }
    let res = app.clone().oneshot(req(Method::GET, "/api/admin/inspect/users/1", &cookies, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json(res).await;
    assert_eq!(v["tasks"][0]["title"], "walk");
    assert_eq!(v["conversations"][0]["title"], "hello");
    assert_eq!(v["memory"][0]["summary"], "likes tea");
    assert!(v["config_path"].as_str().unwrap().ends_with("users/aki/user.toml"));
    let mid = v["memory"][0]["id"].as_str().unwrap().to_string();
    let cid = v["conversations"][0]["id"].as_i64().unwrap();

    let res = app.clone().oneshot(req(Method::GET, &format!("/api/admin/inspect/users/1/conversations/{cid}"), &cookies, None)).await.unwrap();
    assert_eq!(json(res).await[0]["content"], "hi");

    let res = app.clone().oneshot(req(Method::PUT, "/api/admin/inspect/users/1/config", &cookies, Some(r#"{"toml":"nightly_time = \"9:00\"\n"}"#))).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let res = app.clone().oneshot(req(Method::PUT, "/api/admin/inspect/users/1/config", &cookies, Some(r#"{"toml":"timezone = \"Asia/Tokyo\"\n"}"#))).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(note_server::config::UserConfig::load(cfg.path(), "aki").unwrap().timezone, "Asia/Tokyo");

    let res = app.clone().oneshot(req(Method::GET, &format!("/api/admin/inspect/users/1/memory/{mid}"), &cookies, None)).await.unwrap();
    let raw = json(res).await["content"].as_str().unwrap().to_string();
    assert!(raw.contains("green tea"));
    let edited = raw.replace("green tea", "black tea").replace("likes tea", "likes black tea");
    let body = serde_json::json!({ "content": edited }).to_string();
    let res = app.clone().oneshot(req(Method::PUT, &format!("/api/admin/inspect/users/1/memory/{mid}"), &cookies, Some(&body))).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let f = note_server::memory::read(cfg.path(), "aki", &mid).unwrap().unwrap();
    assert_eq!(f.body, "black tea");
    assert_eq!(f.summary, "likes black tea");

    let res = app.clone().oneshot(req(Method::POST, "/api/admin/inspect/sql", &cookies, Some(r#"{"sql":"UPDATE tasks SET state = 'done'"}"#))).await.unwrap();
    assert_eq!(json(res).await["changes"], 1);
    let res = app.clone().oneshot(req(Method::POST, "/api/admin/inspect/sql", &cookies, Some(r#"{"sql":"SELECT state FROM tasks"}"#))).await.unwrap();
    assert_eq!(json(res).await["rows"], serde_json::json!([["done"]]));

    let res = app.oneshot(req(Method::GET, "/api/admin/log?kind=admin_inspect_sql", &cookies, None)).await.unwrap();
    assert_eq!(json(res).await["rows"].as_array().unwrap().len(), 2);
}
