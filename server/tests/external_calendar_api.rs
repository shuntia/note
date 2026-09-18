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

async fn token_for(app: &axum::Router, cookie: &str, name: &str) -> String {
    let (status, v) =
        with_cookie(app, cookie, Method::POST, "/api/tokens", Some(&format!(r#"{{"name":"{name}"}}"#)))
            .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["token"].as_str().unwrap().to_string()
}

/// An importer's environment: the user's own cookie and bearer token, and a
/// second account with a token of its own to prove the two never meet.
async fn importer() -> (axum::Router, String, String, String, tempfile::TempDir) {
    let (app, cookie, state, cfg) = common::app_with_logged_in_user_and_state().await;
    let mine = token_for(&app, &cookie, "importer").await;
    {
        let conn = state.db();
        auth::create_user(&conn, "bo", "pw", false).unwrap();
    }
    let theirs_cookie = common::login(&app, "bo", "pw").await;
    let theirs = token_for(&app, &theirs_cookie, "theirs").await;
    (app, cookie, mine, theirs, cfg)
}

const GCAL: &str = "/api/calendar/by-external/gcal:c_8f3a:t:busy";
const MEETING: &str = r#"{"title":"Club Meeting","kind":"busy","start_time":"11:00",
    "end_time":"12:00","on_date":"2026-09-19"}"#;

#[tokio::test]
async fn an_upsert_creates_once_and_refreshes_after() {
    let (app, cookie, token, _theirs, _cfg) = importer().await;
    let (status, made) = with_bearer(&app, &token, Method::PUT, GCAL, Some(MEETING)).await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert_eq!(made["external_id"], "gcal:c_8f3a:t:busy");
    assert_eq!(made["title"], "Club Meeting");
    assert_eq!(made["kind"], "busy");
    assert_eq!(made["quiet"], true);
    assert_eq!((made["days"].as_i64(), made["on_date"].as_str()), (Some(0), Some("2026-09-19")));
    let id = made["id"].as_i64().unwrap();

    let (status, again) = with_bearer(
        &app,
        &token,
        Method::PUT,
        GCAL,
        Some(
            r#"{"title":"Club Meeting (moved)","kind":"busy","start_time":"13:00",
                "end_time":"14:00","on_date":"2026-09-21"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["id"], id, "the second run finds the entry it made");
    assert_eq!(again["title"], "Club Meeting (moved)");
    assert_eq!(again["start_time"], "13:00");
    assert_eq!(again["on_date"], "2026-09-21");
    assert_eq!(again["external_id"], "gcal:c_8f3a:t:busy");

    let (_, all) = with_cookie(&app, &cookie, Method::GET, "/api/calendar", None).await;
    assert_eq!(all["entries"].as_array().unwrap().len(), 1, "no second copy");
}

#[tokio::test]
async fn a_refreshed_entry_keeps_the_dates_the_user_skipped() {
    let (app, cookie, token, _theirs, _cfg) = importer().await;
    let (_, made) = with_bearer(
        &app,
        &token,
        Method::PUT,
        GCAL,
        Some(
            r#"{"title":"standup","kind":"busy","start_time":"09:00","end_time":"09:15",
                "days":31}"#,
        ),
    )
    .await;
    let id = made["id"].as_i64().unwrap();
    let (status, _) = with_cookie(
        &app,
        &cookie,
        Method::POST,
        &format!("/api/calendar/{id}/skip"),
        Some(r#"{"date":"2026-09-21"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, again) = with_bearer(
        &app,
        &token,
        Method::PUT,
        GCAL,
        Some(
            r#"{"title":"standup","kind":"busy","start_time":"09:30","end_time":"09:45",
                "days":31}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["exceptions"], serde_json::json!(["2026-09-21"]));
    assert_eq!(again["start_time"], "09:30");
}

#[tokio::test]
async fn one_external_id_per_account_and_no_further() {
    let (app, cookie, mine, theirs, _cfg) = importer().await;
    let (status, ours) = with_bearer(&app, &mine, Method::PUT, GCAL, Some(MEETING)).await;
    assert_eq!(status, StatusCode::CREATED, "{ours}");

    let (status, alsotheirs) = with_bearer(&app, &theirs, Method::PUT, GCAL, Some(MEETING)).await;
    assert_eq!(status, StatusCode::CREATED, "the same id in another account is another entry");
    assert_ne!(alsotheirs["id"], ours["id"]);

    let (_, mine_list) = with_cookie(&app, &cookie, Method::GET, "/api/calendar", None).await;
    assert_eq!(mine_list["entries"].as_array().unwrap().len(), 1);
    assert_eq!(mine_list["entries"][0]["id"], ours["id"]);
}

#[tokio::test]
async fn delete_by_external_id_is_symmetric() {
    let (app, _cookie, token, theirs, _cfg) = importer().await;
    with_bearer(&app, &token, Method::PUT, GCAL, Some(MEETING)).await;

    let (status, _) = with_bearer(&app, &theirs, Method::DELETE, GCAL, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "another account's id is not there to delete");

    let (status, _) = with_bearer(&app, &token, Method::DELETE, GCAL, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, err) = with_bearer(&app, &token, Method::DELETE, GCAL, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(err["error"].is_string(), "{err}");

    let (status, _) = with_bearer(&app, &token, Method::PUT, GCAL, Some(MEETING)).await;
    assert_eq!(status, StatusCode::CREATED, "a deleted entry is made again");
}

#[tokio::test]
async fn the_session_reaches_the_same_routes() {
    let (app, cookie, _token, _theirs, _cfg) = importer().await;
    let (status, made) = with_cookie(&app, &cookie, Method::PUT, GCAL, Some(MEETING)).await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let (status, _) = with_cookie(&app, &cookie, Method::DELETE, GCAL, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn the_upsert_route_needs_a_credential() {
    let (app, _cookie, _token, _theirs, _cfg) = importer().await;
    for method in [Method::PUT, Method::DELETE] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri(GCAL)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(MEETING))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{method}");
    }
}

#[tokio::test]
async fn a_rejected_upsert_says_why_and_writes_nothing() {
    let (app, cookie, token, _theirs, _cfg) = importer().await;
    for body in [
        r#"{"title":"","kind":"busy","start_time":"11:00","end_time":"12:00","on_date":"2026-09-19"}"#,
        r#"{"title":"x","kind":"party","start_time":"11:00","end_time":"12:00","on_date":"2026-09-19"}"#,
        r#"{"title":"x","kind":"busy","start_time":"12:00","end_time":"11:00","on_date":"2026-09-19"}"#,
        r#"{"title":"x","kind":"busy","start_time":"11:00","end_time":"12:00"}"#,
        r#"{"title":"x","kind":"busy","start_time":"11:00","end_time":"12:00","on_date":"2026-09-19",
            "surprise":1}"#,
        r#"{"title":"x","kind":"busy","start_time":"11:00","end_time":"12:00","on_date":"2026-09-19",
            "external_id":"gcal:other"}"#,
    ] {
        let (status, err) = with_bearer(&app, &token, Method::PUT, GCAL, Some(body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(err["error"].is_string(), "{err}");
    }
    let (status, err) = with_bearer(&app, &token, Method::PUT, GCAL, Some("{oops")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"], "malformed JSON body");

    let (_, all) = with_cookie(&app, &cookie, Method::GET, "/api/calendar", None).await;
    assert!(all["entries"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn an_entry_the_user_made_carries_no_outside_name() {
    let (app, cookie, token, _theirs, _cfg) = importer().await;
    let (status, made) = with_cookie(&app, &cookie, Method::POST, "/api/calendar", Some(MEETING)).await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert!(made["external_id"].is_null());

    let (status, _) = with_bearer(&app, &token, Method::PUT, GCAL, Some(MEETING)).await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, all) = with_cookie(&app, &cookie, Method::GET, "/api/calendar", None).await;
    let names: Vec<Option<&str>> = all["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["external_id"].as_str())
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&None) && names.contains(&Some("gcal:c_8f3a:t:busy")));

    let (_, day) = with_cookie(&app, &cookie, Method::GET, "/api/calendar/day/2026-09-19", None).await;
    assert_eq!(day["occurrences"].as_array().unwrap().len(), 2);
    assert!(day["occurrences"][0].get("external_id").is_none(), "an occurrence is unchanged");
}

#[tokio::test]
async fn a_created_entry_may_name_itself_once() {
    let (app, cookie, _token, _theirs, _cfg) = importer().await;
    let body = r#"{"title":"Club Meeting","kind":"busy","start_time":"11:00",
        "end_time":"12:00","on_date":"2026-09-19","external_id":"gcal:c_8f3a:t:busy"}"#;
    let (status, made) = with_cookie(&app, &cookie, Method::POST, "/api/calendar", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert_eq!(made["external_id"], "gcal:c_8f3a:t:busy");

    let (status, err) = with_cookie(&app, &cookie, Method::POST, "/api/calendar", Some(body)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert!(err["error"].is_string());

    let (status, again) = with_cookie(&app, &cookie, Method::PUT, GCAL, Some(MEETING)).await;
    assert_eq!(status, StatusCode::OK, "the upsert finds the entry the user named");
    assert_eq!(again["id"], made["id"]);
}

#[tokio::test]
async fn the_list_answers_a_token_so_an_importer_can_reconcile() {
    let (app, _cookie, token, theirs, _cfg) = importer().await;
    with_bearer(&app, &token, Method::PUT, GCAL, Some(MEETING)).await;
    let (status, list) = with_bearer(&app, &token, Method::GET, "/api/calendar", None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let ids: Vec<&str> =
        list["entries"].as_array().unwrap().iter().filter_map(|e| e["external_id"].as_str()).collect();
    assert_eq!(ids, ["gcal:c_8f3a:t:busy"]);
    let (status, other) = with_bearer(&app, &theirs, Method::GET, "/api/calendar", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(other["entries"].as_array().unwrap().is_empty(), "another account sees nothing");
}
