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

const LANDLORD_STEPS: &str = r#"{"steps":[
    {"title":"find the last email thread","duration_min":5},
    {"title":"photos of the ceiling","duration_min":5},
    {"title":"write and send","duration_min":10}]}"#;

#[tokio::test]
async fn split_creates_steps_and_totals_the_parent_duration() {
    let (app, cookie, _tmp) = app_with_user().await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"email landlord"}"#).await;
    let (status, node) = post(&app, &cookie, "/api/tasks/1/split", LANDLORD_STEPS).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(node["duration_min"], 20);
    assert_eq!(node["duration_source"], "user");
    assert_eq!(node["children"].as_array().unwrap().len(), 3);
    assert_eq!(node["children"][2]["duration_min"], 10);

    let two = r#"{"steps":[{"title":"a","duration_min":5},{"title":"b","duration_min":5}]}"#;
    let (status, _) = post(&app, &cookie, "/api/tasks/1/split", two).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "a task with steps cannot be re-split");
    let (status, _) = post(&app, &cookie, "/api/tasks/2/split", two).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "a step cannot be split");

    post(&app, &cookie, "/api/tasks", r#"{"title":"other"}"#).await;
    let (status, _) = post(
        &app,
        &cookie,
        "/api/tasks/5/split",
        r#"{"steps":[{"title":"only","duration_min":5}]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = post(
        &app,
        &cookie,
        "/api/tasks/5/split",
        r#"{"steps":[{"title":"a","duration_min":5},{"title":"b","duration_min":7}]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = post(&app, &cookie, "/api/tasks/999/split", two).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn flatten_removes_every_step_in_one_call_and_hands_them_back() {
    let (app, cookie, _tmp) = app_with_user().await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"email landlord"}"#).await;
    post(&app, &cookie, "/api/tasks/1/split", LANDLORD_STEPS).await;

    let (status, out) = post(&app, &cookie, "/api/tasks/1/flatten", "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["task"]["id"], 1);
    assert_eq!(out["task"]["duration_min"], 20);
    assert_eq!(out["task"]["children"].as_array().unwrap().len(), 0);
    assert_eq!(out["removed"].as_array().unwrap().len(), 3);
    assert_eq!(out["removed"][0]["title"], "find the last email thread");
    assert_eq!(list(&app, &cookie).await.as_array().unwrap().len(), 1);

    // undo is the inverse call
    let (status, node) = post(&app, &cookie, "/api/tasks/1/split", LANDLORD_STEPS).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(node["children"].as_array().unwrap().len(), 3);

    // flattening a task with no steps is a no-op, not an error
    let (_, loose) = post(&app, &cookie, "/api/tasks", r#"{"title":"loose"}"#).await;
    let path = format!("/api/tasks/{}/flatten", loose["id"].as_i64().unwrap());
    let (status, out) = post(&app, &cookie, &path, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["removed"].as_array().unwrap().len(), 0);

    let (status, _) = post(&app, &cookie, "/api/tasks/999/flatten", "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn is_now_round_trips_through_create_patch_and_list() {
    let (app, cookie, _tmp) = app_with_user().await;
    let (status, t) = post(&app, &cookie, "/api/tasks", r#"{"title":"call dentist"}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(t["is_now"], false, "a new task lands in Later");

    let (status, t) = patch_task(&app, &cookie, 1, r#"{"is_now":true}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(t["is_now"], true);
    assert_eq!(list(&app, &cookie).await[0]["is_now"], true, "the flag survives a reload");

    let (_, t) = patch_task(&app, &cookie, 1, r#"{"is_now":false}"#).await;
    assert_eq!(t["is_now"], false);

    // an unrelated patch leaves the flag alone
    patch_task(&app, &cookie, 1, r#"{"is_now":true}"#).await;
    let (_, t) = patch_task(&app, &cookie, 1, r#"{"notes":"ring at 9"}"#).await;
    assert_eq!(t["is_now"], true);

    let (_, t) =
        post(&app, &cookie, "/api/tasks", r#"{"title":"refill meds","is_now":true}"#).await;
    assert_eq!(t["is_now"], true);
}

#[tokio::test]
async fn tasks_carry_the_time_they_last_changed() {
    let (app, cookie, _tmp) = app_with_user().await;
    let (_, t) = post(&app, &cookie, "/api/tasks", r#"{"title":"call dentist"}"#).await;
    let created = t["updated_at"].as_str().unwrap().to_string();
    assert!(created.parse::<jiff::Timestamp>().is_ok(), "not a timestamp: {created}");

    let (_, t) = patch_task(&app, &cookie, 1, r#"{"state":"done"}"#).await;
    let done = t["updated_at"].as_str().unwrap();
    assert!(done >= created.as_str(), "finishing a task did not move its timestamp");
    assert_eq!(list(&app, &cookie).await[0]["updated_at"], done);
}

#[tokio::test]
async fn a_fourth_task_is_refused_entry_to_now() {
    let (app, cookie, _tmp) = app_with_user().await;
    for title in ["a", "b", "c", "d"] {
        post(&app, &cookie, "/api/tasks", &format!(r#"{{"title":"{title}"}}"#)).await;
    }
    for id in 1..=3 {
        let (status, _) = patch_task(&app, &cookie, id, r#"{"is_now":true}"#).await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, body) = patch_task(&app, &cookie, 4, r#"{"is_now":true}"#).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("Now"));
    let v = list(&app, &cookie).await;
    assert_eq!(v[3]["is_now"], false, "the refused task did not move");
    assert_eq!(v[0]["is_now"], true, "and nothing already in Now was displaced");

    let (status, _) = post(&app, &cookie, "/api/tasks", r#"{"title":"e","is_now":true}"#).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // re-asserting the flag on a task already in Now is not a fourth
    let (status, _) = patch_task(&app, &cookie, 1, r#"{"is_now":true}"#).await;
    assert_eq!(status, StatusCode::OK);

    // finishing one frees its slot without clearing its flag
    patch_task(&app, &cookie, 1, r#"{"state":"done"}"#).await;
    let (status, t) = patch_task(&app, &cookie, 4, r#"{"is_now":true}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(t["is_now"], true);
    let v = list(&app, &cookie).await;
    assert_eq!(v[0]["is_now"], true, "undo must find the done task still in Now");
    assert_eq!(v[0]["state"], "done");
}

#[tokio::test]
async fn reopening_a_done_now_task_demotes_the_newest_instead_of_failing() {
    let (app, cookie, _tmp) = app_with_user().await;
    for title in ["a", "b", "c", "d"] {
        post(&app, &cookie, "/api/tasks", &format!(r#"{{"title":"{title}","is_now":true}}"#)).await;
        // the fourth would be refused, so free a slot first
        if title == "c" {
            patch_task(&app, &cookie, 1, r#"{"state":"done"}"#).await;
        }
    }
    let (status, t) = patch_task(&app, &cookie, 1, r#"{"state":"open"}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(t["is_now"], true, "undo restores the task to its old group");
    assert_eq!(t["demoted_from_now"][0], 4, "the newest fell back to Later");
    let v = list(&app, &cookie).await;
    let live_now = v
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["is_now"] == true && t["state"] != "done")
        .count();
    assert_eq!(live_now, 3);
    assert_eq!(v[3]["is_now"], false);
}

#[tokio::test]
async fn a_step_can_never_be_in_now() {
    let (app, cookie, _tmp) = app_with_user().await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"email landlord"}"#).await;
    let (status, step) =
        post(&app, &cookie, "/api/tasks", r#"{"title":"find the thread","parent_id":1}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(step["is_now"], false);

    let (status, _) = patch_task(&app, &cookie, 2, r#"{"is_now":true}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = post(
        &app,
        &cookie,
        "/api/tasks",
        r#"{"title":"another step","parent_id":1,"is_now":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // a task in Now that becomes a step leaves Now with the move
    post(&app, &cookie, "/api/tasks", r#"{"title":"loose","is_now":true}"#).await;
    let (status, t) = patch_task(&app, &cookie, 3, r#"{"parent_id":1}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(t["is_now"], false);
    assert_eq!(t["parent_id"], 1);

    let v = list(&app, &cookie).await;
    assert_eq!(v[0]["children"][0]["is_now"], false);
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
