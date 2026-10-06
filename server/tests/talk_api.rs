mod common;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::providers::{mock::MockLLM, ChatResponse, ToolCall};
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn talk_runs_a_session_and_returns_the_reply() {
    let llm = Arc::new(MockLLM::scripted(vec![
        ChatResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "task_create".into(),
                args: r#"{"title":"call mom"}"#.into(),
            }],
        },
        ChatResponse { text: "done — added call mom".into(), tool_calls: vec![] },
        ChatResponse { text: "anything else?".into(), tool_calls: vec![] },
    ]));
    let (app, cookie, _cfg) = common::app_with_logged_in_user_and_llm(llm).await;

    let res = app
        .clone()
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"remind me to call mom"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["reply"], "done — added call mom");

    // the finished session must have released its gate slot
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"thanks"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .clone()
        .oneshot(
            Request::get("/api/tasks")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v[0]["title"], "call mom");
}

#[tokio::test]
async fn empty_message_is_400_and_no_cookie_is_401() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"   "}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["error"].as_str().unwrap().contains("16384"));

    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn concurrent_talk_for_same_user_is_conflict() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let user_id: i64 = {
        let conn = state.db.lock().unwrap();
        conn.query_row("SELECT id FROM users LIMIT 1", [], |r| r.get(0)).unwrap()
    };
    let _permit = state.talk_gate.clone().try_enter(user_id).unwrap();
    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"], "a reply is already in progress");
}

#[tokio::test]
async fn talk_at_global_capacity_is_service_unavailable() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let permits: Vec<_> = (0..note_server::MAX_CONCURRENT_TALKS)
        .map(|i| state.talk_gate.try_enter(-1 - i as i64).unwrap())
        .collect();
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(res.headers()[header::RETRY_AFTER], "5");

    // the rejected user was rolled back out of the active set
    drop(permits);
    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

struct FailingLLM;
impl note_server::providers::LLMProvider for FailingLLM {
    fn chat(
        &self,
        _: &note_server::providers::ChatRequest,
    ) -> anyhow::Result<note_server::providers::ChatResponse> {
        anyhow::bail!("provider down")
    }
}

#[tokio::test]
async fn provider_failure_is_bad_gateway_and_logged() {
    let (app, cookie, state, _cfg) =
        common::app_with_logged_in_user_llm_and_state(Arc::new(FailingLLM)).await;
    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["error"].as_str().unwrap().contains("unavailable"));

    let logged: i64 = {
        let conn = state.db.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM event_log WHERE kind='talk_error'", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(logged, 1);
}

#[tokio::test]
async fn oversized_message_is_bad_request_with_body() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let big = "a".repeat(16 * 1024 + 1);
    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({ "message": big }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["error"].as_str().unwrap().contains("16384"));
}

#[tokio::test]
async fn blank_reply_is_replaced_with_the_canned_line_and_persisted_as_such() {
    let llm =
        Arc::new(MockLLM::scripted(vec![ChatResponse { text: "   ".into(), tool_calls: vec![] }]));
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["reply"], note_server::text::empty_reply(note_server::text::Lang::En));

    let persisted: String = {
        let conn = state.db.lock().unwrap();
        conn.query_row(
            "SELECT content FROM talk_messages WHERE role = 'assistant'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(persisted, note_server::text::empty_reply(note_server::text::Lang::En));
}

#[tokio::test]
async fn talk_persists_the_exchange_and_surfaces_tool_steps() {
    type MsgRow = (String, String, Option<String>, Option<String>, bool);
    let llm = Arc::new(MockLLM::scripted(vec![
        ChatResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "task_create".into(),
                args: r#"{"title":"call mom"}"#.into(),
            }],
        },
        ChatResponse { text: "done — added call mom".into(), tool_calls: vec![] },
    ]));
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;

    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"remind me to call mom"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let conv_id = v["conversation_id"].as_i64().unwrap();
    assert_eq!(v["reply"], "done — added call mom");
    let steps = v["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0]["name"], "task_create");
    assert_eq!(steps[0]["args"], r#"{"title":"call mom"}"#);
    assert!(steps[0]["result"].as_str().unwrap().contains("task_id"));
    assert_eq!(steps[0]["is_error"], false);

    let conn = state.db.lock().unwrap();
    let title: String = conn
        .query_row("SELECT title FROM conversations WHERE id = ?1", [conv_id], |r| r.get(0))
        .unwrap();
    assert_eq!(title, "remind me to call mom");
    let mut stmt = conn
        .prepare(
            "SELECT role, content, tool_name, tool_args, is_error
             FROM talk_messages WHERE conversation_id = ?1 ORDER BY id",
        )
        .unwrap();
    let rows: Vec<MsgRow> = stmt
        .query_map([conv_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].0, "user");
    assert_eq!(rows[0].1, "remind me to call mom");
    assert!(rows[0].2.is_none());
    assert_eq!(rows[1].0, "tool");
    assert!(rows[1].1.contains("task_id"));
    assert_eq!(rows[1].2.as_deref(), Some("task_create"));
    assert_eq!(rows[1].3.as_deref(), Some(r#"{"title":"call mom"}"#));
    assert!(!rows[1].4);
    assert_eq!(rows[2].0, "assistant");
    assert_eq!(rows[2].1, "done — added call mom");
}

#[tokio::test]
async fn multiple_tool_calls_keep_call_order_in_steps_and_rows() {
    let llm = Arc::new(MockLLM::scripted(vec![
        ChatResponse {
            text: String::new(),
            tool_calls: vec![
                ToolCall {
                    id: "c1".into(),
                    name: "task_create".into(),
                    args: r#"{"title":"call mom"}"#.into(),
                },
                ToolCall { id: "c2".into(), name: "schedule_insert".into(), args: "{}".into() },
            ],
        },
        ChatResponse { text: "one worked, one didn't".into(), tool_calls: vec![] },
    ]));
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;

    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"call mom and book it"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let steps = v["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["name"], "task_create");
    assert_eq!(steps[0]["is_error"], false);
    // schedule_insert is forbidden on the talk surface, so the error reaches the wire
    assert_eq!(steps[1]["name"], "schedule_insert");
    assert_eq!(steps[1]["is_error"], true);

    let conn = state.db.lock().unwrap();
    let mut stmt = conn
        .prepare("SELECT role, tool_name FROM talk_messages ORDER BY id")
        .unwrap();
    let rows: Vec<(String, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let rows: Vec<(&str, Option<&str>)> =
        rows.iter().map(|(r, t)| (r.as_str(), t.as_deref())).collect();
    assert_eq!(
        rows,
        vec![
            ("user", None),
            ("tool", Some("task_create")),
            ("tool", Some("schedule_insert")),
            ("assistant", None),
        ]
    );
}

#[tokio::test]
async fn bad_conversation_at_global_capacity_is_404_not_503() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let _permits: Vec<_> = (0..note_server::MAX_CONCURRENT_TALKS)
        .map(|i| state.talk_gate.try_enter(-1 - i as i64).unwrap())
        .collect();
    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hi","conversation_id":9999}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"], "conversation not found");
}

#[tokio::test]
async fn second_post_with_the_conversation_id_replays_history() {
    use note_server::providers::Message;
    let llm = Arc::new(MockLLM::scripted(vec![
        ChatResponse { text: "hello aki".into(), tool_calls: vec![] },
        ChatResponse { text: "still here".into(), tool_calls: vec![] },
    ]));
    let (app, cookie, _state, _cfg) =
        common::app_with_logged_in_user_llm_and_state(llm.clone()).await;

    let res = app
        .clone()
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hi there"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let conv_id = v["conversation_id"].as_i64().unwrap();

    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({ "message": "you there?", "conversation_id": conv_id })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["conversation_id"].as_i64().unwrap(), conv_id);
    assert_eq!(v["reply"], "still here");

    let seen = llm.seen();
    assert_eq!(seen[0].n_messages, 1);
    assert_eq!(seen[1].n_messages, 3);
    assert!(matches!(&seen[1].messages[0], Message::User(t) if t == "hi there"));
    assert!(matches!(&seen[1].messages[1], Message::Assistant { text, .. } if text == "hello aki"));
    assert!(matches!(&seen[1].messages[2], Message::User(t) if t == "you there?"));
}

#[tokio::test]
async fn a_thread_longer_than_the_window_carries_its_summary_into_the_prompt() {
    use note_server::talk;
    let llm = Arc::new(MockLLM::scripted(vec![ChatResponse {
        text: "still here".into(),
        tool_calls: vec![],
    }]));
    let (app, cookie, state, _cfg) =
        common::app_with_logged_in_user_llm_and_state(llm.clone()).await;
    let at: jiff::Timestamp = "2026-09-17T09:00:00Z".parse().unwrap();
    let id = {
        let conn = state.db();
        let id = talk::create(&conn, 1, "the essay", at).unwrap();
        for i in 0..40 {
            talk::append_text(&conn, id, "user", &format!("turn {i}"), at).unwrap();
        }
        talk::store_summary(&conn, id, "Aki worked through the essay outline.", 10, at).unwrap();
        id
    };

    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({ "message": "you there?", "conversation_id": id })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let system = &llm.seen()[0].system;
    assert!(
        system.contains("Earlier in this conversation: Aki worked through the essay outline."),
        "{system}"
    );
}

#[tokio::test]
async fn a_short_thread_needs_no_summary_in_the_prompt() {
    use note_server::talk;
    let llm = Arc::new(MockLLM::scripted(vec![ChatResponse {
        text: "still here".into(),
        tool_calls: vec![],
    }]));
    let (app, cookie, state, _cfg) =
        common::app_with_logged_in_user_llm_and_state(llm.clone()).await;
    let at: jiff::Timestamp = "2026-09-17T09:00:00Z".parse().unwrap();
    let id = {
        let conn = state.db();
        let id = talk::create(&conn, 1, "the essay", at).unwrap();
        talk::append_text(&conn, id, "user", "the essay is due friday", at).unwrap();
        talk::store_summary(&conn, id, "Aki brought the Friday essay.", 1, at).unwrap();
        id
    };

    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({ "message": "you there?", "conversation_id": id })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(!llm.seen()[0].system.contains("Earlier in this conversation"));
}

#[tokio::test]
async fn absent_or_unowned_conversation_is_404_and_nothing_persisted() {
    let llm = Arc::new(MockLLM::scripted(vec![ChatResponse {
        text: "should never run".into(),
        tool_calls: vec![],
    }]));
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    let other_conv: i64 = {
        let conn = state.db.lock().unwrap();
        note_server::auth::create_user(&conn, "bo", "pw", false).unwrap();
        let bo_id: i64 = conn
            .query_row("SELECT id FROM users WHERE username = 'bo'", [], |r| r.get(0))
            .unwrap();
        note_server::talk::create(&conn, bo_id, "bo's chat", jiff::Timestamp::now()).unwrap()
    };

    for conv_id in [9999, other_conv] {
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/talk")
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "message": "hi", "conversation_id": conv_id })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "conversation not found");
    }

    let conn = state.db.lock().unwrap();
    let messages: i64 = conn
        .query_row("SELECT COUNT(*) FROM talk_messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(messages, 0);
    let conversations: i64 = conn
        .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(conversations, 1);
}

#[tokio::test]
async fn failed_session_persists_nothing() {
    let (app, cookie, state, _cfg) =
        common::app_with_logged_in_user_llm_and_state(Arc::new(FailingLLM)).await;
    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);

    let conn = state.db.lock().unwrap();
    let conversations: i64 = conn
        .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(conversations, 0);
    let messages: i64 = conn
        .query_row("SELECT COUNT(*) FROM talk_messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(messages, 0);
}

#[tokio::test]
async fn the_daily_session_cap_refuses_a_talk_turn() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    {
        let conn = state.db.lock().unwrap();
        for _ in 0..state.agent_sessions_per_day {
            note_server::log::record(&conn, Some(1), "agent_session", "kind=Talk").unwrap();
        }
    }
    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"], "daily session limit reached");
}

async fn talk_status(app: &axum::Router, cookie: &str) -> StatusCode {
    app.clone()
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn a_zero_cap_lifts_the_daily_limit() {
    let (_app, cookie, mut state, _cfg) = common::app_with_logged_in_user_and_state().await;
    {
        let conn = state.db.lock().unwrap();
        note_server::log::record(&conn, Some(1), "agent_session", "kind=Talk").unwrap();
    }
    state.agent_sessions_per_day = 1;
    assert_eq!(
        talk_status(&note_server::api::router(state.clone()), &cookie).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    state.agent_sessions_per_day = 0;
    assert_eq!(talk_status(&note_server::api::router(state), &cookie).await, StatusCode::OK);
}

#[tokio::test]
async fn talk_returns_timing_and_the_transcript_keeps_the_trace() {
    let llm = Arc::new(
        MockLLM::scripted(vec![
            ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "task_create".into(),
                    args: r#"{"title":"call mom"}"#.into(),
                }],
            },
            ChatResponse { text: "done".into(), tool_calls: vec![] },
        ])
        .thinking(vec!["a task first", "now the reply"]),
    );
    let (app, cookie, _cfg) = common::app_with_logged_in_user_and_llm(llm).await;

    let res = app
        .clone()
        .oneshot(
            Request::post("/api/talk")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"remind me to call mom"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let conv_id = v["conversation_id"].as_i64().unwrap();
    assert!(v["thought_ms"].as_i64().unwrap() >= 0, "{}", v["thought_ms"]);
    assert_eq!(v["steps"][0]["thinking"], "a task first");
    assert_eq!(v["reasoning"], "now the reply", "the outcome keeps the round that answered");

    let res = app
        .oneshot(
            Request::get(format!("/api/conversations/{conv_id}/messages"))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let rows: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    // a tool row stays ahead of the assistant row it belongs to
    assert_eq!(rows[0]["role"], "user");
    assert_eq!(rows[1]["role"], "tool");
    assert_eq!(rows[2]["role"], "assistant");
    assert!(rows[0]["reasoning"].is_null());
    assert_eq!(rows[1]["reasoning"], "a task first", "the call carries the round that made it");
    assert_eq!(rows[2]["reasoning"], "now the reply");
    assert_eq!(rows[2]["thought_ms"], v["thought_ms"]);
}
