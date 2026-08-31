mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn json(res: axum::response::Response) -> serde_json::Value {
    let body = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn get(cookie: &str) -> Request<Body> {
    Request::get("/api/settings")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

fn put(cookie: &str, body: &str) -> Request<Body> {
    Request::put("/api/settings")
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn write(dir: &std::path::Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

#[tokio::test]
async fn get_returns_effective_values_and_choices() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let res = app.oneshot(get(&cookie)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json(res).await;
    assert_eq!(v["display_name"], "X");
    assert_eq!(v["timezone"], "UTC");
    assert_eq!(v["nightly_time"], "03:00");
    assert_eq!(v["template"], "default");
    assert_eq!(v["templates"], serde_json::json!(["default"]));
    let zones = v["timezones"].as_array().unwrap();
    assert!(zones.len() > 100, "expected a full tzdb, got {}", zones.len());
    assert!(zones.contains(&serde_json::json!("UTC")));
    assert!(zones.contains(&serde_json::json!("Asia/Tokyo")));
    let sorted: Vec<_> = {
        let mut z = zones.clone();
        z.sort_by_key(|v| v.as_str().unwrap().to_string());
        z
    };
    assert_eq!(zones, &sorted);
}

#[tokio::test]
async fn templates_merge_defaults_and_user_overrides() {
    let (app, cookie, cfg) = common::app_with_logged_in_user().await;
    let events = "[[events]]\nkind='focus'\ntime='09:00'\ndays=['mon']\n";
    write(cfg.path(), "defaults/templates/deep-work.toml", events);
    write(cfg.path(), "users/aki/templates/default.toml", events);
    write(cfg.path(), "users/aki/templates/mine.toml", events);
    write(cfg.path(), "users/aki/templates/notes.md", "ignored");
    let v = json(app.oneshot(get(&cookie)).await.unwrap()).await;
    assert_eq!(
        v["templates"],
        serde_json::json!(["deep-work", "default", "mine"])
    );
}

#[tokio::test]
async fn put_persists_to_disk_and_is_reflected_by_get() {
    let (app, cookie, cfg) = common::app_with_logged_in_user().await;
    let res = app
        .clone()
        .oneshot(put(
            &cookie,
            r#"{"timezone":"Asia/Tokyo","nightly_time":"22:30"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json(res).await;
    assert_eq!(v["timezone"], "Asia/Tokyo");
    assert_eq!(v["nightly_time"], "22:30");
    assert_eq!(v["display_name"], "X");
    assert!(v.get("templates").is_none());
    assert!(v.get("timezones").is_none());

    let raw = std::fs::read_to_string(cfg.path().join("users/aki/user.toml")).unwrap();
    let on_disk: toml::Value = raw.parse().unwrap();
    let table = on_disk.as_table().unwrap();
    assert_eq!(table.len(), 4, "unexpected keys in {raw}");
    assert_eq!(table["display_name"].as_str(), Some("X"));
    assert_eq!(table["timezone"].as_str(), Some("Asia/Tokyo"));
    assert_eq!(table["nightly_time"].as_str(), Some("22:30"));
    assert_eq!(table["template"].as_str(), Some("default"));

    let v = json(app.oneshot(get(&cookie)).await.unwrap()).await;
    assert_eq!(v["timezone"], "Asia/Tokyo");
    assert_eq!(v["nightly_time"], "22:30");
}

#[tokio::test]
async fn put_display_name_is_trimmed_and_quotes_survive_a_round_trip() {
    let (app, cookie, cfg) = common::app_with_logged_in_user().await;
    let res = app
        .clone()
        .oneshot(put(&cookie, r#"{"display_name":"  a \"b\" \\ c  "}"#))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(json(res).await["display_name"], r#"a "b" \ c"#);
    let raw = std::fs::read_to_string(cfg.path().join("users/aki/user.toml")).unwrap();
    let on_disk: toml::Value = raw.parse().unwrap();
    assert_eq!(
        on_disk["display_name"].as_str(),
        Some(r#"a "b" \ c"#),
        "unparseable or lossy write: {raw}"
    );
    let v = json(app.oneshot(get(&cookie)).await.unwrap()).await;
    assert_eq!(v["display_name"], r#"a "b" \ c"#);
}

#[tokio::test]
async fn invalid_fields_are_rejected_by_name() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let cases = [
        (r#"{"display_name":"   "}"#, "display_name"),
        (
            &format!(r#"{{"display_name":"{}"}}"#, "a".repeat(65)),
            "display_name",
        ),
        (r#"{"timezone":"Not/AZone"}"#, "timezone"),
        (r#"{"nightly_time":"3:00"}"#, "nightly_time"),
        (r#"{"nightly_time":"24:00"}"#, "nightly_time"),
        (r#"{"template":"nope"}"#, "template"),
    ];
    for (body, field) in cases {
        let res = app.clone().oneshot(put(&cookie, body)).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "accepted {body}");
        let v = json(res).await;
        let msg = v["error"].as_str().unwrap_or_default();
        assert!(msg.contains(field), "error {msg:?} does not name {field}");
    }
}

#[tokio::test]
async fn unknown_fields_are_rejected_not_ignored() {
    let (app, cookie, cfg) = common::app_with_logged_in_user().await;
    let res = app
        .oneshot(put(&cookie, r#"{"tempalte":"default"}"#))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!cfg.path().join("users/aki/user.toml").exists());
}

#[tokio::test]
async fn a_rejected_put_writes_nothing() {
    let (app, cookie, cfg) = common::app_with_logged_in_user().await;
    let res = app
        .oneshot(put(
            &cookie,
            r#"{"display_name":"Aki","timezone":"Not/AZone"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(!cfg.path().join("users/aki/user.toml").exists());
}

#[tokio::test]
async fn unauthenticated_settings_requests_are_401() {
    let (app, _cookie, _cfg) = common::app_with_logged_in_user().await;
    let res = app
        .clone()
        .oneshot(
            Request::get("/api/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = app
        .oneshot(
            Request::put("/api/settings")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"timezone":"Asia/Tokyo"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
