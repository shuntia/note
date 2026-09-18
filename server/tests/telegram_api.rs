mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::providers::{mock::MockLLM, ChatResponse};
use note_server::AppState;
use std::sync::Arc;
use tower::ServiceExt;

async fn json(res: axum::response::Response) -> serde_json::Value {
    let body = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn says(texts: &[&str]) -> Arc<MockLLM> {
    Arc::new(MockLLM::scripted(
        texts.iter().map(|t| ChatResponse { text: (*t).into(), tool_calls: vec![] }).collect(),
    ))
}

fn updates(items: &[(i64, i64, &str)]) -> String {
    let list: Vec<serde_json::Value> = items
        .iter()
        .map(|(id, chat, text)| {
            serde_json::json!({
                "update_id": id,
                "message": {
                    "chat": { "id": chat },
                    "from": { "username": "aki_t" },
                    "text": text,
                },
            })
        })
        .collect();
    serde_json::json!({ "ok": true, "result": list }).to_string()
}

fn presses(items: &[(i64, i64, &str, &str)]) -> String {
    let list: Vec<serde_json::Value> = items
        .iter()
        .map(|(id, chat, callback_id, data)| {
            serde_json::json!({
                "update_id": id,
                "callback_query": {
                    "id": callback_id,
                    "from": { "username": "aki_t" },
                    "message": { "message_id": 90, "chat": { "id": chat }, "text": "Check-in" },
                    "data": data,
                },
            })
        })
        .collect();
    serde_json::json!({ "ok": true, "result": list }).to_string()
}

const ANSWERED: &str = r#"{"ok":true,"result":true}"#;

/// A plan of that date for `user_id`, holding one 09:00 routine.
fn routine(state: &AppState, user_id: i64, date: &str) -> i64 {
    let conn = state.db();
    conn.execute("INSERT INTO plans (user_id, date, created_at) VALUES (?1, ?2, 'c')", (user_id, date))
        .unwrap();
    let plan_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, flexibility, slide_window_min)
         VALUES (?1, 'checkin_call', '09:00', '09:00', 'slide', 60)",
        [plan_id],
    )
    .unwrap();
    conn.last_insert_rowid()
}

/// A 50-minute block holding a task of its own.
fn block(state: &AppState, user_id: i64, date: &str) -> i64 {
    let conn = state.db();
    conn.execute(
        "INSERT INTO tasks (user_id, title, created_at, updated_at)
         VALUES (?1, 'read the chapter', 'c', 'c')",
        [user_id],
    )
    .unwrap();
    let task_id = conn.last_insert_rowid();
    conn.execute("INSERT INTO plans (user_id, date, created_at) VALUES (?1, ?2, 'c')", (user_id, date))
        .unwrap();
    let plan_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, end_wall_time, span_min, alert)
         VALUES (?1, 'read the chapter', '14:00', '14:00', '14:50', 50, 0)",
        [plan_id],
    )
    .unwrap();
    let event_id = conn.last_insert_rowid();
    conn.execute("INSERT INTO event_tasks (event_id, task_id) VALUES (?1, ?2)", (event_id, task_id))
        .unwrap();
    event_id
}

fn status(state: &AppState, event_id: i64) -> (String, String) {
    let conn = state.db();
    conn.query_row("SELECT status, wall_time FROM events WHERE id = ?1", [event_id], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
    .unwrap()
}

fn actions_logged(state: &AppState) -> Vec<String> {
    let conn = state.db();
    let mut stmt = conn
        .prepare("SELECT detail FROM event_log WHERE kind = 'telegram_action' ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
}

fn link(state: &AppState, chat_id: i64) {
    let conn = state.db();
    conn.execute(
        "INSERT INTO telegram_links (user_id, chat_id, handle, linked_at)
         VALUES (1, ?1, 'aki_t', '2026-09-17T09:00:00Z')",
        [chat_id],
    )
    .unwrap();
}

async fn settings(app: &axum::Router, cookie: &str) -> serde_json::Value {
    let res = app
        .clone()
        .oneshot(
            Request::get("/api/settings").header(header::COOKIE, cookie).body(Body::empty()).unwrap(),
        )
        .await
        .unwrap();
    json(res).await
}

async fn conversations(app: &axum::Router, cookie: &str) -> serde_json::Value {
    let res = app
        .clone()
        .oneshot(
            Request::get("/api/conversations")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    json(res).await
}

fn rows(state: &AppState, conversation_id: i64) -> Vec<(String, String)> {
    let conn = state.db();
    let mut stmt = conn
        .prepare("SELECT role, content FROM talk_messages WHERE conversation_id = ?1 ORDER BY id")
        .unwrap();
    stmt.query_map([conversation_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[tokio::test]
async fn a_start_code_links_the_chat_and_settings_follows() {
    let fake = common::fake_telegram();
    fake.answer(common::GET_ME);
    let (app, cookie, state, _cfg) = common::app_with_telegram(says(&[]), &fake.base).await;
    assert_eq!(fake.call().0, "getMe");

    let before = settings(&app, &cookie).await;
    assert_eq!(before["telegram_enabled"], true);
    assert_eq!(before["telegram_linked"], false);
    assert_eq!(before["telegram_bot"], "note_bot");

    let res = app
        .clone()
        .oneshot(
            Request::post("/api/telegram/link")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let issued = json(res).await;
    let code = issued["code"].as_str().unwrap().to_string();
    assert_eq!(issued["bot"], "note_bot");
    assert_eq!(issued["url"], format!("https://t.me/note_bot?start={code}"));

    fake.answer(&updates(&[(11, 42, &format!("/start {code}"))]));
    fake.answer(common::SENT);
    let mut chats = note_server::telegram::Chats::default();
    note_server::telegram::poll_once(&state, &mut chats).await.unwrap();
    assert_eq!(fake.call().0, "getUpdates");
    let (method, sent) = fake.call();
    assert_eq!(method, "sendMessage");
    assert_eq!(sent["chat_id"], 42);
    assert_eq!(sent["text"], "Linked to Note as X");

    assert_eq!(settings(&app, &cookie).await["telegram_linked"], true);
    {
        let conn = state.db();
        assert_eq!(note_server::telegram::cursor(&conn).unwrap(), 11);
        let live: i64 = conn
            .query_row("SELECT COUNT(*) FROM telegram_link_codes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(live, 0, "the code is spent");
    }

    let res = app
        .clone()
        .oneshot(
            Request::delete("/api/telegram/link")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(settings(&app, &cookie).await["telegram_linked"], false);
}

#[tokio::test]
async fn an_unknown_chat_is_turned_away_once_an_hour() {
    let fake = common::fake_telegram();
    fake.answer(common::GET_ME);
    let (_app, _cookie, state, _cfg) = common::app_with_telegram(says(&[]), &fake.base).await;
    fake.call();

    fake.answer(&updates(&[(11, 99, "hello?"), (12, 99, "anyone?"), (13, 77, "hello?")]));
    fake.answer(common::SENT);
    fake.answer(common::SENT);
    let mut chats = note_server::telegram::Chats::default();
    note_server::telegram::poll_once(&state, &mut chats).await.unwrap();
    assert_eq!(fake.call().0, "getUpdates");

    let (_, first) = fake.call();
    assert_eq!(first["chat_id"], 99);
    assert_eq!(first["text"], "This bot is private. Link it from Note's settings.");
    let (_, other) = fake.call();
    assert_eq!(other["chat_id"], 77, "a different chat is answered in its turn");
    assert!(fake.silent(), "the second message from the same chat says nothing");

    let conn = state.db();
    let threads: i64 =
        conn.query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0)).unwrap();
    assert_eq!(threads, 0, "a stranger opens nothing");
}

#[tokio::test]
async fn a_message_runs_a_turn_that_the_web_holds_the_record_of() {
    let fake = common::fake_telegram();
    fake.answer(common::GET_ME);
    let (app, cookie, state, _cfg) =
        common::app_with_telegram(says(&["the day is yours"]), &fake.base).await;
    fake.call();
    link(&state, 42);
    let (_conn_id, mut frames) = state.hub.register(1).unwrap();

    fake.answer(&updates(&[(11, 42, "how does today look?")]));
    fake.answer(common::SENT);
    let mut chats = note_server::telegram::Chats::default();
    note_server::telegram::poll_once(&state, &mut chats).await.unwrap();
    fake.call();

    let (method, sent) = fake.call();
    assert_eq!(method, "sendMessage");
    assert_eq!(sent["chat_id"], 42);
    assert_eq!(sent["text"], "the day is yours");

    let list = conversations(&app, &cookie).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["title"], "how does today look?");
    assert_eq!(list[0]["via"], "telegram");
    let id = list[0]["id"].as_i64().unwrap();
    assert_eq!(
        rows(&state, id),
        vec![
            ("user".to_string(), "how does today look?".to_string()),
            ("assistant".to_string(), "the day is yours".to_string()),
        ]
    );
    {
        let conn = state.db();
        let at: Option<String> = conn
            .query_row("SELECT telegram_at FROM conversations WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        assert!(at.is_some(), "the thread carries no telegram stamp");
    }

    let frame: serde_json::Value =
        serde_json::from_str(&frames.recv().await.expect("a session frame")).unwrap();
    assert_eq!(frame["type"], "agent");
    assert_eq!(frame["conversation_id"], serde_json::Value::Null);
}

#[tokio::test]
async fn the_window_continues_a_thread_and_slash_new_opens_another() {
    let fake = common::fake_telegram();
    fake.answer(common::GET_ME);
    let (app, cookie, state, _cfg) =
        common::app_with_telegram(says(&["one", "two", "three"]), &fake.base).await;
    fake.call();
    link(&state, 42);
    let mut chats = note_server::telegram::Chats::default();

    for (id, text) in [(11, "morning"), (12, "still here")] {
        fake.answer(&updates(&[(id, 42, text)]));
        fake.answer(common::SENT);
        note_server::telegram::poll_once(&state, &mut chats).await.unwrap();
        fake.call();
        fake.call();
    }
    let list = conversations(&app, &cookie).await;
    assert_eq!(list.as_array().unwrap().len(), 1, "the window kept one thread");
    let first = list[0]["id"].as_i64().unwrap();
    assert_eq!(rows(&state, first).len(), 4);

    fake.answer(&updates(&[(13, 42, "/new")]));
    fake.answer(common::SENT);
    note_server::telegram::poll_once(&state, &mut chats).await.unwrap();
    fake.call();
    let (_, said) = fake.call();
    assert_eq!(said["text"], "Fresh start.");

    fake.answer(&updates(&[(14, 42, "something else")]));
    fake.answer(common::SENT);
    note_server::telegram::poll_once(&state, &mut chats).await.unwrap();
    fake.call();
    fake.call();

    let list = conversations(&app, &cookie).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(list[0]["title"], "something else");
    assert_ne!(list[0]["id"].as_i64().unwrap(), first);
    {
        let conn = state.db();
        assert_eq!(note_server::telegram::cursor(&conn).unwrap(), 14);
    }
}

#[tokio::test]
async fn a_web_reply_to_a_telegram_thread_is_mirrored_and_hands_it_back() {
    let fake = common::fake_telegram();
    fake.answer(common::GET_ME);
    let (app, cookie, state, _cfg) =
        common::app_with_telegram(says(&["from the chat", "from the app", "again"]), &fake.base)
            .await;
    fake.call();
    link(&state, 42);
    let mut chats = note_server::telegram::Chats::default();

    fake.answer(&updates(&[(11, 42, "morning")]));
    fake.answer(common::SENT);
    note_server::telegram::poll_once(&state, &mut chats).await.unwrap();
    fake.call();
    fake.call();
    let id = conversations(&app, &cookie).await[0]["id"].as_i64().unwrap();

    fake.answer(common::SENT);
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"message":"back at my desk","conversation_id":{id}}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let (method, mirrored) = fake.call();
    assert_eq!(method, "sendMessage");
    assert_eq!(mirrored["chat_id"], 42);
    assert_eq!(mirrored["text"], "from the app");

    let list = conversations(&app, &cookie).await;
    assert_eq!(list[0]["via"], "web", "the thread follows the user back");

    let res = app
        .clone()
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"message":"and again","conversation_id":{id}}}"#)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(fake.silent(), "a thread the user is holding on the web stays there");
}

#[tokio::test]
async fn a_server_with_no_bot_says_so_and_issues_no_code() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let s = settings(&app, &cookie).await;
    assert_eq!(s["telegram_enabled"], false);
    assert_eq!(s["telegram_linked"], false);
    assert_eq!(s["telegram_bot"], "");

    let res = app
        .clone()
        .oneshot(
            Request::post("/api/telegram/link")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn each_button_settles_what_it_names_and_writes_it_back_onto_the_message() {
    let fake = common::fake_telegram();
    fake.answer(common::GET_ME);
    let (_app, _cookie, state, _cfg) = common::app_with_telegram(says(&[]), &fake.base).await;
    fake.call();
    link(&state, 42);
    let (_conn_id, mut frames) = state.hub.register(1).unwrap();
    let mut chats = note_server::telegram::Chats::default();

    let done = routine(&state, 1, "2026-01-05");
    let snoozed = routine(&state, 1, "2026-01-06");
    let dropped = routine(&state, 1, "2026-01-07");

    for (update_id, event_id, data, toast) in [
        (21, done, format!("ev:done:{done}"), "Done"),
        (22, snoozed, format!("ev:snooze:{snoozed}:15"), "Snoozed 15 min"),
        (23, dropped, format!("ev:drop:{dropped}"), "Dropped"),
    ] {
        fake.answer(&presses(&[(update_id, 42, "q1", &data)]));
        for _ in 0..3 {
            fake.answer(ANSWERED);
        }
        note_server::telegram::poll_once(&state, &mut chats).await.unwrap();
        assert_eq!(fake.call().0, "getUpdates");

        let (method, answered) = fake.call();
        assert_eq!(method, "answerCallbackQuery");
        assert_eq!(answered["callback_query_id"], "q1");
        assert_eq!(answered["text"], toast);

        let (method, cleared) = fake.call();
        assert_eq!(method, "editMessageReplyMarkup");
        assert_eq!(cleared["chat_id"], 42);
        assert_eq!(cleared["message_id"], 90);
        assert!(cleared.get("reply_markup").is_none());

        let (method, edited) = fake.call();
        assert_eq!(method, "editMessageText");
        assert_eq!(edited["text"], format!("Check-in\n✓ {toast}"));

        let frame: serde_json::Value =
            serde_json::from_str(&frames.recv().await.expect("a changed frame")).unwrap();
        assert_eq!(frame["type"], "changed");
        assert!(status(&state, event_id).0 != "pending", "the event was not settled");
    }

    assert_eq!(status(&state, done).0, "done");
    assert_eq!(status(&state, snoozed), ("snoozed".to_string(), "09:15".to_string()));
    assert_eq!(status(&state, dropped).0, "dropped");
    assert_eq!(
        actions_logged(&state),
        vec![
            format!("ev:done:{done}: Done"),
            format!("ev:snooze:{snoozed}:15: Snoozed 15 min"),
            format!("ev:drop:{dropped}: Dropped"),
        ]
    );
    {
        let conn = state.db();
        assert_eq!(note_server::telegram::cursor(&conn).unwrap(), 23);
    }
}

#[tokio::test]
async fn start_session_opens_the_block_it_was_sent_with() {
    let fake = common::fake_telegram();
    fake.answer(common::GET_ME);
    let (_app, _cookie, state, _cfg) = common::app_with_telegram(says(&[]), &fake.base).await;
    fake.call();
    link(&state, 42);
    let event_id = block(&state, 1, "2026-01-05");

    fake.answer(&presses(&[(21, 42, "q1", &format!("block:start:{event_id}"))]));
    for _ in 0..3 {
        fake.answer(ANSWERED);
    }
    let mut chats = note_server::telegram::Chats::default();
    note_server::telegram::poll_once(&state, &mut chats).await.unwrap();
    fake.call();
    assert_eq!(fake.call().1["text"], "Session started");
    fake.call();
    assert_eq!(fake.call().1["text"], "Check-in\n✓ Session started");

    let conn = state.db();
    let (title, planned, event): (String, Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT title, planned_min, event_id FROM work_sessions
             WHERE user_id = 1 AND ended_at IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(title, "read the chapter");
    assert_eq!(planned, Some(50), "the session runs as long as the block");
    assert_eq!(event, Some(event_id));
}

#[tokio::test]
async fn a_button_for_someone_elses_day_is_refused_and_disarmed() {
    let fake = common::fake_telegram();
    fake.answer(common::GET_ME);
    let (_app, _cookie, state, _cfg) = common::app_with_telegram(says(&[]), &fake.base).await;
    fake.call();
    link(&state, 42);
    let theirs = {
        let conn = state.db();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('bo', 'x', 'member')",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    };
    let foreign = routine(&state, theirs, "2026-01-05");

    let mut chats = note_server::telegram::Chats::default();
    for (update_id, data) in [
        (21, format!("ev:done:{foreign}")),
        (22, "ev:done:9999".to_string()),
        (23, "carry:2026-01-05".to_string()),
        (24, "nonsense".to_string()),
    ] {
        fake.answer(&presses(&[(update_id, 42, "q1", &data)]));
        fake.answer(ANSWERED);
        fake.answer(ANSWERED);
        note_server::telegram::poll_once(&state, &mut chats).await.unwrap();
        fake.call();
        let (method, answered) = fake.call();
        assert_eq!(method, "answerCallbackQuery");
        assert_eq!(answered["text"], "That one is gone.", "data: {data}");
        assert_eq!(fake.call().0, "editMessageReplyMarkup");
        assert!(fake.silent(), "a refusal writes nothing onto the message");
    }

    assert_eq!(status(&state, foreign).0, "pending");
    assert!(actions_logged(&state).is_empty());
}

#[tokio::test]
async fn a_press_from_a_chat_note_does_not_know_is_turned_away() {
    let fake = common::fake_telegram();
    fake.answer(common::GET_ME);
    let (_app, _cookie, state, _cfg) = common::app_with_telegram(says(&[]), &fake.base).await;
    fake.call();
    let event_id = routine(&state, 1, "2026-01-05");

    fake.answer(&presses(&[(21, 99, "q1", &format!("ev:done:{event_id}"))]));
    fake.answer(ANSWERED);
    fake.answer(ANSWERED);
    let mut chats = note_server::telegram::Chats::default();
    note_server::telegram::poll_once(&state, &mut chats).await.unwrap();
    fake.call();
    assert_eq!(fake.call().1["text"], "That one is gone.");
    assert_eq!(fake.call().0, "editMessageReplyMarkup");
    assert_eq!(status(&state, event_id).0, "pending");
}

#[tokio::test]
async fn a_checkin_carries_its_three_buttons_into_the_chat() {
    let fake = common::fake_telegram();
    fake.answer(common::GET_ME);
    let (_app, _cookie, state, _cfg) = common::app_with_telegram(says(&[]), &fake.base).await;
    fake.call();
    link(&state, 42);

    fake.answer(common::SENT);
    let event_id = routine(&state, 1, "2026-01-05");
    let ev = note_server::runner::FiredEvent {
        event_id,
        user_id: 1,
        username: "aki".into(),
        kind: "checkin_call".into(),
        wall_time: "09:00".into(),
        date: "2026-01-05".into(),
        channel: "push".into(),
        message: String::new(),
    };
    let db = state.db.clone();
    let ladder = state.channels.clone();
    tokio::task::spawn_blocking(move || {
        note_server::channels::deliver_event(&db, &ladder, &ev);
    })
    .await
    .unwrap();

    let (method, sent) = fake.call();
    assert_eq!(method, "sendMessage");
    assert_eq!(
        sent["reply_markup"],
        serde_json::json!({ "inline_keyboard": [[
            { "text": "Done", "callback_data": format!("ev:done:{event_id}") },
            { "text": "Snooze 15", "callback_data": format!("ev:snooze:{event_id}:15") },
            { "text": "Drop", "callback_data": format!("ev:drop:{event_id}") },
        ]] })
    );
}
