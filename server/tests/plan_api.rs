mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::{api, auth, db, AppState};
use tower::ServiceExt;

#[tokio::test]
async fn today_generates_and_returns_events() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let res = app
        .clone()
        .oneshot(
            Request::get("/api/plan/today?date=2026-08-31")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v[0]["kind"], "checkin_call");
    assert_eq!(v[0]["wall_time"], "09:00");

    let res = app
        .oneshot(
            Request::post("/api/events/1/done")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn today_without_date_uses_user_timezone() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let res = app
        .oneshot(
            Request::get("/api/plan/today")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn unparseable_date_is_400() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let res = app
        .oneshot(
            Request::get("/api/plan/today?date=not-a-date")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn shift_moves_the_event_and_drop_marks_it_dropped() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    app.clone()
        .oneshot(
            Request::get("/api/plan/today?date=2026-08-31")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let res = app
        .clone()
        .oneshot(
            Request::post("/api/events/1/shift")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"minutes":45}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .clone()
        .oneshot(
            Request::post("/api/events/1/drop")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .oneshot(
            Request::get("/api/plan/today?date=2026-08-31")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v[0]["wall_time"], "09:45");
    assert_eq!(v[0]["status"], "dropped");
}

#[tokio::test]
async fn shift_outside_the_slide_window_is_400() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    app.clone()
        .oneshot(
            Request::get("/api/plan/today?date=2026-08-31")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let shift = |minutes: i64| {
        let app = app.clone();
        let cookie = cookie.clone();
        async move {
            app.oneshot(
                Request::post("/api/events/1/shift")
                    .header(header::COOKIE, cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(r#"{{"minutes":{minutes}}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        }
    };

    // the template event's window is 60 minutes
    assert_eq!(shift(90).await, StatusCode::BAD_REQUEST);
    assert_eq!(shift(30).await, StatusCode::OK);
}

#[tokio::test]
async fn other_users_event_is_404() {
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", false).unwrap();
    auth::create_user(&conn, "yuki", "pw2", false).unwrap();
    let cfg = common::config_dir();
    let app = api::router(AppState::new(conn, cfg.path().to_path_buf()));
    let owner = common::login(&app, "aki", "pw").await;
    let other = common::login(&app, "yuki", "pw2").await;

    app.clone()
        .oneshot(
            Request::get("/api/plan/today?date=2026-08-31")
                .header(header::COOKIE, &owner)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let res = app
        .clone()
        .oneshot(
            Request::post("/api/events/1/shift")
                .header(header::COOKIE, &other)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"minutes":45}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let res = app
        .oneshot(
            Request::post("/api/events/1/done")
                .header(header::COOKIE, &other)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
