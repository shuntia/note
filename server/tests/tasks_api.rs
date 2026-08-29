use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::{api, auth, db, AppState};
use tower::ServiceExt;

async fn login(app: &axum::Router) -> String {
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"username":"aki","password":"pw"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string()
}

#[tokio::test]
async fn create_list_update_task() {
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", false).unwrap();
    let app = api::router(AppState::new(conn));
    let cookie = login(&app).await;

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
    let app = api::router(AppState::new(conn));
    let cookie = login(&app).await;
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
