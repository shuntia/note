mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;

async fn json(res: axum::response::Response) -> serde_json::Value {
    let body = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn get(path: &str, cookie: &str) -> Request<Body> {
    Request::get(path)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

fn put(path: &str, cookie: &str, body: String) -> Request<Body> {
    Request::put(path)
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn about_routes_get_put_and_clear() {
    use tower::ServiceExt;
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;

    let v = json(app.clone().oneshot(get("/api/about", &cookie)).await.unwrap()).await;
    assert_eq!(v["content"], "");

    let res = app
        .clone()
        .oneshot(put("/api/about", &cookie, r#"{"content":"  I have ADHD. \n"}"#.to_string()))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(json(res).await["content"], "I have ADHD.");
    let v = json(app.clone().oneshot(get("/api/about", &cookie)).await.unwrap()).await;
    assert_eq!(v["content"], "I have ADHD.");

    let huge = serde_json::json!({ "content": "x".repeat(9_000) }).to_string();
    let res = app.clone().oneshot(put("/api/about", &cookie, huge)).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(res).await["error"], "content must be at most 8192 bytes");
    let v = json(app.clone().oneshot(get("/api/about", &cookie)).await.unwrap()).await;
    assert_eq!(v["content"], "I have ADHD.", "a rejected write keeps what was there");

    let res = app
        .clone()
        .oneshot(put("/api/about", &cookie, r#"{"content":"ok","name":"persona"}"#.to_string()))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let res = app
        .clone()
        .oneshot(put("/api/about", &cookie, r#"{"content":"   "}"#.to_string()))
        .await
        .unwrap();
    assert_eq!(json(res).await["content"], "");

    let res = app
        .clone()
        .oneshot(get("/api/prompts/persona", &cookie))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let res = app
        .clone()
        .oneshot(Request::get("/api/about").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = app
        .oneshot(
            Request::put("/api/about")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"content":"sneaky"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn about_is_per_user() {
    use tower::ServiceExt;
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    {
        let conn = state.db.lock().unwrap();
        note_server::auth::create_user(&conn, "bo", "pw", false).unwrap();
    }
    let bo = common::login(&app, "bo", "pw").await;

    let res = app
        .clone()
        .oneshot(put("/api/about", &cookie, r#"{"content":"aki only"}"#.to_string()))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let v = json(app.oneshot(get("/api/about", &bo)).await.unwrap()).await;
    assert_eq!(v["content"], "");
}
