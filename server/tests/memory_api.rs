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
    let sourced = add(&state, "aki", "semantic", "mira lives next door", "since september");
    let own = add(&state, "aki", "semantic", "likes green tea", "no sugar");
    {
        let conn = state.db();
        conn.execute(
            "INSERT INTO memory_sources (user_id, source_id, memory_id) VALUES (1, ?1, ?2)",
            ("inbox:canvas-18", &sourced),
        )
        .unwrap();
    }

    let v = json(
        app.clone()
            .oneshot(get(&format!("/api/memory/{sourced}"), &cookie))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(v["sources"], serde_json::json!(["inbox:canvas-18"]));

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

fn write(method: &str, path: &str, cookie: &str, body: Option<&str>) -> Request<Body> {
    let req = Request::builder().method(method).uri(path).header(header::COOKIE, cookie);
    match body {
        Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap()
}

async fn with_embeddings() -> (axum::Router, String, AppState, tempfile::TempDir) {
    use note_server::providers::mock::{MockEmbeddings, MockLLM};
    let cfg = common::config_dir();
    let dir = cfg.path().to_path_buf();
    let conn = note_server::db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", true).unwrap();
    let state = AppState::new(conn, dir.clone(), dir).with_providers(
        std::sync::Arc::new(MockLLM::scripted(Vec::new())),
        Some(std::sync::Arc::new(MockEmbeddings)),
    );
    let app = note_server::api::router(state.clone());
    let cookie = common::login(&app, "aki", "pw").await;
    (app, cookie, state, cfg)
}

fn vector_of(state: &AppState, id: &str) -> Option<Vec<f32>> {
    use rusqlite::OptionalExtension;
    let blob: Option<Vec<u8>> = state
        .db()
        .query_row("SELECT vector FROM memory_vectors WHERE user = 'aki' AND id = ?1", [id], |r| r.get(0))
        .optional()
        .unwrap();
    blob.map(|b| b.as_chunks::<4>().0.iter().copied().map(f32::from_le_bytes).collect())
}

fn embedded(summary: &str, body: &str) -> Vec<f32> {
    use note_server::providers::EmbeddingsProvider;
    note_server::providers::mock::MockEmbeddings
        .embed(&[&format!("{summary}\n{body}")])
        .unwrap()
        .remove(0)
}

#[tokio::test]
async fn an_edit_rewrites_the_fact_and_its_search_and_vector_rows() {
    let (app, cookie, state, _cfg) = with_embeddings().await;
    let id = {
        let conn = state.db();
        let v = embedded("likes green tea", "no sugar");
        memory::add(&conn, &state.data_dir, "aki", "semantic", "likes green tea", "no sugar", Some(&v)).unwrap()
    };
    let path = format!("/api/memory/{id}");

    let res = app
        .clone()
        .oneshot(write("PATCH", &path, &cookie, Some(r#"{"summary":"likes oolong","body":"a little honey"}"#)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json(res).await;
    assert_eq!(v["summary"], "likes oolong");
    assert_eq!(v["body"], "a little honey");
    assert_eq!(vector_of(&state, &id), Some(embedded("likes oolong", "a little honey")));

    let hits = |q: &str| {
        let conn = state.db();
        memory::query(&conn, "aki", q, 10, None).unwrap().into_iter().map(|h| h.id).collect::<Vec<_>>()
    };
    assert_eq!(hits("honey"), vec![id.clone()]);
    assert!(hits("sugar").is_empty(), "the old words leave the search index");

    let res = app
        .clone()
        .oneshot(write("PATCH", &path, &cookie, Some(r#"{"body":"no honey after all"}"#)))
        .await
        .unwrap();
    let v = json(res).await;
    assert_eq!(v["summary"], "likes oolong", "a field left out is kept");
    assert_eq!(v["body"], "no honey after all");
    let listed = json(app.oneshot(get("/api/memory", &cookie)).await.unwrap()).await;
    assert_eq!(listed["items"][0]["summary"], "likes oolong");
}

#[tokio::test]
async fn an_edit_refuses_a_bad_summary_an_archived_fact_and_a_working_note() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let old = add(&state, "aki", "semantic", "s", "b");
    let live = {
        let conn = state.db();
        memory::supersede(&conn, &state.data_dir, "aki", &old, "s2", "b2", None).unwrap().unwrap()
    };
    let send = |id: &str, body: &str| {
        app.clone().oneshot(write("PATCH", &format!("/api/memory/{id}"), &cookie, Some(body)))
    };

    assert_eq!(send(&live, r#"{"summary":"  "}"#).await.unwrap().status(), StatusCode::BAD_REQUEST);
    let long = format!(r#"{{"summary":"{}"}}"#, "x".repeat(201));
    assert_eq!(send(&live, &long).await.unwrap().status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        send(&live, r#"{"category":"episodic"}"#).await.unwrap().status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(send(&old, r#"{"summary":"rewrite history"}"#).await.unwrap().status(), StatusCode::CONFLICT);
    assert_eq!(send("not-an-id", r#"{"summary":"x"}"#).await.unwrap().status(), StatusCode::NOT_FOUND);

    let note = "00000000-0000-4000-8000-0000000000a1";
    {
        let conn = state.db();
        let f = memory::MemoryFile {
            id: note.into(),
            category: memory::NOTE.into(),
            summary: "call the bank".into(),
            body: String::new(),
            supersedes: None,
            until: None,
            created: "2026-10-07T00:00:00Z".into(),
            archived: false,
            source: None,
            from: None,
            touched_at: None,
            last_nudged_at: None,
        };
        memory::put(&conn, &state.data_dir, "aki", &f).unwrap();
    }
    assert_eq!(send(note, r#"{"summary":"x"}"#).await.unwrap().status(), StatusCode::NOT_FOUND);
    let res = app.clone().oneshot(write("DELETE", &format!("/api/memory/{note}"), &cookie, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert!(memory::read(&state.data_dir, "aki", note).unwrap().is_some());
}

#[tokio::test]
async fn forgetting_a_fact_takes_its_history_and_provenance_with_it() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let old = add(&state, "aki", "semantic", "mira lives next door", "since september");
    let live = {
        let conn = state.db();
        let live = memory::supersede(&conn, &state.data_dir, "aki", &old, "mira moved", "to osaka", None)
            .unwrap()
            .unwrap();
        conn.execute(
            "INSERT INTO memory_sources (user_id, source_id, memory_id) VALUES (1, 'harvest:2026-09-18', ?1)",
            [&live],
        )
        .unwrap();
        live
    };
    let kept = add(&state, "aki", "semantic", "likes green tea", "no sugar");
    let path = format!("/api/memory/{live}");

    let res = app.clone().oneshot(write("DELETE", &path, &cookie, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(app.clone().oneshot(get(&path, &cookie)).await.unwrap().status(), StatusCode::NOT_FOUND);
    let res = app.clone().oneshot(get(&format!("/api/memory/{old}"), &cookie)).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND, "the version it replaced goes too");
    let v = json(app.clone().oneshot(get("/api/memory", &cookie)).await.unwrap()).await;
    assert_eq!(ids(&v), vec![kept]);
    let sources: i64 = state.db().query_row("SELECT COUNT(*) FROM memory_sources", [], |r| r.get(0)).unwrap();
    assert_eq!(sources, 0);

    let res = app.oneshot(write("DELETE", &path, &cookie, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn another_user_can_neither_edit_nor_forget_a_fact() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let akis = add(&state, "aki", "semantic", "aki's fact", "private");
    {
        let conn = state.db.lock().unwrap();
        auth::create_user(&conn, "bo", "pw", false).unwrap();
    }
    let bo = common::login(&app, "bo", "pw").await;
    let path = format!("/api/memory/{akis}");

    let res = app.clone().oneshot(write("PATCH", &path, &bo, Some(r#"{"summary":"bo was here"}"#))).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = app.clone().oneshot(write("DELETE", &path, &bo, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    for req in [
        write("PATCH", &path, &cookie, Some(r#"{"summary":"forged"}"#)),
        write("DELETE", &path, &cookie, None),
    ] {
        let (mut parts, body) = req.into_parts();
        parts.headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        parts.headers.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        let res = app.clone().oneshot(Request::from_parts(parts, body)).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }
    let v = json(app.oneshot(get(&path, &cookie)).await.unwrap()).await;
    assert_eq!(v["summary"], "aki's fact");
}
