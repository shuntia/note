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
    // The day's own routines stay out of these counts: the template lays nothing.
    std::fs::create_dir_all(dir.join("users/aki/templates")).unwrap();
    std::fs::write(dir.join("users/aki/templates/default.toml"), "events = []\n").unwrap();
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

/// Brings every waiting trigger forward to the top of today, so one sweep
/// fires it whatever time the suite runs at: an offset laid late in the day
/// lands on tomorrow's plan, so the plan moves as well as the time.
fn make_due(w: &World) {
    let conn = w.state.db.lock().unwrap();
    let today = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
    let plan = note_server::plan::ensure(&conn, &w.state.config_dir, "aki", 1, today).unwrap();
    conn.execute(
        "UPDATE events SET plan_id = ?1, wall_time = '00:00' WHERE kind = 'trigger'",
        [plan],
    )
    .unwrap();
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
        call("c1", "trigger_set", r#"{"at":"+30min","prompt":"still on the chapter?","cancel_on":"reply","session_check":true}"#),
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
async fn a_reminder_laid_inside_a_work_session_stays_on_the_day() {
    let w = world(vec![
        call("c1", "trigger_set", r#"{"at":"+90min","prompt":"remind them to hand it in","cancel_on":"reply"}"#),
        call("c2", "say", r#"{"text":"half an hour in — how far did you get?"}"#),
    ])
    .await;
    start_session(&w).await;
    make_due(&w);

    note_server::runner::sweep_once(&w.state);

    let laid: Vec<Option<i64>> = rows(
        &w,
        "SELECT work_session_id FROM events WHERE kind = 'trigger' AND cancel_if = 'replied'",
    );
    assert_eq!(laid, vec![None], "only a session check belongs to the session");
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
                session_check: false,
                system: false,
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
    std::fs::write(w.state.config_dir.join("users/aki/user.toml"), "triggers_per_day = 4\n").unwrap();

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
    let w = world(vec![ChatResponse { text: String::new(), tool_calls: vec![] }]).await;
    start_session(&w).await;
    make_due(&w);

    note_server::runner::sweep_once(&w.state);

    assert!(w.push.seen().is_empty());
    assert_eq!(statuses(&w), vec!["fired"]);
    assert_eq!(logged(&w, "trigger_error").len(), 1);
}

/// Lays today's day-laying trigger as the sweep would have, ready to fire.
fn lay_the_day(w: &World) {
    std::fs::write(w.state.config_dir.join("users/aki/user.toml"), "day_start = \"00:00\"\n").unwrap();
    let conn = w.state.db.lock().unwrap();
    let noon = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .at(12, 0, 0, 0)
        .to_zoned(jiff::tz::TimeZone::UTC)
        .unwrap()
        .timestamp();
    assert_eq!(note_server::wake::check(&conn, &w.state.config_dir, noon).unwrap().len(), 1);
    conn.execute("UPDATE events SET wall_time = '00:00' WHERE origin = 'lay_day'", []).unwrap();
}

#[tokio::test]
async fn the_first_wake_up_lays_the_rest_of_the_day_and_logs_it() {
    let w = world(vec![
        call("c1", "trigger_set", r#"{"at":"+60min","prompt":"after the essay block: is it moving?"}"#),
        call("c2", "stay_quiet", r#"{"reason":"the day is laid"}"#),
    ])
    .await;
    lay_the_day(&w);

    note_server::runner::sweep_once(&w.state);

    assert!(w.push.seen().is_empty(), "laying the day says nothing");
    let wake_ups: Vec<String> = rows(&w, "SELECT origin FROM events WHERE kind = 'trigger' ORDER BY id");
    assert_eq!(wake_ups, ["lay_day", "agent"]);
    assert_eq!(statuses(&w)[0], "done");
    let changes = logged(&w, "wake_change");
    assert_eq!(changes.len(), 1);
    assert!(changes[0].contains("trigger_set"), "{}", changes[0]);
}

/// Answers its script, then fails every round after it.
struct FailsAfter(MockLLM);

impl note_server::providers::LLMProvider for FailsAfter {
    fn chat(&self, req: &note_server::providers::ChatRequest) -> anyhow::Result<ChatResponse> {
        let resp = self.0.chat(req)?;
        anyhow::ensure!(!resp.tool_calls.is_empty(), "the provider is down");
        Ok(resp)
    }
}

#[tokio::test]
async fn a_wake_up_that_fails_after_a_write_still_logs_the_write() {
    let w = world(Vec::new()).await;
    lay_plain(&w);
    let llm = Arc::new(FailsAfter(MockLLM::scripted(vec![call(
        "c1",
        "trigger_set",
        r#"{"at":"+60min","prompt":"see if the essay moved"}"#,
    )])));
    let state = w.state.clone().with_providers(llm, None);

    note_server::runner::sweep_once(&state);

    assert_eq!(logged(&w, "trigger_error").len(), 1);
    let changes = logged(&w, "wake_change");
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert!(changes[0].starts_with("event 1: trigger_set"), "{}", changes[0]);
}

/// Each ring's user, conversation and reason.
type Rang = (i64, Option<i64>, Option<String>);

struct LiveWeb(std::sync::Mutex<Vec<Rang>>);

impl note_server::channels::WebCalls for LiveWeb {
    fn has_live(&self, _: i64) -> bool {
        true
    }
    fn ring(&self, user_id: i64, msg: &note_server::channels::OutboundMessage, _: Option<&str>) -> bool {
        self.0.lock().unwrap().push((user_id, msg.conversation_id, msg.reason.clone()));
        true
    }
}

fn lay_plain(w: &World) {
    let conn = w.state.db.lock().unwrap();
    let today = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
    let plan = note_server::plan::ensure(&conn, &w.state.config_dir, "aki", 1, today).unwrap();
    note_server::triggers::insert(&conn, plan, "00:00", "see how the essay is going", "agent", None, None, None,
        jiff::Timestamp::now()).unwrap();
}

fn tool_results(w: &World, round: usize) -> Vec<String> {
    w.llm.seen()[round]
        .messages
        .iter()
        .filter_map(|m| match m {
            note_server::providers::Message::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect()
}

fn wait_results(w: &World, round: usize) -> serde_json::Value {
    let results = tool_results(w, round);
    serde_json::from_str(results.last().expect("the resumed round answers the waiting say")).unwrap()
}

const WAIT: &str = r#"{"text":"start the essay now?","await_min":15}"#;

#[tokio::test]
async fn a_waiting_say_resumes_its_session_when_the_wait_runs_out() {
    let w = world(vec![call("c1", "say", WAIT), call("c2", "stay_quiet", r#"{"reason":"next wake-up calls"}"#)]).await;
    lay_plain(&w);
    note_server::runner::sweep_once(&w.state);
    assert_eq!(w.push.seen()[0].1.body, "start the essay now?");
    assert_eq!(statuses(&w), ["done", "pending"]);
    note_server::runner::sweep_once(&w.state);
    assert_eq!(w.llm.seen().len(), 1, "nothing resumes before the wait is over");

    make_due(&w);
    note_server::runner::sweep_once(&w.state);
    assert_eq!(wait_results(&w, 1), serde_json::json!({ "result": "Timed out" }));
    assert_eq!(statuses(&w), ["done", "done"]);
    assert!(rows::<i64>(&w, "SELECT event_id FROM paused_sessions").is_empty());
    assert_eq!(logged(&w, "trigger_quiet"), ["event 2: next wake-up calls"]);
}

#[tokio::test]
async fn an_answer_ends_the_wait_and_the_session_goes_on_in_the_same_thread() {
    let w = world(vec![call("c1", "say", WAIT), call("c2", "say", r#"{"text":"good, the intro first"}"#)]).await;
    lay_plain(&w);
    note_server::runner::sweep_once(&w.state);
    let thread = w.push.seen()[0].1.conversation_id.unwrap();
    {
        let conn = w.state.db.lock().unwrap();
        let chat = note_server::talk::create(&conn, 1, "chat", jiff::Timestamp::now()).unwrap();
        note_server::talk::append_text(&conn, chat, "user", "ok starting", jiff::Timestamp::now()).unwrap();
    }

    note_server::runner::sweep_once(&w.state);
    assert_eq!(
        wait_results(&w, 1),
        serde_json::json!({ "result": "User responded before timeout", "replies": ["ok starting"] })
    );
    let seen = w.push.seen();
    assert_eq!(seen[1].1.body, "good, the intro first");
    assert_eq!(seen[1].1.conversation_id, Some(thread), "the session goes on where it spoke");
}

const CHECK: &str = r#"{"reason":"talk the essay through"}"#;
const CONFIRM: &str = r#"{"token":"$token","opening":"Want to talk the essay through?","brief":"essay due friday; intro done"}"#;

#[tokio::test]
async fn a_user_outside_their_day_is_written_to_not_rung() {
    let w = world(vec![
        call("c1", "call_user", CHECK),
        call("c2", "say", r#"{"text":"the essay can wait till you're back"}"#),
    ])
    .await;
    lay_plain(&w);
    std::fs::write(w.state.config_dir.join("users/aki/user.toml"), "close_day_time = \"00:00\"\n").unwrap();
    let web = Arc::new(LiveWeb(std::sync::Mutex::default()));
    let state = w.state.clone().with_web_calls(web.clone());

    note_server::runner::sweep_once(&state);

    assert!(web.0.lock().unwrap().is_empty());
    assert_eq!(w.push.seen()[0].1.body, "the essay can wait till you're back");
    assert!(logged(&w, "trigger_rang").is_empty());
    let refused = tool_results(&w, 1);
    assert!(refused.iter().any(|r| r.contains(r#""kind":"busy""#)), "{refused:?}");
}

#[tokio::test]
async fn a_free_user_with_the_app_open_gets_the_call_there() {
    let w = world(vec![call("c1", "call_user", CHECK), call("c2", "call_user", CONFIRM)]).await;
    lay_plain(&w);
    std::fs::write(w.state.config_dir.join("users/aki/user.toml"), "day_start = \"\"\nclose_day_time = \"\"\n").unwrap();
    let web = Arc::new(LiveWeb(std::sync::Mutex::default()));
    let state = w.state.clone().with_web_calls(web.clone());

    note_server::runner::sweep_once(&state);

    let rang = web.0.lock().unwrap().clone();
    assert_eq!(rang.len(), 1);
    assert!(rang[0].1.is_some(), "the call joins the thread the words landed in");
    assert_eq!(rang[0].2.as_deref(), Some("talk the essay through"), "the call knows why it rang");
    assert!(w.push.seen().is_empty(), "a call is not also a notification");
    assert_eq!(logged(&w, "trigger_rang"), ["event 1: web: talk the essay through"]);
    assert_eq!(logged(&w, "trigger_said"), ["event 1: Want to talk the essay through?"]);
    assert_eq!(statuses(&w), ["done"]);
    let checked = tool_results(&w, 1);
    assert!(checked[0].contains("anticipate what it will cover"), "{checked:?}");
}

#[tokio::test]
async fn a_check_alone_rings_nothing_and_leaves_the_session_open() {
    let w = world(vec![
        call("c1", "call_user", CHECK),
        call("c2", "stay_quiet", r#"{"reason":"a message will do"}"#),
    ])
    .await;
    lay_plain(&w);
    note_server::presence::touch(&w.state.db.lock().unwrap(), 1, jiff::Timestamp::now()).unwrap();
    let web = Arc::new(LiveWeb(std::sync::Mutex::default()));
    let state = w.state.clone().with_web_calls(web.clone());

    note_server::runner::sweep_once(&state);

    assert!(web.0.lock().unwrap().is_empty());
    assert_eq!(w.llm.seen().len(), 2, "the check did not end the session");
    assert_eq!(logged(&w, "trigger_quiet"), ["event 1: a message will do"]);
}

#[tokio::test]
async fn a_call_the_user_asked_for_rings_while_they_are_away_and_logs_their_words() {
    let w = world(vec![
        call("c1", "call_user", r#"{"reason":"the 11:00 call","asked":true,"quote":"call me at 11 tomorrow"}"#),
        call("c2", "call_user", r#"{"token":"$token","opening":"It's eleven, as you asked.","brief":"the 11:00 bus"}"#),
    ])
    .await;
    lay_plain(&w);
    {
        let conn = w.state.db.lock().unwrap();
        let now = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1);
        let thread = note_server::talk::create(&conn, 1, "chat", now).unwrap();
        note_server::talk::append_text(&conn, thread, "user", "Call me at 11 tomorrow, please", now).unwrap();
        conn.execute("UPDATE events SET conversation_id = ?1 WHERE kind = 'trigger'", [thread]).unwrap();
    }
    w.state.db.lock().unwrap()
        .execute("UPDATE users SET last_active_at = '2000-01-01T00:00:00Z'", []).unwrap();
    let web = Arc::new(LiveWeb(std::sync::Mutex::default()));
    let state = w.state.clone().with_web_calls(web.clone());

    note_server::runner::sweep_once(&state);

    assert_eq!(web.0.lock().unwrap().len(), 1);
    assert_eq!(
        logged(&w, "trigger_rang"),
        ["event 1: web: the 11:00 call; asked: \"call me at 11 tomorrow\""]
    );
}

#[tokio::test]
async fn a_chat_rings_now_and_opens_in_the_check_in_thread() {
    let w = world(vec![
        call("c1", "call_user", r#"{"reason":"they asked for a call now","asked":true,"quote":"call me now"}"#),
        call("c2", "call_user", r#"{"token":"$token","opening":"Hi, you wanted a call?","brief":"nothing pressing"}"#),
        ChatResponse { text: "Ringing you now.".into(), tool_calls: vec![] },
    ])
    .await;
    let web = Arc::new(LiveWeb(std::sync::Mutex::default()));
    let app = api::router(w.state.clone().with_web_calls(web.clone()));
    let req = Request::post("/api/talk")
        .header(header::COOKIE, &w.cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"message":"call me now"}"#))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let rang = web.0.lock().unwrap().clone();
    assert_eq!(rang.len(), 1);
    assert_eq!(rang[0].2.as_deref(), Some("they asked for a call now"));
    let thread = rang[0].1.expect("the opening lands in a thread");
    let opening: Vec<String> = rows(
        &w,
        &format!("SELECT content FROM talk_messages WHERE conversation_id = {thread} AND role = 'assistant'"),
    );
    assert_eq!(opening, ["Hi, you wanted a call?"]);
    assert_eq!(logged(&w, "call_user_rang"), ["talk: web: they asked for a call now; asked: \"call me now\""]);
}

#[tokio::test]
async fn the_trigger_log_shows_the_owners_triggers_and_how_they_ended() {
    let w = world(vec![call("c1", "say", r#"{"text":"how is the chapter going?"}"#)]).await;
    start_session(&w).await;
    make_due(&w);
    note_server::runner::sweep_once(&w.state);
    lay_plain(&w);

    let (status, body) = get(&w, "/api/triggers").await;
    assert_eq!(status, StatusCode::OK);
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 2, "{body}");
    assert_eq!((events[0]["outcome"].as_str(), events[0]["text"].as_str()), (Some("said"), Some("how is the chapter going?")));
    assert_eq!(events[1]["status"], "pending");
    assert_eq!(events[1]["prompt"], "see how the essay is going");
    assert!(events[1]["outcome"].is_null());

    auth::create_user(&w.state.db.lock().unwrap(), "bo", "pw", false).unwrap();
    let theirs = common::login(&w.app, "bo", "pw").await;
    let req = Request::get("/api/triggers").header(header::COOKIE, &theirs).body(Body::empty()).unwrap();
    let res = w.app.clone().oneshot(req).await.unwrap();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let other: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(other["events"], serde_json::json!([]), "another account sees none of them");

    let res = w.app.clone().oneshot(Request::get("/api/triggers").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_wake_up_that_ends_in_plain_text_still_speaks() {
    let w = world(vec![ChatResponse { text: "Back to the chapter?".into(), tool_calls: vec![] }]).await;
    start_session(&w).await;
    make_due(&w);

    note_server::runner::sweep_once(&w.state);

    let seen = w.push.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1.body, "Back to the chapter?");
    assert_eq!(statuses(&w), vec!["done"]);
    assert_eq!(logged(&w, "trigger_said").len(), 1);
}

#[tokio::test]
async fn past_the_daily_session_cap_a_wake_up_is_dropped_unrun() {
    let mut w = world(vec![call("c1", "say", r#"{"text":"never sent"}"#)]).await;
    start_session(&w).await;
    make_due(&w);
    w.state.agent_sessions_per_day = 1;
    note_server::log::record(&w.state.db.lock().unwrap(), Some(1), "agent_session", "earlier").unwrap();

    note_server::runner::sweep_once(&w.state);

    assert!(w.push.seen().is_empty());
    assert_eq!(statuses(&w), vec!["dropped"]);
    assert_eq!(logged(&w, "session_capped"), vec!["wake-up"]);
    assert!(logged(&w, "trigger_said").is_empty());
}
