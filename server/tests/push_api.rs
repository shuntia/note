mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

fn sub_body(endpoint: &str) -> String {
    serde_json::json!({ "endpoint": endpoint, "keys": { "p256dh": "pk", "auth": "au" } }).to_string()
}

#[tokio::test]
async fn subscribe_unsubscribe_roundtrip_and_validation() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let post = |uri: &str, body: String, cookie: &str| {
        Request::post(uri)
            .header("content-type", "application/json")
            .header("cookie", cookie.to_string())
            .body(Body::from(body))
            .unwrap()
    };
    let res = app
        .clone()
        .oneshot(post("/api/push/subscribe", sub_body("https://push.example/x"), &cookie))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .clone()
        .oneshot(post("/api/push/subscribe", sub_body("ftp://nope"), &cookie))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let long = format!("https://push.example/{}", "x".repeat(3000));
    let res = app
        .clone()
        .oneshot(post("/api/push/subscribe", sub_body(&long), &cookie))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = app
        .clone()
        .oneshot(post(
            "/api/push/subscribe",
            serde_json::json!({
                "endpoint": "https://push.example/y",
                "keys": { "p256dh": "", "auth": "au" }
            })
            .to_string(),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = app
        .clone()
        .oneshot(post(
            "/api/push/unsubscribe",
            serde_json::json!({ "endpoint": long }).to_string(),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = app
        .clone()
        .oneshot(
            Request::get("/api/push/vapid_public_key")
                .header("cookie", cookie.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let res = app
        .clone()
        .oneshot(post(
            "/api/push/unsubscribe",
            serde_json::json!({ "endpoint": "https://push.example/x" }).to_string(),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .oneshot(post(
            "/api/push/unsubscribe",
            serde_json::json!({ "endpoint": "https://push.example/x" }).to_string(),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn push_routes_require_auth() {
    let (app, _cookie, _cfg) = common::app_with_logged_in_user().await;
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/push/subscribe")
                .header("content-type", "application/json")
                .body(Body::from(sub_body("https://push.example/x")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let res = app
        .clone()
        .oneshot(
            Request::post("/api/push/unsubscribe")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "endpoint": "https://push.example/x" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let res = app
        .oneshot(Request::get("/api/push/vapid_public_key").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
