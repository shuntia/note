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
        .oneshot(post("/api/push/subscribe", sub_body("https://203.0.113.9/send/x"), &cookie))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .clone()
        .oneshot(post("/api/push/subscribe", sub_body("ftp://nope"), &cookie))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let long = format!("https://203.0.113.9/{}", "x".repeat(3000));
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
                "endpoint": "https://203.0.113.9/send/y",
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
            serde_json::json!({ "endpoint": "https://203.0.113.9/send/x" }).to_string(),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .oneshot(post(
            "/api/push/unsubscribe",
            serde_json::json!({ "endpoint": "https://203.0.113.9/send/x" }).to_string(),
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
                .body(Body::from(sub_body("https://203.0.113.9/send/x")))
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
                    serde_json::json!({ "endpoint": "https://203.0.113.9/send/x" }).to_string(),
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

/// A push endpoint is fetched by the server, so an endpoint that resolves into
/// the private network would turn deliveries into an SSRF probe.
#[tokio::test]
async fn endpoints_inside_the_private_network_are_refused() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    for endpoint in [
        "http://203.0.113.9/send/x",
        "https://127.0.0.1/send/x",
        "https://[::1]/send/x",
        "https://10.1.2.3/send/x",
        "https://192.168.0.5/send/x",
        "https://172.16.9.9/send/x",
        "https://169.254.169.254/latest/meta-data",
        "https://100.125.222.56/send/x",
        "https://[fd00::1]/send/x",
        "https://[fe80::1]/send/x",
        "https://0.0.0.0/send/x",
        "https://localhost/send/x",
        "https://note.localhost/send/x",
        "https://nothing.invalid/send/x",
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/push/subscribe")
                    .header("content-type", "application/json")
                    .header("cookie", cookie.clone())
                    .body(Body::from(sub_body(endpoint)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY, "accepted {endpoint}");
    }
}

#[tokio::test]
async fn subscriptions_are_capped_and_an_endpoint_belongs_to_one_user() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let subscribe = |endpoint: String, cookie: String| {
        let app = app.clone();
        async move {
            app.oneshot(
                Request::post("/api/push/subscribe")
                    .header("content-type", "application/json")
                    .header("cookie", cookie)
                    .body(Body::from(sub_body(&endpoint)))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        }
    };
    for i in 0..10 {
        let status = subscribe(format!("https://203.0.113.9/send/{i}"), cookie.clone()).await;
        assert_eq!(status, StatusCode::OK, "device {i}");
    }
    let status = subscribe("https://203.0.113.9/send/11".into(), cookie.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // re-subscribing an endpoint already stored is a replacement, not an 11th row
    let status = subscribe("https://203.0.113.9/send/0".into(), cookie.clone()).await;
    assert_eq!(status, StatusCode::OK);

    {
        let conn = state.db.lock().unwrap();
        note_server::auth::create_user(&conn, "bo", "pw", false).unwrap();
    }
    let bo = common::login(&app, "bo", "pw").await;
    let status = subscribe("https://203.0.113.9/send/0".into(), bo).await;
    assert_eq!(status, StatusCode::CONFLICT);
    {
        let conn = state.db.lock().unwrap();
        let subs = note_server::push_subs::list(&conn, 1).unwrap();
        assert_eq!(subs.len(), 10, "aki must keep the endpoint");
    }
}
