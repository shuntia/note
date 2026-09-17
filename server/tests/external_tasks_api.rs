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

/// An importer's environment: the user's own bearer token, and a second
/// account with a token of its own to prove the two never meet.
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

const CANVAS: &str = "/api/tasks/by-external/canvas:assignment:12345";

#[tokio::test]
async fn an_upsert_creates_once_and_updates_after() {
    let (app, _cookie, token, _theirs, _cfg) = importer().await;
    let (status, made) = with_bearer(
        &app,
        &token,
        Method::PUT,
        CANVAS,
        Some(
            r#"{"title":"Biology ch.4","due_at":"2026-09-19T23:59:00+09:00",
                "url":"https://canvas.example/a/12","notes":"worksheet attached"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert_eq!(made["external_id"], "canvas:assignment:12345");
    assert_eq!(made["source"], "import");
    assert_eq!(made["due_at"], "2026-09-19T14:59:00Z");
    assert_eq!(made["url"], "https://canvas.example/a/12");
    assert_eq!(made["children"].as_array().unwrap().len(), 0);
    let id = made["id"].as_i64().unwrap();

    let (status, again) = with_bearer(
        &app,
        &token,
        Method::PUT,
        CANVAS,
        Some(r#"{"title":"Biology ch.4 (revised)","due_at":"2026-09-21T23:59:00+09:00"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["id"], id, "the second run finds the task it made");
    assert_eq!(again["title"], "Biology ch.4 (revised)");
    assert_eq!(again["due_at"], "2026-09-21T14:59:00Z");

    let (_, all) = with_bearer(&app, &token, Method::GET, "/api/tasks", None).await;
    assert_eq!(all.as_array().unwrap().len(), 1, "no second copy");
}

#[tokio::test]
async fn an_upsert_leaves_the_agents_work_and_the_users_decision_alone() {
    let (app, cookie, token, _theirs, _cfg) = importer().await;
    let (_, made) =
        with_bearer(&app, &token, Method::PUT, CANVAS, Some(r#"{"title":"Biology ch.4"}"#)).await;
    let id = made["id"].as_i64().unwrap();
    with_cookie(
        &app,
        &cookie,
        Method::PATCH,
        &format!("/api/tasks/{id}"),
        Some(r#"{"description":"Read chapter 4.","duration_min":45}"#),
    )
    .await;
    with_cookie(
        &app,
        &cookie,
        Method::POST,
        &format!("/api/tasks/{id}/split"),
        Some(r#"{"steps":[{"title":"read","duration_min":25},{"title":"answer","duration_min":20}]}"#),
    )
    .await;

    let (status, again) = with_bearer(
        &app,
        &token,
        Method::PUT,
        CANVAS,
        Some(r#"{"title":"Biology ch.4","description":"the LMS blurb","duration_min":5}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["description"], "Read chapter 4.", "the brief survives the next run");
    assert_eq!(again["duration_min"], 45);
    assert_eq!(again["children"].as_array().unwrap().len(), 2);

    // dropped wins: the fields refresh, the user's decision stands
    with_cookie(&app, &cookie, Method::PATCH, &format!("/api/tasks/{id}"), Some(r#"{"state":"dropped"}"#)).await;
    let (status, again) = with_bearer(
        &app,
        &token,
        Method::PUT,
        CANVAS,
        Some(r#"{"title":"Biology ch.4 again","state":"open"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["state"], "dropped");
    assert_eq!(again["title"], "Biology ch.4 again");
}

#[tokio::test]
async fn done_travels_in_one_direction_only() {
    let (app, _cookie, token, _theirs, _cfg) = importer().await;
    with_bearer(&app, &token, Method::PUT, CANVAS, Some(r#"{"title":"Biology ch.4"}"#)).await;
    let (_, done) =
        with_bearer(&app, &token, Method::PUT, CANVAS, Some(r#"{"title":"Biology ch.4","state":"done"}"#))
            .await;
    assert_eq!(done["state"], "done", "the LMS says it was handed in");

    let (_, back) =
        with_bearer(&app, &token, Method::PUT, CANVAS, Some(r#"{"title":"Biology ch.4","state":"open"}"#))
            .await;
    assert_eq!(back["state"], "done", "an import never reopens a finished task");
}

#[tokio::test]
async fn deleting_an_imported_task_buries_its_external_id() {
    let (app, cookie, token, _theirs, _cfg) = importer().await;
    let (_, made) =
        with_bearer(&app, &token, Method::PUT, CANVAS, Some(r#"{"title":"Biology ch.4"}"#)).await;
    let id = made["id"].as_i64().unwrap();

    let (status, _) =
        with_cookie(&app, &cookie, Method::DELETE, &format!("/api/tasks/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, declined) =
        with_bearer(&app, &token, Method::PUT, CANVAS, Some(r#"{"title":"Biology ch.4"}"#)).await;
    assert_eq!(status, StatusCode::GONE, "{declined}");
    assert_eq!(declined["external_id"], "canvas:assignment:12345");
    assert!(declined["deleted_at"].is_string());

    let (_, all) = with_bearer(&app, &token, Method::GET, "/api/tasks", None).await;
    assert_eq!(all.as_array().unwrap().len(), 0, "the task stays deleted");
}

#[tokio::test]
async fn delete_by_external_id_is_symmetric() {
    let (app, _cookie, token, _theirs, _cfg) = importer().await;
    with_bearer(&app, &token, Method::PUT, CANVAS, Some(r#"{"title":"Biology ch.4"}"#)).await;

    let (status, _) = with_bearer(&app, &token, Method::DELETE, CANVAS, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = with_bearer(&app, &token, Method::DELETE, CANVAS, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) =
        with_bearer(&app, &token, Method::PUT, CANVAS, Some(r#"{"title":"Biology ch.4"}"#)).await;
    assert_eq!(status, StatusCode::GONE);
}

#[tokio::test]
async fn deleting_a_typed_task_leaves_no_tombstone() {
    let (app, cookie, token, _theirs, _cfg) = importer().await;
    let (_, made) =
        with_cookie(&app, &cookie, Method::POST, "/api/tasks", Some(r#"{"title":"call dentist"}"#)).await;
    let id = made["id"].as_i64().unwrap();
    let (status, _) =
        with_cookie(&app, &cookie, Method::DELETE, &format!("/api/tasks/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) =
        with_bearer(&app, &token, Method::PUT, CANVAS, Some(r#"{"title":"Biology ch.4"}"#)).await;
    assert_eq!(status, StatusCode::CREATED, "an unrelated delete buries nothing");
}

#[tokio::test]
async fn one_external_id_per_account_and_no_further() {
    let (app, _cookie, mine, theirs, _cfg) = importer().await;
    let (status, ours) =
        with_bearer(&app, &mine, Method::PUT, CANVAS, Some(r#"{"title":"mine"}"#)).await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, alsotheirs) =
        with_bearer(&app, &theirs, Method::PUT, CANVAS, Some(r#"{"title":"theirs"}"#)).await;
    assert_eq!(status, StatusCode::CREATED, "the same id in another account is another task");
    assert_ne!(alsotheirs["id"], ours["id"]);

    let (status, _) = with_bearer(&app, &theirs, Method::DELETE, "/api/tasks/by-external/nothing", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (_, mine_list) = with_bearer(&app, &mine, Method::GET, "/api/tasks", None).await;
    assert_eq!(mine_list.as_array().unwrap().len(), 1);
    assert_eq!(mine_list[0]["title"], "mine");
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
                    .uri(CANVAS)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"title":"x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{method}");
    }
}

#[tokio::test]
async fn an_external_id_the_body_disagrees_with_is_refused() {
    let (app, _cookie, token, _theirs, _cfg) = importer().await;
    let (status, _) = with_bearer(
        &app,
        &token,
        Method::PUT,
        CANVAS,
        Some(r#"{"title":"x","external_id":"canvas:assignment:999"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
