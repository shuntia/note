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
    _cfg: TempDir,
}

async fn world(script: Vec<ChatResponse>) -> World {
    let cfg = common::config_dir();
    let dir = cfg.path().to_path_buf();
    // The day's own routines stay out of these counts: the template lays nothing.
    std::fs::create_dir_all(dir.join("users/aki/templates")).unwrap();
    std::fs::write(dir.join("users/aki/templates/default.toml"), "events = []\n").unwrap();
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", true).unwrap();
    let push = Arc::new(MockChannel::new("mockpush"));
    let ladder: Vec<Arc<dyn Channel>> = vec![push.clone()];
    let state = AppState::new(conn, dir.clone(), dir)
        .with_channels(ladder)
        .with_providers(Arc::new(MockLLM::scripted(script)), None);
    let app = api::router(state.clone());
    let cookie = common::login(&app, "aki", "pw").await;
    World { app, cookie, state, push, _cfg: cfg }
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

async fn post(w: &World, path: &str, body: &str) -> (StatusCode, serde_json::Value) {
    send(w, Request::post(path), body).await
}

async fn get(w: &World, path: &str) -> (StatusCode, serde_json::Value) {
    send(w, Request::get(path), "").await
}

async fn start(w: &World, body: &str) -> serde_json::Value {
    let (status, session) = post(w, "/api/sessions", body).await;
    assert_eq!(status, StatusCode::OK, "{session}");
    session
}

fn rows<T: rusqlite::types::FromSql>(w: &World, sql: &str) -> Vec<T> {
    let conn = w.state.db.lock().unwrap();
    let mut stmt = conn.prepare(sql).unwrap();
    let out = stmt.query_map([], |r| r.get(0)).unwrap();
    out.collect::<rusqlite::Result<_>>().unwrap()
}

fn prompts(w: &World) -> Vec<String> {
    rows(w, "SELECT prompt FROM events WHERE kind = 'trigger' ORDER BY id")
}

fn statuses(w: &World) -> Vec<String> {
    rows(w, "SELECT status FROM events WHERE kind = 'trigger' ORDER BY id")
}

/// Winds a running session's clock back, so a sweep finds its phase over.
fn age_phase(w: &World, minutes: i64) {
    let then = jiff::Timestamp::now() - jiff::Span::new().minutes(minutes);
    let conn = w.state.db.lock().unwrap();
    conn.execute(
        "UPDATE work_sessions SET phase_started_at = ?1 WHERE ended_at IS NULL",
        [then.to_string()],
    )
    .unwrap();
}

async fn turn_on_pomodoro(w: &World) {
    let (status, saved) = send(
        w,
        Request::put("/api/settings"),
        r#"{"pomodoro_enabled":true,"pomodoro_work_min":25,"pomodoro_break_min":5}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
}

#[tokio::test]
async fn the_open_session_is_the_whole_face_the_client_paints() {
    let w = world(Vec::new()).await;
    let session = start(
        &w,
        r#"{"title":"Common App essay draft","planned_min":60,"step_index":1,"step_count":3,
            "step_name":"the opening paragraph","notes":"kitchen table"}"#,
    )
    .await;

    assert_eq!(session["task_id"], serde_json::Value::Null);
    assert_eq!(session["event_id"], serde_json::Value::Null);
    assert_eq!(session["title"], "Common App essay draft");
    assert_eq!(session["planned_min"], 60);
    assert_eq!(session["paused_at"], serde_json::Value::Null);
    assert_eq!(session["paused_ms"], 0);
    assert_eq!(session["mode"], "single");
    assert_eq!(session["work_min"], 25);
    assert_eq!(session["break_min"], 5);
    assert_eq!(session["phase"], "work");
    assert_eq!(session["phase_started_at"], session["started_at"]);
    assert_eq!(session["phase_paused_ms"], 0);
    assert_eq!(session["round"], 1);
    assert_eq!(session["step_index"], 1);
    assert_eq!(session["step_count"], 3);
    assert_eq!(session["step_name"], "the opening paragraph");
    assert_eq!(session["notes"], "kitchen table");
    assert!(session["conversation_id"].is_i64(), "the session holds its own thread");
    assert!(session["started_at"].as_str().unwrap().parse::<jiff::Timestamp>().is_ok());

    assert_eq!(get(&w, "/api/sessions/open").await.1, session, "open is the same object");
}

#[tokio::test]
async fn a_session_lays_its_checks_and_takes_them_along_when_it_ends() {
    let w = world(Vec::new()).await;
    let session = start(&w, r#"{"title":"read the chapter","planned_min":30}"#).await;
    let id = session["id"].as_i64().unwrap();

    assert_eq!(prompts(&w).len(), 2, "the midpoint and the planned end");
    assert_eq!(prompts(&w)[0], "Progress check on read the chapter.");
    assert!(prompts(&w)[1].contains("has run its planned 30 minutes"), "{:?}", prompts(&w)[1]);

    let (status, ended) = post(&w, &format!("/api/sessions/{id}/end"), r#"{"outcome":"done"}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ended["ended"], id);
    assert_eq!(statuses(&w), vec!["dropped", "dropped", "pending"]);
    assert!(prompts(&w)[2].contains("just ended (done"), "{:?}", prompts(&w)[2]);
    assert_eq!(get(&w, "/api/sessions/open").await.1, serde_json::Value::Null);

    let (status, again) =
        post(&w, &format!("/api/sessions/{id}/end"), r#"{"outcome":"done"}"#).await;
    assert_eq!(status, StatusCode::OK, "saying stop twice is not an error");
    assert_eq!(again["ended"], serde_json::Value::Null);

    let (status, _) = post(&w, &format!("/api/sessions/{id}/end"), r#"{"outcome":"quit"}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = post(&w, "/api/sessions", r#"{"title":"  "}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn starting_again_stops_what_was_running_and_leaves_it_a_farewell() {
    let w = world(Vec::new()).await;
    let first = start(&w, r#"{"title":"read the chapter"}"#).await;
    let second = start(&w, r#"{"title":"write the essay"}"#).await;
    assert_ne!(first["id"], second["id"]);
    assert_eq!(get(&w, "/api/sessions/open").await.1["id"], second["id"]);

    let outcomes: Vec<String> = rows(&w, "SELECT outcome FROM work_sessions WHERE ended_at IS NOT NULL");
    assert_eq!(outcomes, vec!["stopped"]);
    let farewell = prompts(&w)
        .into_iter()
        .find(|p| p.contains("just ended"))
        .expect("the stopped session says goodbye");
    assert!(farewell.starts_with("The session on read the chapter just ended (stopped,"), "{farewell}");
}

#[tokio::test]
async fn the_farewell_speaks_on_the_next_sweep_in_the_sessions_own_thread() {
    let w = world(vec![ChatResponse {
        text: String::new(),
        tool_calls: vec![ToolCall {
            id: "c1".into(),
            name: "say".into(),
            args: r#"{"text":"that is a wrap — what is left of it?"}"#.into(),
        }],
    }])
    .await;
    let session = start(&w, r#"{"title":"read the chapter"}"#).await;
    let id = session["id"].as_i64().unwrap();
    let thread = session["conversation_id"].as_i64().unwrap();
    post(&w, &format!("/api/sessions/{id}/end"), r#"{"outcome":"done"}"#).await;

    note_server::runner::sweep_once(&w.state);

    let seen = w.push.seen();
    let farewell: Vec<_> = seen.iter().filter(|m| m.1.conversation_id == Some(thread)).collect();
    assert_eq!(farewell.len(), 1, "the farewell needs no lead time: {seen:?}");
    assert_eq!(farewell[0].1.body, "that is a wrap — what is left of it?");
    let said: Vec<String> = rows(
        &w,
        &format!(
            "SELECT content FROM talk_messages WHERE conversation_id = {thread} AND role = 'assistant'"
        ),
    );
    assert_eq!(said, vec!["that is a wrap — what is left of it?"]);
}

#[tokio::test]
async fn pausing_holds_the_clock_and_a_stale_id_reaches_nothing() {
    let w = world(Vec::new()).await;
    let session = start(&w, r#"{"title":"read the chapter"}"#).await;
    let id = session["id"].as_i64().unwrap();

    let (status, paused) = post(&w, &format!("/api/sessions/{id}/pause"), "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(paused["paused_at"].is_string());
    let (status, twice) = post(&w, &format!("/api/sessions/{id}/pause"), "").await;
    assert_eq!(status, StatusCode::OK, "pausing twice is a no-op");
    assert_eq!(twice["paused_at"], paused["paused_at"]);

    let (status, running) = post(&w, &format!("/api/sessions/{id}/resume"), "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(running["paused_at"], serde_json::Value::Null);
    assert!(running["paused_ms"].as_i64().unwrap() >= 0);
    let (status, _) = post(&w, &format!("/api/sessions/{id}/resume"), "").await;
    assert_eq!(status, StatusCode::OK, "resuming a running session is a no-op");

    for path in ["pause", "resume", "skip_break"] {
        let (status, _) = post(&w, &format!("/api/sessions/{}/{path}", id + 99), "").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path} on a session that is not open");
    }
    let (status, _) = post(
        &w,
        &format!("/api/sessions/{}/step", id + 99),
        r#"{"step_index":1,"step_name":"nowhere"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_step_the_session_is_on_comes_back_with_the_session() {
    let w = world(Vec::new()).await;
    let session = start(&w, r#"{"title":"read the chapter","step_count":3}"#).await;
    let id = session["id"].as_i64().unwrap();

    let (status, moved) = post(
        &w,
        &format!("/api/sessions/{id}/step"),
        r#"{"step_index":2,"step_name":"photos of the ceiling"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["step_index"], 2);
    assert_eq!(moved["step_name"], "photos of the ceiling");
    assert_eq!(moved["step_count"], 3);
    assert_eq!(get(&w, "/api/sessions/open").await.1["step_name"], "photos of the ceiling");

    let (status, _) =
        post(&w, &format!("/api/sessions/{id}/step"), r#"{"step_index":-1,"step_name":"x"}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_pomodoro_session_turns_its_rounds_and_says_so() {
    let w = world(Vec::new()).await;
    turn_on_pomodoro(&w).await;
    let session = start(&w, r#"{"title":"read the chapter"}"#).await;
    let id = session["id"].as_i64().unwrap();
    assert_eq!(session["mode"], "pomodoro");
    assert!(prompts(&w).is_empty(), "a pomodoro session keeps its own time");

    note_server::runner::sweep_once(&w.state);
    assert!(w.push.seen().is_empty(), "the round has not run out yet");

    age_phase(&w, 26);
    note_server::runner::sweep_once(&w.state);
    let seen = w.push.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1.title, "Break");
    assert_eq!(seen[0].1.body, "5 min. Round 1 of read the chapter done.");
    assert_eq!(seen[0].1.conversation_id, session["conversation_id"].as_i64());
    let open = get(&w, "/api/sessions/open").await.1;
    assert_eq!((open["phase"].as_str(), open["round"].as_i64()), (Some("break"), Some(1)));

    let (status, back) = post(&w, &format!("/api/sessions/{id}/skip_break"), "").await;
    assert_eq!(status, StatusCode::OK, "{back}");
    assert_eq!((back["phase"].as_str(), back["round"].as_i64()), (Some("work"), Some(2)));

    age_phase(&w, 26);
    note_server::runner::sweep_once(&w.state);
    age_phase(&w, 6);
    note_server::runner::sweep_once(&w.state);
    let seen = w.push.seen();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[2].1.title, "Round 3");
    assert_eq!(seen[2].1.body, "Back to read the chapter.");
    assert_eq!(get(&w, "/api/sessions/open").await.1["round"], 3);
}

#[tokio::test]
async fn a_paused_pomodoro_session_waits_where_it_is() {
    let w = world(Vec::new()).await;
    turn_on_pomodoro(&w).await;
    let session = start(&w, r#"{"title":"read the chapter"}"#).await;
    let id = session["id"].as_i64().unwrap();
    post(&w, &format!("/api/sessions/{id}/pause"), "").await;

    age_phase(&w, 40);
    note_server::runner::sweep_once(&w.state);

    assert!(w.push.seen().is_empty());
    assert_eq!(get(&w, "/api/sessions/open").await.1["phase"], "work");
}

/// A block holding `task`, already due, on today's plan with nothing else on it.
async fn due_block(w: &World, title: &str, notify: &str) -> i64 {
    let (status, task) = post(
        w,
        "/api/tasks",
        &format!(r#"{{"title":"{title}","notify":"{notify}"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{task}");
    let task_id = task["id"].as_i64().unwrap();
    let now = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC);
    let start = format!("{:02}:{:02}", now.hour(), now.minute());
    let conn = w.state.db.lock().unwrap();
    let plan_id = note_server::plan::ensure(
        &conn,
        w.state.config_dir.as_path(),
        "aki",
        1,
        now.date(),
    )
    .unwrap();
    conn.execute("DELETE FROM events WHERE plan_id = ?1 AND end_wall_time IS NULL", [plan_id])
        .unwrap();
    conn.execute(
        "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, end_wall_time, alert,
                             flexibility, channel)
         VALUES (?1, ?2, ?3, ?3, '23:59', 0, 'fixed', 'push')",
        (plan_id, title, &start),
    )
    .unwrap();
    let event_id = conn.last_insert_rowid();
    conn.execute("INSERT INTO event_tasks (event_id, task_id) VALUES (?1, ?2)", (event_id, task_id))
        .unwrap();
    event_id
}

#[tokio::test]
async fn a_block_announces_itself_the_way_its_task_asks() {
    let w = world(Vec::new()).await;
    let loud = due_block(&w, "read the chapter", "notify").await;
    let quiet = due_block(&w, "tidy the desk", "chat").await;
    let silent = due_block(&w, "water the plants", "none").await;

    note_server::runner::sweep_once(&w.state);

    let seen = w.push.seen();
    assert_eq!(seen.len(), 1, "only the task that asked for a notification gets one");
    assert_eq!(seen[0].1.title, "Starting now");
    assert_eq!(seen[0].1.body, "read the chapter · until 23:59");
    assert_eq!(seen[0].1.event_id, Some(loud));
    assert_eq!(
        seen[0].1.actions.iter().map(|a| (a.label.as_str(), a.data.as_str())).collect::<Vec<_>>(),
        vec![("Start session", format!("block:start:{loud}").as_str())]
    );

    let lines: Vec<String> = rows(
        &w,
        "SELECT m.content FROM talk_messages m JOIN conversations c ON c.id = m.conversation_id
         WHERE c.checkin_date IS NOT NULL",
    );
    assert_eq!(lines, vec!["Starting now: tidy the desk, until 23:59."]);

    let statuses: Vec<String> = rows(
        &w,
        &format!("SELECT status FROM events WHERE id IN ({loud}, {quiet}, {silent}) ORDER BY id"),
    );
    assert_eq!(statuses, vec!["fired", "fired", "pending"], "a silent block is left where it was");

    note_server::runner::sweep_once(&w.state);
    assert_eq!(w.push.seen().len(), 1, "a block starts once");
}
