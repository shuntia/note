mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::{SinkExt, StreamExt};
use note_server::voice::Voice;
use note_server::{auth, db, AppState};
use note_voice_proto::testkit::{eventually, fast, Recording};
use note_voice_proto::{dial_forever, CallBody, Dir, Direction, LiveState, Media, MemOutbox, Origin, Outcome, Pcm, Peer, Role};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Rig {
    _dir: tempfile::TempDir,
    state: AppState,
    voice: Arc<Voice>,
    listen: tokio::task::JoinHandle<()>,
    fake: Peer,
    fake_rec: Arc<Recording>,
    _fake_outbox: Arc<Mutex<MemOutbox>>,
    addr: std::net::SocketAddr,
    cookie: String,
}

fn configured(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("defaults/prompts")).unwrap();
    std::fs::write(
        dir.join("defaults/user.toml"),
        "display_name = \"Aki\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n",
    )
    .unwrap();
    let shipped = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../config/defaults/prompts/voice.md");
    std::fs::copy(shipped, dir.join("defaults/prompts/voice.md")).unwrap();
}

async fn serve(app: axum::Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    configured(dir.path());
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", false).unwrap();
    let state = AppState::new(conn, dir.path().to_path_buf(), dir.path().to_path_buf());
    let voice = Voice::with_config(state.db.clone(), fast(Role::Note));
    let state = state.with_voice(voice.clone());
    let socket = dir.path().join("voice.sock");
    let listen = voice.listen(&socket).unwrap();
    let fake_rec = Arc::new(Recording::default());
    let fake_outbox: Arc<Mutex<MemOutbox>> = Arc::default();
    let fake = Peer::new(fast(Role::Voice), Dir::ToNote, fake_rec.clone(), Box::new(fake_outbox.clone()));
    tokio::spawn(dial_forever(fake.clone(), socket));
    let (v, f) = (voice.clone(), fake.clone());
    eventually("link up", || v.is_up() && f.is_up()).await;
    let app = note_server::api::router(state.clone());
    let cookie = common::login(&app, "aki", "pw").await;
    let addr = serve(app).await;
    Rig { _dir: dir, state, voice, listen, fake, fake_rec, _fake_outbox: fake_outbox, addr, cookie }
}

async fn open(addr: std::net::SocketAddr, cookie: &str, query: &str) -> Ws {
    let mut req = format!("ws://{addr}/api/call/ws{query}").into_client_request().unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

/// The socket at `path`, opened with `origin` when given; the refusal's status when the handshake is refused.
async fn open_from(addr: std::net::SocketAddr, cookie: &str, path: &str, origin: Option<&str>) -> Result<Ws, u16> {
    let mut req = format!("ws://{addr}{path}").into_client_request().unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    if let Some(origin) = origin {
        req.headers_mut().insert("origin", origin.parse().unwrap());
    }
    match tokio_tungstenite::connect_async(req).await {
        Ok((ws, _)) => Ok(ws),
        Err(tokio_tungstenite::tungstenite::Error::Http(res)) => Err(res.status().as_u16()),
        Err(e) => panic!("{e}"),
    }
}

fn rung(conversation_id: Option<i64>) -> note_server::channels::OutboundMessage {
    note_server::channels::OutboundMessage {
        title: "Note".into(),
        body: "Your essay is due at five.".into(),
        urgency: note_server::channels::Urgency::Normal,
        checkin: false,
        event_id: Some(8),
        conversation_id,
        actions: Vec::new(),
    }
}

async fn next(ws: &mut Ws) -> Option<Message> {
    loop {
        match tokio::time::timeout(Duration::from_secs(15), ws.next()).await.expect("a frame in time") {
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(m)) => return Some(m),
            _ => return None,
        }
    }
}

/// The next JSON frame of `kind`, skipping audio and other kinds.
async fn frame(ws: &mut Ws, kind: &str) -> Value {
    loop {
        match next(ws).await {
            Some(Message::Text(t)) => {
                let v: Value = serde_json::from_str(t.as_str()).unwrap();
                if v["type"] == kind {
                    return v;
                }
            }
            Some(Message::Binary(_)) => {}
            other => panic!("no {kind} frame: {other:?}"),
        }
    }
}

async fn audio(ws: &mut Ws) -> Vec<u8> {
    loop {
        match next(ws).await {
            Some(Message::Binary(b)) => return b.to_vec(),
            Some(Message::Text(_)) => {}
            other => panic!("no audio: {other:?}"),
        }
    }
}

fn starts(r: &Rig) -> Vec<(String, CallBody)> {
    r.fake_rec
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, _, b)| matches!(b, CallBody::Start { .. }))
        .map(|(id, _, b)| (id.clone(), b.clone()))
        .collect()
}

async fn started(r: &Rig, n: usize) -> String {
    eventually("a web Start", || starts(r).len() >= n).await;
    let (id, body) = starts(r)[n - 1].clone();
    assert!(matches!(body, CallBody::Start { origin: Origin::Web, .. }), "{body:?}");
    id
}

fn audio_in(r: &Rig, id: &str) -> Vec<Vec<i16>> {
    r.fake_rec
        .media
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(c, m)| match m {
            Media::AudioIn { pcm } if c == id => Some(pcm.0.clone()),
            _ => None,
        })
        .collect()
}

fn pcm_bytes(value: i16, samples: usize) -> Vec<u8> {
    std::iter::repeat_n(value.to_le_bytes(), samples).flatten().collect()
}

#[tokio::test]
async fn the_call_socket_needs_a_session() {
    let (app, _cookie, _cfg) = common::app_with_logged_in_user().await;
    let res = app.oneshot(Request::get("/api/call/ws").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_web_call_relays_audio_both_ways_and_drains_after_hang_up() {
    let r = rig().await;
    let mut ws = open(r.addr, &r.cookie, "").await;
    assert_eq!(frame(&mut ws, "open").await["rate"], 48_000);
    let id = started(&r, 1).await;
    let CallBody::Start { direction, room_id, .. } = starts(&r)[0].1.clone() else { unreachable!() };
    assert_eq!((direction, room_id.as_str()), (Direction::Inbound, ""));

    r.fake.send_call(&id, CallBody::Outcome { outcome: Outcome::Answered }).unwrap();
    r.fake.send_media(&id, Media::State { state: LiveState::Listening });
    assert_eq!(frame(&mut ws, "state").await["state"], "listening");

    ws.send(Message::Binary(pcm_bytes(7, 320).into())).await.unwrap();
    eventually("the caller's audio reaches the voice side", || audio_in(&r, &id) == vec![vec![7; 320]]).await;

    r.fake.send_media(&id, Media::AudioOut { pcm: Pcm(vec![5; 480]), rate: 48_000 });
    assert_eq!(audio(&mut ws).await, pcm_bytes(5, 480));

    r.fake.send_call(&id, CallBody::Draft { turn: 1, text: "hello there".into(), language: None }).unwrap();
    assert_eq!(frame(&mut ws, "caption").await["text"], "hello there");

    ws.send(Message::Text(r#"{"type":"hangup"}"#.into())).await.unwrap();
    let rec = r.fake_rec.clone();
    eventually("the hang-up reaches the voice side", || rec.bodies(&id).contains(&CallBody::HangUp)).await;
    r.fake.send_media(&id, Media::AudioOut { pcm: Pcm(vec![6; 480]), rate: 48_000 });
    assert_eq!(audio(&mut ws).await, pcm_bytes(6, 480), "Note's last words still reach the browser");

    r.fake.send_call(&id, CallBody::Ended).unwrap();
    let ended = frame(&mut ws, "ended").await;
    assert_eq!(ended["reason"], "ended");
    let conversation = ended["conversation_id"].as_i64().expect("the call's thread");
    let via: String = r.state.db().query_row("SELECT via FROM conversations WHERE id = ?1", [conversation], |x| x.get(0)).unwrap();
    assert_eq!(via, "voice");
    assert!(matches!(next(&mut ws).await, None | Some(Message::Close(_))));
}

#[tokio::test(flavor = "multi_thread")]
async fn muted_audio_reaches_the_voice_side_as_silence() {
    let r = rig().await;
    let mut ws = open(r.addr, &r.cookie, "").await;
    frame(&mut ws, "open").await;
    let id = started(&r, 1).await;
    ws.send(Message::Text(r#"{"type":"mute","on":true}"#.into())).await.unwrap();
    ws.send(Message::Binary(pcm_bytes(900, 320).into())).await.unwrap();
    eventually("a muted frame", || !audio_in(&r, &id).is_empty()).await;
    assert_eq!(audio_in(&r, &id), vec![vec![0; 320]]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_call_replaces_the_first() {
    let r = rig().await;
    let mut first = open(r.addr, &r.cookie, "").await;
    frame(&mut first, "open").await;
    let one = started(&r, 1).await;
    let mut second = open(r.addr, &r.cookie, "").await;
    frame(&mut second, "open").await;
    let two = started(&r, 2).await;
    assert_ne!(one, two);
    assert_eq!(frame(&mut first, "ended").await["reason"], "replaced");
    let rec = r.fake_rec.clone();
    eventually("the first call is hung up", || rec.bodies(&one).contains(&CallBody::HangUp)).await;
    assert!(!r.fake_rec.bodies(&two).contains(&CallBody::HangUp));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_web_call_while_a_phone_call_is_up_ends_busy() {
    let r = rig().await;
    r.state
        .db()
        .execute(
            "INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at) VALUES ('phone', 1, 'outbound', 'answered', 'x', 'x')",
            [],
        )
        .unwrap();
    let mut ws = open(r.addr, &r.cookie, "").await;
    assert_eq!(frame(&mut ws, "ended").await["reason"], "busy");
    assert!(starts(&r).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreign_thread_is_not_joined() {
    let r = rig().await;
    let foreign = {
        let conn = r.state.db();
        auth::create_user(&conn, "eve", "pw", false).unwrap();
        note_server::talk::create(&conn, 2, "eve's", jiff::Timestamp::now()).unwrap()
    };
    let mut ws = open(r.addr, &r.cookie, &format!("?conversation_id={foreign}")).await;
    frame(&mut ws, "open").await;
    let id = started(&r, 1).await;
    let thread: Option<i64> = r.state.db().query_row("SELECT thread_id FROM voice_calls WHERE id = ?1", [&id], |x| x.get(0)).unwrap();
    assert_eq!(thread, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_without_the_voice_service_ends_unavailable() {
    let (app, cookie, _state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let addr = serve(app).await;
    let mut ws = open(addr, &cookie, "").await;
    assert_eq!(frame(&mut ws, "ended").await["reason"], "unavailable");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_silent_socket_is_hung_up_after_five_seconds() {
    let r = rig().await;
    let mut ws = open(r.addr, &r.cookie, "").await;
    frame(&mut ws, "open").await;
    let id = started(&r, 1).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!r.fake_rec.bodies(&id).contains(&CallBody::HangUp), "not before five seconds");
    let rec = r.fake_rec.clone();
    eventually("hung up", || rec.bodies(&id).contains(&CallBody::HangUp)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_socket_hangs_up_at_once() {
    let r = rig().await;
    let mut ws = open(r.addr, &r.cookie, "").await;
    frame(&mut ws, "open").await;
    let id = started(&r, 1).await;
    ws.close(None).await.unwrap();
    let rec = r.fake_rec.clone();
    eventually("hung up", || rec.bodies(&id).contains(&CallBody::HangUp)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_oversized_frame_ends_the_socket_and_hangs_up() {
    let r = rig().await;
    let mut ws = open(r.addr, &r.cookie, "").await;
    frame(&mut ws, "open").await;
    let id = started(&r, 1).await;
    ws.send(Message::Binary(pcm_bytes(7, 1601).into())).await.unwrap();
    let rec = r.fake_rec.clone();
    eventually("hung up", || rec.bodies(&id).contains(&CallBody::HangUp)).await;
    assert!(audio_in(&r, &id).is_empty());
    assert!(matches!(next(&mut ws).await, None | Some(Message::Close(_))));
}

#[tokio::test(flavor = "multi_thread")]
async fn tiny_frames_do_not_keep_a_silent_call_alive() {
    let r = rig().await;
    let mut ws = open(r.addr, &r.cookie, "").await;
    frame(&mut ws, "open").await;
    let id = started(&r, 1).await;
    let (mut tx, _rx) = ws.split();
    let rec = r.fake_rec.clone();
    let hung_up = tokio::time::timeout(Duration::from_secs(8), async {
        while !rec.bodies(&id).contains(&CallBody::HangUp) {
            tx.send(Message::Binary(vec![1u8].into())).await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    assert!(hung_up.is_ok(), "one-byte frames held the call open");
    assert!(audio_in(&r, &id).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_voice_link_ends_the_browser_call() {
    let r = rig().await;
    let mut ws = open(r.addr, &r.cookie, "").await;
    frame(&mut ws, "open").await;
    started(&r, 1).await;
    r.listen.abort();
    let v = r.voice.clone();
    eventually("Note sees the link down", || !v.is_up()).await;
    let (mut tx, mut rx) = ws.split();
    let keep_talking = async {
        loop {
            if tx.send(Message::Binary(pcm_bytes(1, 320).into())).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };
    let ended = async {
        while let Some(Ok(m)) = rx.next().await {
            if let Message::Text(t) = m {
                let v: Value = serde_json::from_str(t.as_str()).unwrap();
                if v["type"] == "ended" {
                    return v;
                }
            }
        }
        panic!("the socket closed without an ended frame");
    };
    let ended = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::select! { v = ended => v, () = keep_talking => panic!("the socket closed first") }
    })
    .await
    .expect("ended within the link limit");
    assert_eq!(ended["reason"], "unavailable");
}

#[tokio::test(flavor = "multi_thread")]
async fn me_says_whether_calls_are_on() {
    let r = rig().await;
    let app = note_server::api::router(r.state.clone());
    let res = app
        .oneshot(Request::get("/api/me").header("cookie", &r.cookie).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = http_body_util::BodyExt::collect(res.into_body()).await.unwrap().to_bytes();
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["voice"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ring_reaches_only_a_visible_app_and_its_answer_speaks_the_thread() {
    let r = rig().await;
    let thread = {
        let conn = r.state.db();
        let id = note_server::talk::create(&conn, 1, "chat", jiff::Timestamp::now()).unwrap();
        note_server::talk::append_text(&conn, id, "assistant", "Your essay is due at five.", jiff::Timestamp::now()).unwrap();
        id
    };
    let (conn_id, mut rx) = r.state.hub.register(1).unwrap();
    assert!(!r.state.web_calls.has_live(1));
    assert!(!r.state.web_calls.ring(1, &rung(Some(thread))), "no app in view");
    r.state.hub.set_visible(1, conn_id, true);
    assert!(r.state.web_calls.has_live(1));
    assert!(r.state.web_calls.ring(1, &rung(Some(thread))));
    let incoming: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
    assert_eq!((incoming["type"].as_str(), incoming["conversation_id"].as_i64()), (Some("incoming"), Some(thread)));
    let token = incoming["ring"].as_str().unwrap().to_string();

    let mut ws = open(r.addr, &r.cookie, &format!("?ring={token}")).await;
    frame(&mut ws, "open").await;
    let id = started(&r, 1).await;
    let CallBody::Start { direction, .. } = starts(&r)[0].1.clone() else { unreachable!() };
    assert_eq!(direction, Direction::Outbound, "Note's call speaks first");
    let (message, thread_id): (String, i64) = r
        .state
        .db()
        .query_row("SELECT message, thread_id FROM voice_calls WHERE id = ?1", [&id], |x| Ok((x.get(0)?, x.get(1)?)))
        .unwrap();
    assert!(message.contains("Your essay is due at five."), "{message}");
    assert_eq!(thread_id, thread);
    let taken: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
    assert_eq!((taken["type"].as_str(), taken["ring"].as_str()), (Some("ring_taken"), Some(token.as_str())));

    let mut again = open(r.addr, &r.cookie, &format!("?ring={token}")).await;
    assert_eq!(frame(&mut again, "ended").await["reason"], "missed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreign_page_cannot_open_either_socket() {
    let r = rig().await;
    for path in ["/api/ws", "/api/call/ws"] {
        assert_eq!(open_from(r.addr, &r.cookie, path, Some("https://evil.example")).await.err(), Some(403), "{path}");
        assert_eq!(open_from(r.addr, &r.cookie, path, Some("null")).await.err(), Some(403), "{path}");
    }
    let mut events = open_from(r.addr, &r.cookie, "/api/ws", Some("http://localhost:3271")).await.unwrap();
    events.close(None).await.unwrap();
    let mut call = open_from(r.addr, &r.cookie, "/api/call/ws", Some("http://localhost:3271")).await.unwrap();
    frame(&mut call, "open").await;
    let mut bare = open_from(r.addr, &r.cookie, "/api/ws", None).await.unwrap();
    bare.close(None).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_served_from_another_address_of_this_server_opens_its_sockets() {
    let r = rig().await;
    let own = format!("http://{}", r.addr);
    for path in ["/api/ws", "/api/call/ws"] {
        let mut ws = open_from(r.addr, &r.cookie, path, Some(&own)).await.unwrap();
        ws.close(None).await.unwrap();
        assert_eq!(open_from(r.addr, &r.cookie, path, Some("http://evil.example")).await.err(), Some(403), "{path}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_user_holding_two_call_sockets_is_refused_a_third() {
    let r = rig().await;
    let _held = (r.state.call_sockets.take(1).unwrap(), r.state.call_sockets.take(1).unwrap());
    assert_eq!(open_from(r.addr, &r.cookie, "/api/call/ws", None).await.err(), Some(429));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declined_ring_sends_its_message_on_at_once_and_stops_ringing() {
    let r = rig().await;
    let mut app = open_from(r.addr, &r.cookie, "/api/ws", None).await.unwrap();
    app.send(Message::Text(r#"{"type":"visible","on":true}"#.into())).await.unwrap();
    eventually("the app in view", || r.state.web_calls.has_live(1)).await;
    assert!(r.state.web_calls.ring(1, &rung(None)));
    let token = frame(&mut app, "incoming").await["ring"].as_str().unwrap().to_string();
    app.send(Message::Text(serde_json::json!({ "type": "decline", "ring": token }).to_string().into())).await.unwrap();
    let (mut taken, mut delivered) = (None, None);
    while taken.is_none() || delivered.is_none() {
        let Some(Message::Text(t)) = next(&mut app).await else { panic!("the app socket closed") };
        let v: Value = serde_json::from_str(t.as_str()).unwrap();
        match v["type"].as_str() {
            Some("ring_taken") => taken = Some(v["ring"].clone()),
            Some("event") => delivered = Some(v["body"].clone()),
            _ => {}
        }
    }
    assert_eq!(taken.unwrap(), token.as_str(), "other pages stop ringing");
    assert_eq!(delivered.unwrap(), "Your essay is due at five.");
    let mut late = open(r.addr, &r.cookie, &format!("?ring={token}")).await;
    assert_eq!(frame(&mut late, "ended").await["reason"], "missed", "a declined ring cannot be answered");
}
