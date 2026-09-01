use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::{api, auth, db, AppState};
use tower::ServiceExt;

async fn login(app: &axum::Router, username: &str, password: &str) -> String {
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"username":"{username}","password":"{password}"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string()
}

async fn app_with_user() -> (axum::Router, String, tempfile::TempDir) {
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", false).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let app = api::router(AppState::new(conn, tmp.path().to_path_buf(), tmp.path().to_path_buf()));
    let cookie = login(&app, "aki", "pw").await;
    (app, cookie, tmp)
}

async fn read(res: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

async fn post(
    app: &axum::Router,
    cookie: &str,
    path: &str,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let res = app
        .clone()
        .oneshot(
            Request::post(path)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    read(res).await
}

async fn patch_task(
    app: &axum::Router,
    cookie: &str,
    id: i64,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let res = app
        .clone()
        .oneshot(
            Request::patch(format!("/api/tasks/{id}"))
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    read(res).await
}

async fn list(app: &axum::Router, cookie: &str) -> serde_json::Value {
    let res = app
        .clone()
        .oneshot(
            Request::get("/api/tasks")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    read(res).await.1
}

#[tokio::test]
async fn task_defaults_carry_no_duration_and_no_parent() {
    let (app, cookie, _tmp) = app_with_user().await;
    let (status, t) = post(&app, &cookie, "/api/tasks", r#"{"title":"call dentist"}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert!(t["duration_min"].is_null());
    assert_eq!(t["duration_source"], "none");
    assert!(t["parent_id"].is_null());
}

#[tokio::test]
async fn duration_is_five_minute_granular_and_user_sourced() {
    let (app, cookie, _tmp) = app_with_user().await;
    let (_, t) =
        post(&app, &cookie, "/api/tasks", r#"{"title":"email landlord","duration_min":20}"#).await;
    assert_eq!(t["duration_min"], 20);
    assert_eq!(t["duration_source"], "user");

    let (status, _) = post(&app, &cookie, "/api/tasks", r#"{"title":"odd","duration_min":7}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = patch_task(&app, &cookie, 1, r#"{"duration_min":23}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = patch_task(&app, &cookie, 1, r#"{"duration_min":0}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, t) = patch_task(&app, &cookie, 1, r#"{"duration_min":null}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert!(t["duration_min"].is_null());
    assert_eq!(t["duration_source"], "none");
}

#[tokio::test]
async fn grandchild_task_is_rejected() {
    let (app, cookie, _tmp) = app_with_user().await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"parent"}"#).await;
    let (status, child) = post(
        &app,
        &cookie,
        "/api/tasks",
        r#"{"title":"step","parent_id":1,"duration_min":5}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(child["parent_id"], 1);

    let (status, _) =
        post(&app, &cookie, "/api/tasks", r#"{"title":"deeper","parent_id":2}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    post(&app, &cookie, "/api/tasks", r#"{"title":"loose"}"#).await;
    let (status, _) = patch_task(&app, &cookie, 3, r#"{"parent_id":2}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = patch_task(&app, &cookie, 1, r#"{"parent_id":3}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = patch_task(&app, &cookie, 3, r#"{"parent_id":3}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = patch_task(&app, &cookie, 3, r#"{"parent_id":999}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn list_nests_children_under_their_parent() {
    let (app, cookie, _tmp) = app_with_user().await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"email landlord"}"#).await;
    post(
        &app,
        &cookie,
        "/api/tasks",
        r#"{"title":"find the thread","parent_id":1,"duration_min":5}"#,
    )
    .await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"refill meds"}"#).await;

    let v = list(&app, &cookie).await;
    assert_eq!(v.as_array().unwrap().len(), 2);
    assert_eq!(v[0]["title"], "email landlord");
    assert_eq!(v[0]["children"][0]["title"], "find the thread");
    assert_eq!(v[0]["children"][0]["duration_min"], 5);
    assert_eq!(v[1]["title"], "refill meds");
    assert_eq!(v[1]["children"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn completing_the_last_step_completes_the_parent() {
    let (app, cookie, _tmp) = app_with_user().await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"email landlord"}"#).await;
    post(
        &app,
        &cookie,
        "/api/tasks",
        r#"{"title":"find the thread","parent_id":1,"duration_min":5}"#,
    )
    .await;
    post(
        &app,
        &cookie,
        "/api/tasks",
        r#"{"title":"write and send","parent_id":1,"duration_min":10}"#,
    )
    .await;

    let (_, t) = patch_task(&app, &cookie, 2, r#"{"state":"done"}"#).await;
    assert_eq!(t["state"], "done");
    assert!(t.get("parent").is_none(), "parent must not change while a step is open");

    let (_, t) = patch_task(&app, &cookie, 3, r#"{"state":"done"}"#).await;
    assert_eq!(t["parent"]["id"], 1);
    assert_eq!(t["parent"]["state"], "done");
    let v = list(&app, &cookie).await;
    assert_eq!(v[0]["state"], "done");

    let (_, t) = patch_task(&app, &cookie, 3, r#"{"state":"open"}"#).await;
    assert_eq!(t["parent"]["state"], "in_progress");

    // the cascade only ever takes `done` back off a parent; a parent already
    // under way stays under way
    patch_task(&app, &cookie, 2, r#"{"state":"open"}"#).await;
    let v = list(&app, &cookie).await;
    assert_eq!(v[0]["state"], "in_progress");
}

#[tokio::test]
async fn completing_a_parent_completes_its_steps_and_dropping_drops_them() {
    let (app, cookie, _tmp) = app_with_user().await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"email landlord"}"#).await;
    post(
        &app,
        &cookie,
        "/api/tasks",
        r#"{"title":"find the thread","parent_id":1,"duration_min":5}"#,
    )
    .await;
    post(
        &app,
        &cookie,
        "/api/tasks",
        r#"{"title":"write and send","parent_id":1,"duration_min":10}"#,
    )
    .await;

    patch_task(&app, &cookie, 1, r#"{"state":"done"}"#).await;
    let v = list(&app, &cookie).await;
    assert_eq!(v[0]["children"][0]["state"], "done");
    assert_eq!(v[0]["children"][1]["state"], "done");

    patch_task(&app, &cookie, 1, r#"{"state":"dropped"}"#).await;
    let v = list(&app, &cookie).await;
    assert_eq!(v.as_array().unwrap().len(), 0);
    let (_, t) = patch_task(&app, &cookie, 2, r#"{"notes":"x"}"#).await;
    assert_eq!(t["state"], "dropped");
}

#[tokio::test]
async fn create_list_update_task() {
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", false).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let app = api::router(AppState::new(conn, tmp.path().to_path_buf(), tmp.path().to_path_buf()));
    let cookie = login(&app, "aki", "pw").await;

    let res = app.clone().oneshot(
        Request::post("/api/tasks")
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"title":"call dentist"}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.clone().oneshot(
        Request::patch("/api/tasks/1")
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"state":"done"}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.oneshot(
        Request::get("/api/tasks").header(header::COOKIE, &cookie)
            .body(Body::empty()).unwrap(),
    ).await.unwrap();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v[0]["state"], "done");
}

#[tokio::test]
async fn invalid_state_is_rejected() {
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", false).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let app = api::router(AppState::new(conn, tmp.path().to_path_buf(), tmp.path().to_path_buf()));
    let cookie = login(&app, "aki", "pw").await;
    app.clone().oneshot(
        Request::post("/api/tasks")
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"title":"x"}"#)).unwrap(),
    ).await.unwrap();
    let res = app.oneshot(
        Request::patch("/api/tasks/1")
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"state":"exploded"}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn patch_by_non_owner_is_404() {
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", false).unwrap();
    auth::create_user(&conn, "yuki", "pw2", false).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let app = api::router(AppState::new(conn, tmp.path().to_path_buf(), tmp.path().to_path_buf()));
    let owner_cookie = login(&app, "aki", "pw").await;
    let other_cookie = login(&app, "yuki", "pw2").await;

    app.clone().oneshot(
        Request::post("/api/tasks")
            .header(header::COOKIE, &owner_cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"title":"call dentist"}"#)).unwrap(),
    ).await.unwrap();

    let owned_res = app.clone().oneshot(
        Request::patch("/api/tasks/1")
            .header(header::COOKIE, &other_cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"state":"done"}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(owned_res.status(), StatusCode::NOT_FOUND);
    let owned_body = owned_res.into_body().collect().await.unwrap().to_bytes();

    let missing_res = app.oneshot(
        Request::patch("/api/tasks/999")
            .header(header::COOKIE, &other_cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"state":"done"}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(missing_res.status(), StatusCode::NOT_FOUND);
    let missing_body = missing_res.into_body().collect().await.unwrap().to_bytes();

    assert_eq!(owned_body, missing_body);
}
