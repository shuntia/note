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

fn delete(path: &str, cookie: &str) -> Request<Body> {
    Request::delete(path)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn prompts_routes_get_put_delete() {
    use tower::ServiceExt;
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;

    let v = json(app.clone().oneshot(get("/api/prompts/persona", &cookie)).await.unwrap()).await;
    assert_eq!(v["name"], "persona");
    assert_eq!(v["content"], "you are note");
    assert_eq!(v["custom"], false);

    let v = json(app.clone().oneshot(get("/api/prompts/planning", &cookie)).await.unwrap()).await;
    assert_eq!(v["content"], "plan the day");

    let res = app
        .clone()
        .oneshot(put(
            "/api/prompts/persona",
            &cookie,
            r#"{"content":"you are testy"}"#.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json(res).await;
    assert_eq!(v["name"], "persona");
    assert_eq!(v["content"], "you are testy");
    assert_eq!(v["custom"], true);

    let v = json(app.clone().oneshot(get("/api/prompts/persona", &cookie)).await.unwrap()).await;
    assert_eq!(v["content"], "you are testy");
    assert_eq!(v["custom"], true);
    // the other prompt is untouched by an override of this one
    let v = json(app.clone().oneshot(get("/api/prompts/planning", &cookie)).await.unwrap()).await;
    assert_eq!(v["custom"], false);

    for bad in [r#"{"content":"   "}"#, r#"{"content":""}"#] {
        let res = app
            .clone()
            .oneshot(put("/api/prompts/persona", &cookie, bad.to_string()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "accepted {bad}");
        assert_eq!(
            json(res).await["error"],
            "content must be non-blank and at most 32768 bytes"
        );
    }

    let huge = serde_json::json!({ "content": "x".repeat(40_000) }).to_string();
    let res = app
        .clone()
        .oneshot(put("/api/prompts/persona", &cookie, huge))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // a rejected write leaves the stored override in place
    let v = json(app.clone().oneshot(get("/api/prompts/persona", &cookie)).await.unwrap()).await;
    assert_eq!(v["content"], "you are testy");

    let res = app
        .clone()
        .oneshot(put(
            "/api/prompts/persona",
            &cookie,
            r#"{"content":"ok","name":"persona"}"#.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let res = app
        .clone()
        .oneshot(delete("/api/prompts/persona", &cookie))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json(res).await;
    assert_eq!(v["content"], "you are note");
    assert_eq!(v["custom"], false);

    let res = app
        .clone()
        .oneshot(delete("/api/prompts/persona", &cookie))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "resetting twice is not an error");

    for bad in ["evil", "..", "%2e%2e%2fetc%2fpasswd"] {
        let path = format!("/api/prompts/{bad}");
        let res = app.clone().oneshot(get(&path, &cookie)).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "GET accepted {bad:?}");
        let res = app
            .clone()
            .oneshot(put(&path, &cookie, r#"{"content":"x"}"#.to_string()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "PUT accepted {bad:?}");
        let res = app.clone().oneshot(delete(&path, &cookie)).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "DELETE accepted {bad:?}");
    }

    let res = app
        .clone()
        .oneshot(Request::get("/api/prompts/persona").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = app
        .clone()
        .oneshot(
            Request::put("/api/prompts/persona")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"content":"sneaky"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = app
        .oneshot(Request::delete("/api/prompts/persona").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn prompt_overrides_are_per_user() {
    use tower::ServiceExt;
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    {
        let conn = state.db.lock().unwrap();
        note_server::auth::create_user(&conn, "bo", "pw", false).unwrap();
    }
    let bo = common::login(&app, "bo", "pw").await;

    let res = app
        .clone()
        .oneshot(put(
            "/api/prompts/persona",
            &cookie,
            r#"{"content":"aki only"}"#.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let v = json(app.oneshot(get("/api/prompts/persona", &bo)).await.unwrap()).await;
    assert_eq!(v["content"], "you are note");
    assert_eq!(v["custom"], false);
}
