mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn read(res: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

async fn owner(app: &axum::Router, cookie: &str, method: Method, path: &str, body: Option<&str>) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(path).header(header::COOKIE, cookie).header("sec-fetch-site", "same-origin");
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    read(app.clone().oneshot(req.body(Body::from(body.unwrap_or("").to_string())).unwrap()).await.unwrap()).await
}

fn in_days(days: i64) -> String {
    (jiff::Timestamp::now() + jiff::Span::new().hours(24 * days)).to_string()
}

async fn mint(app: &axum::Router, cookie: &str, name: &str, scope: &str) -> serde_json::Value {
    let (status, v) = owner(
        app,
        cookie,
        Method::POST,
        "/api/shares",
        Some(&format!(r#"{{"name":"{name}","brief":"be warm","scope":{scope},"expires_at":"{}"}}"#, in_days(30))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    v
}

fn token_of(v: &serde_json::Value) -> String {
    v["url"].as_str().unwrap().rsplit('/').next().unwrap().to_string()
}

#[tokio::test]
async fn owner_mints_lists_patches_and_revokes() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let made = mint(&app, &cookie, "Mom", "{}").await;
    assert!(made["url"].as_str().unwrap().contains("/s/share_"), "{made}");
    assert!(token_of(&made).starts_with("share_"), "{made}");
    assert_eq!(made["scope"]["horizon_days"], 3);
    assert_eq!(made["messages_today"], 0);

    let (status, list) = owner(&app, &cookie, Method::GET, "/api/shares", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["url"], made["url"], "the url is listed for the link's whole life");

    let id = made["id"].as_i64().unwrap();
    let (status, patched) = owner(&app, &cookie, Method::PATCH, &format!("/api/shares/{id}"), Some(r#"{"name":"Mother","scope":{"categories":["school"],"details":true}}"#)).await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["name"], "Mother");
    assert_eq!(patched["scope"]["categories"][0], "school");
    assert_eq!(patched["url"], made["url"]);

    let (status, _) = owner(&app, &cookie, Method::DELETE, &format!("/api/shares/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = owner(&app, &cookie, Method::DELETE, &format!("/api/shares/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_far_expiry_is_clamped_and_bad_input_is_422() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (status, v) = owner(&app, &cookie, Method::POST, "/api/shares", Some(&format!(r#"{{"name":"Far","expires_at":"{}"}}"#, in_days(400)))).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let exp: jiff::Timestamp = v["expires_at"].as_str().unwrap().parse().unwrap();
    let ceiling = jiff::Timestamp::now() + jiff::Span::new().hours(24 * 120);
    assert!(exp <= ceiling && exp > ceiling - jiff::Span::new().minutes(5));
    let (status, _) = owner(&app, &cookie, Method::POST, "/api/shares", Some(&format!(r#"{{"name":"","expires_at":"{}"}}"#, in_days(1)))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = owner(&app, &cookie, Method::POST, "/api/shares", Some(&format!(r#"{{"name":"x","scope":{{"horizon_days":0}},"expires_at":"{}"}}"#, in_days(1)))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = owner(&app, &cookie, Method::POST, "/api/shares", Some(r#"{"name":"x","expires_at":"2000-01-01T00:00:00Z"}"#)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_cross_site_write_is_refused_and_a_stranger_gets_nothing() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let req = Request::post("/api/shares")
        .header(header::COOKIE, &cookie)
        .header("sec-fetch-site", "cross-site")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!(r#"{{"name":"x","expires_at":"{}"}}"#, in_days(1))))
        .unwrap();
    let (status, _) = read(app.clone().oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = read(app.clone().oneshot(Request::get("/api/shares").body(Body::empty()).unwrap()).await.unwrap()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

use note_server::providers::{mock::MockLLM, ChatResponse, ToolCall};
use std::sync::Arc;

async fn visitor(app: &axum::Router, method: Method, path: &str, body: Option<&str>, cookie: Option<&str>) -> axum::response::Response {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    app.clone().oneshot(req.body(Body::from(body.unwrap_or("").to_string())).unwrap()).await.unwrap()
}

fn cookie_of(res: &axum::response::Response) -> Option<String> {
    res.headers().get(header::SET_COOKIE).and_then(|v| v.to_str().ok()).map(|v| v.split(';').next().unwrap().to_string())
}

fn scripted(replies: Vec<ChatResponse>) -> Arc<MockLLM> {
    Arc::new(MockLLM::scripted(replies))
}

fn say(text: &str) -> ChatResponse {
    ChatResponse { text: text.into(), tool_calls: vec![] }
}

fn call_tool(name: &str, args: &str) -> ChatResponse {
    ChatResponse { text: String::new(), tool_calls: vec![ToolCall { id: "c1".into(), name: name.into(), args: args.into() }] }
}

#[tokio::test]
async fn a_visitor_reads_the_link_the_view_and_talks_with_a_cookie_thread() {
    let llm = scripted(vec![say("Aki has a lab report due Friday."), say("Nothing else today.")]);
    let (app, cookie, _cfg) = common::app_with_logged_in_user_and_llm(llm.clone()).await;
    owner(&app, &cookie, Method::POST, "/api/tasks", Some(r#"{"title":"lab report","category":"school","urgency":"high"}"#)).await;
    let made = mint(&app, &cookie, "Mom", r#"{"categories":["school"]}"#).await;
    let token = token_of(&made);

    let res = visitor(&app, Method::GET, &format!("/api/share/{token}"), None, None).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["cache-control"], "no-store");
    assert_eq!(res.headers()["referrer-policy"], "no-referrer");
    assert_eq!(res.headers()["x-robots-tag"], "noindex");
    let set = cookie_of(&res).expect("a visitor cookie is set on the first response");
    assert!(set.starts_with("share_visitor="));
    let (_, info) = read(res).await;
    assert_eq!(info["owner"], "X");
    assert_eq!(info["name"], "Mom");
    assert_eq!(info["scope"]["categories"][0], "school");

    let (status, view) = read(visitor(&app, Method::GET, &format!("/api/share/{token}/view"), None, Some(&set)).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["tasks"][0]["title"], "lab report");
    assert_eq!(view["tasks"][0]["urgency"], "high");

    let (status, turn) = read(visitor(&app, Method::POST, &format!("/api/share/{token}/messages"), Some(r#"{"message":"What does Aki have?"}"#), Some(&set)).await).await;
    assert_eq!(status, StatusCode::OK, "{turn}");
    assert_eq!(turn["reply"], "Aki has a lab report due Friday.");
    assert_eq!(turn["note"], false);
    let seen = llm.seen();
    assert!(seen[0].system.contains("lab report"));
    assert!(seen[0].tool_names.contains(&"task_list".to_string()));
    assert!(!seen[0].tool_names.iter().any(|n| n.starts_with("memory_")));

    let thread = turn["thread"].as_i64().expect("the turn names its thread");
    let (_, msgs) = read(visitor(&app, Method::GET, &format!("/api/share/{token}/messages?thread={thread}"), None, Some(&set)).await).await;
    assert_eq!(msgs.as_array().unwrap().len(), 2);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[1]["content"], "Aki has a lab report due Friday.");

    // a stranger cannot read or continue someone else's thread
    let res = visitor(&app, Method::GET, &format!("/api/share/{token}/messages?thread={thread}"), None, None).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = visitor(&app, Method::POST, &format!("/api/share/{token}/messages"), Some(&format!(r#"{{"message":"hi","thread":{thread}}}"#)), None).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // the same visitor sending without a thread (a reloaded page) starts a new one
    let (status, again) = read(visitor(&app, Method::POST, &format!("/api/share/{token}/messages"), Some(r#"{"message":"Anything else?"}"#), Some(&set)).await).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["reply"], "Nothing else today.");
    assert_ne!(again["thread"], thread);
    let seen = llm.seen();
    assert_eq!(seen[1].n_messages, seen[0].n_messages, "a new thread carries no history");

    let (status, threads) = owner(&app, &cookie, Method::GET, &format!("/api/shares/{}/threads", made["id"]), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(threads.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn opening_the_link_counts_a_visit_measured_from_the_owner() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let made = mint(&app, &cookie, "Mom", "{}").await;
    let token = token_of(&made);
    let located = |lat: &str, lon: &str, city: &str| {
        Request::get(format!("/api/share/{token}"))
            .header("cf-iplatitude", lat)
            .header("cf-iplongitude", lon)
            .header("cf-ipcity", city)
            .header("cf-ipcountry", "US")
    };
    let seen = Request::get("/api/me")
        .header(header::COOKIE, &cookie)
        .header("cf-iplatitude", "47.61")
        .header("cf-iplongitude", "-122.33")
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.clone().oneshot(seen).await.unwrap().status(), StatusCode::OK);

    let first = app.clone().oneshot(located("45.52", "-122.68", "Portland").body(Body::empty()).unwrap()).await.unwrap();
    let set = cookie_of(&first).unwrap();
    let near_again = located("45.52", "-122.68", "Portland").header(header::COOKIE, &set).body(Body::empty()).unwrap();
    app.clone().oneshot(near_again).await.unwrap();
    app.clone().oneshot(located("40.71", "-74.01", "New York").body(Body::empty()).unwrap()).await.unwrap();

    let (_, list) = owner(&app, &cookie, Method::GET, "/api/shares", None).await;
    assert_eq!(list[0]["visitors"], 2);
    assert_eq!(list[0]["distant_visits"], 1);
    let (status, visits) = owner(&app, &cookie, Method::GET, &format!("/api/shares/{}/visits", made["id"]), None).await;
    assert_eq!(status, StatusCode::OK);
    let visits = visits.as_array().unwrap();
    assert_eq!(visits.len(), 3);
    assert_eq!(visits[0]["city"], "New York");
    assert_eq!(visits[0]["distant"], true);
    assert_eq!(visits[1]["distant"], false);
}

#[tokio::test]
async fn a_share_token_opens_no_other_door_and_a_dead_link_is_404() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let made = mint(&app, &cookie, "Mom", "{}").await;
    let token = token_of(&made);
    let res = visitor(&app, Method::GET, "/api/me", None, Some(&format!("session={token}"))).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = app.clone().oneshot(Request::get("/api/tasks").header(header::AUTHORIZATION, format!("Bearer {token}")).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = visitor(&app, Method::GET, "/api/share/share_nope", None, None).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    owner(&app, &cookie, Method::DELETE, &format!("/api/shares/{}", made["id"]), None).await;
    let res = visitor(&app, Method::GET, &format!("/api/share/{token}"), None, None).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_share_token_cannot_read_the_circle() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let token = token_of(&mint(&app, &cookie, "Mom", "{}").await);
    for path in ["/api/tasks/queue", "/api/sessions/today"] {
        let res = app.clone().oneshot(Request::get(path).header(header::AUTHORIZATION, format!("Bearer {token}")).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{path}");
        let res = visitor(&app, Method::GET, path, None, Some(&format!("session={token}"))).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{path}");
        let (status, _) = owner(&app, &cookie, Method::GET, path, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
    }
}

#[tokio::test]
async fn an_expired_link_refuses_a_message_and_persists_nothing() {
    let llm = scripted(vec![say("hi")]);
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    let made = mint(&app, &cookie, "Mom", "{}").await;
    let token = token_of(&made);
    state.db().execute("UPDATE shares SET expires_at = '2000-01-01T00:00:00Z'", []).unwrap();
    let res = visitor(&app, Method::POST, &format!("/api/share/{token}/messages"), Some(r#"{"message":"hello?"}"#), None).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let n: i64 = state.db().query_row("SELECT COUNT(*) FROM share_messages", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
    let t: i64 = state.db().query_row("SELECT COUNT(*) FROM share_threads", [], |r| r.get(0)).unwrap();
    assert_eq!(t, 0, "a failed first turn leaves no empty thread");
}

#[tokio::test]
async fn the_link_cap_is_429_and_the_owners_budget_and_activity_stay_untouched() {
    let llm = scripted((0..5).map(|_| say("ok")).collect());
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    let made = mint(&app, &cookie, "Mom", r#"{"messages_per_day":2}"#).await;
    let token = token_of(&made);
    for _ in 0..2 {
        let res = visitor(&app, Method::POST, &format!("/api/share/{token}/messages"), Some(r#"{"message":"hi"}"#), None).await;
        assert_eq!(res.status(), StatusCode::OK);
    }
    let (status, body) = read(visitor(&app, Method::POST, &format!("/api/share/{token}/messages"), Some(r#"{"message":"hi"}"#), None).await).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(body["error"].as_str().unwrap().contains("limit"));
    {
        let conn = state.db();
        let since = jiff::Timestamp::now() - jiff::Span::new().hours(24);
        assert_eq!(note_server::log::agent_sessions_since(&conn, 1, since).unwrap(), 0);
        let shares: i64 = conn.query_row("SELECT COUNT(*) FROM event_log WHERE kind = 'share_session'", [], |r| r.get(0)).unwrap();
        assert_eq!(shares, 2);
    }
    let (_, list) = owner(&app, &cookie, Method::GET, "/api/shares", None).await;
    assert_eq!(list[0]["messages_today"], 2);
}

fn calls(list: &[(&str, &str)]) -> ChatResponse {
    ChatResponse {
        text: String::new(),
        tool_calls: list.iter().enumerate().map(|(i, (name, args))| ToolCall { id: format!("c{i}"), name: (*name).into(), args: (*args).into() }).collect(),
    }
}

fn transcript(chats: &[note_server::providers::mock::RecordedChat]) -> String {
    let mut out = String::new();
    for chat in chats {
        out.push_str(&chat.system);
        for m in &chat.messages {
            out.push_str(&format!("{m:?}"));
        }
    }
    out
}

fn tool_results(chats: &[note_server::providers::mock::RecordedChat]) -> Vec<String> {
    chats
        .last()
        .map(|c| {
            c.messages
                .iter()
                .filter_map(|m| match m {
                    note_server::providers::Message::ToolResult { content, .. } => Some(content.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn nothing_outside_the_scope_reaches_the_prompt_the_tools_or_the_view() {
    let (app, cookie, state, cfg) = common::app_with_logged_in_user_and_state().await;
    // private material in every store
    std::fs::create_dir_all(cfg.path().join("users/aki")).unwrap();
    std::fs::write(note_server::context::standing_path(cfg.path(), "aki"), "STANDING-SECRET").unwrap();
    owner(&app, &cookie, Method::POST, "/api/tasks", Some(r#"{"title":"therapy forms","category":"health","description":"HEALTH-SECRET"}"#)).await;
    owner(&app, &cookie, Method::POST, "/api/tasks", Some(r#"{"title":"lab report","category":"school","description":"SCHOOL-DETAIL"}"#)).await;
    let (_, hidden_goal) = owner(&app, &cookie, Method::POST, "/api/goals", Some(r#"{"title":"GOAL-SECRET","description":"private"}"#)).await;
    owner(&app, &cookie, Method::POST, "/api/tasks", Some(&format!(r#"{{"title":"forms 2","category":"health","goal_id":{}}}"#, hidden_goal["id"]))).await;
    {
        let conn = state.db();
        conn.execute("INSERT INTO conversations (user_id, title, created_at, updated_at) VALUES (1, 'CHAT-SECRET', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute("INSERT INTO talk_messages (conversation_id, role, content, created_at) VALUES (1, 'user', 'CHAT-BODY-SECRET', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute("INSERT INTO memory_index (user, id, category, summary, path) VALUES ('aki', 'm1', 'semantic', 'MEMORY-SECRET', 'x.md')", []).unwrap();
    }
    let today = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
    let tomorrow = today.tomorrow().unwrap();
    let (status, _) = owner(&app, &cookie, Method::POST, "/api/calendar", Some(&format!(r#"{{"title":"CAL-SECRET appointment","kind":"fixed","start_time":"15:00","end_time":"16:00","on_date":"{today}"}}"#))).await;
    assert_eq!(status, StatusCode::CREATED);
    // tomorrow, so the block is laid whatever the time of day
    let (status, _) = owner(&app, &cookie, Method::POST, "/api/calendar", Some(&format!(r#"{{"title":"desk time","kind":"free","start_time":"10:00","end_time":"12:00","on_date":"{tomorrow}"}}"#))).await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, hidden) = owner(&app, &cookie, Method::POST, "/api/tasks", Some(r#"{"title":"HIDDEN-BLOCK task","category":"health","duration_min":30}"#)).await;
    let hidden_id = hidden["id"].as_i64().unwrap();
    let (status, placed) = owner(&app, &cookie, Method::POST, &format!("/api/plan/{tomorrow}/allocate"), Some("{}")).await;
    assert_eq!(status, StatusCode::OK, "{placed}");
    {
        let conn = state.db();
        let blocks: i64 = conn.query_row("SELECT COUNT(*) FROM event_tasks WHERE task_id = ?1", [hidden_id], |r| r.get(0)).unwrap();
        assert!(blocks > 0, "the hidden task has a block on the plan: {placed}");
    }

    let read_hidden = format!(r#"{{"task_id":{hidden_id}}}"#);
    let plan_tomorrow = format!(r#"{{"date":"{tomorrow}"}}"#);
    let probes: Vec<(&str, &str)> = vec![
        ("task_list", "{}"),
        ("task_read", &read_hidden),
        ("task_search", r#"{"query":"therapy"}"#),
        ("plan_list", &plan_tomorrow),
        ("calendar_list", "{}"),
        ("goal_list", "{}"),
    ];
    let llm = scripted(vec![calls(&probes), say("done looking"), calls(&probes), say("done again")]);
    let mut state = state.clone();
    state.llm = llm.clone();
    let app = note_server::api::router(state);

    // today off: no day, no calendar, no blocks
    let off = mint(&app, &cookie, "Mom", r#"{"categories":["school"],"today":false}"#).await;
    let off_token = token_of(&off);
    let (status, view) = read(visitor(&app, Method::GET, &format!("/api/share/{off_token}/view"), None, None).await).await;
    assert_eq!(status, StatusCode::OK);
    let (status, turn) = read(visitor(&app, Method::POST, &format!("/api/share/{off_token}/messages"), Some(r#"{"message":"tell me everything"}"#), None).await).await;
    assert_eq!(status, StatusCode::OK, "{turn}");
    let first = llm.seen();
    let mut everything = transcript(&first);
    everything.push_str(&view.to_string());
    for secret in ["STANDING-SECRET", "HEALTH-SECRET", "SCHOOL-DETAIL", "GOAL-SECRET", "CHAT-SECRET", "CHAT-BODY-SECRET", "MEMORY-SECRET", "CAL-SECRET", "HIDDEN-BLOCK", "therapy forms", "forms 2"] {
        assert!(!everything.contains(secret), "{secret} leaked with today off:\n{everything}");
    }
    assert!(everything.contains("lab report"));
    assert!(everything.contains("not shared on this link"), "{everything}");

    // today on: calendar titles are shared, a hidden block is only Busy
    let on = mint(&app, &cookie, "Dad", r#"{"categories":["school"],"today":true}"#).await;
    let on_token = token_of(&on);
    let (status, view) = read(visitor(&app, Method::GET, &format!("/api/share/{on_token}/view"), None, None).await).await;
    assert_eq!(status, StatusCode::OK);
    let rows: Vec<&serde_json::Value> = view["days"].as_array().unwrap().iter().flat_map(|d| d["rows"].as_array().unwrap()).collect();
    assert!(rows.iter().any(|r| r["busy"] == true && r["title"] == "Busy"), "{view}");
    let view_text = view.to_string();
    assert!(view_text.contains("CAL-SECRET"), "calendar titles travel when today is on: {view_text}");
    let (status, turn) = read(visitor(&app, Method::POST, &format!("/api/share/{on_token}/messages"), Some(r#"{"message":"tell me everything"}"#), None).await).await;
    assert_eq!(status, StatusCode::OK, "{turn}");
    let second = llm.seen().split_off(first.len());
    assert!(tool_results(&second).iter().any(|r| r.contains(r#""kind":"busy""#)), "{:?}", tool_results(&second));
    let mut everything = transcript(&second);
    everything.push_str(&view_text);
    for secret in ["STANDING-SECRET", "HEALTH-SECRET", "SCHOOL-DETAIL", "GOAL-SECRET", "CHAT-SECRET", "CHAT-BODY-SECRET", "MEMORY-SECRET", "HIDDEN-BLOCK", "therapy forms", "forms 2"] {
        assert!(!everything.contains(secret), "{secret} leaked with today on:\n{everything}");
    }
    assert!(everything.contains("lab report"));

    // details on: the shown task's description travels, the hidden ones still do not
    let (status, _) = owner(&app, &cookie, Method::PATCH, &format!("/api/shares/{}", on["id"]), Some(r#"{"scope":{"categories":["school"],"today":true,"details":true}}"#)).await;
    assert_eq!(status, StatusCode::OK);
    let (_, view) = read(visitor(&app, Method::GET, &format!("/api/share/{on_token}/view"), None, None).await).await;
    assert_eq!(view["tasks"][0]["description"], "SCHOOL-DETAIL");
    let view_text = view.to_string();
    for secret in ["HEALTH-SECRET", "therapy forms", "HIDDEN-BLOCK", "forms 2", "GOAL-SECRET"] {
        assert!(!view_text.contains(secret), "{secret} leaked with details on:\n{view_text}");
    }
}

#[tokio::test]
async fn a_note_is_filed_and_delivered_only_when_the_switch_is_on() {
    use note_server::channels::mock::MockChannel;
    let llm = scripted(vec![call_tool("share_note", r#"{"text":"I will be late tonight"}"#), say("unused")]);
    let cfg = common::config_dir();
    let dir = cfg.path().to_path_buf();
    let conn = note_server::db::open_memory().unwrap();
    note_server::auth::create_user(&conn, "aki", "pw", true).unwrap();
    let mock = Arc::new(MockChannel::new("mock"));
    let mut state = note_server::AppState::new(conn, dir.clone(), dir).with_providers(llm.clone(), None);
    state.channels = vec![mock.clone()];
    let app = note_server::api::router(state.clone());
    let cookie = common::login(&app, "aki", "pw").await;

    let off = mint(&app, &cookie, "Mom", "{}").await;
    let (status, _) = read(visitor(&app, Method::POST, &format!("/api/share/{}/messages", token_of(&off)), Some(r#"{"message":"tell aki I'm late"}"#), None).await).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!llm.seen()[0].tool_names.contains(&"share_note".to_string()), "notes off: the tool is not offered");
    assert!(mock.seen().is_empty());

    let llm2 = scripted(vec![call_tool("share_note", r#"{"text":"I will be late tonight"}"#)]);
    let mut state2 = state.clone();
    state2.llm = llm2.clone();
    let app2 = note_server::api::router(state2);
    let on = mint(&app2, &cookie, "Dad", r#"{"notes":true}"#).await;
    let (status, turn) = read(visitor(&app2, Method::POST, &format!("/api/share/{}/messages", token_of(&on)), Some(r#"{"message":"tell aki I'm late"}"#), None).await).await;
    assert_eq!(status, StatusCode::OK, "{turn}");
    assert_eq!(turn["note"], true);
    assert_eq!(turn["reply"], "Passed on to X.");
    let seen = mock.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1.title, "Note from Dad");
    assert_eq!(seen[0].1.body, "I will be late tonight");
    let (_, threads) = owner(&app2, &cookie, Method::GET, &format!("/api/shares/{}/threads", on["id"]), None).await;
    let thread: Vec<(&str, &str)> = threads[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["role"].as_str().unwrap(), m["content"].as_str().unwrap()))
        .collect();
    assert_eq!(
        thread,
        vec![("user", "tell aki I'm late"), ("note", "I will be late tonight"), ("assistant", "Passed on to X.")]
    );
}

#[tokio::test]
async fn a_failed_turn_leaves_nothing_on_the_thread() {
    struct Down;
    impl note_server::providers::LLMProvider for Down {
        fn chat(&self, _req: &note_server::providers::ChatRequest) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("provider down")
        }
    }
    let llm = Arc::new(Down);
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_llm_and_state(llm).await;
    let token = token_of(&mint(&app, &cookie, "Mom", "{}").await);
    let res = visitor(&app, Method::POST, &format!("/api/share/{token}/messages"), Some(r#"{"message":"hello?"}"#), None).await;
    assert!(!res.status().is_success(), "{}", res.status());
    let n: i64 = state.db().query_row("SELECT COUNT(*) FROM share_messages", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn a_goal_title_travels_only_when_goals_are_shared() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let (_, goal) = owner(&app, &cookie, Method::POST, "/api/goals", Some(r#"{"title":"GOAL-TITLE-SECRET"}"#)).await;
    let (_, task) = owner(&app, &cookie, Method::POST, "/api/tasks", Some(&format!(r#"{{"title":"lab report","category":"school","goal_id":{}}}"#, goal["id"]))).await;
    let read_task = format!(r#"{{"task_id":{}}}"#, task["id"]);
    let probes: Vec<(&str, &str)> = vec![("task_list", "{}"), ("task_read", &read_task), ("task_search", r#"{"query":"lab"}"#)];
    for goals in [false, true] {
        let llm = scripted(vec![calls(&probes), say("done looking")]);
        let mut state = state.clone();
        state.llm = llm.clone();
        let app = note_server::api::router(state);
        let token = token_of(&mint(&app, &cookie, &format!("goals {goals}"), &format!(r#"{{"categories":["school"],"goals":{goals}}}"#)).await);
        let (_, view) = read(visitor(&app, Method::GET, &format!("/api/share/{token}/view"), None, None).await).await;
        let (status, turn) = read(visitor(&app, Method::POST, &format!("/api/share/{token}/messages"), Some(r#"{"message":"what is it for?"}"#), None).await).await;
        assert_eq!(status, StatusCode::OK, "{turn}");
        let mut everything = transcript(&llm.seen());
        everything.push_str(&view.to_string());
        assert!(everything.contains("lab report"));
        assert_eq!(everything.contains("GOAL-TITLE-SECRET"), goals, "goals {goals}:\n{everything}");
    }
}

#[tokio::test]
async fn another_users_link_is_404_the_cap_is_409_and_the_log_names_the_link() {
    let cfg = common::config_dir();
    let dir = cfg.path().to_path_buf();
    let conn = note_server::db::open_memory().unwrap();
    note_server::auth::create_user(&conn, "aki", "pw", true).unwrap();
    note_server::auth::create_user(&conn, "rin", "pw", false).unwrap();
    let limits = note_server::config::LimitsConfig { shares_per_user: 2, ..Default::default() };
    let state = note_server::AppState::new(conn, dir.clone(), dir).with_limits(&limits);
    let app = note_server::api::router(state.clone());
    let aki = common::login(&app, "aki", "pw").await;
    let rin = common::login(&app, "rin", "pw").await;

    let first = mint(&app, &aki, "Mom", "{}").await;
    let id = first["id"].as_i64().unwrap();
    for (method, path, body) in [
        (Method::PATCH, format!("/api/shares/{id}"), Some(r#"{"name":"mine now"}"#)),
        (Method::DELETE, format!("/api/shares/{id}"), None),
        (Method::GET, format!("/api/shares/{id}/threads"), None),
    ] {
        let (status, _) = owner(&app, &rin, method.clone(), &path, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
    }

    mint(&app, &aki, "Dad", "{}").await;
    let (status, _) = owner(&app, &aki, Method::POST, "/api/shares", Some(&format!(r#"{{"name":"Third","expires_at":"{}"}}"#, in_days(1)))).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, _) = owner(&app, &aki, Method::DELETE, &format!("/api/shares/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let conn = state.db();
    for kind in ["share_created", "share_revoked"] {
        let detail: String = conn
            .query_row("SELECT detail FROM event_log WHERE kind = ?1 ORDER BY id LIMIT 1", [kind], |r| r.get(0))
            .unwrap();
        assert!(detail.starts_with(&format!("share={id} ")), "{kind}: {detail}");
    }
}

#[tokio::test]
async fn misses_from_one_address_are_limited_and_another_address_is_not() {
    let (app, _cookie, _cfg) = common::app_with_logged_in_user().await;
    let miss = |ip: &'static str| {
        let app = app.clone();
        async move {
            app.oneshot(Request::get("/api/share/share_nope").header("x-forwarded-for", ip).body(Body::empty()).unwrap()).await.unwrap().status()
        }
    };
    for _ in 0..60 {
        assert_eq!(miss("203.0.113.9").await, StatusCode::NOT_FOUND);
    }
    assert_eq!(miss("203.0.113.9").await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(miss("198.51.100.4").await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_malformed_visitor_cookie_is_replaced_and_a_stray_share_path_keeps_the_envelope() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let token = token_of(&mint(&app, &cookie, "Mom", "{}").await);
    let res = visitor(&app, Method::GET, &format!("/api/share/{token}"), None, Some("share_visitor=..junk..")).await;
    assert_eq!(res.status(), StatusCode::OK);
    let set = cookie_of(&res).expect("a malformed key is replaced");
    assert_eq!(set.strip_prefix("share_visitor=").unwrap().len(), 22);
    let res = visitor(&app, Method::GET, &format!("/api/share/{token}"), None, Some(&set)).await;
    assert!(res.headers().get(header::SET_COOKIE).is_none(), "a well-formed key is kept");

    let res = visitor(&app, Method::GET, &format!("/api/share/{token}/nope"), None, None).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(res.headers()["cache-control"], "no-store");
    assert_eq!(res.headers()["referrer-policy"], "no-referrer");
    assert_eq!(res.headers()["x-robots-tag"], "noindex");
}
