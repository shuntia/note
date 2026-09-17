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
    assert_eq!(table.len(), 6, "unexpected keys in {raw}");
    assert_eq!(table["display_name"].as_str(), Some("X"));
    assert_eq!(table["timezone"].as_str(), Some("Asia/Tokyo"));
    assert_eq!(table["nightly_time"].as_str(), Some("22:30"));
    assert_eq!(table["template"].as_str(), Some("default"));
    assert_eq!(table["show_arc_between_sessions"].as_bool(), Some(true));
    assert_eq!(table["counter"].as_str(), Some("remaining"));

    let v = json(app.oneshot(get(&cookie)).await.unwrap()).await;
    assert_eq!(v["timezone"], "Asia/Tokyo");
    assert_eq!(v["nightly_time"], "22:30");
}

#[tokio::test]
async fn get_lists_one_row_per_template_entry() {
    let (app, cookie, cfg) = common::app_with_logged_in_user().await;
    write(cfg.path(), "defaults/templates/default.toml", concat!(
        "[[events]]\nkind='meds'\ntime='08:00'\ndays=['mon']\n",
        "[[events]]\nentry='block'\nkind='Work time'\ntime='09:30'\nend_time='12:30'\ndays=['mon']\n"));
    let v = json(app.oneshot(get(&cookie)).await.unwrap()).await;
    let rows = v["schedule"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["index"], 0);
    assert_eq!(rows[0]["kind"], "meds");
    assert_eq!(rows[0]["entry"], "routine");
    assert_eq!(rows[0]["alert"], true);
    assert_eq!(rows[0]["end_time"], "08:15");
    assert_eq!(rows[1]["entry"], "block");
    assert_eq!(rows[1]["time"], "09:30");
    assert_eq!(rows[1]["end_time"], "12:30");
    assert_eq!(rows[1]["alert"], false);
}

#[tokio::test]
async fn a_toggled_bell_persists_and_survives_a_nightly_rebuild() {
    let (app, cookie, state, cfg) = common::app_with_logged_in_user_and_state().await;
    let res = app
        .clone()
        .oneshot(put(&cookie, r#"{"alerts":[{"index":0,"alert":false}]}"#))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(json(res).await["schedule"][0]["alert"], false);
    assert!(cfg.path().join("users/aki/templates/default.toml").exists());

    let v = json(app.oneshot(get(&cookie)).await.unwrap()).await;
    assert_eq!(v["schedule"][0]["alert"], false);

    // the nightly job rebuilds tomorrow from the same template file
    let ucfg = note_server::config::UserConfig::load(cfg.path(), "aki").unwrap();
    let tmpl = note_server::templates::Template::load(cfg.path(), "aki", &ucfg.template).unwrap();
    let conn = state.db.lock().unwrap();
    let date: jiff::civil::Date = "2026-09-02".parse().unwrap();
    note_server::plan::generate(&conn, 1, &tmpl, date).unwrap();
    let evs = note_server::plan::events_for(&conn, 1, date).unwrap();
    assert_eq!(evs.len(), 1);
    assert!(!evs[0].alert);
    assert!(note_server::runner::fire_due(
        &conn,
        cfg.path(),
        "2026-09-02T23:00:00Z".parse().unwrap()
    )
    .unwrap()
    .is_empty());
}

#[tokio::test]
async fn a_bell_on_a_block_or_a_missing_row_is_rejected() {
    let (app, cookie, cfg) = common::app_with_logged_in_user().await;
    write(cfg.path(), "defaults/templates/default.toml",
        "[[events]]\nentry='block'\nkind='Work time'\ntime='09:30'\nend_time='12:30'\ndays=['mon']\n");
    for body in [
        r#"{"alerts":[{"index":0,"alert":false}]}"#,
        r#"{"alerts":[{"index":4,"alert":false}]}"#,
    ] {
        let res = app.clone().oneshot(put(&cookie, body)).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "accepted {body}");
        let v = json(res).await;
        assert!(v["error"].as_str().unwrap().contains("alerts"), "{v}");
    }
    assert!(!cfg.path().join("users/aki/templates/default.toml").exists());
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

#[tokio::test]
async fn home_settings_default_and_round_trip() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let v = json(app.clone().oneshot(get(&cookie)).await.unwrap()).await;
    assert_eq!(v["show_arc_between_sessions"], true);
    assert_eq!(v["counter"], "remaining");

    let res = app
        .clone()
        .oneshot(put(
            &cookie,
            r#"{"show_arc_between_sessions":false,"counter":"elapsed"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json(res).await;
    assert_eq!(v["show_arc_between_sessions"], false);
    assert_eq!(v["counter"], "elapsed");

    let v = json(app.oneshot(get(&cookie)).await.unwrap()).await;
    assert_eq!(v["show_arc_between_sessions"], false);
    assert_eq!(v["counter"], "elapsed");
}

#[tokio::test]
async fn counter_only_takes_the_two_words() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let res = app
        .oneshot(put(&cookie, r#"{"counter":"sideways"}"#))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = json(res).await;
    assert!(v["error"].as_str().unwrap().starts_with("counter"));
}

#[tokio::test]
async fn background_features_are_visible_and_flippable() {
    let (app, cookie, cfg) = common::app_with_logged_in_user().await;
    let v = json(app.clone().oneshot(get(&cookie)).await.unwrap()).await;
    assert_eq!(v["category"], "member");
    assert_eq!(v["nightly_enabled"], true);
    assert_eq!(v["checkins_enabled"], true);

    let res = app
        .clone()
        .oneshot(put(&cookie, r#"{"nightly_enabled":false}"#))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json(res).await;
    assert_eq!(v["nightly_enabled"], false);
    assert_eq!(v["checkins_enabled"], true);

    let raw = std::fs::read_to_string(cfg.path().join("users/aki/user.toml")).unwrap();
    assert!(raw.contains("nightly_enabled = false"), "unexpected file: {raw}");
    let v = json(app.oneshot(get(&cookie)).await.unwrap()).await;
    assert_eq!(v["nightly_enabled"], false);
}

#[tokio::test]
async fn a_test_account_starts_with_its_background_features_off() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    {
        let conn = state.db.lock().unwrap();
        note_server::auth::set_category(&conn, "aki", "test").unwrap();
    }
    let v = json(app.oneshot(get(&cookie)).await.unwrap()).await;
    assert_eq!(v["category"], "test");
    assert_eq!(v["nightly_enabled"], false);
    assert_eq!(v["checkins_enabled"], false);
}

#[tokio::test]
async fn ntfy_is_reported_off_with_the_default_topic_until_it_is_configured() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let v = json(app.oneshot(get(&cookie)).await.unwrap()).await;
    assert_eq!(v["ntfy_enabled"], false);
    assert_eq!(v["ntfy_topic"], "note-aki");
}

#[tokio::test]
async fn ntfy_topic_is_written_validated_and_cleared() {
    let (app, cookie, cfg) = common::app_with_logged_in_user().await;
    let path = cfg.path().join("users/aki/user.toml");

    let v = json(app.clone().oneshot(put(&cookie, r#"{"ntfy_topic":"my-desk"}"#)).await.unwrap()).await;
    assert_eq!(v["ntfy_topic"], "my-desk");
    assert_eq!(json(app.clone().oneshot(get(&cookie)).await.unwrap()).await["ntfy_topic"], "my-desk");

    for bad in [r#"{"ntfy_topic":"has space"}"#, r#"{"ntfy_topic":"has/slash"}"#, r#"{"ntfy_topic":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#] {
        let res = app.clone().oneshot(put(&cookie, bad)).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY, "accepted {bad}");
        assert!(json(res).await["error"].is_string());
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("my-desk"), "a rejected topic touched the file: {raw}");
    }

    let v = json(app.clone().oneshot(put(&cookie, r#"{"ntfy_topic":""}"#)).await.unwrap()).await;
    assert_eq!(v["ntfy_topic"], "note-aki");
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains("ntfy_topic"), "the cleared override stayed in the file: {raw}");
}

/// A topic is a shared namespace on the ntfy server: taking the name another
/// account's deliveries go to would subscribe this user to them.
#[tokio::test]
async fn another_users_default_topic_cannot_be_claimed() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    {
        let conn = state.db.lock().unwrap();
        note_server::auth::create_user(&conn, "bo", "pw", false).unwrap();
    }

    let res = app.clone().oneshot(put(&cookie, r#"{"ntfy_topic":"note-bo"}"#)).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(json(res).await["error"].is_string());

    // the user's own default, and a name no account answers to, are fine
    let v = json(app.clone().oneshot(put(&cookie, r#"{"ntfy_topic":"note-aki"}"#)).await.unwrap()).await;
    assert_eq!(v["ntfy_topic"], "note-aki");
    let v = json(app.oneshot(put(&cookie, r#"{"ntfy_topic":"note-nobody"}"#)).await.unwrap()).await;
    assert_eq!(v["ntfy_topic"], "note-nobody");
}
