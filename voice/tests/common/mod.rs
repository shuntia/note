use axum::extract::{Path, Query, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use note_voice_proto::testkit::{fast, Recording};
use note_voice_proto::{
    listen_forever, BoxFuture, CallBody, Dir, Handler, MemOutbox, Peer, Refusal, Reply, Request, Role,
};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Default)]
pub struct Hs {
    pub created: Vec<Value>,
    pub invites: Vec<(String, String)>,
    pub state_puts: Vec<(String, String, String, Value)>,
    /// Room, event type, content, and the event id handed back.
    pub sends: Vec<(String, String, Value, String)>,
    pub syncs: VecDeque<Value>,
    pub fail_sends: bool,
    /// Members of each room, as `joined_members` reports them.
    pub joined: HashMap<String, Vec<String>>,
    pub direct_get_fails: bool,
    pub direct_put_fails: bool,
    pub direct_puts: Vec<Value>,
    /// Answered with a 400 once, as a homeserver refuses a stale token.
    pub reject_since: Option<String>,
    pub sync_sinces: Vec<Option<String>>,
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

async fn direct(State(hs): State<SharedHs>) -> (axum::http::StatusCode, Json<Value>) {
    if hs.lock().unwrap().direct_get_fails {
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "errcode": "M_UNKNOWN" })));
    }
    (axum::http::StatusCode::NOT_FOUND, Json(json!({ "errcode": "M_NOT_FOUND" })))
}

async fn set_direct(State(hs): State<SharedHs>, Json(body): Json<Value>) -> (axum::http::StatusCode, Json<Value>) {
    let mut hs = hs.lock().unwrap();
    if hs.direct_put_fails {
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "errcode": "M_UNKNOWN" })));
    }
    hs.direct_puts.push(body);
    (axum::http::StatusCode::OK, Json(json!({})))
}

async fn joined_members(State(hs): State<SharedHs>, Path(room): Path<String>) -> Json<Value> {
    let members = hs.lock().unwrap().joined.get(&room).cloned().unwrap_or_default();
    let joined: serde_json::Map<String, Value> = members.into_iter().map(|m| (m, json!({}))).collect();
    Json(json!({ "joined": joined }))
}

async fn sync(
    State(hs): State<SharedHs>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    {
        let mut hs = hs.lock().unwrap();
        let since = q.get("since").cloned();
        hs.sync_sinces.push(since.clone());
        if since.is_some() && hs.reject_since == since {
            hs.reject_since = None;
            return Err((axum::http::StatusCode::BAD_REQUEST, Json(json!({ "errcode": "M_UNKNOWN" }))));
        }
    }
    let n: u64 = q.get("since").and_then(|s| s.trim_start_matches('s').parse().ok()).unwrap_or(0);
    for _ in 0..20 {
        if let Some(mut next) = hs.lock().unwrap().syncs.pop_front() {
            next["next_batch"] = json!(format!("s{}", n + 1));
            return Ok(Json(next));
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    Ok(Json(json!({ "next_batch": format!("s{}", n + 1), "rooms": {} })))
}

pub async fn homeserver() -> (String, SharedHs) {
    let hs: SharedHs = Arc::default();
    let app = Router::new()
        .route("/_matrix/client/v3/account/whoami", get(whoami))
        .route("/_matrix/client/v3/createRoom", post(create_room))
        .route("/_matrix/client/v3/rooms/{room}/invite", post(invite))
        .route("/_matrix/client/v3/rooms/{room}/joined_members", get(joined_members))
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

pub fn joined_room(room: &str, events: &[Value]) -> Value {
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

/// Holds every call frame Note receives until released.
#[derive(Default)]
pub struct Hold {
    held: Mutex<bool>,
    released: Condvar,
}

impl Hold {
    pub fn set(&self, held: bool) {
        *self.held.lock().unwrap() = held;
        self.released.notify_all();
    }
}

struct HeldNote {
    rec: Arc<Recording>,
    hold: Arc<Hold>,
}

impl Handler for HeldNote {
    fn applied(&self, call_id: &str) -> u64 {
        self.rec.applied(call_id)
    }

    fn apply(&self, call_id: &str, seq: u64, body: CallBody) -> Result<(), String> {
        let held = self.hold.held.lock().unwrap();
        drop(self.hold.released.wait_while(held, |h| *h).unwrap());
        self.rec.apply(call_id, seq, body)
    }

    fn request(&self, body: Request) -> BoxFuture<Result<Reply, Refusal>> {
        self.rec.request(body)
    }

    fn acked(&self, call_id: &str, upto: u64) {
        self.rec.acked(call_id, upto);
    }
}

pub struct FakeNote {
    pub peer: Peer,
    pub rec: Arc<Recording>,
    pub hold: Arc<Hold>,
    pub _outbox: Arc<Mutex<MemOutbox>>,
}

impl Drop for FakeNote {
    fn drop(&mut self) {
        self.hold.set(false);
    }
}

pub fn fake_note(socket: &std::path::Path) -> FakeNote {
    let rec = Arc::new(Recording::default());
    let hold: Arc<Hold> = Arc::default();
    let outbox: Arc<Mutex<MemOutbox>> = Arc::default();
    let handler = Arc::new(HeldNote { rec: rec.clone(), hold: hold.clone() });
    let peer = Peer::new(fast(Role::Note), Dir::ToVoice, handler, Box::new(outbox.clone()));
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    tokio::spawn(listen_forever(peer.clone(), listener));
    FakeNote { peer, rec, hold, _outbox: outbox }
}

/// Hears nothing and says nothing; `loaded: false` has no languages.
pub struct SilentEngines {
    pub loaded: bool,
}

struct Deaf;

impl note_voice::audio::engines::Vad for Deaf {
    fn push(&mut self, _window: &[f32]) -> bool {
        false
    }

    fn reset(&mut self) {}
}

impl note_voice::audio::engines::SpeechToText for Deaf {
    fn accept(&mut self, _samples_16k: &[f32]) {}

    fn partial(&mut self) -> String {
        String::new()
    }

    fn finish(&mut self) -> String {
        String::new()
    }
}

impl note_voice::audio::engines::TurnDetector for Deaf {
    fn complete(&self, _samples_16k: &[f32]) -> f32 {
        0.0
    }
}

impl note_voice::audio::engines::TextToSpeech for Deaf {
    fn synthesize(&self, _text: &str, _voice: &str) -> anyhow::Result<Vec<i16>> {
        Ok(Vec::new())
    }

    fn voices(&self) -> Vec<note_voice::audio::engines::VoiceInfo> {
        Vec::new()
    }
}

impl note_voice::audio::engines::SpeechEngines for SilentEngines {
    fn languages(&self) -> Vec<String> {
        if self.loaded { vec!["en".into()] } else { Vec::new() }
    }

    fn vad(&self, _language: &str) -> anyhow::Result<Box<dyn note_voice::audio::engines::Vad>> {
        Ok(Box::new(Deaf))
    }

    fn stt(&self, _language: &str) -> anyhow::Result<Box<dyn note_voice::audio::engines::SpeechToText>> {
        Ok(Box::new(Deaf))
    }

    fn turn(&self, _language: &str) -> Arc<dyn note_voice::audio::engines::TurnDetector> {
        Arc::new(Deaf)
    }

    fn tts(&self, _language: &str) -> Arc<dyn note_voice::audio::engines::TextToSpeech> {
        Arc::new(Deaf)
    }
}

/// A room where the user stays, silent, until the bot leaves.
struct QuietRoom;

#[async_trait::async_trait]
impl note_voice::media::MediaIo for QuietRoom {
    async fn recv(&self) -> Option<Vec<f32>> {
        std::future::pending().await
    }

    async fn send(&self, _frame: &[i16; 480]) -> anyhow::Result<()> {
        Ok(())
    }

    fn clear(&self) {}

    async fn left(&self) {
        std::future::pending::<()>().await;
    }

    async fn leave(&self) {}
}

/// Joins a `QuietRoom`, or fails every join with `fail`.
pub struct FakeJoin {
    pub fail: bool,
}

#[async_trait::async_trait]
impl note_voice::media::MediaJoin for FakeJoin {
    async fn join(
        &self,
        _matrix: &note_voice::matrix::Matrix,
        _livekit_service_url: &str,
        _room_id: &str,
        _mxid: &str,
    ) -> anyhow::Result<Box<dyn note_voice::media::MediaIo>> {
        if self.fail {
            anyhow::bail!("no audio track from the user");
        }
        Ok(Box::new(QuietRoom))
    }
}

pub fn backends(loaded: bool, join_fails: bool) -> note_voice::service::Backends {
    note_voice::service::Backends {
        engines: Arc::new(SilentEngines { loaded }),
        media: Arc::new(FakeJoin { fail: join_fails }),
    }
}
