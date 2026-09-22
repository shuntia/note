mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
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
    let res = req.body(Body::from(body.unwrap_or_default().to_string())).unwrap();
    let res = app.clone().oneshot(res).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

async fn goals(app: &axum::Router, cookie: &str) -> serde_json::Value {
    call(app, cookie, Method::GET, "/api/goals", None).await.1
}

#[tokio::test]
async fn a_goal_is_made_listed_patched_and_deleted() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    assert_eq!(goals(&app, &cookie).await.as_array().unwrap().len(), 0);

    let (status, made) = call(
        &app,
        &cookie,
        Method::POST,
        "/api/goals",
        Some(r#"{"title":"get into the programme","due_at":"2026-11-01T12:00:00Z"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let id = made["id"].as_i64().unwrap();
    assert_eq!(made["state"], "open");
    assert_eq!((made["tasks"].as_i64(), made["done_tasks"].as_i64()), (Some(0), Some(0)));

    let (status, patched) = call(
        &app,
        &cookie,
        Method::PATCH,
        &format!("/api/goals/{id}"),
        Some(r#"{"title":"get in early","due_at":null}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["title"], "get in early");
    assert!(patched["due_at"].is_null());

    let (status, _) =
        call(&app, &cookie, Method::DELETE, &format!("/api/goals/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(goals(&app, &cookie).await.as_array().unwrap().len(), 0);
    let (status, _) =
        call(&app, &cookie, Method::DELETE, &format!("/api/goals/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_goal_row_counts_its_tasks_and_names_the_next_one_due() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (_, goal) =
        call(&app, &cookie, Method::POST, "/api/goals", Some(r#"{"title":"apply"}"#)).await;
    let id = goal["id"].as_i64().unwrap();
    let task = async |title: &str, due: &str| {
        let body = format!(r#"{{"title":"{title}","goal_id":{id},"due_at":"{due}"}}"#);
        call(&app, &cookie, Method::POST, "/api/tasks", Some(&body)).await.1
    };
    let essay = task("essay", "2026-10-25T12:00:00Z").await;
    task("form", "2026-10-02T12:00:00Z").await;
    call(
        &app,
        &cookie,
        Method::PATCH,
        &format!("/api/tasks/{}", essay["id"].as_i64().unwrap()),
        Some(r#"{"state":"done"}"#),
    )
    .await;

    let row = &goals(&app, &cookie).await[0];
    assert_eq!(row["tasks"], 2);
    assert_eq!(row["done_tasks"], 1);
    assert_eq!(row["next_task_title"], "form");
    assert_eq!(row["next_due_at"], "2026-10-02T12:00:00Z");
}

#[tokio::test]
async fn a_deleted_goal_leaves_its_tasks_behind() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (_, goal) =
        call(&app, &cookie, Method::POST, "/api/goals", Some(r#"{"title":"apply"}"#)).await;
    let id = goal["id"].as_i64().unwrap();
    call(
        &app,
        &cookie,
        Method::POST,
        "/api/tasks",
        Some(&format!(r#"{{"title":"essay","goal_id":{id}}}"#)),
    )
    .await;
    call(&app, &cookie, Method::DELETE, &format!("/api/goals/{id}"), None).await;

    let tasks = call(&app, &cookie, Method::GET, "/api/tasks", None).await.1;
    assert_eq!(tasks[0]["title"], "essay");
    assert!(tasks[0]["goal_id"].is_null());
    assert!(tasks[0]["goal_title"].is_null());
}

#[tokio::test]
async fn a_blank_title_and_an_unknown_goal_are_refused() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (status, _) =
        call(&app, &cookie, Method::POST, "/api/goals", Some(r#"{"title":"   "}"#)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(
        &app,
        &cookie,
        Method::PATCH,
        "/api/goals/999",
        Some(r#"{"title":"nowhere"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
