mod common;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::{auth, talk, AppState};
use tower::ServiceExt;

async fn json(res: axum::response::Response) -> serde_json::Value {
    let body = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

async fn get(app: &axum::Router, path: &str, cookie: &str) -> axum::response::Response {
    app.clone()
        .oneshot(Request::get(path).header(header::COOKIE, cookie).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn rename(
    app: &axum::Router,
    id: i64,
    cookie: &str,
    title: &str,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::patch(format!("/api/conversations/{id}"))
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({ "title": title }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn delete(app: &axum::Router, id: i64, cookie: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::delete(format!("/api/conversations/{id}"))
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

fn seed(state: &AppState, user_id: i64, title: &str, at: &str) -> i64 {
    let conn = state.db.lock().unwrap();
    let at: jiff::Timestamp = at.parse().unwrap();
    let id = talk::create(&conn, user_id, title, at).unwrap();
    talk::append_text(&conn, id, "user", "hi", at).unwrap();
    talk::append_text(&conn, id, "assistant", "hello", at).unwrap();
    id
}

#[tokio::test]
async fn list_is_empty_for_a_fresh_user() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let res = get(&app, "/api/conversations", &cookie).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(json(res).await, serde_json::json!([]));
}

#[tokio::test]
async fn list_is_newest_updated_first() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let old = seed(&state, 1, "older", "2026-08-29T09:00:00Z");
    let new = seed(&state, 1, "newer", "2026-08-30T09:00:00Z");

    let v = json(get(&app, "/api/conversations", &cookie).await).await;
    assert_eq!(v.as_array().unwrap().len(), 2);
    assert_eq!(v[0]["id"], new);
    assert_eq!(v[0]["title"], "newer");
    assert_eq!(v[1]["id"], old);
    assert_eq!(v[0]["updated_at"], "2026-08-30T09:00:00Z");
}

#[tokio::test]
async fn a_row_carries_the_summary_the_idle_pass_wrote() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let plain = seed(&state, 1, "plain", "2026-08-29T09:00:00Z");
    let summarised = seed(&state, 1, "the essay", "2026-08-30T09:00:00Z");
    {
        let conn = state.db();
        talk::store_summary(
            &conn,
            summarised,
            "Aki brought the Friday essay and it went into Now.",
            2,
            "2026-08-30T10:00:00Z".parse().unwrap(),
        )
        .unwrap();
    }

    let v = json(get(&app, "/api/conversations", &cookie).await).await;
    assert_eq!(v[0]["id"], summarised);
    assert_eq!(v[0]["summary"], "Aki brought the Friday essay and it went into Now.");
    assert_eq!(v[1]["id"], plain);
    assert!(v[1]["summary"].is_null());
}

#[tokio::test]
async fn rename_round_trips_into_the_list() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let id = seed(&state, 1, "untitled", "2026-08-30T09:00:00Z");

    let res = rename(&app, id, &cookie, "  groceries  ").await;
    assert_eq!(res.status(), StatusCode::OK);

    let v = json(get(&app, "/api/conversations", &cookie).await).await;
    assert_eq!(v[0]["title"], "groceries");
    assert_eq!(v[0]["title_kind"], "user", "a rename is the user's own name for it");
}

#[tokio::test]
async fn a_thread_nobody_has_spoken_in_is_not_listed() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let spoken = seed(&state, 1, "the essay", "2026-08-30T09:00:00Z");
    {
        let conn = state.db();
        talk::create(&conn, 1, "Session: read the chapter", "2026-08-31T09:00:00Z".parse().unwrap())
            .unwrap();
    }

    let v = json(get(&app, "/api/conversations", &cookie).await).await;
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["id"], spoken);
    assert_eq!(v[0]["title_kind"], "draft");
}

#[tokio::test]
async fn blank_and_overlong_titles_are_rejected() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let id = seed(&state, 1, "untitled", "2026-08-30T09:00:00Z");

    for bad in ["   ", &"a".repeat(121)] {
        let res = rename(&app, id, &cookie, bad).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            json(res).await["error"],
            "title must be non-blank and at most 120 characters"
        );
    }

    let res = rename(&app, id, &cookie, &"a".repeat(120)).await;
    assert_eq!(res.status(), StatusCode::OK);

    let v = json(get(&app, "/api/conversations", &cookie).await).await;
    assert_eq!(v[0]["title"], "a".repeat(120));
}

#[tokio::test]
async fn messages_are_ordered_and_expose_tool_rows() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let id = seed(&state, 1, "chat", "2026-08-30T09:00:00Z");
    {
        let conn = state.db.lock().unwrap();
        let at: jiff::Timestamp = "2026-08-30T09:01:00Z".parse().unwrap();
        talk::append_tool(
            &conn,
            id,
            "task_create",
            r#"{"title":"x"}"#,
            r#"{"id":1}"#,
            true,
            None,
            at,
        )
        .unwrap();
    }

    let v = json(get(&app, &format!("/api/conversations/{id}/messages"), &cookie).await).await;
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["role"], "user");
    assert_eq!(rows[0]["content"], "hi");
    assert!(rows[0]["tool_name"].is_null());
    assert!(rows[0]["tool_args"].is_null());
    assert_eq!(rows[0]["is_error"], serde_json::Value::Bool(false));
    assert_eq!(rows[0]["created_at"], "2026-08-30T09:00:00Z");
    assert_eq!(rows[1]["role"], "assistant");
    assert_eq!(rows[2]["role"], "tool");
    assert_eq!(rows[2]["content"], r#"{"id":1}"#);
    assert_eq!(rows[2]["tool_name"], "task_create");
    assert_eq!(rows[2]["tool_args"], r#"{"title":"x"}"#);
    assert_eq!(rows[2]["is_error"], serde_json::Value::Bool(true));
    assert!(rows[0]["id"].as_i64().unwrap() < rows[2]["id"].as_i64().unwrap());
}

#[tokio::test]
async fn delete_removes_the_conversation_and_its_messages() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let id = seed(&state, 1, "chat", "2026-08-30T09:00:00Z");

    let res = delete(&app, id, &cookie).await;
    assert_eq!(res.status(), StatusCode::OK);

    let v = json(get(&app, "/api/conversations", &cookie).await).await;
    assert_eq!(v, serde_json::json!([]));

    let left: i64 = {
        let conn = state.db.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM talk_messages WHERE conversation_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(left, 0);

    assert_eq!(delete(&app, id, &cookie).await.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn another_users_conversation_is_indistinguishable_from_absent() {
    let (app, _cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let theirs = seed(&state, 1, "aki's chat", "2026-08-30T09:00:00Z");
    {
        let conn = state.db.lock().unwrap();
        auth::create_user(&conn, "bo", "pw", false).unwrap();
    }
    let bo = common::login(&app, "bo", "pw").await;

    assert_eq!(rename(&app, theirs, &bo, "mine now").await.status(), StatusCode::NOT_FOUND);
    assert_eq!(delete(&app, theirs, &bo).await.status(), StatusCode::NOT_FOUND);
    let res = get(&app, &format!("/api/conversations/{theirs}/messages"), &bo).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(json(res).await["error"], "conversation not found");

    // an absent id answers identically
    assert_eq!(rename(&app, 999, &bo, "x").await.status(), StatusCode::NOT_FOUND);

    assert!(json(get(&app, "/api/conversations", &bo).await).await.as_array().unwrap().is_empty());

    // and the owner's rows are untouched
    let title: String = {
        let conn = state.db.lock().unwrap();
        conn.query_row("SELECT title FROM conversations WHERE id = ?1", [theirs], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(title, "aki's chat");
}

#[tokio::test]
async fn unauthenticated_requests_are_rejected() {
    let (app, _cookie, _cfg) = common::app_with_logged_in_user().await;
    for req in [
        Request::get("/api/conversations").body(Body::empty()).unwrap(),
        Request::get("/api/conversations/1/messages").body(Body::empty()).unwrap(),
        Request::delete("/api/conversations/1").body(Body::empty()).unwrap(),
        Request::patch("/api/conversations/1")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"title":"x"}"#))
            .unwrap(),
    ] {
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn an_assistant_row_carries_its_trace_and_an_older_one_is_null() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    // seed's assistant row goes in the way a pre-migration one did
    let id = seed(&state, 1, "chat", "2026-08-30T09:00:00Z");
    {
        let conn = state.db.lock().unwrap();
        let at: jiff::Timestamp = "2026-08-30T09:01:00Z".parse().unwrap();
        talk::append_assistant(&conn, id, "here you go", "weighed two options", 2600, at).unwrap();
    }

    let v = json(get(&app, &format!("/api/conversations/{id}/messages"), &cookie).await).await;
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows[0]["reasoning"].is_null() && rows[0]["thought_ms"].is_null());
    assert_eq!(rows[1]["role"], "assistant");
    assert!(rows[1]["reasoning"].is_null() && rows[1]["thought_ms"].is_null());
    assert_eq!(rows[2]["role"], "assistant");
    assert_eq!(rows[2]["reasoning"], "weighed two options");
    assert_eq!(rows[2]["thought_ms"], 2600);
}
