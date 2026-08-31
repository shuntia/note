mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::{auth, memory, AppState};
use tower::ServiceExt;

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

fn add(state: &AppState, user: &str, category: &str, summary: &str, body: &str) -> String {
    let conn = state.db.lock().unwrap();
    memory::add(&conn, &state.data_dir, user, category, summary, body, None).unwrap()
}

fn ids(v: &serde_json::Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn memory_routes_list_search_and_read() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let tea = add(&state, "aki", "semantic", "likes green tea", "no sugar");
    let dentist = add(&state, "aki", "episodic", "dentist visit", "molar checked");

    let v = json(app.clone().oneshot(get("/api/memory", &cookie)).await.unwrap()).await;
    assert_eq!(ids(&v), vec![dentist.clone(), tea.clone()]);
    assert_eq!(v["items"][0]["category"], "episodic");
    assert_eq!(v["items"][0]["summary"], "dentist visit");

    let v = json(
        app.clone()
            .oneshot(get("/api/memory?category=semantic", &cookie))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ids(&v), vec![tea.clone()]);

    let v = json(
        app.clone()
            .oneshot(get("/api/memory?q=dentist", &cookie))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ids(&v), vec![dentist.clone()]);

    // a search ignores the category filter rather than intersecting with it
    let v = json(
        app.clone()
            .oneshot(get("/api/memory?q=dentist&category=semantic", &cookie))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ids(&v), vec![dentist.clone()]);

    let v = json(
        app.clone()
            .oneshot(get("/api/memory?limit=0", &cookie))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ids(&v), vec![dentist.clone()], "limit must clamp up to 1");

    let res = app
        .clone()
        .oneshot(get("/api/memory?category=secret", &cookie))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(res).await["error"], "unknown category");

    let res = app
        .clone()
        .oneshot(get(&format!("/api/memory/{tea}"), &cookie))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json(res).await;
    assert_eq!(v["id"], tea);
    assert_eq!(v["category"], "semantic");
    assert_eq!(v["summary"], "likes green tea");
    assert_eq!(v["body"], "no sugar");
    assert_eq!(v["supersedes"], serde_json::Value::Null);
    assert_eq!(v["archived"], false);
    assert!(v["created"].as_str().is_some_and(|s| !s.is_empty()));

    for bad in ["nope", "..", "%2e%2e%2fetc"] {
        let res = app
            .clone()
            .oneshot(get(&format!("/api/memory/{bad}"), &cookie))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "accepted id {bad:?}");
    }

    let res = app
        .clone()
        .oneshot(Request::get("/api/memory").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = app
        .oneshot(
            Request::get(format!("/api/memory/{tea}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn memory_routes_are_per_user() {
    let (app, _cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let akis = add(&state, "aki", "semantic", "aki's fact", "private");
    {
        let conn = state.db.lock().unwrap();
        auth::create_user(&conn, "bo", "pw", false).unwrap();
    }
    let bo = common::login(&app, "bo", "pw").await;

    let v = json(app.clone().oneshot(get("/api/memory", &bo)).await.unwrap()).await;
    assert_eq!(ids(&v), Vec::<String>::new());

    let res = app
        .oneshot(get(&format!("/api/memory/{akis}"), &bo))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
