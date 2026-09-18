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

    // a blank q browses rather than searching for the empty string
    let v = json(
        app.clone()
            .oneshot(get("/api/memory?q=%20", &cookie))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ids(&v), vec![dentist.clone(), tea.clone()]);

    // every parameter blank is the same request as no parameters at all
    let res = app
        .clone()
        .oneshot(get("/api/memory?category=&q=&limit=", &cookie))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(ids(&json(res).await), vec![dentist.clone(), tea.clone()]);

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
        .oneshot(get("/api/memory?limit=ten", &cookie))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(res).await["error"], "limit must be a number");

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
async fn a_fact_names_what_wrote_it_on_the_users_behalf() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let harvested = add(&state, "aki", "semantic", "mira lives next door", "since september");
    let own = add(&state, "aki", "semantic", "likes green tea", "no sugar");
    {
        let conn = state.db();
        conn.execute(
            "INSERT INTO memory_sources (user_id, source_id, memory_id) VALUES (1, ?1, ?2)",
            ("harvest:2026-09-18", &harvested),
        )
        .unwrap();
    }

    let v = json(
        app.clone()
            .oneshot(get(&format!("/api/memory/{harvested}"), &cookie))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(v["sources"], serde_json::json!(["harvest:2026-09-18"]));

    let v = json(app.oneshot(get(&format!("/api/memory/{own}"), &cookie)).await.unwrap()).await;
    assert_eq!(v["sources"], serde_json::json!([]));
}

#[tokio::test]
async fn memory_list_caps_at_two_hundred() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    for i in 0..205 {
        add(&state, "aki", "semantic", &format!("fact {i}"), "body");
    }
    let v = json(
        app.clone()
            .oneshot(get("/api/memory?limit=999", &cookie))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ids(&v).len(), 200);

    let v = json(app.oneshot(get("/api/memory", &cookie)).await.unwrap()).await;
    assert_eq!(ids(&v).len(), 100, "the default limit still applies");
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
