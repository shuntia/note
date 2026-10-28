mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or_default())
}

fn get(cookie: &str) -> Request<Body> {
    Request::get("/api/voice/mics").header(header::COOKIE, cookie).body(Body::empty()).unwrap()
}

fn put(cookie: &str, client: &str) -> axum::http::request::Builder {
    Request::put(format!("/api/voice/mics/{client}"))
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json")
}

#[tokio::test]
async fn each_user_sees_and_sets_only_their_own_clients() {
    let (app, aki, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let (aki_id, bo_id) = {
        let conn = state.db.lock().unwrap();
        note_server::auth::create_user(&conn, "bo", "pw", false).unwrap();
        let id = |name: &str| conn.query_row("SELECT id FROM users WHERE username = ?1", [name], |r| r.get::<_, i64>(0)).unwrap();
        (id("aki"), id("bo"))
    };
    let bo = common::login(&app, "bo", "pw").await;
    {
        let conn = state.db.lock().unwrap();
        let now = jiff::Timestamp::now();
        note_server::voice::mics::seen(&conn, aki_id, "web:aa11", "Laptop", now).unwrap();
        note_server::voice::mics::seen(&conn, bo_id, "desktop:bb22", "Headset", now).unwrap();
    }

    let (status, v) = send(&app, get(&aki)).await;
    assert_eq!(status, StatusCode::OK);
    let mics = v["mics"].as_array().unwrap();
    assert_eq!(mics.len(), 2, "{v}");
    assert_eq!(mics[0], serde_json::json!({ "client": "element", "kind": "element", "label": "", "gain_db": 0.0, "auto": false }));
    assert_eq!((mics[1]["client"].as_str(), mics[1]["auto"].as_bool()), (Some("web:aa11"), Some(true)));

    let body = r#"{"gain_db":12}"#;
    let (status, v) = send(&app, put(&aki, "web:aa11").body(Body::from(body)).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((v["gain_db"].as_f64(), v["auto"].as_bool()), (Some(12.0), Some(false)), "a slider set by hand turns auto off");

    let (status, _) = send(&app, put(&aki, "desktop:bb22").body(Body::from(body)).unwrap()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "bo's device is not aki's to set");
    let (_, v) = send(&app, get(&bo)).await;
    assert_eq!(v["mics"][1]["gain_db"].as_f64(), Some(0.0));
    assert!(v["mics"].as_array().unwrap().iter().all(|m| m["client"] != "web:aa11"), "{v}");

    let too_much = r#"{"gain_db":30}"#;
    let (status, _) = send(&app, put(&aki, "element").body(Body::from(too_much)).unwrap()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let auto = r#"{"auto":true}"#;
    let (status, v) = send(&app, put(&aki, "element").body(Body::from(auto)).unwrap()).await;
    assert_eq!((status, v["auto"].as_bool()), (StatusCode::OK, Some(true)));
}

#[tokio::test]
async fn a_cross_site_write_and_a_stranger_are_refused() {
    let (app, aki, _cfg) = common::app_with_logged_in_user().await;
    let body = r#"{"gain_db":6}"#;
    let req = put(&aki, "element").header(header::ORIGIN, "https://evil.example").body(Body::from(body)).unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::FORBIDDEN);
    let anon = Request::get("/api/voice/mics").body(Body::empty()).unwrap();
    assert_eq!(send(&app, anon).await.0, StatusCode::UNAUTHORIZED);
}
