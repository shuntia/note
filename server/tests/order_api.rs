mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
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
        .oneshot(req.body(body.map_or_else(Body::empty, |b| Body::from(b.to_string()))).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

async fn task(app: &axum::Router, cookie: &str, title: &str) -> i64 {
    let (status, t) =
        call(app, cookie, Method::POST, "/api/tasks", Some(&format!(r#"{{"title":"{title}"}}"#))).await;
    assert_eq!(status, StatusCode::OK, "{t}");
    t["id"].as_i64().unwrap()
}

fn today() -> String {
    jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date().to_string()
}

/// The open task items of today's list, first to last.
async fn order(app: &axum::Router, cookie: &str) -> serde_json::Value {
    let (status, day) = call(app, cookie, Method::GET, &format!("/api/day/{}", today()), None).await;
    assert_eq!(status, StatusCode::OK, "{day}");
    day["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "task" && (i["state"] == "open" || i["state"] == "in_progress"))
        .map(|i| i["task"].clone())
        .collect()
}

#[tokio::test]
async fn the_day_list_is_read_and_replaced_whole() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let essay = task(&app, &cookie, "essay").await;
    let laundry = task(&app, &cookie, "laundry").await;
    assert_eq!(order(&app, &cookie).await, json!([]));

    let (_conn_id, mut rx) = state.hub.register(1).unwrap();
    let body = format!(r#"{{"task_ids":[{laundry},{essay}]}}"#);
    let (status, set) = call(&app, &cookie, Method::PUT, &format!("/api/day/{}/order", today()), Some(&body)).await;
    assert_eq!(status, StatusCode::OK, "{set}");
    assert_eq!((set["date"].as_str(), &set["task_ids"]), (Some(today().as_str()), &json!([laundry, essay])));
    let frame: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
    assert_eq!(frame["type"], "changed");
    assert_eq!(order(&app, &cookie).await, json!([laundry, essay]));

    let (status, _) =
        call(&app, &cookie, Method::PATCH, &format!("/api/tasks/{laundry}"), Some(r#"{"state":"done"}"#)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(order(&app, &cookie).await, json!([essay]), "a finished task leaves the open list");
}

#[tokio::test]
async fn a_bad_order_is_refused_and_changes_nothing() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let essay = task(&app, &cookie, "essay").await;
    for body in [
        r#"{"task_ids":[404404]}"#.to_string(),
        format!(r#"{{"task_ids":[{essay},{essay}]}}"#),
        format!(r#"{{"task_ids":[{essay}],"date":"2026-01-01"}}"#),
    ] {
        let (status, _) = call(&app, &cookie, Method::PUT, &format!("/api/day/{}/order", today()), Some(&body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    assert_eq!(order(&app, &cookie).await, json!([]));
}

#[tokio::test]
async fn the_old_order_route_is_gone() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (status, _) = call(&app, &cookie, Method::GET, "/api/order", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn now_starts_the_head_of_the_order() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    task(&app, &cookie, "essay").await;
    let laundry = task(&app, &cookie, "laundry").await;

    let (_, q) = call(&app, &cookie, Method::GET, "/api/tasks/candidates?limit=5", None).await;
    assert_eq!(q[0]["task"]["title"], "essay");
    assert_eq!(q[0]["reason"], "oldest");

    call(&app, &cookie, Method::PUT, &format!("/api/day/{}/order", today()), Some(&format!(r#"{{"task_ids":[{laundry}]}}"#))).await;
    let (status, q) = call(&app, &cookie, Method::GET, "/api/tasks/candidates?limit=5", None).await;
    assert_eq!(status, StatusCode::OK, "{q}");
    let q = q.as_array().unwrap();
    assert_eq!(q.len(), 2);
    assert_eq!(q[0]["task"]["title"], "laundry");
    assert_eq!(q[0]["reason"], "order");
    assert_eq!(q[1]["task"]["title"], "essay");
    assert!(q[0]["task"]["children"].is_array());
}
