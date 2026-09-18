mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::channels::mock::MockChannel;
use note_server::channels::Channel;
use note_server::providers::mock::MockLLM;
use note_server::providers::{ChatResponse, ToolCall};
use note_server::{api, auth, db, AppState};
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

struct World {
    app: axum::Router,
    cookie: String,
    state: AppState,
    push: Arc<MockChannel>,
    llm: Arc<MockLLM>,
    _cfg: TempDir,
}

async fn world(script: Vec<ChatResponse>) -> World {
    let cfg = common::config_dir();
    let dir = cfg.path().to_path_buf();
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", true).unwrap();
    let push = Arc::new(MockChannel::new("mockpush"));
    let llm = Arc::new(MockLLM::scripted(script));
    let ladder: Vec<Arc<dyn Channel>> = vec![push.clone()];
    let state = AppState::new(conn, dir.clone(), dir)
        .with_channels(ladder)
        .with_providers(llm.clone(), None);
    let app = api::router(state.clone());
    let cookie = common::login(&app, "aki", "pw").await;
    World { app, cookie, state, push, llm, _cfg: cfg }
}

fn call(id: &str, name: &str, args: &str) -> ChatResponse {
    ChatResponse {
        text: String::new(),
        tool_calls: vec![ToolCall { id: id.into(), name: name.into(), args: args.into() }],
    }
}

async fn post(w: &World, path: &str, body: &str) -> (StatusCode, serde_json::Value) {
    send(w, Request::post(path), body).await
}

async fn get(w: &World, path: &str) -> (StatusCode, serde_json::Value) {
    send(w, Request::get(path), "").await
}

async fn send(
    w: &World,
    req: axum::http::request::Builder,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let req = req
        .header(header::COOKIE, &w.cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(if body.is_empty() { Body::empty() } else { Body::from(body.to_string()) })
        .unwrap();
    let res = w.app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

/// Brings every waiting trigger forward to the top of the day, so one sweep
/// fires it whatever time the suite runs at.
fn make_due(w: &World) {
    let conn = w.state.db.lock().unwrap();
    conn.execute("UPDATE events SET wall_time = '00:00' WHERE kind = 'trigger'", []).unwrap();
}

fn rows<T: rusqlite::types::FromSql>(w: &World, sql: &str) -> Vec<T> {
    let conn = w.state.db.lock().unwrap();
    let mut stmt = conn.prepare(sql).unwrap();
    let out = stmt.query_map([], |r| r.get(0)).unwrap();
    out.collect::<rusqlite::Result<_>>().unwrap()
}

fn logged(w: &World, kind: &str) -> Vec<String> {
    rows(w, &format!("SELECT detail FROM event_log WHERE kind = '{kind}' ORDER BY id"))
}

fn statuses(w: &World) -> Vec<String> {
    rows(w, "SELECT status FROM events WHERE kind = 'trigger' ORDER BY id")
}

/// A session with no planned length, so the only trigger waiting is the
/// midpoint check these tests are about.
async fn start_session(w: &World) -> i64 {
    let (status, body) = post(w, "/api/sessions", r#"{"title":"read the chapter"}"#).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["id"].as_i64().unwrap()
}

#[tokio::test]
async fn a_trigger_that_speaks_lands_in_the_thread_and_on_a_channel() {
    let w = world(vec![call("c1", "say", r#"{"text":"how is the chapter going?"}"#)]).await;
    start_session(&w).await;
    make_due(&w);

    note_server::runner::sweep_once(&w.state);

    let seen = w.push.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1.title, "Note");
    assert_eq!(seen[0].1.body, "how is the chapter going?");
    let event_id = seen[0].1.event_id.expect("the words name the trigger they came from");
    assert_eq!(
        seen[0].1.actions.iter().map(|a| a.data.as_str()).collect::<Vec<_>>(),
        vec![
            format!("ev:done:{event_id}"),
            format!("ev:snooze:{event_id}:15"),
            format!("ev:drop:{event_id}"),
        ]
    );
    let thread = seen[0].1.conversation_id.expect("the words land in a thread");

    let said: Vec<String> = rows(
        &w,
        &format!(
            "SELECT content FROM talk_messages WHERE conversation_id = {thread}
             AND role = 'assistant'"
        ),
    );
    assert_eq!(said, vec!["how is the chapter going?"]);
    let checkin_date: Vec<Option<String>> =
        rows(&w, &format!("SELECT checkin_date FROM conversations WHERE id = {thread}"));
    assert!(checkin_date[0].is_some(), "with no thread of its own it joins the day's");
    assert_eq!(statuses(&w), vec!["done"]);
    assert_eq!(logged(&w, "trigger_said").len(), 1);

    // the session saw the prompt and the work session it belongs to
    let seen = w.llm.seen();
    assert_eq!(seen.len(), 1);
    let opening = format!("{:?}", seen[0].messages.last().unwrap());
    assert!(opening.contains("Progress check on read the chapter."), "{opening}");
    assert!(opening.contains("read the chapter"), "{opening}");
}

#[tokio::test]
async fn a_trigger_that_stays_quiet_sends_nothing_and_settles() {
    let w = world(vec![call("c1", "stay_quiet", r#"{"reason":"they answered a minute ago"}"#)]).await;
    start_session(&w).await;
    make_due(&w);

    note_server::runner::sweep_once(&w.state);

    assert!(w.push.seen().is_empty());
    assert_eq!(statuses(&w), vec!["done"]);
    let quiet = logged(&w, "trigger_quiet");
    assert_eq!(quiet.len(), 1);
    assert!(quiet[0].contains("they answered a minute ago"), "{}", quiet[0]);
    let said: Vec<i64> = rows(&w, "SELECT COUNT(*) FROM talk_messages");
    assert_eq!(said, vec![0], "the session's own thread stays empty");
}

#[tokio::test]
async fn a_firing_trigger_may_leave_one_follow_up_behind_inside_its_work_session() {
    let w = world(vec![
        call("c1", "wait_until", r#"{"at":"+30min","prompt":"still on the chapter?"}"#),
        call("c2", "say", r#"{"text":"half an hour in — how far did you get?"}"#),
    ])
    .await;
    let session = start_session(&w).await;
    make_due(&w);

    note_server::runner::sweep_once(&w.state);

    let laid: Vec<i64> = rows(
        &w,
        "SELECT work_session_id FROM events WHERE kind = 'trigger' AND cancel_if = 'replied'",
    );
    assert_eq!(laid, vec![session], "the follow-up belongs to the session that is running");
    let budget: Vec<i64> = rows(
        &w,
        "SELECT COUNT(*) FROM events WHERE kind = 'trigger' AND work_session_id IS NULL",
    );
    assert_eq!(budget, vec![0], "a session's own checks never spend the day's budget");
    assert_eq!(w.push.seen().len(), 1);
}

#[tokio::test]
async fn a_trigger_whose_reason_settled_itself_is_cancelled_before_any_session_runs() {
    let w = world(vec![call("c1", "say", r#"{"text":"never sent"}"#)]).await;
    let (status, task) = post(&w, "/api/tasks", r#"{"title":"the chapter"}"#).await;
    assert_eq!(status, StatusCode::OK, "{task}");
    let task_id = task["id"].as_i64().unwrap();
    {
        let conn = w.state.db.lock().unwrap();
        note_server::triggers::lay(
            &conn,
            &note_server::triggers::Lay {
                config_dir: w.state.config_dir.as_path(),
                user_id: 1,
                username: "aki",
                at: "+60min",
                prompt: "did the chapter get read?",
                date: jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date(),
                cancel: Some(note_server::triggers::Cancel::TaskDone(task_id)),
                conversation_id: None,
                work_session_id: None,
                now: jiff::Timestamp::now(),
            },
        )
        .unwrap();
    }
    let (status, _) = send(
        &w,
        Request::patch(format!("/api/tasks/{task_id}")),
        r#"{"state":"done"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    make_due(&w);

    note_server::runner::sweep_once(&w.state);

    assert_eq!(statuses(&w), vec!["dropped"]);
    assert!(w.llm.seen().is_empty(), "a cancelled trigger costs no tokens");
    assert!(w.push.seen().is_empty());
    assert_eq!(logged(&w, "trigger_cancelled").len(), 1);
    assert!(logged(&w, "event_fired").is_empty());
}

#[tokio::test]
async fn a_talk_session_raises_the_days_budget_once_the_user_has_agreed() {
    let w = world(vec![
        call("c1", "trigger_budget", r#"{"extra":3,"reason":"they asked to be pushed today"}"#),
        ChatResponse { text: "Raised today's check-in budget by 3.".into(), tool_calls: vec![] },
    ])
    .await;

    let (status, reply) =
        post(&w, "/api/talk", r#"{"message":"push me harder today"}"#).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(reply["reply"].as_str().unwrap().contains("Raised"), "{reply}");

    let extra: Vec<i64> = rows(&w, "SELECT extra FROM trigger_budgets");
    assert_eq!(extra, vec![3]);
    assert_eq!(logged(&w, "trigger_budget").len(), 1);

    let (status, settings) = get(&w, "/api/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["triggers_per_day"], 4, "the setting itself is untouched");
}

#[tokio::test]
async fn the_allowance_is_a_setting_the_user_owns() {
    let w = world(Vec::new()).await;
    let (status, saved) = send(
        &w,
        Request::put("/api/settings"),
        r#"{"triggers_per_day":8}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["triggers_per_day"], 8);
    assert_eq!(get(&w, "/api/settings").await.1["triggers_per_day"], 8);

    let (status, _) = send(&w, Request::put("/api/settings"), r#"{"triggers_per_day":99}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(get(&w, "/api/settings").await.1["triggers_per_day"], 8);
}

#[tokio::test]
async fn a_trigger_a_session_could_not_decide_sends_nothing_and_stays_where_it_is() {
    let w = world(vec![ChatResponse { text: "thinking out loud".into(), tool_calls: vec![] }]).await;
    start_session(&w).await;
    make_due(&w);

    note_server::runner::sweep_once(&w.state);

    assert!(w.push.seen().is_empty());
    assert_eq!(statuses(&w), vec!["fired"]);
    assert_eq!(logged(&w, "trigger_error").len(), 1);
}
