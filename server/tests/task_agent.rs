mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::providers::{mock::MockLLM, ChatRequest, ChatResponse, LLMProvider, ToolCall};
use note_server::{auth, tokens};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

/// Answers from a script and then fails, so a test can mutate the task through
/// real tool calls and still end the session with a provider error.
struct ScriptThenFail {
    script: Mutex<std::collections::VecDeque<ChatResponse>>,
}

impl ScriptThenFail {
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self { script: Mutex::new(responses.into()) }
    }
}

impl LLMProvider for ScriptThenFail {
    fn chat(&self, _req: &ChatRequest) -> anyhow::Result<ChatResponse> {
        match self.script.lock().unwrap().pop_front() {
            Some(r) => Ok(r),
            None => anyhow::bail!("provider down"),
        }
    }
}

fn call(id: &str, name: &str, args: &str) -> ChatResponse {
    ChatResponse {
        text: String::new(),
        tool_calls: vec![ToolCall { id: id.into(), name: name.into(), args: args.into() }],
    }
}

fn text(t: &str) -> ChatResponse {
    ChatResponse { text: t.into(), tool_calls: vec![] }
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

async fn make_task(app: &axum::Router, cookie: &str, title: &str) -> i64 {
    let (status, t) = send(
        app,
        cookie_auth(cookie),
        Method::POST,
        "/api/tasks",
        Some(&format!(r#"{{"title":"{title}"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{t}");
    t["id"].as_i64().unwrap()
}

async fn tasks_json(app: &axum::Router, cookie: &str) -> serde_json::Value {
    let (status, v) = send(app, cookie_auth(cookie), Method::GET, "/api/tasks", None).await;
    assert_eq!(status, StatusCode::OK);
    v
}

fn token_for(state: &note_server::AppState, user_id: i64) -> String {
    let conn = state.db.lock().unwrap();
    tokens::create(&conn, user_id, "importer").unwrap().token
}

#[tokio::test]
async fn a_bearer_token_briefs_a_task() {
    let llm = Arc::new(MockLLM::scripted(vec![
        call(
            "c1",
            "task_update",
            r#"{"task_id":1,"description":"Read chapter 4 and answer the questions.\nHand in: worksheet in class.","duration_min":45}"#,
        ),
        call(
            "c2",
            "task_split",
            r#"{"task_id":1,"steps":[{"title":"read chapter 4","duration_min":25},{"title":"answer the questions","duration_min":20}]}"#,
        ),
        text("briefed"),
    ]));
    let (app, cookie, state, _cfg) =
        common::app_with_logged_in_user_llm_and_state(llm.clone()).await;
    let id = make_task(&app, &cookie, "Biology ch.4").await;
    assert_eq!(id, 1);
    let token = token_for(&state, 1);

    let (status, v) = send(
        &app,
        bearer(&token),
        Method::POST,
        "/api/tasks/1/agent",
        Some(r#"{"context":"Due Friday. Worksheet handed out in class."}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["task_id"], 1);
    assert_eq!(v["outcome"], "briefed");

    let steps = v["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["name"], "task_update");
    assert_eq!(steps[0]["is_error"], false);
    assert!(steps[0]["args"].as_str().unwrap().contains("Hand in"));
    assert!(!steps[0]["result"].as_str().unwrap().is_empty());
    assert_eq!(steps[1]["name"], "task_split");
    assert_eq!(steps[1]["is_error"], false);

    let task = &v["task"];
    assert!(task["description"].as_str().unwrap().starts_with("Read chapter 4"));
    assert_eq!(task["duration_source"], "agent");
    assert_eq!(task["duration_min"], 45);
    assert_eq!(task["state"], "open");
    assert_eq!(task["children"].as_array().unwrap().len(), 2);
    assert_eq!(task["children"][0]["title"], "read chapter 4");

    // the session saw the import prompt and only the two scoped tools
    let seen = llm.seen();
    assert!(seen[0].system.contains("brief the assignment"), "{}", seen[0].system);
    assert_eq!(seen[0].tool_names, vec!["task_update", "task_split"]);
    let opening = match &seen[0].messages[0] {
        note_server::providers::Message::User(t) => t.clone(),
        other => panic!("expected the task as the opening message, got {other:?}"),
    };
    assert!(opening.contains("Biology ch.4"));
    assert!(opening.contains("Worksheet handed out in class."));

    // nothing was persisted as a talk conversation
    let (status, convs) =
        send(&app, cookie_auth(&cookie), Method::GET, "/api/conversations", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(convs.as_array().unwrap().is_empty(), "{convs}");
}

#[tokio::test]
async fn a_scripted_drop_reports_dropped() {
    let llm = Arc::new(MockLLM::scripted(vec![
        call(
            "c1",
            "task_update",
            r#"{"task_id":1,"state":"dropped","description":"Not homework: announcement about the field trip."}"#,
        ),
        text("dropped"),
    ]));
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    make_task(&app, &cookie, "Field trip notice").await;

    let (status, v) = send(
        &app,
        cookie_auth(&cookie),
        Method::POST,
        "/api/tasks/1/agent",
        Some("{}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["outcome"], "dropped");
    assert_eq!(v["task"]["state"], "dropped");
    assert!(v["task"]["description"].as_str().unwrap().starts_with("Not homework: "));
}

#[tokio::test]
async fn a_session_that_changes_nothing_reports_unchanged() {
    let llm = Arc::new(MockLLM::scripted(vec![text("nothing to do here")]));
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    make_task(&app, &cookie, "Reference sheet").await;

    // an empty body is the same as no context
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/tasks/1/agent")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, v) = read(res).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["outcome"], "unchanged");
    assert!(v["steps"].as_array().unwrap().is_empty());
    assert_eq!(v["task"]["title"], "Reference sheet");
}

#[tokio::test]
async fn a_rejected_tool_call_is_reported_and_leaves_the_task_unchanged() {
    // the scoped session refuses a foreign id, Now, and a re-split
    let llm = Arc::new(MockLLM::scripted(vec![
        call("c1", "task_update", r#"{"task_id":2,"description":"not mine to touch"}"#),
        call("c2", "task_update", r#"{"task_id":1,"is_now":true}"#),
        text("could not"),
    ]));
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    make_task(&app, &cookie, "Essay").await;
    make_task(&app, &cookie, "Other task").await;

    let (status, v) =
        send(&app, cookie_auth(&cookie), Method::POST, "/api/tasks/1/agent", Some("{}")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["outcome"], "unchanged");
    let steps = v["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    for s in steps {
        assert_eq!(s["is_error"], true, "{s}");
        assert!(s["result"].as_str().unwrap().contains("rejected"), "{s}");
    }
    assert_eq!(v["task"]["is_now"], false);
    assert_eq!(v["task"]["description"], "");
}

#[tokio::test]
async fn a_step_id_is_409_with_an_error_body() {
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let id = make_task(&app, &cookie, "Essay").await;
    let (status, v) = send(
        &app,
        cookie_auth(&cookie),
        Method::POST,
        &format!("/api/tasks/{id}/split"),
        Some(r#"{"steps":[{"title":"a","duration_min":5},{"title":"b","duration_min":5}]}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let step = v["children"][0]["id"].as_i64().unwrap();

    let (status, v) = send(
        &app,
        cookie_auth(&cookie),
        Method::POST,
        &format!("/api/tasks/{step}/agent"),
        Some("{}"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(!v["error"].as_str().unwrap_or("").is_empty(), "{v}");
}

#[tokio::test]
async fn an_unknown_or_foreign_task_is_404_with_an_error_body() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let id = make_task(&app, &cookie, "mine").await;
    let bo = {
        let conn = state.db.lock().unwrap();
        auth::create_user(&conn, "bo", "pw", false).unwrap()
    };
    let token_b = token_for(&state, bo);

    let (status, v) =
        send(&app, bearer(&token_b), Method::POST, &format!("/api/tasks/{id}/agent"), Some("{}"))
            .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!v["error"].as_str().unwrap_or("").is_empty(), "{v}");

    let (status, v) =
        send(&app, cookie_auth(&cookie), Method::POST, "/api/tasks/9999/agent", Some("{}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!v["error"].as_str().unwrap_or("").is_empty(), "{v}");
}

#[tokio::test]
async fn an_oversized_context_is_422() {
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_and_state().await;
    make_task(&app, &cookie, "Essay").await;
    let body = format!(r#"{{"context":"{}"}}"#, "x".repeat(32 * 1024 + 1));
    let (status, v) =
        send(&app, cookie_auth(&cookie), Method::POST, "/api/tasks/1/agent", Some(&body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!v["error"].as_str().unwrap_or("").is_empty(), "{v}");
    // nothing ran
    assert_eq!(tasks_json(&app, &cookie).await[0]["description"], "");
}

#[tokio::test]
async fn a_provider_failure_is_502_and_restores_the_task_and_its_steps() {
    let llm = Arc::new(ScriptThenFail::new(vec![
        call(
            "c1",
            "task_update",
            r#"{"task_id":1,"description":"half-written brief","duration_min":30}"#,
        ),
        call(
            "c2",
            "task_split",
            r#"{"task_id":1,"steps":[{"title":"one","duration_min":5},{"title":"two","duration_min":5}]}"#,
        ),
    ]));
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    make_task(&app, &cookie, "Essay").await;
    // give it a description, a duration and steps of its own first
    send(
        &app,
        cookie_auth(&cookie),
        Method::PATCH,
        "/api/tasks/1",
        Some(r#"{"description":"the caller's own text","duration_min":15}"#),
    )
    .await;
    send(
        &app,
        cookie_auth(&cookie),
        Method::POST,
        "/api/tasks/1/split",
        Some(r#"{"steps":[{"title":"draft","duration_min":20},{"title":"edit","duration_min":10}]}"#),
    )
    .await;
    let before = tasks_json(&app, &cookie).await;

    let (status, v) =
        send(&app, cookie_auth(&cookie), Method::POST, "/api/tasks/1/agent", Some("{}")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(!v["error"].as_str().unwrap_or("").is_empty(), "{v}");

    assert_eq!(tasks_json(&app, &cookie).await, before, "a failed session left the task changed");
}

#[tokio::test]
async fn a_cookie_session_briefs_too() {
    let llm = Arc::new(MockLLM::scripted(vec![
        call("c1", "task_update", r#"{"task_id":1,"description":"one line of brief"}"#),
        text("briefed"),
    ]));
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    make_task(&app, &cookie, "Essay").await;

    let (status, v) = send(
        &app,
        cookie_auth(&cookie),
        Method::POST,
        "/api/tasks/1/agent",
        Some(r#"{"context":"due monday"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["outcome"], "briefed");
    assert_eq!(v["task"]["description"], "one line of brief");
}

#[tokio::test]
async fn a_re_brief_keeps_the_existing_steps() {
    let llm = Arc::new(MockLLM::scripted(vec![
        call("c1", "task_update", r#"{"task_id":1,"description":"a fresher brief"}"#),
        text("briefed"),
    ]));
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm.clone()).await;
    make_task(&app, &cookie, "Essay").await;
    send(
        &app,
        cookie_auth(&cookie),
        Method::POST,
        "/api/tasks/1/split",
        Some(r#"{"steps":[{"title":"draft","duration_min":20},{"title":"edit","duration_min":10}]}"#),
    )
    .await;
    send(&app, cookie_auth(&cookie), Method::PATCH, "/api/tasks/2", Some(r#"{"state":"done"}"#))
        .await;

    let (status, v) =
        send(&app, cookie_auth(&cookie), Method::POST, "/api/tasks/1/agent", Some("{}")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["outcome"], "briefed");
    let children = v["task"]["children"].as_array().unwrap();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0]["title"], "draft");
    assert_eq!(children[0]["state"], "done");

    // the model was told which steps already exist, so it knows not to re-split
    let opening = match &llm.seen()[0].messages[0] {
        note_server::providers::Message::User(t) => t.clone(),
        other => panic!("expected the task as the opening message, got {other:?}"),
    };
    assert!(opening.contains("draft"), "{opening}");
}

#[tokio::test]
async fn a_second_brief_for_the_same_user_is_refused() {
    let llm = Arc::new(MockLLM::scripted(vec![text("briefed")]));
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    make_task(&app, &cookie, "Essay").await;
    let _permit = state.talk_gate.clone().try_enter(1).unwrap();

    let (status, v) =
        send(&app, cookie_auth(&cookie), Method::POST, "/api/tasks/1/agent", Some("{}")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{v}");
    assert_eq!(v["error"], "a session is already in progress");
}

#[tokio::test]
async fn the_daily_session_cap_refuses_a_brief() {
    let llm = Arc::new(MockLLM::scripted(vec![text("briefed")]));
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    make_task(&app, &cookie, "Essay").await;
    {
        let conn = state.db.lock().unwrap();
        for _ in 0..state.agent_sessions_per_day {
            note_server::log::record(&conn, Some(1), "agent_session", "kind=Import").unwrap();
        }
    }

    let (status, v) =
        send(&app, cookie_auth(&cookie), Method::POST, "/api/tasks/1/agent", Some("{}")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{v}");
    assert_eq!(v["error"], "daily session limit reached");
}

#[tokio::test]
async fn a_bearer_session_is_attributed_to_its_token() {
    let llm = Arc::new(MockLLM::scripted(vec![text("briefed")]));
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    make_task(&app, &cookie, "Essay").await;
    let token = token_for(&state, 1);

    let (status, v) = send(&app, bearer(&token), Method::POST, "/api/tasks/1/agent", Some("{}")).await;
    assert_eq!(status, StatusCode::OK, "{v}");

    let detail: String = {
        let conn = state.db.lock().unwrap();
        conn.query_row(
            "SELECT detail FROM event_log WHERE kind = 'agent_session'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(detail.ends_with(" token=1"), "{detail}");
}
