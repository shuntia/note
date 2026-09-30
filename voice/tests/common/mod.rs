use axum::extract::{Path, Query, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use note_voice_proto::testkit::{fast, Recording};
use note_voice_proto::{listen_forever, Dir, MemOutbox, Peer, Role};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct Hs {
    pub created: Vec<Value>,
    pub invites: Vec<(String, String)>,
    pub state_puts: Vec<(String, String, String, Value)>,
    /// Room, event type, content, and the event id handed back.
    pub sends: Vec<(String, String, Value, String)>,
    pub syncs: VecDeque<Value>,
    pub fail_sends: bool,
    rooms: u32,
    events: u32,
}

pub type SharedHs = Arc<Mutex<Hs>>;

async fn whoami() -> Json<Value> {
    Json(json!({ "user_id": "@note:t", "device_id": "DEV" }))
}

async fn create_room(State(hs): State<SharedHs>, Json(body): Json<Value>) -> Json<Value> {
    let mut hs = hs.lock().unwrap();
    hs.rooms += 1;
    hs.created.push(body);
    Json(json!({ "room_id": format!("!room{}:t", hs.rooms) }))
}

async fn invite(State(hs): State<SharedHs>, Path(room): Path<String>, Json(body): Json<Value>) -> Json<Value> {
    hs.lock().unwrap().invites.push((room, body["user_id"].as_str().unwrap_or_default().into()));
    Json(json!({}))
}

async fn put_state(
    State(hs): State<SharedHs>,
    Path((room, kind, key)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let mut hs = hs.lock().unwrap();
    hs.events += 1;
    hs.state_puts.push((room, kind, key, body));
    Json(json!({ "event_id": format!("$s{}", hs.events) }))
}

async fn send(
    State(hs): State<SharedHs>,
    Path((room, kind, _txn)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    let mut hs = hs.lock().unwrap();
    if hs.fail_sends {
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            Json(json!({ "errcode": "M_UNKNOWN_TOKEN", "error": "revoked" })),
        ));
    }
    hs.events += 1;
    let event_id = format!("$e{}", hs.events);
    hs.sends.push((room, kind, body, event_id.clone()));
    Ok(Json(json!({ "event_id": event_id })))
}

async fn direct() -> (axum::http::StatusCode, Json<Value>) {
    (axum::http::StatusCode::NOT_FOUND, Json(json!({ "errcode": "M_NOT_FOUND" })))
}

async fn set_direct() -> Json<Value> {
    Json(json!({}))
}

async fn sync(State(hs): State<SharedHs>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let n: u64 = q.get("since").and_then(|s| s.trim_start_matches('s').parse().ok()).unwrap_or(0);
    for _ in 0..20 {
        if let Some(mut next) = hs.lock().unwrap().syncs.pop_front() {
            next["next_batch"] = json!(format!("s{}", n + 1));
            return Json(next);
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    Json(json!({ "next_batch": format!("s{}", n + 1), "rooms": {} }))
}

pub async fn homeserver() -> (String, SharedHs) {
    let hs: SharedHs = Arc::default();
    let app = Router::new()
        .route("/_matrix/client/v3/account/whoami", get(whoami))
        .route("/_matrix/client/v3/createRoom", post(create_room))
        .route("/_matrix/client/v3/rooms/{room}/invite", post(invite))
        .route("/_matrix/client/v3/rooms/{room}/state/{kind}/{key}", put(put_state))
        .route("/_matrix/client/v3/rooms/{room}/send/{kind}/{txn}", put(send))
        .route("/_matrix/client/v3/user/{user}/account_data/m.direct", get(direct).put(set_direct))
        .route("/_matrix/client/v3/sync", get(sync))
        .with_state(hs.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, hs)
}

pub fn joined_room(room: &str, events: Vec<Value>) -> Value {
    json!({ "rooms": { "join": { room: { "timeline": { "events": events }, "state": { "events": [] } } } } })
}

pub fn member_event(user: &str, active: bool) -> Value {
    let content = if active { json!({ "application": "m.call", "device_id": "PHONE" }) } else { json!({}) };
    json!({
        "type": "org.matrix.msc3401.call.member",
        "state_key": format!("_{user}_PHONE_m.call"),
        "sender": user,
        "content": content,
    })
}

pub fn decline_event(user: &str, notification: &str) -> Value {
    json!({
        "type": "org.matrix.msc4310.rtc.decline",
        "sender": user,
        "content": { "m.relates_to": { "rel_type": "m.reference", "event_id": notification } },
    })
}

pub fn join_event(user: &str) -> Value {
    json!({ "type": "m.room.member", "state_key": user, "sender": user, "content": { "membership": "join" } })
}

pub struct FakeNote {
    pub peer: Peer,
    pub rec: Arc<Recording>,
    pub _outbox: Arc<Mutex<MemOutbox>>,
}

pub fn fake_note(socket: &std::path::Path) -> FakeNote {
    let rec = Arc::new(Recording::default());
    let outbox: Arc<Mutex<MemOutbox>> = Arc::default();
    let peer = Peer::new(fast(Role::Note), Dir::ToVoice, rec.clone(), Box::new(outbox.clone()));
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    tokio::spawn(listen_forever(peer.clone(), listener));
    FakeNote { peer, rec, _outbox: outbox }
}
