mod common;

use note_server::providers::{mock::MockLLM, ChatResponse};
use note_server::{auth, db, AppState};
use std::sync::Arc;

const WHOAMI: &str = r#"{"user_id":"@note:t","device_id":"SRV"}"#;
const SENT: &str = r#"{"event_id":"$1"}"#;

fn says(texts: &[&str]) -> Arc<MockLLM> {
    Arc::new(MockLLM::scripted(
        texts.iter().map(|t| ChatResponse { text: (*t).into(), tool_calls: vec![] }).collect(),
    ))
}

fn sync(next: &str, messages: &[(&str, &str, &str)]) -> String {
    let mut join = serde_json::Map::new();
    for (room, sender, text) in messages {
        let events = join
            .entry((*room).to_string())
            .or_insert_with(|| serde_json::json!({ "timeline": { "events": [] } }));
        events["timeline"]["events"].as_array_mut().unwrap().push(serde_json::json!({
            "type": "m.room.message",
            "sender": sender,
            "content": { "msgtype": "m.text", "body": text },
        }));
    }
    serde_json::json!({ "next_batch": next, "rooms": { "join": join } }).to_string()
}

/// A Matrix-configured server with `aki` linked as `@aki:t` in `!dm:t`, its link in `link_state`.
fn rig(llm: Arc<MockLLM>, link_state: &str) -> (AppState, common::Fake, tempfile::TempDir) {
    let fake = common::fake_http();
    fake.answer(WHOAMI);
    let cfg = common::config_dir();
    let dir = cfg.path().to_path_buf();
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", true).unwrap();
    conn.execute(
        "INSERT INTO matrix_links (user_id, mxid, room_id, state, created_at) VALUES (1, '@aki:t', '!dm:t', ?1, 'x')",
        [link_state],
    )
    .unwrap();
    let token_file = dir.join("matrix.token");
    std::fs::write(&token_file, "syt_secret").unwrap();
    let settings = note_server::config::MatrixSettings { homeserver: fake.base.clone(), token_file };
    let mut state = AppState::new(conn, dir.clone(), dir.clone()).with_providers(llm, None);
    let ch = note_server::channels::matrix::MatrixChannel::new(state.db.clone(), dir, &settings).unwrap();
    ch.identify().unwrap();
    state = state.with_matrix(ch);
    fake.took();
    (state, fake, cfg)
}

/// Stores a cursor so the next poll answers what it finds. A test that expects
/// silence queues a spare answer first: the fake only hands back a request it
/// has answered, so a stray send would otherwise go unseen.
fn caught_up(state: &AppState) {
    note_server::matrix::set_cursor(&state.db(), "s0").unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_first_sync_answers_nothing_it_finds() {
    let (state, fake, _cfg) = rig(says(&["never"]), "linked");
    fake.answer(&sync("s1", &[("!dm:t", "@aki:t", "old message")]));
    fake.answer(SENT);
    let mut rooms = note_server::matrix::Rooms::default();
    note_server::matrix::poll_once(&state, &mut rooms).await.unwrap();
    let raw = fake.took();
    assert!(raw.contains("timeout=0"), "{raw}");
    assert!(fake.silent(), "nothing is sent");
    assert_eq!(note_server::matrix::cursor(&state.db()).unwrap().as_deref(), Some("s1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_from_the_linked_account_is_answered_in_its_dm() {
    let (state, fake, _cfg) = rig(says(&["hello back"]), "linked");
    caught_up(&state);
    fake.answer(&sync(
        "s1",
        &[("!dm:t", "@aki:t", "hello"), ("!dm:t", "@eve:t", "me too"), ("!other:t", "@aki:t", "elsewhere")],
    ));
    fake.answer(SENT);
    fake.answer(SENT);
    let mut rooms = note_server::matrix::Rooms::default();
    note_server::matrix::poll_once(&state, &mut rooms).await.unwrap();
    let raw = fake.took();
    assert!(raw.contains("since=s0"), "{raw}");
    let sent = fake.took();
    assert!(sent.starts_with("PUT /_matrix/client/v3/rooms/%21dm%3At/send/m.room.message/"), "{sent}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(sent.split_once("\r\n\r\n").unwrap().1).unwrap(),
        serde_json::json!({ "msgtype": "m.text", "body": "hello back" })
    );
    assert!(fake.silent(), "the stranger and the other room get nothing");
    let (via, stamped): (String, bool) = state
        .db()
        .query_row("SELECT via, matrix_at IS NOT NULL FROM conversations", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!((via.as_str(), stamped), ("matrix", true));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invited_link_is_not_answered() {
    let (state, fake, _cfg) = rig(says(&["never"]), "invited");
    caught_up(&state);
    fake.answer(&sync("s1", &[("!dm:t", "@aki:t", "hello")]));
    fake.answer(SENT);
    let mut rooms = note_server::matrix::Rooms::default();
    note_server::matrix::poll_once(&state, &mut rooms).await.unwrap();
    fake.took();
    assert!(fake.silent());
}

#[tokio::test(flavor = "multi_thread")]
async fn new_starts_a_fresh_thread_and_otherwise_the_recent_one_continues() {
    let (state, fake, _cfg) = rig(says(&["one", "two", "three"]), "linked");
    caught_up(&state);
    let mut rooms = note_server::matrix::Rooms::default();
    for (next, text) in [("s1", "first"), ("s2", "second"), ("s3", "/new"), ("s4", "third")] {
        fake.answer(&sync(next, &[("!dm:t", "@aki:t", text)]));
        fake.answer(SENT);
        note_server::matrix::poll_once(&state, &mut rooms).await.unwrap();
        fake.took();
        fake.took();
    }
    let threads: i64 = state.db().query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0)).unwrap();
    assert_eq!(threads, 2, "first and second share a thread; third opens another");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_web_turn_in_a_matrix_thread_posts_nothing_to_matrix() {
    let (state, fake, _cfg) = rig(says(&["from matrix", "from web"]), "linked");
    caught_up(&state);
    fake.answer(&sync("s1", &[("!dm:t", "@aki:t", "hello")]));
    fake.answer(SENT);
    let mut rooms = note_server::matrix::Rooms::default();
    note_server::matrix::poll_once(&state, &mut rooms).await.unwrap();
    fake.took();
    fake.took();
    let id: i64 = state.db().query_row("SELECT id FROM conversations", [], |r| r.get(0)).unwrap();
    fake.answer(SENT);
    note_server::talk::run_turn(&state, 1, "aki", Some(id), "and from the web", note_server::talk::Via::Web)
        .await
        .unwrap();
    assert!(fake.silent(), "a web turn stays on the web");
}
