mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::{auth, calendar};
use tower::ServiceExt;

async fn call(
    app: &axum::Router,
    cookie: &str,
    method: Method,
    path: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, cookie);
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.unwrap_or("").to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

const SCHOOL: &str = r#"{"title":"school","kind":"fixed","start_time":"08:15",
    "end_time":"15:30","days":31}"#;

async fn school(app: &axum::Router, cookie: &str) -> i64 {
    let (status, row) = call(app, cookie, Method::POST, "/api/calendar", Some(SCHOOL)).await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    row["id"].as_i64().unwrap()
}

#[tokio::test]
async fn an_entry_is_created_listed_patched_and_deleted() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (status, row) = call(&app, &cookie, Method::POST, "/api/calendar", Some(SCHOOL)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(row["title"], "school");
    assert_eq!(row["kind"], "fixed");
    assert_eq!(row["quiet"], true);
    assert_eq!(row["days"], 31);
    assert_eq!(row["day_names"], serde_json::json!(["mon", "tue", "wed", "thu", "fri"]));
    assert_eq!(row["on_date"], serde_json::Value::Null);
    assert_eq!(row["exceptions"], serde_json::json!([]));
    let id = row["id"].as_i64().unwrap();

    let (status, body) = call(&app, &cookie, Method::GET, "/api/calendar", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entries"].as_array().unwrap().len(), 1);
    assert_eq!(body["entries"][0]["id"], id);

    let (status, row) = call(
        &app,
        &cookie,
        Method::PATCH,
        &format!("/api/calendar/{id}"),
        Some(r#"{"end_time":"16:00","quiet":false}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(row["end_time"], "16:00");
    assert_eq!(row["quiet"], false);
    assert_eq!(row["start_time"], "08:15", "an untouched field stays");

    let (status, _) =
        call(&app, &cookie, Method::DELETE, &format!("/api/calendar/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = call(&app, &cookie, Method::GET, "/api/calendar", None).await;
    assert!(body["entries"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_one_off_entry_carries_its_date() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (status, row) = call(
        &app,
        &cookie,
        Method::POST,
        "/api/calendar",
        Some(r#"{"title":"exam","kind":"fixed","start_time":"09:00","end_time":"11:30",
                 "on_date":"2026-09-18"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!((row["days"].as_i64(), row["on_date"].as_str()), (Some(0), Some("2026-09-18")));
}

#[tokio::test]
async fn a_rejected_entry_says_why_and_writes_nothing() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    for body in [
        r#"{"title":"","kind":"fixed","start_time":"08:15","end_time":"15:30","days":31}"#,
        r#"{"title":"x","kind":"party","start_time":"08:15","end_time":"15:30","days":31}"#,
        r#"{"title":"x","kind":"fixed","start_time":"15:30","end_time":"08:15","days":31}"#,
        r#"{"title":"x","kind":"fixed","start_time":"08:15","end_time":"15:30"}"#,
        r#"{"title":"x","kind":"fixed","start_time":"08:15","end_time":"15:30","days":31,
            "surprise":1}"#,
    ] {
        let (status, err) = call(&app, &cookie, Method::POST, "/api/calendar", Some(body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(err["error"].is_string(), "{err}");
    }
    let (status, err) = call(&app, &cookie, Method::POST, "/api/calendar", Some("{oops")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"], "malformed JSON body");

    let (_, body) = call(&app, &cookie, Method::GET, "/api/calendar", None).await;
    assert!(body["entries"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn the_hundred_and_first_entry_is_a_conflict() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    {
        let conn = state.db.lock().unwrap();
        for i in 0..calendar::MAX_ENTRIES {
            calendar::create(
                &conn,
                1,
                calendar::Fields {
                    title: format!("entry {i}"),
                    kind: "busy".into(),
                    start_time: "08:00".into(),
                    end_time: "09:00".into(),
                    days: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();
        }
    }
    let (status, err) = call(&app, &cookie, Method::POST, "/api/calendar", Some(SCHOOL)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(err["error"].as_str().unwrap().contains("100"), "{err}");
}

#[tokio::test]
async fn skipping_a_date_takes_it_off_the_day_and_unskipping_puts_it_back() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let id = school(&app, &cookie).await;

    // 2026-09-18 is a Friday
    let (status, day) = call(&app, &cookie, Method::GET, "/api/calendar/day/2026-09-18", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(day["date"], "2026-09-18");
    assert_eq!(day["occurrences"][0]["entry_id"], id);
    assert_eq!(day["occurrences"][0]["title"], "school");
    assert_eq!(day["occurrences"][0]["start"], "08:15");
    assert_eq!(day["occurrences"][0]["end"], "15:30");
    assert_eq!(day["occurrences"][0]["quiet"], true);
    assert_eq!(day["quiet_now"], serde_json::Value::Null, "not today");

    let (status, _) = call(
        &app,
        &cookie,
        Method::POST,
        &format!("/api/calendar/{id}/skip"),
        Some(r#"{"date":"2026-09-18"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, day) = call(&app, &cookie, Method::GET, "/api/calendar/day/2026-09-18", None).await;
    assert!(day["occurrences"].as_array().unwrap().is_empty());
    let (_, body) = call(&app, &cookie, Method::GET, "/api/calendar", None).await;
    assert_eq!(body["entries"][0]["exceptions"], serde_json::json!(["2026-09-18"]));

    let (status, _) = call(
        &app,
        &cookie,
        Method::DELETE,
        &format!("/api/calendar/{id}/skip/2026-09-18"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, day) = call(&app, &cookie, Method::GET, "/api/calendar/day/2026-09-18", None).await;
    assert_eq!(day["occurrences"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn today_reports_whether_the_day_is_quiet_right_now() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (status, row) = call(
        &app,
        &cookie,
        Method::POST,
        "/api/calendar",
        Some(r#"{"title":"all day","kind":"fixed","start_time":"00:01","end_time":"23:59",
                 "days":127}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");

    let today = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
    let (_, day) =
        call(&app, &cookie, Method::GET, &format!("/api/calendar/day/{today}"), None).await;
    assert_eq!(day["quiet_now"], "23:59");
}

#[tokio::test]
async fn an_unparseable_day_is_a_400() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (status, err) =
        call(&app, &cookie, Method::GET, "/api/calendar/day/not-a-date", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].is_string());
}

#[tokio::test]
async fn another_users_entry_is_a_404_on_every_route() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let id = school(&app, &cookie).await;
    {
        let conn = state.db.lock().unwrap();
        auth::create_user(&conn, "rin", "pw", false).unwrap();
    }
    let theirs = common::login(&app, "rin", "pw").await;

    let (status, _) = call(&app, &theirs, Method::GET, "/api/calendar", None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = call(&app, &theirs, Method::GET, "/api/calendar", None).await;
    assert!(body["entries"].as_array().unwrap().is_empty(), "the calendar is per user");

    let (status, _) = call(
        &app,
        &theirs,
        Method::PATCH,
        &format!("/api/calendar/{id}"),
        Some(r#"{"title":"mine now"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) =
        call(&app, &theirs, Method::DELETE, &format!("/api/calendar/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        &theirs,
        Method::POST,
        &format!("/api/calendar/{id}/skip"),
        Some(r#"{"date":"2026-09-18"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        &theirs,
        Method::DELETE,
        &format!("/api/calendar/{id}/skip/2026-09-18"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (_, day) = call(&app, &cookie, Method::GET, "/api/calendar/day/2026-09-18", None).await;
    assert_eq!(day["occurrences"].as_array().unwrap().len(), 1, "the owner still has it");
}

#[tokio::test]
async fn every_calendar_route_needs_a_session() {
    let (app, _cookie, _cfg) = common::app_with_logged_in_user().await;
    for (method, path) in [
        (Method::GET, "/api/calendar"),
        (Method::POST, "/api/calendar"),
        (Method::PATCH, "/api/calendar/1"),
        (Method::DELETE, "/api/calendar/1"),
        (Method::POST, "/api/calendar/1/skip"),
        (Method::DELETE, "/api/calendar/1/skip/2026-09-18"),
        (Method::GET, "/api/calendar/day/2026-09-18"),
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri(path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{method} {path}");
    }
}

#[tokio::test]
async fn the_day_plan_can_carry_the_calendar_beside_its_events() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    school(&app, &cookie).await;

    let (status, plain) =
        call(&app, &cookie, Method::GET, "/api/plan/today?date=2026-09-18", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(plain.is_array(), "the existing shape is untouched");

    let (status, both) = call(
        &app,
        &cookie,
        Method::GET,
        "/api/plan/today?date=2026-09-18&calendar=1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(both["events"], plain);
    assert_eq!(both["calendar"][0]["title"], "school");
    assert_eq!(both["calendar"][0]["kind"], "fixed");
}
