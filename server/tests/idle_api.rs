mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
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

/// A fixed-offset zone whose wall clock reads midday whenever the suite runs,
/// so half an hour ago is always today and before the close of the day.
fn midday_zone() -> String {
    let hour = i32::from(jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).hour());
    // Etc/GMT+N runs N hours behind UTC, so the sign reads backwards.
    format!("Etc/GMT{:+}", hour - 12)
}

async fn world(script: Vec<ChatResponse>) -> World {
    let cfg = common::config_dir();
    let dir = cfg.path().to_path_buf();
    std::fs::create_dir_all(dir.join("users/aki/templates")).unwrap();
    std::fs::write(dir.join("users/aki/templates/default.toml"), "events = []\n").unwrap();
    std::fs::write(dir.join("users/aki/user.toml"), format!("timezone = \"{}\"\n", midday_zone()))
        .unwrap();
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

fn rows<T: rusqlite::types::FromSql>(w: &World, sql: &str) -> Vec<T> {
    let conn = w.state.db.lock().unwrap();
    let mut stmt = conn.prepare(sql).unwrap();
    let out = stmt.query_map([], |r| r.get(0)).unwrap();
    out.collect::<rusqlite::Result<_>>().unwrap()
}

const BANK: &str = "00000000-0000-4000-8000-0000000000b1";

/// Quiet for half an hour with one active note, `BANK`.
fn gone_quiet(w: &World) {
    let conn = w.state.db();
    let then = jiff::Timestamp::now() - jiff::Span::new().minutes(30);
    conn.execute(
        "UPDATE users SET last_active_at = ?1 WHERE id = 1",
        [note_server::presence::stamp(then)],
    )
    .unwrap();
    let at = note_server::notes::stamp(then);
    note_server::memory::put(
        &conn,
        &w.state.data_dir,
        "aki",
        &note_server::memory::MemoryFile {
            id: BANK.into(),
            category: note_server::memory::NOTE.into(),
            summary: "call the bank".into(),
            body: String::new(),
            supersedes: None,
            until: None,
            created: at.clone(),
            archived: false,
            source: None,
            from: None,
            touched_at: Some(at),
            last_nudged_at: None,
        },
    )
    .unwrap();
}

fn last_nudged(w: &World) -> Option<String> {
    note_server::notes::get(&w.state.data_dir, "aki", BANK).unwrap().unwrap().last_nudged_at
}

#[tokio::test]
async fn a_quiet_user_with_an_open_note_is_nudged_about_it_once() {
    let w = world(vec![call("c1", "say", &format!(r#"{{"text":"the bank closes at five","notes":["{BANK}"]}}"#))])
        .await;
    gone_quiet(&w);

    note_server::runner::sweep_once(&w.state);

    let seen = w.push.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1.body, "the bank closes at five");
    assert!(seen[0].1.actions.is_empty(), "a nudge carries no event buttons");
    let origins: Vec<String> = rows(&w, "SELECT origin FROM events WHERE kind = 'trigger'");
    assert_eq!(origins, vec!["idle"]);
    assert!(last_nudged(&w).is_some(), "the named note carries the nudge");
    let opening = format!("{:?}", w.llm.seen()[0].messages.last().unwrap());
    assert!(opening.contains("call the bank"), "{opening}");
    assert!(opening.contains("Nothing from the user for 30 min."), "{opening}");

    note_server::runner::sweep_once(&w.state);
    assert_eq!(w.push.seen().len(), 1, "one quiet stretch, one nudge");
}

#[tokio::test]
async fn a_nudge_held_back_stays_held_until_another_stretch_of_quiet() {
    let w = world(vec![call("c1", "stay_quiet", r#"{"reason":"nothing on the list is urgent"}"#)])
        .await;
    gone_quiet(&w);

    note_server::runner::sweep_once(&w.state);
    note_server::runner::sweep_once(&w.state);

    assert!(w.push.seen().is_empty());
    let statuses: Vec<String> = rows(&w, "SELECT status FROM events WHERE kind = 'trigger'");
    assert_eq!(statuses, vec!["done"]);
    assert!(last_nudged(&w).is_none());
}

#[tokio::test]
async fn coming_back_before_the_nudge_fires_calls_it_off() {
    let w = world(vec![]).await;
    gone_quiet(&w);
    {
        let conn = w.state.db();
        let laid =
            note_server::idle::check(&conn, &w.state.config_dir, &w.state.data_dir, jiff::Timestamp::now()).unwrap();
        assert_eq!(laid.len(), 1);
    }
    let res = w
        .app
        .clone()
        .oneshot(
            Request::post("/api/presence")
                .header(header::COOKIE, &w.cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    note_server::runner::sweep_once(&w.state);

    assert!(w.llm.seen().is_empty(), "no session ran");
    assert!(w.push.seen().is_empty());
    let statuses: Vec<String> = rows(&w, "SELECT status FROM events WHERE kind = 'trigger'");
    assert_eq!(statuses, vec!["dropped"]);
    let why: Vec<String> =
        rows(&w, "SELECT detail FROM event_log WHERE kind = 'trigger_cancelled'");
    assert!(why[0].contains("active"), "{}", why[0]);
}
