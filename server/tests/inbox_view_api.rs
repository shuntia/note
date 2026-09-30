mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::{auth, inbox, AppState};
use tower::ServiceExt;

async fn call(app: &axum::Router, cookie: Option<&str>, method: Method, path: &str) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(c) = cookie {
        req = req.header("cookie", c);
    }
    let res = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

fn at(second: i64) -> jiff::Timestamp {
    jiff::Timestamp::from_second(1_790_000_000 + second).unwrap()
}

fn seed(state: &AppState, user_id: i64, source: &str, second: i64) -> i64 {
    let conn = state.db.lock().unwrap();
    inbox::upsert(&conn, user_id, source, "announcement", &format!("{source} title\nbody of {source}"), at(second)).unwrap()
}

fn titles(v: &serde_json::Value) -> Vec<String> {
    v["items"].as_array().unwrap().iter().map(|i| i["title"].as_str().unwrap().to_string()).collect()
}

#[tokio::test]
async fn the_list_is_newest_first_and_pages_with_before() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    for (i, s) in ["a", "b", "c"].iter().enumerate() {
        seed(&state, 1, s, i64::try_from(i).unwrap());
    }
    let (status, v) = call(&app, Some(&cookie), Method::GET, "/api/inbox").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(titles(&v), ["c title", "b title", "a title"]);
    assert_eq!(v["latest"], inbox::stamp(at(2)));
    let first = &v["items"][0];
    assert_eq!(first["kind"], "announcement");
    assert_eq!(first["outcome"], serde_json::Value::Null);
    assert!(first.get("body").is_none(), "the list carries no bodies");

    let (_, page) = call(&app, Some(&cookie), Method::GET, "/api/inbox?limit=2").await;
    assert_eq!(titles(&page), ["c title", "b title"]);
    let cursor = page["items"][1]["received_at"].as_str().unwrap();
    let path = format!("/api/inbox?limit=2&before={}", cursor.replace(':', "%3A"));
    let (status, rest) = call(&app, Some(&cookie), Method::GET, &path).await;
    assert_eq!(status, StatusCode::OK, "{rest}");
    assert_eq!(titles(&rest), ["a title"]);
    assert_eq!(rest["latest"], inbox::stamp(at(2)), "latest ignores the cursor");
}

#[tokio::test]
async fn an_empty_inbox_has_no_latest() {
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let (status, v) = call(&app, Some(&cookie), Method::GET, "/api/inbox").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["items"], serde_json::json!([]));
    assert_eq!(v["latest"], serde_json::Value::Null);
}

#[tokio::test]
async fn bad_query_values_are_400_and_limit_is_clamped() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    for i in 0..3 {
        seed(&state, 1, &format!("s{i}"), i);
    }
    for path in ["/api/inbox?limit=many", "/api/inbox?before=yesterday", "/api/inbox?limit=-1"] {
        let (status, v) = call(&app, Some(&cookie), Method::GET, path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert!(!v["error"].as_str().unwrap_or("").is_empty(), "{path}: {v}");
    }
    let (_, v) = call(&app, Some(&cookie), Method::GET, "/api/inbox?limit=0").await;
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    let (_, v) = call(&app, Some(&cookie), Method::GET, "/api/inbox?limit=&before=").await;
    assert_eq!(v["items"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn an_item_reads_back_with_its_reason_and_memories() {
    let (app, cookie, state, cfg) = common::app_with_logged_in_user_and_state().await;
    let id = seed(&state, 1, "lms:post:77", 0);
    let mem = {
        let conn = state.db.lock().unwrap();
        let mem = note_server::memory::add_until(&conn, cfg.path(), "aki", &note_server::memory::Fact { category: "semantic", summary: "Biology quiz", body: "Biology: quiz Friday.", until: None }, None)
        .unwrap();
        conn.execute(
            "INSERT INTO memory_sources (user_id, source_id, memory_id) VALUES (1, 'lms:post:77', ?1)",
            [&mem],
        )
        .unwrap();
        inbox::record_decision(&conn, 1, "lms:post:77", "remembered", "a dated quiz", at(1)).unwrap();
        mem
    };
    let (status, v) = call(&app, Some(&cookie), Method::GET, &format!("/api/inbox/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["title"], "lms:post:77 title");
    assert_eq!(v["body"], "lms:post:77 title\nbody of lms:post:77");
    assert_eq!(v["outcome"], "remembered");
    assert_eq!(v["reason"], "a dated quiz");
    assert_eq!(v["decided_at"], inbox::stamp(at(1)));
    assert_eq!(v["memories"], serde_json::json!([{"id": mem, "summary": "Biology quiz", "archived": false}]));
}

#[tokio::test]
async fn another_users_item_is_not_found_and_not_listed() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let bo = {
        let conn = state.db.lock().unwrap();
        auth::create_user(&conn, "bo", "pw", false).unwrap()
    };
    let theirs = seed(&state, bo, "theirs", 0);
    let (_, v) = call(&app, Some(&cookie), Method::GET, "/api/inbox").await;
    assert_eq!(v["items"], serde_json::json!([]));
    for path in [format!("/api/inbox/{theirs}"), "/api/inbox/abc".into(), "/api/inbox/999".into()] {
        let (status, _) = call(&app, Some(&cookie), Method::GET, &path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn the_routes_need_a_session() {
    let (app, _cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let id = seed(&state, 1, "s1", 0);
    for path in ["/api/inbox".to_string(), format!("/api/inbox/{id}")] {
        let (status, _) = call(&app, None, Method::GET, &path).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
    }
}

async fn app_with_signal(state: &AppState, signal: Option<std::path::PathBuf>) -> (axum::Router, String) {
    let app = note_server::api::router(state.clone().with_inbox_refresh(signal));
    let cookie = common::login(&app, "aki", "pw").await;
    (app, cookie)
}

#[tokio::test]
async fn refresh_writes_the_time_and_answers_202() {
    let (_app, _cookie, state, cfg) = common::app_with_logged_in_user_and_state().await;
    let signal = cfg.path().join("run/inbox-refresh");
    std::fs::create_dir_all(signal.parent().unwrap()).unwrap();
    let (app, cookie) = app_with_signal(&state, Some(signal.clone())).await;

    let (_, v) = call(&app, Some(&cookie), Method::GET, "/api/inbox").await;
    assert_eq!(v["refresh"], true);
    let (status, v) = call(&app, Some(&cookie), Method::POST, "/api/inbox/refresh").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    let requested = v["requested_at"].as_str().unwrap().to_string();
    assert_eq!(std::fs::read_to_string(&signal).unwrap().trim(), requested);

    // an item that arrives after the press reads as newer than it
    {
        let conn = state.db.lock().unwrap();
        inbox::upsert(&conn, 1, "s1", "announcement", "x", jiff::Timestamp::now()).unwrap();
    }
    let (_, v) = call(&app, Some(&cookie), Method::GET, "/api/inbox").await;
    assert!(v["latest"].as_str().unwrap() > requested.as_str());
}

#[tokio::test]
async fn refresh_without_a_signal_is_409_and_the_list_says_so() {
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let (_, v) = call(&app, Some(&cookie), Method::GET, "/api/inbox").await;
    assert_eq!(v["refresh"], false);
    let (status, v) = call(&app, Some(&cookie), Method::POST, "/api/inbox/refresh").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(!v["error"].as_str().unwrap_or("").is_empty(), "{v}");
}

#[tokio::test]
async fn refresh_into_a_missing_directory_is_503() {
    let (_app, _cookie, state, cfg) = common::app_with_logged_in_user_and_state().await;
    let (app, cookie) = app_with_signal(&state, Some(cfg.path().join("absent/inbox-refresh"))).await;
    let (status, v) = call(&app, Some(&cookie), Method::POST, "/api/inbox/refresh").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(v["error"], "refresh is unavailable");
    let conn = state.db.lock().unwrap();
    let logged: i64 = conn
        .query_row("SELECT COUNT(*) FROM event_log WHERE kind = 'inbox_refresh_error'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(logged, 1);
}

#[tokio::test]
async fn refresh_needs_a_session() {
    let (_app, _cookie, state, cfg) = common::app_with_logged_in_user_and_state().await;
    let signal = cfg.path().join("inbox-refresh");
    let (app, _cookie) = app_with_signal(&state, Some(signal.clone())).await;
    let (status, _) = call(&app, None, Method::POST, "/api/inbox/refresh").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!signal.exists());
}
