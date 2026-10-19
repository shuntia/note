mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use note_server::channels::Action;
use note_server::{actions, api, auth, db, AppState};
use tower::ServiceExt;

fn world() -> (axum::Router, AppState, i64, tempfile::TempDir) {
    let cfg = common::config_dir();
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aitest", "pw", false).unwrap();
    let today = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
    let plan = note_server::plan::ensure(&conn, cfg.path(), "aitest", 1, today).unwrap();
    conn.execute(
        "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, status) VALUES (?1, 'checkin', '00:00', '00:00', 'fired')",
        [plan],
    )
    .unwrap();
    let event = conn.last_insert_rowid();
    let state = AppState::new(conn, cfg.path().into(), cfg.path().into());
    (api::router(state.clone()), state, event, cfg)
}

async fn press(app: &axum::Router, key: &str) -> StatusCode {
    let req = Request::post("/api/actions")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({ "key": key }).to_string()))
        .unwrap();
    app.clone().oneshot(req).await.unwrap().status()
}

fn status(state: &AppState, event: i64) -> String {
    state.db().query_row("SELECT status FROM events WHERE id = ?1", [event], |r| r.get(0)).unwrap()
}

#[tokio::test]
async fn a_pushed_button_is_pressed_by_its_key_alone_and_only_once() {
    let (app, state, event, _cfg) = world();
    let keys = vec![actions::push_key(), actions::push_key()];
    let buttons = vec![
        Action { label: "Done".into(), data: format!("ev:done:{event}") },
        Action { label: "Drop".into(), data: format!("ev:drop:{event}") },
    ];
    actions::keep(&state.db(), 1, &actions::push_key(), &keys, &buttons, jiff::Timestamp::now()).unwrap();

    assert_eq!(press(&app, &keys[0]).await, StatusCode::NO_CONTENT);
    assert_eq!(status(&state, event), "done");
    assert_eq!(press(&app, &keys[1]).await, StatusCode::NOT_FOUND, "its sibling is spent with it");
    assert_eq!(status(&state, event), "done");
}

#[tokio::test]
async fn a_key_never_given_out_presses_nothing() {
    let (app, state, event, _cfg) = world();
    assert_eq!(press(&app, &actions::push_key()).await, StatusCode::NOT_FOUND);
    assert_eq!(press(&app, &format!("mx:$e:✅ ev:done:{event}")).await, StatusCode::NOT_FOUND);
    assert_eq!(status(&state, event), "fired");
}
