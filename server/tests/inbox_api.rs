mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::providers::{mock::MockLLM, ChatRequest, ChatResponse, LLMProvider, ToolCall};
use note_server::{auth, tokens, AppState};
use rusqlite::OptionalExtension;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

/// Answers from a script and then fails, so a test can drive the session up to
/// a point and still end it with a provider error.
struct ScriptThenFail {
    script: Mutex<std::collections::VecDeque<ChatResponse>>,
}

impl LLMProvider for ScriptThenFail {
    fn chat(&self, _req: &ChatRequest) -> anyhow::Result<ChatResponse> {
        match self.script.lock().unwrap().pop_front() {
            Some(r) => Ok(r),
            None => anyhow::bail!("provider down"),
        }
    }
}

fn decide(id: &str, args: &str) -> ChatResponse {
    ChatResponse {
        text: String::new(),
        tool_calls: vec![ToolCall {
            id: id.into(),
            name: "inbox_decide".into(),
            args: args.into(),
        }],
    }
}

async fn read(res: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

async fn send(
    app: &axum::Router,
    auth_header: (&str, String),
    method: Method,
    path: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(auth_header.0, auth_header.1);
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.unwrap_or("").to_string())).unwrap())
        .await
        .unwrap();
    read(res).await
}

fn cookie_auth(cookie: &str) -> (&'static str, String) {
    ("cookie", cookie.to_string())
}

fn bearer(token: &str) -> (&'static str, String) {
    ("authorization", format!("Bearer {token}"))
}

fn token_for(state: &AppState, user_id: i64) -> String {
    let conn = state.db.lock().unwrap();
    tokens::create(&conn, user_id, "importer").unwrap().token
}

async fn post_inbox(
    app: &axum::Router,
    auth: (&str, String),
    body: &str,
) -> (StatusCode, serde_json::Value) {
    send(app, auth, Method::POST, "/api/agent/inbox", Some(body)).await
}

async fn memory_items(app: &axum::Router, cookie: &str) -> Vec<serde_json::Value> {
    let (status, v) = send(app, cookie_auth(cookie), Method::GET, "/api/memory", None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["items"].as_array().unwrap().clone()
}

fn source_rows(state: &AppState) -> i64 {
    let conn = state.db.lock().unwrap();
    conn.query_row("SELECT COUNT(*) FROM memory_sources", [], |r| r.get(0)).unwrap()
}

fn live_files(cfg: &tempfile::TempDir, user: &str) -> usize {
    std::fs::read_dir(cfg.path().join(format!("memory/{user}/semantic")))
        .map(|d| d.flatten().count())
        .unwrap_or(0)
}

fn inbox_row(state: &AppState, source: &str) -> Option<serde_json::Value> {
    let conn = state.db.lock().unwrap();
    conn.query_row(
        "SELECT kind, title, body, received_at, outcome, reason, decided_at
         FROM inbox_items WHERE user_id = 1 AND source_id = ?1",
        [source],
        |r| {
            Ok(serde_json::json!({
                "kind": r.get::<_, String>(0)?,
                "title": r.get::<_, String>(1)?,
                "body": r.get::<_, String>(2)?,
                "received_at": r.get::<_, String>(3)?,
                "outcome": r.get::<_, Option<String>>(4)?,
                "reason": r.get::<_, Option<String>>(5)?,
                "decided_at": r.get::<_, Option<String>>(6)?,
            }))
        },
    )
    .optional()
    .unwrap()
}

fn inbox_rows(state: &AppState) -> i64 {
    let conn = state.db.lock().unwrap();
    conn.query_row("SELECT COUNT(*) FROM inbox_items", [], |r| r.get(0)).unwrap()
}

const REMEMBER_TWO: &str = r#"{"source_id":"lms:post:77","outcome":"remembered",
    "reason":"two dated facts about the biology quiz",
    "facts":[
      {"summary":"Biology quiz on chapter 4","body":"Biology: the chapter 4 quiz is on 2026-09-25, per the post \"Quiz Friday\".","until":"2026-09-25"},
      {"summary":"Biology late work policy","body":"Biology: late work loses 10% a day, per the post \"Quiz Friday\"."}
    ]}"#;

#[tokio::test]
async fn a_remembered_item_writes_its_facts_and_returns_their_ids() {
    let llm = Arc::new(MockLLM::scripted(vec![decide("c1", REMEMBER_TWO)]));
    let (app, cookie, state, cfg) =
        common::app_with_logged_in_user_llm_and_state(llm.clone()).await;
    let token = token_for(&state, 1);

    let (status, v) = post_inbox(
        &app,
        bearer(&token),
        r#"{"source_id":"lms:post:77","kind":"announcement","context":"Quiz Friday. Late work loses 10% a day."}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["source_id"], "lms:post:77");
    assert_eq!(v["outcome"], "remembered");
    assert_eq!(v["reason"], "two dated facts about the biology quiz");

    let ids: Vec<String> = v["memory_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i.as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 2, "{v}");
    for id in &ids {
        assert!(
            cfg.path().join(format!("memory/aki/semantic/{id}.md")).exists(),
            "no file for {id}"
        );
    }

    let items = memory_items(&app, &cookie).await;
    let listed: Vec<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
    for id in &ids {
        assert!(listed.contains(&id.as_str()), "{id} missing from /api/memory");
    }
    assert!(items.iter().all(|i| i["category"] == "semantic"), "{items:?}");
    assert_eq!(source_rows(&state), 2);

    let steps = v["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0]["name"], "inbox_decide");
    assert_eq!(steps[0]["is_error"], false);

    // the inbox prompt alone, the inbox tool surface, one model round
    let seen = llm.seen();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].system.contains("read one item"), "{}", seen[0].system);
    assert!(!seen[0].system.contains("you are note"), "{}", seen[0].system);
    assert!(!seen[0].system.contains("Today's plan"), "{}", seen[0].system);
    assert_eq!(seen[0].tool_names, vec!["memory_query", "memory_read", "inbox_decide"]);
    let opening = match &seen[0].messages[0] {
        note_server::providers::Message::User(t) => t.clone(),
        other => panic!("expected the item as the opening message, got {other:?}"),
    };
    assert!(opening.starts_with("Source: lms:post:77\nKind: announcement\n\n"), "{opening}");
    assert!(opening.contains("Quiz Friday."), "{opening}");
}

#[tokio::test]
async fn the_until_date_reaches_the_memory_file() {
    let llm = Arc::new(MockLLM::scripted(vec![decide("c1", REMEMBER_TWO)]));
    let (app, _cookie, state, cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    let token = token_for(&state, 1);
    let (status, v) = post_inbox(
        &app,
        bearer(&token),
        r#"{"source_id":"lms:post:77","kind":"announcement","context":"x"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let dated = v["memory_ids"][0].as_str().unwrap();
    let raw =
        std::fs::read_to_string(cfg.path().join(format!("memory/aki/semantic/{dated}.md"))).unwrap();
    assert!(raw.contains("until: 2026-09-25"), "{raw}");

    let undated = v["memory_ids"][1].as_str().unwrap();
    let raw = std::fs::read_to_string(cfg.path().join(format!("memory/aki/semantic/{undated}.md")))
        .unwrap();
    assert!(!raw.contains("until:"), "{raw}");
}

#[tokio::test]
async fn nothing_writes_no_facts() {
    let llm = Arc::new(MockLLM::scripted(vec![decide(
        "c1",
        r#"{"source_id":"lms:post:9","outcome":"nothing","reason":"a greeting with nothing durable"}"#,
    )]));
    let (app, cookie, state, cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;

    let (status, v) = post_inbox(
        &app,
        cookie_auth(&cookie),
        r#"{"source_id":"lms:post:9","kind":"announcement","context":"Good morning everyone!"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["outcome"], "nothing");
    assert!(!v["reason"].as_str().unwrap().is_empty());
    assert!(v["memory_ids"].as_array().unwrap().is_empty(), "{v}");
    assert!(memory_items(&app, &cookie).await.is_empty());
    assert_eq!(source_rows(&state), 0);
    assert_eq!(live_files(&cfg, "aki"), 0);
}

#[tokio::test]
async fn task_writes_no_facts_and_names_its_reason() {
    let llm = Arc::new(MockLLM::scripted(vec![decide(
        "c1",
        r#"{"source_id":"lms:material:4","outcome":"task","reason":"a study guide to work through"}"#,
    )]));
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;

    let (status, v) = post_inbox(
        &app,
        cookie_auth(&cookie),
        r#"{"source_id":"lms:material:4","kind":"material","context":"Study guide for the unit test."}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["outcome"], "task");
    assert_eq!(v["reason"], "a study guide to work through");
    assert!(v["memory_ids"].as_array().unwrap().is_empty(), "{v}");
    assert!(memory_items(&app, &cookie).await.is_empty());
}

#[tokio::test]
async fn re_sending_a_source_archives_the_facts_it_wrote_before() {
    let llm = Arc::new(MockLLM::scripted(vec![
        decide("c1", REMEMBER_TWO),
        decide(
            "c2",
            r#"{"source_id":"lms:post:77","outcome":"remembered","reason":"the quiz moved",
                "facts":[{"summary":"Biology quiz moved","body":"Biology: the chapter 4 quiz moved to 2026-09-28.","until":"2026-09-28"}]}"#,
        ),
        decide(
            "c3",
            r#"{"source_id":"lms:post:77","outcome":"nothing","reason":"the edit added nothing durable"}"#,
        ),
    ]));
    let (app, cookie, state, cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    let body = r#"{"source_id":"lms:post:77","kind":"announcement","context":"x"}"#;

    let (_, first) = post_inbox(&app, cookie_auth(&cookie), body).await;
    let old: Vec<String> = first["memory_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i.as_str().unwrap().to_string())
        .collect();

    let (status, second) = post_inbox(&app, cookie_auth(&cookie), body).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let new = second["memory_ids"][0].as_str().unwrap().to_string();
    assert_eq!(second["memory_ids"].as_array().unwrap().len(), 1);

    for id in &old {
        assert!(
            cfg.path().join(format!("memory/aki/archive/{id}.md")).exists(),
            "{id} was not archived"
        );
        assert!(!cfg.path().join(format!("memory/aki/semantic/{id}.md")).exists());
    }
    let listed: Vec<String> = memory_items(&app, &cookie)
        .await
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(listed, vec![new.clone()]);
    assert_eq!(source_rows(&state), 1);

    // a re-send that decides nothing still clears what the source held
    let (status, third) = post_inbox(&app, cookie_auth(&cookie), body).await;
    assert_eq!(status, StatusCode::OK, "{third}");
    assert_eq!(third["outcome"], "nothing");
    assert!(memory_items(&app, &cookie).await.is_empty());
    assert_eq!(source_rows(&state), 0);
    assert!(cfg.path().join(format!("memory/aki/archive/{new}.md")).exists());
}

#[tokio::test]
async fn another_users_source_id_never_touches_my_facts() {
    let llm = Arc::new(MockLLM::scripted(vec![
        decide("c1", REMEMBER_TWO),
        decide(
            "c2",
            r#"{"source_id":"lms:post:77","outcome":"remembered","reason":"bo's own copy",
                "facts":[{"summary":"Bo's biology quiz","body":"Biology: bo's quiz is on 2026-09-25."}]}"#,
        ),
    ]));
    let (app, cookie, state, cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    let body = r#"{"source_id":"lms:post:77","kind":"announcement","context":"x"}"#;
    let (_, mine) = post_inbox(&app, cookie_auth(&cookie), body).await;
    let mine: Vec<String> = mine["memory_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i.as_str().unwrap().to_string())
        .collect();

    let bo = {
        let conn = state.db.lock().unwrap();
        auth::create_user(&conn, "bo", "pw", false).unwrap()
    };
    let bo_token = token_for(&state, bo);
    let (status, theirs) = post_inbox(&app, bearer(&bo_token), body).await;
    assert_eq!(status, StatusCode::OK, "{theirs}");
    let bo_id = theirs["memory_ids"][0].as_str().unwrap();

    // mine are untouched and live; bo's are bo's alone
    for id in &mine {
        assert!(cfg.path().join(format!("memory/aki/semantic/{id}.md")).exists());
    }
    assert_eq!(live_files(&cfg, "aki"), 2);
    assert_eq!(live_files(&cfg, "bo"), 1);

    let bo_cookie = common::login(&app, "bo", "pw").await;
    let bo_listed: Vec<String> = memory_items(&app, &bo_cookie)
        .await
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(bo_listed, vec![bo_id.to_string()]);
    let my_listed: Vec<String> = memory_items(&app, &cookie)
        .await
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    assert!(!my_listed.contains(&bo_id.to_string()));
    assert_eq!(my_listed.len(), 2);
}

#[tokio::test]
async fn a_foreign_source_id_in_the_tool_call_is_rejected() {
    let llm = Arc::new(MockLLM::scripted(vec![
        decide(
            "c1",
            r#"{"source_id":"lms:post:OTHER","outcome":"nothing","reason":"not my scope"}"#,
        ),
        decide(
            "c2",
            r#"{"source_id":"lms:post:77","outcome":"nothing","reason":"second try, right scope"}"#,
        ),
    ]));
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    let (status, v) = post_inbox(
        &app,
        cookie_auth(&cookie),
        r#"{"source_id":"lms:post:77","kind":"announcement","context":"x"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["outcome"], "nothing");
    let steps = v["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["is_error"], true);
    assert!(steps[0]["result"].as_str().unwrap().contains("rejected"), "{}", steps[0]);
    assert_eq!(steps[1]["is_error"], false);
}

#[tokio::test]
async fn a_provider_failure_is_502_and_writes_nothing() {
    let llm = Arc::new(ScriptThenFail {
        script: Mutex::new(
            vec![ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "memory_query".into(),
                    args: r#"{"query":"biology"}"#.into(),
                }],
            }]
            .into(),
        ),
    });
    let (app, cookie, state, cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;

    let (status, v) = post_inbox(
        &app,
        cookie_auth(&cookie),
        r#"{"source_id":"lms:post:77","kind":"announcement","context":"x"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{v}");
    assert!(!v["error"].as_str().unwrap_or("").is_empty(), "{v}");
    assert_eq!(live_files(&cfg, "aki"), 0);
    assert_eq!(source_rows(&state), 0);
    assert!(memory_items(&app, &cookie).await.is_empty());
}

#[tokio::test]
async fn a_session_that_never_decides_is_502() {
    let llm = Arc::new(MockLLM::scripted(vec![
        ChatResponse { text: "I am not sure".into(), tool_calls: vec![] },
    ]));
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    let (status, v) = post_inbox(
        &app,
        cookie_auth(&cookie),
        r#"{"source_id":"lms:post:77","kind":"announcement","context":"x"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{v}");
    assert!(!v["error"].as_str().unwrap_or("").is_empty(), "{v}");
}

#[tokio::test]
async fn a_malformed_body_says_so_and_nothing_more() {
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let (status, v) =
        post_inbox(&app, cookie_auth(&cookie), r#"{"source_id": unquoted}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"], "malformed JSON body");
}

#[tokio::test]
async fn bad_fields_are_422_with_an_error_body() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let oversize = format!(
        r#"{{"source_id":"lms:post:1","kind":"announcement","context":"{}"}}"#,
        "x".repeat(32 * 1024 + 1)
    );
    let bad = [
        oversize.as_str(),
        r#"{"source_id":"lms:post:1","kind":"gossip","context":"x"}"#,
        r#"{"source_id":"","kind":"announcement","context":"x"}"#,
        r#"{"source_id":"lms/post/1","kind":"announcement","context":"x"}"#,
    ];
    for body in bad {
        let (status, v) = post_inbox(&app, cookie_auth(&cookie), body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(!v["error"].as_str().unwrap_or("").is_empty(), "{v}");
    }
    assert_eq!(inbox_rows(&state), 0);
}

#[tokio::test]
async fn an_unknown_field_is_a_malformed_body() {
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let (status, v) = post_inbox(
        &app,
        cookie_auth(&cookie),
        r#"{"source_id":"lms:post:1","kind":"announcement","context":"x","extra":1}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"], "malformed JSON body");
}

#[tokio::test]
async fn the_route_needs_a_principal() {
    let (app, _cookie, _state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let res = app
        .oneshot(
            Request::post("/api/agent/inbox")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"source_id":"lms:post:1","kind":"announcement","context":"x"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_second_session_for_the_same_user_is_refused() {
    let llm = Arc::new(MockLLM::scripted(vec![decide("c1", REMEMBER_TWO)]));
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    let _permit = state.talk_gate.clone().try_enter(1).unwrap();

    let (status, v) = post_inbox(
        &app,
        cookie_auth(&cookie),
        r#"{"source_id":"lms:post:77","kind":"announcement","context":"x"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{v}");
    assert_eq!(v["error"], "a session is already in progress");
    assert_eq!(inbox_rows(&state), 0, "a refused item is not kept");
}

#[tokio::test]
async fn the_daily_session_cap_refuses_an_item() {
    let llm = Arc::new(MockLLM::scripted(vec![decide("c1", REMEMBER_TWO)]));
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    {
        let conn = state.db.lock().unwrap();
        for _ in 0..state.agent_sessions_per_day {
            note_server::log::record(&conn, Some(1), "agent_session", "kind=Inbox").unwrap();
        }
    }
    let (status, v) = post_inbox(
        &app,
        cookie_auth(&cookie),
        r#"{"source_id":"lms:post:77","kind":"announcement","context":"x"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{v}");
    assert_eq!(v["error"], "daily session limit reached");
    assert_eq!(inbox_rows(&state), 0, "a refused item is not kept");
}

#[tokio::test]
async fn an_item_is_kept_with_its_decision() {
    let llm = Arc::new(MockLLM::scripted(vec![decide("c1", REMEMBER_TWO)]));
    let (app, _cookie, state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    let token = token_for(&state, 1);
    let context = "\n  Quiz Friday  \nLate work loses 10% a day.";
    let body = serde_json::json!({"source_id":"lms:post:77","kind":"announcement","context":context}).to_string();
    let (status, v) = post_inbox(&app, bearer(&token), &body).await;
    assert_eq!(status, StatusCode::OK, "{v}");

    let row = inbox_row(&state, "lms:post:77").expect("the item is kept");
    assert_eq!(row["kind"], "announcement");
    assert_eq!(row["title"], "Quiz Friday");
    assert_eq!(row["body"], context);
    assert_eq!(row["outcome"], "remembered");
    assert_eq!(row["reason"], "two dated facts about the biology quiz");
    assert!(row["decided_at"].as_str().unwrap() >= row["received_at"].as_str().unwrap());
}

#[tokio::test]
async fn a_re_send_keeps_one_row_and_takes_the_new_decision() {
    let llm = Arc::new(MockLLM::scripted(vec![
        decide("c1", REMEMBER_TWO),
        decide("c2", r#"{"source_id":"lms:post:77","outcome":"nothing","reason":"the edit added nothing durable"}"#),
    ]));
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    post_inbox(&app, cookie_auth(&cookie), r#"{"source_id":"lms:post:77","kind":"announcement","context":"Quiz Friday"}"#).await;
    let first = inbox_row(&state, "lms:post:77").unwrap();
    let (status, _) = post_inbox(&app, cookie_auth(&cookie), r#"{"source_id":"lms:post:77","kind":"material","context":"Quiz moved"}"#).await;
    assert_eq!(status, StatusCode::OK);
    let second = inbox_row(&state, "lms:post:77").unwrap();
    assert_eq!(inbox_rows(&state), 1);
    assert_eq!(second["kind"], "material");
    assert_eq!(second["title"], "Quiz moved");
    assert_eq!(second["outcome"], "nothing");
    assert!(second["received_at"].as_str().unwrap() > first["received_at"].as_str().unwrap());
}

#[tokio::test]
async fn a_failed_re_send_leaves_the_item_undecided_and_its_facts_in_place() {
    let llm = Arc::new(ScriptThenFail { script: Mutex::new(vec![decide("c1", REMEMBER_TWO)].into()) });
    let (app, cookie, state, cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    let (status, _) = post_inbox(&app, cookie_auth(&cookie), r#"{"source_id":"lms:post:77","kind":"announcement","context":"Quiz Friday"}"#).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = post_inbox(&app, cookie_auth(&cookie), r#"{"source_id":"lms:post:77","kind":"announcement","context":"Quiz moved"}"#).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);

    let row = inbox_row(&state, "lms:post:77").unwrap();
    assert_eq!(row["title"], "Quiz moved");
    assert_eq!(row["outcome"], serde_json::Value::Null);
    assert_eq!(row["reason"], serde_json::Value::Null);
    assert_eq!(source_rows(&state), 2);
    assert_eq!(live_files(&cfg, "aki"), 2);
}
