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
    let (app, cookie, _state, tmp) = app_with_user_and_state().await;
    (app, cookie, tmp)
}

async fn app_with_user_and_state() -> (axum::Router, String, AppState, tempfile::TempDir) {
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", false).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let state = AppState::new(conn, tmp.path().to_path_buf(), tmp.path().to_path_buf());
    let app = api::router(state.clone());
    let cookie = login(&app, "aki", "pw").await;
    (app, cookie, state, tmp)
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
    assert_ne!(t["parent"]["state"], "done", "a parent stays open while a step is open");
    assert_eq!(t["parent"]["progress"], 33, "5 of 15 minutes done");

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

#[tokio::test]
async fn delete_removes_task_and_steps_and_404s_after() {
    let (app, cookie, _tmp) = app_with_user().await;
    let (_, parent) = post(&app, &cookie, "/api/tasks", r#"{"title":"parent"}"#).await;
    let pid = parent["id"].as_i64().unwrap();
    post(&app, &cookie, "/api/tasks", &format!(r#"{{"title":"step","parent_id":{pid}}}"#)).await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"other"}"#).await;

    let res = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/tasks/{pid}"))
                .header(header::COOKIE, cookie.as_str())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let all = list(&app, &cookie).await;
    assert_eq!(all.as_array().unwrap().len(), 1);
    assert_eq!(all[0]["title"], "other");

    let res = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/tasks/{pid}"))
                .header(header::COOKIE, cookie.as_str())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_task_carries_its_due_date_and_its_origin() {
    let (app, cookie, _tmp) = app_with_user().await;
    let (status, t) = post(&app, &cookie, "/api/tasks", r#"{"title":"call dentist"}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert!(t["due_at"].is_null());
    assert!(t["external_id"].is_null());
    assert_eq!(t["url"], "");
    assert_eq!(t["source"], "manual");

    let (status, t) = post(
        &app,
        &cookie,
        "/api/tasks",
        r#"{"title":"essay","due_at":"2026-09-19T23:59:00+09:00",
            "url":"https://canvas.example/a/12","description":"two pages","notes":"from the LMS"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{t}");
    assert_eq!(t["due_at"], "2026-09-19T14:59:00Z", "stored as RFC 3339 UTC");
    assert_eq!(t["url"], "https://canvas.example/a/12");
    assert_eq!(t["description"], "two pages");
    assert_eq!(t["notes"], "from the LMS");

    let v = list(&app, &cookie).await;
    assert_eq!(v[1]["due_at"], "2026-09-19T14:59:00Z");
    assert_eq!(v[1]["url"], "https://canvas.example/a/12");
}

#[tokio::test]
async fn due_at_is_set_cleared_and_validated() {
    let (app, cookie, _tmp) = app_with_user().await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"essay"}"#).await;

    let (status, t) = patch_task(&app, &cookie, 1, r#"{"due_at":"2026-09-19T23:59:00Z"}"#).await;
    assert_eq!(status, StatusCode::OK, "{t}");
    assert_eq!(t["due_at"], "2026-09-19T23:59:00Z");

    let (status, t) = patch_task(&app, &cookie, 1, r#"{"notes":"x"}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(t["due_at"], "2026-09-19T23:59:00Z", "an unrelated patch leaves it alone");

    let (status, t) = patch_task(&app, &cookie, 1, r#"{"due_at":null}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert!(t["due_at"].is_null());

    for bad in ["\"friday\"", "\"2026-09-19\"", "\"2026-13-01T00:00:00Z\"", "5"] {
        let (status, _) = patch_task(&app, &cookie, 1, &format!(r#"{{"due_at":{bad}}}"#)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "accepted {bad}");
        let (status, _) =
            post(&app, &cookie, "/api/tasks", &format!(r#"{{"title":"x","due_at":{bad}}}"#)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "accepted {bad} on create");
    }
}

#[tokio::test]
async fn a_step_never_carries_a_due_date_of_its_own() {
    let (app, cookie, _tmp) = app_with_user().await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"essay"}"#).await;
    let (status, _) = post(
        &app,
        &cookie,
        "/api/tasks",
        r#"{"title":"draft","parent_id":1,"due_at":"2026-09-19T23:59:00Z"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    post(&app, &cookie, "/api/tasks", r#"{"title":"draft","parent_id":1}"#).await;
    let (status, _) = patch_task(&app, &cookie, 2, r#"{"due_at":"2026-09-19T23:59:00Z"}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    post(&app, &cookie, "/api/tasks", r#"{"title":"loose","due_at":"2026-09-19T23:59:00Z"}"#).await;
    let (status, _) = patch_task(&app, &cookie, 3, r#"{"parent_id":1}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "a task with a due date cannot become a step");
}

#[tokio::test]
async fn an_unknown_field_on_create_is_refused() {
    let (app, cookie, _tmp) = app_with_user().await;
    let (status, _) = post(&app, &cookie, "/api/tasks", r#"{"title":"x","due":"friday"}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_task_says_how_its_block_announces_itself() {
    let (app, cookie, _tmp) = app_with_user().await;
    let (status, made) = post(&app, &cookie, "/api/tasks", r#"{"title":"the chapter"}"#).await;
    assert_eq!(status, StatusCode::OK, "{made}");
    assert_eq!(made["notify"], "notify", "a block speaks up unless it is told not to");
    let id = made["id"].as_i64().unwrap();

    let (status, quiet) = patch_task(&app, &cookie, id, r#"{"notify":"none"}"#).await;
    assert_eq!(status, StatusCode::OK, "{quiet}");
    assert_eq!(quiet["notify"], "none");
    assert_eq!(list(&app, &cookie).await[0]["notify"], "none");

    let (status, _) = patch_task(&app, &cookie, id, r#"{"notify":"shout"}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, made) =
        post(&app, &cookie, "/api/tasks", r#"{"title":"quiet one","notify":"chat"}"#).await;
    assert_eq!(status, StatusCode::OK, "{made}");
    assert_eq!(made["notify"], "chat");
}

#[tokio::test]
async fn a_task_carries_its_category_and_a_step_reads_the_one_above_it() {
    let (app, cookie, _tmp) = app_with_user().await;
    let (status, made) =
        post(&app, &cookie, "/api/tasks", r#"{"title":"chapter 4","category":"Biology"}"#).await;
    assert_eq!(status, StatusCode::OK, "{made}");
    assert_eq!(made["category"], "Biology");
    let id = made["id"].as_i64().unwrap();

    let (status, step) = post(
        &app,
        &cookie,
        "/api/tasks",
        &format!(r#"{{"title":"read it","parent_id":{id}}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{step}");
    assert_eq!(step["category"], "Biology");

    let (status, refused) =
        patch_task(&app, &cookie, step["id"].as_i64().unwrap(), r#"{"category":"History"}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");

    let (_, cleared) = patch_task(&app, &cookie, id, r#"{"category":""}"#).await;
    assert_eq!(cleared["category"], "");
    assert_eq!(list(&app, &cookie).await[0]["children"][0]["category"], "");
}

#[tokio::test]
async fn a_task_names_the_goal_it_belongs_to() {
    let (app, cookie, _tmp) = app_with_user().await;
    let (status, goal) =
        post(&app, &cookie, "/api/goals", r#"{"title":"get into the programme"}"#).await;
    assert_eq!(status, StatusCode::CREATED, "{goal}");
    let goal_id = goal["id"].as_i64().unwrap();

    let (status, made) = post(
        &app,
        &cookie,
        "/api/tasks",
        &format!(r#"{{"title":"the essay","goal_id":{goal_id}}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{made}");
    assert_eq!(made["goal_id"], goal_id);
    assert_eq!(made["goal_title"], "get into the programme");

    let row = list(&app, &cookie).await;
    assert_eq!(row[0]["goal_title"], "get into the programme");

    let (status, refused) = patch_task(
        &app,
        &cookie,
        made["id"].as_i64().unwrap(),
        r#"{"goal_id":9999}"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
}

#[tokio::test]
async fn a_task_row_says_when_its_next_block_starts() {
    let (app, cookie, state, _tmp) = app_with_user_and_state().await;
    let (_, made) = post(&app, &cookie, "/api/tasks", r#"{"title":"the essay"}"#).await;
    let id = made["id"].as_i64().unwrap();
    assert!(list(&app, &cookie).await[0]["scheduled_at"].is_null());

    let today = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
    {
        let conn = state.db();
        conn.execute(
            "INSERT INTO plans (user_id, date, created_at) VALUES (1, ?1, 'x')",
            [today.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, alert, end_wall_time)
             VALUES (1, 'the essay', '18:15', 0, '19:00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO event_tasks (event_id, task_id) VALUES (?1, ?2)",
            (conn.last_insert_rowid(), id),
        )
        .unwrap();
    }
    assert_eq!(
        list(&app, &cookie).await[0]["scheduled_at"],
        serde_json::json!(format!("{today}T18:15:00+00:00"))
    );
}

#[tokio::test]
async fn urgency_is_created_patched_and_validated() {
    let (app, cookie, _tmp) = app_with_user().await;
    let (status, t) = post(&app, &cookie, "/api/tasks", r#"{"title":"exam","urgency":"high"}"#).await;
    assert_eq!(status, StatusCode::OK, "{t}");
    assert_eq!(t["urgency"], "high");
    assert_eq!(t["pressing"], false);
    let id = t["id"].as_i64().unwrap();
    let (status, t) = patch_task(&app, &cookie, id, r#"{"urgency":"low"}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(t["urgency"], "low");
    let (status, e) = patch_task(&app, &cookie, id, r#"{"urgency":"asap"}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{e}");
}
