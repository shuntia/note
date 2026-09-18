mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::AppState;
use tower::ServiceExt;

async fn call(
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
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

fn today() -> jiff::civil::Date {
    jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date()
}

fn ahead(days: i64) -> jiff::civil::Date {
    today().checked_add(jiff::Span::new().days(days)).unwrap()
}

async fn free_afternoon(app: &axum::Router, cookie: &str, date: jiff::civil::Date) {
    let (status, row) = call(
        app,
        cookie,
        Method::POST,
        "/api/calendar",
        Some(&format!(
            r#"{{"title":"open afternoon","kind":"free","start_time":"16:00",
                 "end_time":"18:30","on_date":"{date}"}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    assert_eq!(row["quiet"], false, "free time never holds a delivery");
}

#[tokio::test]
async fn the_day_carries_the_plan_the_calendar_and_its_free_time() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let date = ahead(2);
    free_afternoon(&app, &cookie, date).await;
    let (status, day) = call(&app, &cookie, Method::GET, &format!("/api/day/{date}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(day["date"], date.to_string());
    assert_eq!(day["events"][0]["kind"], "checkin_call");
    assert_eq!(day["events"][0]["origin"], "template");
    assert_eq!(day["calendar"][0]["kind"], "free");
    assert_eq!(day["free"], serde_json::json!([{ "start": "16:00", "end": "18:30" }]));
    assert!(day["quiet_now"].is_null(), "quiet only answers for today");
    assert_eq!(day["history"], serde_json::json!([]), "a day ahead has no history");
}

#[tokio::test]
async fn free_time_is_what_the_fixed_commitments_leave() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let date = ahead(2);
    free_afternoon(&app, &cookie, date).await;
    let (status, _) = call(
        &app,
        &cookie,
        Method::POST,
        "/api/calendar",
        Some(&format!(
            r#"{{"title":"orchestra","kind":"fixed","start_time":"17:00",
                 "end_time":"18:00","on_date":"{date}"}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, day) = call(&app, &cookie, Method::GET, &format!("/api/day/{date}"), None).await;
    assert_eq!(
        day["free"],
        serde_json::json!([
            { "start": "16:00", "end": "17:00" },
            { "start": "18:00", "end": "18:30" },
        ])
    );
}

#[tokio::test]
async fn an_unparseable_date_is_400() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (status, _) = call(&app, &cookie, Method::GET, "/api/day/not-a-date", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn today_reports_what_has_already_happened() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let date = today();
    let (_, day) = call(&app, &cookie, Method::GET, &format!("/api/day/{date}"), None).await;
    let event_id = day["events"][0]["id"].as_i64().unwrap();
    assert_eq!(day["history"], serde_json::json!([]));

    let (status, _) =
        call(&app, &cookie, Method::POST, &format!("/api/events/{event_id}/done"), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, task) =
        call(&app, &cookie, Method::POST, "/api/tasks", Some(r#"{"title":"read the chapter"}"#)).await;
    assert_eq!(status, StatusCode::OK);
    let task_id = task["id"].as_i64().unwrap();
    let (status, _) = call(
        &app,
        &cookie,
        Method::PATCH,
        &format!("/api/tasks/{task_id}"),
        Some(r#"{"state":"done"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    answered_checkin(&state, date);

    let (_, day) = call(&app, &cookie, Method::GET, &format!("/api/day/{date}"), None).await;
    let history = day["history"].as_array().unwrap();
    let kinds: Vec<&str> = history.iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"event_done"), "{history:?}");
    assert!(kinds.contains(&"task_done"), "{history:?}");
    assert!(kinds.contains(&"checkin"), "{history:?}");
    let done = history.iter().find(|r| r["kind"] == "event_done").unwrap();
    assert_eq!(done["event_id"], event_id);
    assert_eq!(done["label"], "checkin_call");
    assert!(done["time"].as_str().unwrap().len() == 5, "{done}");
    let checkin = history.iter().find(|r| r["kind"] == "checkin").unwrap();
    assert_eq!(checkin["label"], "Check-in answered");
    assert!(checkin["conversation_id"].is_i64());
}

#[tokio::test]
async fn a_moved_event_names_where_it_went() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let date = today();
    let (_, day) = call(&app, &cookie, Method::GET, &format!("/api/day/{date}"), None).await;
    let event_id = day["events"][0]["id"].as_i64().unwrap();
    let (status, _) = call(
        &app,
        &cookie,
        Method::POST,
        &format!("/api/events/{event_id}/move_tomorrow"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, day) = call(&app, &cookie, Method::GET, &format!("/api/day/{date}"), None).await;
    let moved = day["history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "event_moved")
        .expect("the move is history");
    assert!(
        moved["label"].as_str().unwrap().contains(&ahead(1).to_string()),
        "{moved}"
    );
}

#[tokio::test]
async fn a_range_returns_only_the_days_that_have_a_plan() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let from = today();
    let to = ahead(3);
    call(&app, &cookie, Method::GET, &format!("/api/day/{to}"), None).await;
    let (status, body) = call(
        &app,
        &cookie,
        Method::GET,
        &format!("/api/plan/range?from={from}&to={to}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let days = body["days"].as_object().unwrap();
    assert_eq!(days.len(), 1, "reading a week never invents a plan");
    assert_eq!(days[&to.to_string()][0]["kind"], "checkin_call");
}

#[tokio::test]
async fn a_range_is_bounded_and_ordered() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let from = today();
    for (path, why) in [
        (format!("/api/plan/range?from=nope&to={}", ahead(1)), "unparseable"),
        (format!("/api/plan/range?from={from}&to={}", ahead(-1)), "backwards"),
        (format!("/api/plan/range?from={from}&to={}", ahead(14)), "too wide"),
    ] {
        let (status, _) = call(&app, &cookie, Method::GET, &path, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}");
    }
}

#[tokio::test]
async fn allocate_fills_the_free_time_and_a_second_run_keeps_what_is_settled() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let date = ahead(2);
    free_afternoon(&app, &cookie, date).await;
    for title in ["read the chapter", "email the office"] {
        let (status, _) =
            call(&app, &cookie, Method::POST, "/api/tasks", Some(&format!(r#"{{"title":"{title}"}}"#)))
                .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (status, out) =
        call(&app, &cookie, Method::POST, &format!("/api/plan/{date}/allocate"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["plan_date"], date.to_string());
    assert_eq!(out["cleared"], 0);
    let placed = out["placed"].as_array().unwrap();
    assert_eq!(placed.len(), 2);
    assert_eq!(placed[0]["start"], "16:00");
    let kept = placed[0]["event_id"].as_i64().unwrap();

    let (_, day) = call(&app, &cookie, Method::GET, &format!("/api/day/{date}"), None).await;
    let block = day["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == kept)
        .expect("the block is on the day");
    assert_eq!(block["origin"], "auto");
    assert_eq!(block["entry"], "block");
    assert_eq!(block["task"]["title"], "read the chapter");
    assert_eq!(block["task"]["state"], "open");

    let (status, _) = call(&app, &cookie, Method::POST, &format!("/api/events/{kept}/done"), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, again) =
        call(&app, &cookie, Method::POST, &format!("/api/plan/{date}/allocate"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["cleared"], 1, "only the pending block is replaced");
    let (_, day) = call(&app, &cookie, Method::GET, &format!("/api/day/{date}"), None).await;
    assert!(
        day["events"].as_array().unwrap().iter().any(|e| e["id"] == kept),
        "a finished block is never cleared"
    );
}

#[tokio::test]
async fn a_day_that_is_over_cannot_be_filled() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (status, _) =
        call(&app, &cookie, Method::POST, &format!("/api/plan/{}/allocate", ahead(-1)), None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) =
        call(&app, &cookie, Method::POST, "/api/plan/not-a-date/allocate", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

fn answered_checkin(state: &AppState, date: jiff::civil::Date) {
    let conn = state.db();
    let now = jiff::Timestamp::now();
    let id = note_server::talk::checkin_thread(&conn, 1, &date.to_string(), "How is it going?", now)
        .unwrap();
    note_server::talk::append_text(&conn, id, "user", "fine", now).unwrap();
}
