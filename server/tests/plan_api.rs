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
    assert!(v[0].get("moved_to").is_none(), "a user's own drop went nowhere in particular");
    assert_eq!(v[0]["entry"], "routine");
    assert_eq!(v[0]["end_wall_time"], "10:00");
    assert_eq!(v[0]["alert"], true);
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
async fn snooze_pushes_the_event_and_rejects_out_of_range_minutes() {
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

    let snooze = |minutes: i64| {
        let app = app.clone();
        let cookie = cookie.clone();
        async move {
            app.oneshot(
                Request::post("/api/events/1/snooze")
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

    assert_eq!(snooze(0).await, StatusCode::BAD_REQUEST);
    // snooze is not window-bound: 90 exceeds the event's 60 minute slide window
    assert_eq!(snooze(90).await, StatusCode::OK);

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
    assert_eq!(v[0]["wall_time"], "10:30");
    assert_eq!(v[0]["status"], "snoozed");
}

#[tokio::test]
async fn snooze_on_a_decided_event_is_conflict() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
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
    {
        let conn = state.db.lock().unwrap();
        conn.execute("UPDATE events SET status='done' WHERE id=?1", [1]).unwrap();
    }

    let res = app
        .oneshot(
            Request::post("/api/events/1/snooze")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"minutes":15}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn debrief_route_reads_the_stored_row() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;

    let res = app
        .clone()
        .oneshot(
            Request::get("/api/debrief")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    {
        let conn = state.db.lock().unwrap();
        conn.execute(
            "INSERT INTO debriefs (user_id, date, content, created_at)
             VALUES (1, '2026-08-31', 'a calm day ahead', 't')",
            [],
        )
        .unwrap();
    }
    let res = app
        .clone()
        .oneshot(
            Request::get("/api/debrief?date=2026-08-31")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["date"], "2026-08-31");
    assert_eq!(v["content"], "a calm day ahead");

    let res = app
        .clone()
        .oneshot(
            Request::get("/api/debrief?date=notadate")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // another user's debrief is indistinguishable from a missing one
    {
        let conn = state.db.lock().unwrap();
        let other = auth::create_user(&conn, "yuki", "pw2", false).unwrap();
        conn.execute(
            "INSERT INTO debriefs (user_id, date, content, created_at)
             VALUES (?1, '2026-09-01', 'not yours', 't')",
            [other],
        )
        .unwrap();
    }
    let res = app
        .clone()
        .oneshot(
            Request::get("/api/debrief?date=2026-09-01")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let res = app
        .oneshot(Request::get("/api/debrief").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn other_users_event_is_404() {
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", false).unwrap();
    auth::create_user(&conn, "yuki", "pw2", false).unwrap();
    let cfg = common::config_dir();
    let app = api::router(AppState::new(conn, cfg.path().to_path_buf(), cfg.path().to_path_buf()));
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
        .clone()
        .oneshot(
            Request::post("/api/events/1/snooze")
                .header(header::COOKIE, &other)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"minutes":15}"#))
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

#[tokio::test]
async fn every_event_reports_a_span() {
    let (app, cookie, cfg) = common::app_with_logged_in_user().await;
    let p = cfg.path().join("users/aki/templates/default.toml");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        "[[events]]\nkind='checkin'\ntime='09:00'\ndays=['mon','tue','wed','thu','fri','sat','sun']\n\
         [[events]]\nkind='walk'\ntime='18:00'\nend_time='18:45'\ndays=['mon','tue','wed','thu','fri','sat','sun']\n",
    )
    .unwrap();
    let res = app
        .oneshot(
            Request::get("/api/plan/today?date=2026-09-01")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v[0]["entry"], "routine");
    assert_eq!(v[0]["end_wall_time"], "09:15");
    assert_eq!(v[1]["end_wall_time"], "18:45");
}

#[tokio::test]
async fn an_event_can_be_silenced_for_the_day() {
    async fn today(app: &axum::Router, cookie: &str) -> serde_json::Value {
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/plan/today?date=2026-08-31")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let v = today(&app, &cookie).await;
    assert_eq!(v[0]["alert"], true);

    let res = app
        .clone()
        .oneshot(
            Request::post("/api/events/1/alert")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"alert":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = today(&app, &cookie).await;
    assert_eq!(v[0]["alert"], false);

    let res = app
        .oneshot(
            Request::post("/api/events/999/alert")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"alert":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_event_moves_to_tomorrow() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let fetch = |date: &'static str, cookie: String| {
        let app = app.clone();
        async move {
            let res = app
                .oneshot(
                    Request::get(format!("/api/plan/today?date={date}"))
                        .header(header::COOKIE, cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = res.into_body().collect().await.unwrap().to_bytes();
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()
        }
    };
    let today = fetch("2026-08-31", cookie.clone()).await;
    let id = today[0]["id"].as_i64().unwrap();
    let res = app
        .clone()
        .oneshot(
            Request::post(format!("/api/events/{id}/move_tomorrow"))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let moved: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(moved["date"], "2026-09-01");

    let today = fetch("2026-08-31", cookie.clone()).await;
    assert_eq!(today[0]["status"], "dropped");
    assert_eq!(today[0]["moved_to"]["date"], "2026-09-01");
    let tomorrow = fetch("2026-09-01", cookie.clone()).await;
    let copy = tomorrow
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == moved["event_id"])
        .expect("the copy lives in tomorrow's plan");
    assert_eq!(copy["wall_time"], today[0]["wall_time"]);
    assert_eq!(copy["status"], "pending");
}
