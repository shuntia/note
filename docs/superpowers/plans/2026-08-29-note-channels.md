# Note Channels & Ingress Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The delivery layer (`Channel` trait with WebSocket, Web Push, and mock implementations), fired events actually reaching users through a fallback ladder, the `notify_send` outreach tool, and the ingress-hardening debt this server must retire before it faces the internet: auth hardening, talk concurrency caps, and moving embeddings HTTP calls out from under the DB lock.

**Architecture:** Channels are synchronous trait objects (`Arc<dyn Channel>`) owning their own resources (the WS hub, the DB handle for subscription reads); the dispatcher walks a ladder (WebSocket first, Web Push second) and logs `delivery_ok` / `delivery_degraded` — the user sees no errors, only a plainer day. The runner fires events under the lock, then delivers with the lock released. Web Push requests are built by a pure function over the `web-push` crate's encryption (no client feature; sending is a thin `ureq` shell). The embeddings refactor introduces a `tools::prepare` pre-pass run by the agent loop before taking the DB lock, so `tools::dispatch` never performs network I/O.

**Tech Stack:** Existing stack plus `web-push = { version = "0.10", default-features = false }` (VAPID + aes128gcm payload encryption only) and `base64 = "0.22"` (VAPID public key encoding). No client/hyper features — sending stays on `ureq`.

**Spec:** `docs/superpowers/specs/2026-08-29-note-design.md` (Channel layer, Network exposure, Error handling, Interaction model, Testing). Prior phases: `2026-08-29-note-foundation.md`, `2026-08-29-note-agent-tools.md`, `2026-08-29-note-providers-agent.md`.

**Out of scope (follow-on plans):** Voice (Twilio Media Streams + `SpeechProvider`) — depends on this channel layer; the web PWA — independent subsystem. The `voice` event channel value degrades to the push ladder with a `voice_unavailable` log, per the spec's "failed calls fall back to web push".

## Global Constraints

- All prior constraints hold: wall-clock `HH:MM` + IANA tz; comments only at fn declarations where code can't speak; `cargo test` green from repo root before every commit; imperative one-line commit messages.
- **Never hold the DB `Mutex` across network I/O.** Channel delivery reads its inputs under a short lock, releases, does HTTP/WS sends, then re-locks to record outcomes. After Task 8, `tools::dispatch` performs no network I/O at all — embeddings happen in `tools::prepare`, called by the agent loop outside the lock.
- **Delivery is code, never a model call.** Message rendering from events is pure code. Every degradation is logged (`delivery_ok`, `delivery_degraded`, `voice_unavailable`); no delivery failure ever propagates as a user-visible error or kills the runner.
- **Fully testable offline:** the WS hub is tested in-process; Web Push is tested via the pure builder against an embedded test key + RFC 8291 sample subscription; delivery uses `channels::mock::MockChannel`. No test makes a live network call.
- **Tool layer discipline:** `notify_send` goes through `tools::dispatch` like every tool; registries stay nested (CHECKIN ⊂ TALK ⊂ NIGHTLY — `notify_send` is Nightly-only, preserving the subset test).
- **Auth changes must not weaken anything:** argon2 verification moves off the async executor and out from under the DB lock; unknown usernames verify against a dummy hash (timing oracle); failed logins are rate-limited per username; cookies gain `Max-Age` and `Secure` (when `public_base_url` is https); logout deletes the session row; expired sessions are GC'd.
- This plan retires ledgered debt: embeddings-under-the-lock (structural fix), `/api/talk` concurrency cap, the auth-hardening pass, slide-on-decided guard, and the canned empty talk reply.

---

### Task 1: Migration v4, push subscriptions module, subscription API

**Files:**
- Modify: `server/src/db.rs` (append v4 to `MIGRATIONS`)
- Create: `server/src/push_subs.rs`
- Modify: `server/src/lib.rs` (add `pub mod push_subs;`), `server/src/api.rs` (three routes)
- Test: inline in `push_subs.rs` + new `server/tests/push_api.rs`

**Interfaces:**
- Consumes: existing `AppState`, `CurrentUser`, `db::MIGRATIONS` pattern (each step transactional).
- Produces:
  - Migration v4: `push_subscriptions` table and `events.message` column (used by Task 4's renderer and Task 5's tool).
  - `push_subs::Subscription { pub id: i64, pub endpoint: String, pub p256dh: String, pub auth: String }` (Debug, Clone).
  - `push_subs::add(conn, user_id: i64, endpoint: &str, p256dh: &str, auth: &str) -> anyhow::Result<()>` — upserts on endpoint conflict (re-subscription re-owns the row).
  - `push_subs::remove(conn, user_id: i64, endpoint: &str) -> anyhow::Result<bool>`.
  - `push_subs::remove_endpoint(conn, endpoint: &str) -> anyhow::Result<()>` — 410-pruning path, no user scoping.
  - `push_subs::list(conn, user_id: i64) -> anyhow::Result<Vec<Subscription>>`.
  - Routes: `POST /api/push/subscribe`, `POST /api/push/unsubscribe`, `GET /api/push/vapid_public_key` (404 until Task 3 wires the key; the route lands here so the API surface is complete in one place).
  - `AppState` gains `pub vapid_public_key: Option<String>` (set by Task 3's `with_webpush`; `AppState::new` sets `None`).

- [ ] **Step 1: Write the failing migration + module tests**

Append to `db.rs` tests:

```rust
    #[test]
    fn v4_adds_push_subscriptions_and_event_message() {
        let conn = open_memory().unwrap();
        conn.execute(
            "INSERT INTO push_subscriptions (user_id, endpoint, p256dh, auth, created_at)
             VALUES (1, 'https://push.example/x', 'k', 'a', 'now')",
            [],
        )
        .unwrap();
        let msg: String = conn
            .query_row("SELECT message FROM events WHERE 0", [], |r| r.get(0))
            .optional()
            .unwrap()
            .unwrap_or_default();
        assert_eq!(msg, "");
    }
```

Inline in `push_subs.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn conn_with_user() -> rusqlite::Connection {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn add_list_remove_roundtrip() {
        let conn = conn_with_user();
        add(&conn, 1, "https://push.example/a", "pk", "au").unwrap();
        add(&conn, 1, "https://push.example/b", "pk2", "au2").unwrap();
        let subs = list(&conn, 1).unwrap();
        assert_eq!(subs.len(), 2);
        assert!(remove(&conn, 1, "https://push.example/a").unwrap());
        assert!(!remove(&conn, 1, "https://push.example/a").unwrap());
        assert_eq!(list(&conn, 1).unwrap().len(), 1);
    }

    #[test]
    fn resubscribe_same_endpoint_upserts() {
        let conn = conn_with_user();
        add(&conn, 1, "https://push.example/a", "old", "old").unwrap();
        add(&conn, 1, "https://push.example/a", "new", "new").unwrap();
        let subs = list(&conn, 1).unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].p256dh, "new");
    }

    #[test]
    fn remove_endpoint_prunes_regardless_of_user() {
        let conn = conn_with_user();
        add(&conn, 1, "https://push.example/a", "pk", "au").unwrap();
        remove_endpoint(&conn, "https://push.example/a").unwrap();
        assert!(list(&conn, 1).unwrap().is_empty());
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test push_subs v4_adds` fails (no table, no module).

- [ ] **Step 3: Implement**

Append to `MIGRATIONS` in `db.rs`:

```rust
    // v4
    "
    CREATE TABLE push_subscriptions (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        endpoint TEXT NOT NULL UNIQUE,
        p256dh TEXT NOT NULL,
        auth TEXT NOT NULL,
        created_at TEXT NOT NULL
    );
    ALTER TABLE events ADD COLUMN message TEXT NOT NULL DEFAULT '';
    ",
```

`server/src/push_subs.rs`:

```rust
use anyhow::Result;
use rusqlite::Connection;

#[derive(Debug, Clone)]
pub struct Subscription {
    pub id: i64,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

pub fn add(conn: &Connection, user_id: i64, endpoint: &str, p256dh: &str, auth: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO push_subscriptions (user_id, endpoint, p256dh, auth, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(endpoint) DO UPDATE SET
           user_id = excluded.user_id, p256dh = excluded.p256dh, auth = excluded.auth",
        (user_id, endpoint, p256dh, auth, jiff::Timestamp::now().to_string()),
    )?;
    Ok(())
}

pub fn remove(conn: &Connection, user_id: i64, endpoint: &str) -> Result<bool> {
    let n = conn.execute(
        "DELETE FROM push_subscriptions WHERE user_id = ?1 AND endpoint = ?2",
        (user_id, endpoint),
    )?;
    Ok(n > 0)
}

/// Prune path for endpoints the push service reports gone (HTTP 404/410).
pub fn remove_endpoint(conn: &Connection, endpoint: &str) -> Result<()> {
    conn.execute("DELETE FROM push_subscriptions WHERE endpoint = ?1", [endpoint])?;
    Ok(())
}

pub fn list(conn: &Connection, user_id: i64) -> Result<Vec<Subscription>> {
    let mut stmt = conn.prepare(
        "SELECT id, endpoint, p256dh, auth FROM push_subscriptions WHERE user_id = ?1",
    )?;
    let subs = stmt
        .query_map([user_id], |r| {
            Ok(Subscription { id: r.get(0)?, endpoint: r.get(1)?, p256dh: r.get(2)?, auth: r.get(3)? })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(subs)
}
```

API routes in `api.rs` (add to router: `.route("/api/push/subscribe", post(push_subscribe))`, `.route("/api/push/unsubscribe", post(push_unsubscribe))`, `.route("/api/push/vapid_public_key", get(vapid_public_key))`):

```rust
#[derive(Deserialize)]
struct SubKeys {
    p256dh: String,
    auth: String,
}

#[derive(Deserialize)]
struct SubscribeReq {
    endpoint: String,
    keys: SubKeys,
}

const MAX_ENDPOINT_LEN: usize = 2048;

async fn push_subscribe(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<SubscribeReq>,
) -> impl IntoResponse {
    let scheme_ok = req.endpoint.starts_with("https://") || req.endpoint.starts_with("http://");
    if !scheme_ok
        || req.endpoint.len() > MAX_ENDPOINT_LEN
        || req.keys.p256dh.len() > 256
        || req.keys.auth.len() > 64
        || req.keys.p256dh.is_empty()
        || req.keys.auth.is_empty()
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let conn = state.db.lock().unwrap();
    match crate::push_subs::add(&conn, user.id, &req.endpoint, &req.keys.p256dh, &req.keys.auth) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
struct UnsubscribeReq {
    endpoint: String,
}

async fn push_unsubscribe(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<UnsubscribeReq>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::push_subs::remove(&conn, user.id, &req.endpoint) {
        Ok(true) => StatusCode::OK.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn vapid_public_key(State(state): State<AppState>) -> impl IntoResponse {
    match &state.vapid_public_key {
        Some(k) => Json(serde_json::json!({ "key": k })).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
```

`lib.rs`: add `pub vapid_public_key: Option<String>` to `AppState` (`new` sets `None`) and `pub mod push_subs;`.

- [ ] **Step 4: Integration test** — `server/tests/push_api.rs` using the existing `common::app_with_logged_in_user` helper pattern (3-tuple):

```rust
mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

fn sub_body(endpoint: &str) -> String {
    serde_json::json!({ "endpoint": endpoint, "keys": { "p256dh": "pk", "auth": "au" } }).to_string()
}

#[tokio::test]
async fn subscribe_unsubscribe_roundtrip_and_validation() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let post = |uri: &str, body: String, cookie: &str| {
        Request::post(uri)
            .header("content-type", "application/json")
            .header("cookie", cookie.to_string())
            .body(Body::from(body))
            .unwrap()
    };
    let res = app
        .clone()
        .oneshot(post("/api/push/subscribe", sub_body("https://push.example/x"), &cookie))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // bad scheme is rejected
    let res = app
        .clone()
        .oneshot(post("/api/push/subscribe", sub_body("ftp://nope"), &cookie))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // no VAPID key configured in tests → 404
    let res = app
        .clone()
        .oneshot(
            Request::get("/api/push/vapid_public_key")
                .header("cookie", cookie.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let res = app
        .clone()
        .oneshot(post(
            "/api/push/unsubscribe",
            serde_json::json!({ "endpoint": "https://push.example/x" }).to_string(),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // unsubscribing again → 404
    let res = app
        .oneshot(post(
            "/api/push/unsubscribe",
            serde_json::json!({ "endpoint": "https://push.example/x" }).to_string(),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn push_routes_require_auth() {
    let (app, _cookie, _cfg) = common::app_with_logged_in_user().await;
    let res = app
        .oneshot(
            Request::post("/api/push/subscribe")
                .header("content-type", "application/json")
                .body(Body::from(sub_body("https://push.example/x")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
```

(Adjust the `common` helper call to whatever name `tests/common/mod.rs` actually exports — keep the 3-tuple binding so the TempDir lives.)

- [ ] **Step 5: Run all tests, commit** — `cargo test`; commit `feat: push subscription storage and API`.

---

### Task 2: Channel trait, WebSocket hub + channel, /api/ws route

**Files:**
- Create: `server/src/channels/mod.rs`, `server/src/channels/ws.rs`, `server/src/channels/mock.rs`
- Modify: `server/src/lib.rs` (AppState gains `hub` + `channels`; `pub mod channels;`), `server/src/api.rs` (ws route)
- Test: inline in `channels/ws.rs` and `channels/mock.rs`

**Interfaces:**
- Consumes: `AppState`, `CurrentUser`.
- Produces:
  - `channels::Urgency { Low, Normal, High }` (Clone, Copy, Debug, PartialEq).
  - `channels::OutboundMessage { pub title: String, pub body: String, pub urgency: Urgency, pub event_id: Option<i64> }` (Clone, Debug).
  - `channels::Channel: Send + Sync { fn name(&self) -> &'static str; fn deliver(&self, user_id: i64, username: &str, msg: &OutboundMessage) -> anyhow::Result<()> }` — `deliver` returns `Err` when this channel cannot reach the user (no clients connected, no subscriptions); the dispatcher (Task 4) falls through the ladder.
  - `channels::ws::ClientHub::new() -> ClientHub`; `register(&self, user_id: i64) -> (u64, tokio::sync::mpsc::UnboundedReceiver<String>)`; `unregister(&self, user_id: i64, conn_id: u64)`; `send(&self, user_id: i64, text: &str) -> usize` (receivers reached; prunes closed senders).
  - `channels::ws::WsChannel::new(hub: Arc<ClientHub>) -> WsChannel` implementing `Channel` (name `"ws"`); `deliver` sends `{"type":"event","title":…,"body":…,"urgency":…,"event_id":…}` JSON and errors when `send` reached 0 receivers.
  - `channels::mock::MockChannel::new(name: &'static str) -> MockChannel` implementing `Channel`: `set_fail(bool)`, `seen() -> Vec<(i64, OutboundMessage)>` — a public mock, mirroring `providers::mock`, so integration tests can use it.
  - `AppState` gains `pub hub: Arc<channels::ws::ClientHub>` and `pub channels: Vec<Arc<dyn channels::Channel>>`; `AppState::new` creates a fresh hub and a ladder of just `WsChannel` over it. Builder `pub fn with_channels(mut self, channels: Vec<Arc<dyn channels::Channel>>) -> Self`.
  - Route `GET /api/ws` (session-cookie auth via `CurrentUser`), upgrading and pumping hub messages to the socket.

- [ ] **Step 1: Write the failing hub/channel tests**

Inline in `channels/ws.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::{Channel, OutboundMessage, Urgency};
    use std::sync::Arc;

    fn msg() -> OutboundMessage {
        OutboundMessage {
            title: "Check-in".into(),
            body: "at 09:00".into(),
            urgency: Urgency::Normal,
            event_id: Some(7),
        }
    }

    #[test]
    fn hub_delivers_to_all_connections_of_the_user_only() {
        let hub = ClientHub::new();
        let (_id1, mut rx1) = hub.register(1);
        let (_id2, mut rx2) = hub.register(1);
        let (_id3, mut rx3) = hub.register(2);
        assert_eq!(hub.send(1, "hello"), 2);
        assert_eq!(rx1.try_recv().unwrap(), "hello");
        assert_eq!(rx2.try_recv().unwrap(), "hello");
        assert!(rx3.try_recv().is_err());
    }

    #[test]
    fn unregister_and_dropped_receivers_stop_counting() {
        let hub = ClientHub::new();
        let (id1, rx1) = hub.register(1);
        let (_id2, _rx2) = hub.register(1);
        hub.unregister(1, id1);
        drop(rx1);
        drop(_rx2);
        assert_eq!(hub.send(1, "x"), 0);
    }

    #[test]
    fn ws_channel_errors_when_nobody_is_connected() {
        let hub = Arc::new(ClientHub::new());
        let ch = WsChannel::new(hub.clone());
        assert!(ch.deliver(1, "aki", &msg()).is_err());
        let (_id, mut rx) = hub.register(1);
        ch.deliver(1, "aki", &msg()).unwrap();
        let text = rx.try_recv().unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["type"], "event");
        assert_eq!(v["title"], "Check-in");
        assert_eq!(v["event_id"], 7);
    }
}
```

Inline in `channels/mock.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::{Channel, OutboundMessage, Urgency};

    #[test]
    fn mock_records_and_fails_on_demand() {
        let ch = MockChannel::new("mock");
        let m = OutboundMessage { title: "t".into(), body: "b".into(), urgency: Urgency::Low, event_id: None };
        ch.deliver(1, "aki", &m).unwrap();
        ch.set_fail(true);
        assert!(ch.deliver(1, "aki", &m).is_err());
        let seen = ch.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, 1);
        assert_eq!(seen[0].1.title, "t");
    }
}
```

- [ ] **Step 2: Verify failure** — modules don't exist.

- [ ] **Step 3: Implement**

`channels/mod.rs`:

```rust
pub mod mock;
pub mod ws;
// Task 3 adds `pub mod webpush;` when that file exists.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Urgency {
    Low,
    Normal,
    High,
}

#[derive(Clone, Debug)]
pub struct OutboundMessage {
    pub title: String,
    pub body: String,
    pub urgency: Urgency,
    pub event_id: Option<i64>,
}

/// A delivery mechanism. `deliver` returns `Err` when this channel cannot
/// currently reach the user, so the dispatcher can fall through the ladder.
pub trait Channel: Send + Sync {
    fn name(&self) -> &'static str;
    fn deliver(&self, user_id: i64, username: &str, msg: &OutboundMessage) -> anyhow::Result<()>;
}
```

`channels/ws.rs`:

```rust
use super::{Channel, OutboundMessage, Urgency};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

#[derive(Default)]
pub struct ClientHub {
    next_id: AtomicU64,
    conns: Mutex<HashMap<i64, Vec<(u64, UnboundedSender<String>)>>>,
}

impl ClientHub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, user_id: i64) -> (u64, UnboundedReceiver<String>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = unbounded_channel();
        self.conns.lock().unwrap().entry(user_id).or_default().push((id, tx));
        (id, rx)
    }

    pub fn unregister(&self, user_id: i64, conn_id: u64) {
        let mut conns = self.conns.lock().unwrap();
        if let Some(v) = conns.get_mut(&user_id) {
            v.retain(|(id, _)| *id != conn_id);
            if v.is_empty() {
                conns.remove(&user_id);
            }
        }
    }

    /// Sends to every live connection of `user_id`, pruning closed ones, and
    /// returns how many actually received it.
    pub fn send(&self, user_id: i64, text: &str) -> usize {
        let mut conns = self.conns.lock().unwrap();
        let Some(v) = conns.get_mut(&user_id) else { return 0 };
        v.retain(|(_, tx)| tx.send(text.to_string()).is_ok());
        let n = v.len();
        if v.is_empty() {
            conns.remove(&user_id);
        }
        n
    }
}

pub struct WsChannel {
    hub: Arc<ClientHub>,
}

impl WsChannel {
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self { hub }
    }
}

impl Channel for WsChannel {
    fn name(&self) -> &'static str {
        "ws"
    }

    fn deliver(&self, user_id: i64, _username: &str, msg: &OutboundMessage) -> anyhow::Result<()> {
        let urgency = match msg.urgency {
            Urgency::Low => "low",
            Urgency::Normal => "normal",
            Urgency::High => "high",
        };
        let text = serde_json::json!({
            "type": "event",
            "title": msg.title,
            "body": msg.body,
            "urgency": urgency,
            "event_id": msg.event_id,
        })
        .to_string();
        if self.hub.send(user_id, &text) == 0 {
            anyhow::bail!("no connected clients");
        }
        Ok(())
    }
}
```

`channels/mock.rs`:

```rust
use super::{Channel, OutboundMessage};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Recording channel for tests; public for integration tests, mirroring
/// `providers::mock`.
pub struct MockChannel {
    name: &'static str,
    fail: AtomicBool,
    seen: Mutex<Vec<(i64, OutboundMessage)>>,
}

impl MockChannel {
    pub fn new(name: &'static str) -> Self {
        Self { name, fail: AtomicBool::new(false), seen: Mutex::new(Vec::new()) }
    }

    pub fn set_fail(&self, fail: bool) {
        self.fail.store(fail, Ordering::Relaxed);
    }

    pub fn seen(&self) -> Vec<(i64, OutboundMessage)> {
        self.seen.lock().unwrap().clone()
    }
}

impl Channel for MockChannel {
    fn name(&self) -> &'static str {
        self.name
    }

    fn deliver(&self, user_id: i64, _username: &str, msg: &OutboundMessage) -> anyhow::Result<()> {
        if self.fail.load(Ordering::Relaxed) {
            anyhow::bail!("mock channel set to fail");
        }
        self.seen.lock().unwrap().push((user_id, msg.clone()));
        Ok(())
    }
}
```

`lib.rs` — `AppState` gains:

```rust
    pub hub: Arc<crate::channels::ws::ClientHub>,
    pub channels: Vec<Arc<dyn crate::channels::Channel>>,
```

In `new()`:

```rust
        let hub = Arc::new(crate::channels::ws::ClientHub::new());
        let ws: Arc<dyn crate::channels::Channel> =
            Arc::new(crate::channels::ws::WsChannel::new(hub.clone()));
        // …existing fields…, hub, channels: vec![ws],
```

plus:

```rust
    pub fn with_channels(mut self, channels: Vec<Arc<dyn crate::channels::Channel>>) -> Self {
        self.channels = channels;
        self
    }
```

`api.rs` — route `.route("/api/ws", get(ws_connect))` and:

```rust
/// Bridges hub messages to the socket; inbound frames are drained and ignored
/// (delivery is one-way in v1), and either side closing tears the bridge down.
async fn ws_connect(
    user: CurrentUser,
    State(state): State<AppState>,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_pump(socket, state.hub.clone(), user.id))
}

async fn ws_pump(
    mut socket: axum::extract::ws::WebSocket,
    hub: std::sync::Arc<crate::channels::ws::ClientHub>,
    user_id: i64,
) {
    let (conn_id, mut rx) = hub.register(user_id);
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(text) => {
                    if socket.send(axum::extract::ws::Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            inbound = socket.recv() => match inbound {
                Some(Ok(_)) => {}
                _ => break,
            },
        }
    }
    hub.unregister(user_id, conn_id);
}
```

(axum 0.8 re-exports WebSocket support from `axum::extract::ws`; the `ws` feature may need enabling in `server/Cargo.toml`: `axum = { version = "0.8", features = ["ws"] }`.)

- [ ] **Step 4: Run all tests** — hub/mock tests pass; whole suite green (AppState construction sites need no edits — only `new` changed internally).

- [ ] **Step 5: Commit** — `feat: channel trait, websocket hub and in-app delivery route`.

---

### Task 3: Web Push channel

**Files:**
- Create: `server/src/channels/webpush.rs`
- Modify: `server/src/channels/mod.rs` (add `pub mod webpush;`), `server/src/config.rs` (channels section), `server/src/lib.rs` (`with_webpush`), `server/src/main.rs` (wire when configured), `server/Cargo.toml` (deps)
- Test: inline fixture tests in `channels/webpush.rs` + config test

**Interfaces:**
- Consumes: `push_subs::{list, remove_endpoint, Subscription}` (Task 1), `channels::{Channel, OutboundMessage, Urgency}` (Task 2).
- Produces:
  - `config::WebPushSettings { pub vapid_pem_file: PathBuf, pub subject: String }` (Deserialize, Clone, Debug).
  - `config::ChannelsConfig { pub webpush: Option<WebPushSettings> }` (Deserialize, Default, `#[serde(default)]`); `ServerConfig` gains `#[serde(default)] pub channels: ChannelsConfig`.
  - `webpush::BuiltPush { pub endpoint: String, pub headers: Vec<(String, String)>, pub body: Vec<u8> }`.
  - `webpush::build_push(sub: &Subscription, vapid_pem: &[u8], subject: &str, msg: &OutboundMessage) -> anyhow::Result<BuiltPush>` — pure: VAPID signature + aes128gcm payload via the `web-push` crate builders, no I/O.
  - `webpush::public_key_b64(vapid_pem: &[u8]) -> anyhow::Result<String>` — base64url (no pad) of the VAPID public key.
  - `webpush::WebPushChannel::new(db: Arc<Mutex<Connection>>, vapid_pem: Vec<u8>, subject: String) -> anyhow::Result<Self>` implementing `Channel` (name `"webpush"`): lists subscriptions under a short lock, releases, POSTs each via `ureq` (connect 5s, overall 15s), prunes 404/410 endpoints under a fresh short lock, succeeds if ≥1 delivery succeeded.
  - `AppState::with_webpush(mut self, ch: WebPushChannel, public_key: String) -> Self` — appends to the ladder and sets `vapid_public_key`.

**Dependencies** (`server/Cargo.toml`):

```toml
web-push = { version = "0.10", default-features = false }
base64 = "0.22"
```

If `web-push 0.10` without default features fails to expose `VapidSignatureBuilder`/`WebPushMessageBuilder`, fall back to `version = "0.9"` with the same shape — the builder API is stable across both; note the deviation in your report.

- [ ] **Step 1: Write the failing fixture tests**

The test key below is a throwaway P-256 key generated for this repo's tests only (never a real deployment key); the subscription keys are the RFC 8291 test-vector values.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::{OutboundMessage, Urgency};
    use crate::push_subs::Subscription;

    const TEST_PEM: &[u8] = b"-----BEGIN EC PRIVATE KEY-----
MHcCAQEEIHmZ5O6AfVwy/vYIs4KDabU6mZnBmFw1RV7wfeQ0LB7goAoGCCqGSM49
AwEHoUQDQgAEA7nqkgOVsRzMWh/T0AnwWx4Nep2dfQAns3Mn1OkO/t/+V/Voqszi
v5mC8db8ZSK9ruR2mEgvMEvePYwohpr98g==
-----END EC PRIVATE KEY-----
";

    fn sub() -> Subscription {
        Subscription {
            id: 1,
            endpoint: "https://push.example.net/send/abc".into(),
            p256dh: "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4".into(),
            auth: "BTBZMqHH6r4Tts7J_aSIgg".into(),
        }
    }

    fn msg() -> OutboundMessage {
        OutboundMessage {
            title: "Nudge".into(),
            body: "stretch break".into(),
            urgency: Urgency::Normal,
            event_id: Some(3),
        }
    }

    #[test]
    fn build_push_encrypts_and_signs() {
        let built = build_push(&sub(), TEST_PEM, "mailto:admin@example.com", &msg()).unwrap();
        assert_eq!(built.endpoint, "https://push.example.net/send/abc");
        let get = |name: &str| {
            built
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        };
        let auth = get("authorization").expect("vapid authorization header");
        assert!(auth.starts_with("vapid"), "unexpected auth header: {auth}");
        assert_eq!(get("content-encoding").as_deref(), Some("aes128gcm"));
        assert!(get("ttl").is_some());
        // encrypted body: non-empty and not the plaintext payload
        assert!(!built.body.is_empty());
        let plain = serde_json::json!({ "title": "Nudge", "body": "stretch break" }).to_string();
        assert_ne!(built.body, plain.as_bytes());
    }

    #[test]
    fn public_key_is_base64url() {
        let key = public_key_b64(TEST_PEM).unwrap();
        assert!(!key.is_empty());
        assert!(!key.contains('+') && !key.contains('/') && !key.contains('='));
    }

    #[test]
    fn bad_pem_is_an_error() {
        assert!(public_key_b64(b"not a pem").is_err());
        assert!(build_push(&sub(), b"not a pem", "mailto:x@y", &msg()).is_err());
    }
}
```

Config test in `config.rs`:

```rust
    #[test]
    fn channels_section_parses_and_defaults_empty() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "server.toml", concat!(
            "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n",
            "[channels.webpush]\nvapid_pem_file = \"config/vapid.pem\"\nsubject = \"mailto:admin@example.com\"\n"));
        let cfg = ServerConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.channels.webpush.unwrap().subject, "mailto:admin@example.com");
        write(tmp.path(), "server.toml",
            "bind_addr = \"127.0.0.1:0\"\npublic_base_url = \"http://x\"\ndata_dir = \"data\"\n");
        assert!(ServerConfig::load(tmp.path()).unwrap().channels.webpush.is_none());
    }
```

- [ ] **Step 2: Verify failure**, add the dependencies, **implement**

`channels/webpush.rs`:

```rust
use super::{Channel, OutboundMessage, Urgency};
use crate::push_subs::{self, Subscription};
use anyhow::{Context, Result};
use base64::Engine;
use rusqlite::Connection;
use std::sync::{Arc, Mutex};
use web_push::WebPushMessageBuilder;

pub struct BuiltPush {
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Builds the encrypted, VAPID-signed request for one subscription — pure so
/// tests can pin the wire shape without any network.
pub fn build_push(
    sub: &Subscription,
    vapid_pem: &[u8],
    subject: &str,
    msg: &OutboundMessage,
) -> Result<BuiltPush> {
    let info = web_push::SubscriptionInfo::new(&sub.endpoint, &sub.p256dh, &sub.auth);
    let mut sig = web_push::VapidSignatureBuilder::from_pem(vapid_pem, &info)
        .map_err(|e| anyhow::anyhow!("vapid key: {e}"))?;
    sig.add_claim("sub", subject);
    let signature = sig.build().map_err(|e| anyhow::anyhow!("vapid sign: {e}"))?;

    let payload = serde_json::json!({ "title": msg.title, "body": msg.body }).to_string();
    let mut b = WebPushMessageBuilder::new(&info);
    b.set_payload(web_push::ContentEncoding::Aes128Gcm, payload.as_bytes());
    b.set_vapid_signature(signature);
    b.set_ttl(3600);
    let m = b.build().map_err(|e| anyhow::anyhow!("build push: {e}"))?;

    let p = m.payload.context("payload always set")?;
    let mut headers: Vec<(String, String)> =
        p.crypto_headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-encoding")) {
        headers.push(("Content-Encoding".into(), "aes128gcm".into()));
    }
    headers.push(("TTL".into(), m.ttl.to_string()));
    let urgency = match msg.urgency {
        Urgency::Low => "low",
        Urgency::Normal => "normal",
        Urgency::High => "high",
    };
    headers.push(("Urgency".into(), urgency.into()));
    Ok(BuiltPush { endpoint: m.endpoint.to_string(), headers, body: p.content })
}

pub fn public_key_b64(vapid_pem: &[u8]) -> Result<String> {
    let partial = web_push::VapidSignatureBuilder::from_pem_no_sub(vapid_pem)
        .map_err(|e| anyhow::anyhow!("vapid key: {e}"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(partial.get_public_key()))
}

pub struct WebPushChannel {
    db: Arc<Mutex<Connection>>,
    vapid_pem: Vec<u8>,
    subject: String,
    agent: ureq::Agent,
}

impl WebPushChannel {
    pub fn new(db: Arc<Mutex<Connection>>, vapid_pem: Vec<u8>, subject: String) -> Result<Self> {
        public_key_b64(&vapid_pem)?; // reject an unusable key at startup, not at first delivery
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(15))
            .build();
        Ok(Self { db, vapid_pem, subject, agent })
    }

    fn send(&self, built: &BuiltPush) -> std::result::Result<(), ureq::Error> {
        let mut req = self.agent.post(&built.endpoint);
        for (k, v) in &built.headers {
            req = req.set(k, v);
        }
        req.send_bytes(&built.body).map(|_| ())
    }
}

impl Channel for WebPushChannel {
    fn name(&self) -> &'static str {
        "webpush"
    }

    /// Lists subscriptions under a short lock, pushes with the lock released,
    /// prunes endpoints the push service reports gone (404/410).
    fn deliver(&self, user_id: i64, _username: &str, msg: &OutboundMessage) -> Result<()> {
        let subs = {
            let conn = self.db.lock().unwrap();
            push_subs::list(&conn, user_id)?
        };
        if subs.is_empty() {
            anyhow::bail!("no push subscriptions");
        }
        let mut delivered = 0usize;
        let mut gone: Vec<String> = Vec::new();
        let mut last_err = String::new();
        for sub in &subs {
            match build_push(sub, &self.vapid_pem, &self.subject, msg) {
                Err(e) => last_err = e.to_string(),
                Ok(built) => match self.send(&built) {
                    Ok(()) => delivered += 1,
                    Err(ureq::Error::Status(code, _)) if code == 404 || code == 410 => {
                        gone.push(sub.endpoint.clone());
                    }
                    Err(e) => last_err = e.to_string(),
                },
            }
        }
        if !gone.is_empty() {
            let conn = self.db.lock().unwrap();
            for endpoint in &gone {
                let _ = push_subs::remove_endpoint(&conn, endpoint);
            }
        }
        if delivered == 0 {
            anyhow::bail!("no push delivery succeeded: {last_err}");
        }
        Ok(())
    }
}
```

(`ureq` 2.x `AgentBuilder` method names may differ slightly — `timeout_connect` vs `.timeout_connect()`; match what `providers::http_agent` in `server/src/providers/mod.rs` already uses.)

`config.rs` additions:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct WebPushSettings {
    pub vapid_pem_file: PathBuf,
    pub subject: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ChannelsConfig {
    pub webpush: Option<WebPushSettings>,
}
```

and `ServerConfig` gains `#[serde(default)] pub channels: ChannelsConfig`.

`lib.rs`:

```rust
    pub fn with_webpush(mut self, ch: crate::channels::webpush::WebPushChannel, public_key: String) -> Self {
        self.channels.push(Arc::new(ch));
        self.vapid_public_key = Some(public_key);
        self
    }
```

`main.rs`, after `with_providers`:

```rust
    let mut state =
        AppState::new(conn, config_dir, cfg.data_dir.clone()).with_providers(llm, embeddings);
    if let Some(wp) = &cfg.channels.webpush {
        let pem = std::fs::read(&wp.vapid_pem_file)
            .with_context(|| format!("reading {}", wp.vapid_pem_file.display()))?;
        let public_key = channels::webpush::public_key_b64(&pem)?;
        let ch = channels::webpush::WebPushChannel::new(state.db.clone(), pem, wp.subject.clone())?;
        state = state.with_webpush(ch, public_key);
    }
```

(add `channels` to the `use note_server::{…}` list).

- [ ] **Step 3: Run all tests, commit** — `cargo test`; commit `feat: web push channel with vapid signing and subscription pruning`.

---

### Task 4: Delivery dispatcher and runner wiring

**Files:**
- Modify: `server/src/channels/mod.rs` (renderer + dispatcher), `server/src/runner.rs` (`FiredEvent`, delivery off-lock, session GC)
- Modify (ripple): `server/tests/full_day.rs` (fire_due return type)
- Test: inline in `channels/mod.rs` and `runner.rs`

**Interfaces:**
- Consumes: `Channel` ladder (Task 2/3), `debriefs` table (phase 3), `events.message` (Task 1).
- Produces:
  - `runner::FiredEvent { pub event_id: i64, pub user_id: i64, pub username: String, pub kind: String, pub wall_time: String, pub date: String, pub channel: String, pub message: String }` (Clone, Debug).
  - `runner::fire_due(conn, config_dir, now) -> Result<Vec<FiredEvent>>` (was `Vec<i64>` — ripple below).
  - `channels::render(conn: &Connection, ev: &runner::FiredEvent) -> OutboundMessage` — pure code: check-in kinds → title "Check-in" / urgency High; kind `debrief` → title "Good morning", body = that date's debrief content (or "(no debrief yet)"); anything else → title = kind, body "scheduled for {wall_time}"; a non-empty `ev.message` always overrides the body.
  - `channels::deliver_event(db: &Mutex<Connection>, ladder: &[Arc<dyn Channel>], ev: &runner::FiredEvent)` — renders under a short lock, walks the ladder with the lock released, logs `delivery_ok` (with channel name) on first success or `delivery_degraded` (with every channel's error) when all fail; `ev.channel == "voice"` additionally logs `voice_unavailable` first (spec: failed calls fall back to web push). Never returns an error.
  - `runner::spawn` — each sweep: under the lock, GC expired sessions (`DELETE FROM sessions WHERE expires_at < now`) and `fire_due`; then with the lock **released**, deliver all fired events inside one `spawn_blocking`.

**Ripple sites for the `fire_due` return change:** `runner.rs` tests (`fired.len()` still works; `assert_eq!(fire_due(..), vec![good_id])` becomes a comparison on `.iter().map(|f| f.event_id)`), `server/tests/full_day.rs` (`fired_midday.iter().map(|id| …)` → `.map(|f| f.event_id)`; kind assertions can read `f.kind` directly instead of re-querying).

- [ ] **Step 1: Write the failing tests**

Inline in `channels/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::mock::MockChannel;
    use super::*;
    use crate::runner::FiredEvent;
    use std::sync::{Arc, Mutex};

    fn env() -> (Mutex<rusqlite::Connection>, i64) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        (Mutex::new(conn), 1)
    }

    fn ev(kind: &str, channel: &str, message: &str) -> FiredEvent {
        FiredEvent {
            event_id: 11,
            user_id: 1,
            username: "aki".into(),
            kind: kind.into(),
            wall_time: "09:00".into(),
            date: "2026-08-31".into(),
            channel: channel.into(),
            message: message.into(),
        }
    }

    #[test]
    fn render_uses_debrief_content_and_message_override() {
        let (db, _uid) = env();
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO debriefs (user_id, date, content, created_at)
             VALUES (1, '2026-08-31', 'slept well, one task rolled', 'now')",
            [],
        )
        .unwrap();
        let m = render(&conn, &ev("debrief", "push", ""));
        assert_eq!(m.title, "Good morning");
        assert!(m.body.contains("slept well"));

        let m = render(&conn, &ev("checkin_call", "push", ""));
        assert_eq!(m.title, "Check-in");
        assert_eq!(m.urgency, Urgency::High);

        let m = render(&conn, &ev("nudge", "push", "you wanted a stretch break"));
        assert_eq!(m.body, "you wanted a stretch break");
    }

    #[test]
    fn dispatcher_falls_through_ladder_and_logs() {
        let (db, _uid) = env();
        let first = Arc::new(MockChannel::new("first"));
        let second = Arc::new(MockChannel::new("second"));
        first.set_fail(true);
        let ladder: Vec<Arc<dyn Channel>> = vec![first.clone(), second.clone()];

        deliver_event(&db, &ladder, &ev("nudge", "push", ""));
        assert!(first.seen().is_empty());
        assert_eq!(second.seen().len(), 1);
        let conn = db.lock().unwrap();
        let detail: String = conn
            .query_row("SELECT detail FROM event_log WHERE kind='delivery_ok'", [], |r| r.get(0))
            .unwrap();
        assert!(detail.contains("second"), "unexpected detail: {detail}");
    }

    #[test]
    fn all_channels_failing_logs_degraded_not_error() {
        let (db, _uid) = env();
        let only = Arc::new(MockChannel::new("only"));
        only.set_fail(true);
        let ladder: Vec<Arc<dyn Channel>> = vec![only];
        deliver_event(&db, &ladder, &ev("nudge", "push", ""));
        let conn = db.lock().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='delivery_degraded'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn voice_channel_logs_unavailable_then_falls_back() {
        let (db, _uid) = env();
        let push = Arc::new(MockChannel::new("push"));
        let ladder: Vec<Arc<dyn Channel>> = vec![push.clone()];
        deliver_event(&db, &ladder, &ev("checkin_call", "voice", ""));
        assert_eq!(push.seen().len(), 1);
        let conn = db.lock().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='voice_unavailable'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
```

In `runner.rs` tests, add:

```rust
    #[test]
    fn expired_sessions_are_garbage_collected() {
        let (conn, _tmp, uid) = setup("UTC");
        conn.execute(
            "INSERT INTO sessions (token, user_id, expires_at) VALUES ('old', ?1, '2020-01-01T00:00:00Z')",
            [uid],
        ).unwrap();
        conn.execute(
            "INSERT INTO sessions (token, user_id, expires_at) VALUES ('new', ?1, '2099-01-01T00:00:00Z')",
            [uid],
        ).unwrap();
        gc_sessions(&conn, "2026-08-31T00:00:00Z".parse().unwrap()).unwrap();
        let left: i64 = conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 1);
    }

    #[test]
    fn fired_events_carry_kind_channel_and_message() {
        let (conn, tmp, uid) = setup("UTC");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, uid, &one_event_template("00:00"), date).unwrap();
        conn.execute("UPDATE events SET message='remember the thing'", []).unwrap();
        let now: jiff::Timestamp = "2026-08-31T12:00:00Z".parse().unwrap();
        let fired = fire_due(&conn, tmp.path(), now).unwrap();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].kind, "nudge");
        assert_eq!(fired[0].channel, "push");
        assert_eq!(fired[0].message, "remember the thing");
        assert_eq!(fired[0].date, "2026-08-31");
    }
```

- [ ] **Step 2: Verify failure, implement**

`runner.rs`:

```rust
#[derive(Clone, Debug)]
pub struct FiredEvent {
    pub event_id: i64,
    pub user_id: i64,
    pub username: String,
    pub kind: String,
    pub wall_time: String,
    pub date: String,
    pub channel: String,
    pub message: String,
}
```

Extend the `fire_due` SELECT to `e.id, p.user_id, u.username, p.date, e.wall_time, e.kind, e.channel, e.message` (the `Candidate` struct grows the three fields), and on fire push `FiredEvent { event_id: c.event_id, user_id: c.user_id, username: c.username.clone(), kind: c.kind.clone(), wall_time: c.wall_time.clone(), date: c.date.clone(), channel: c.channel.clone(), message: c.message.clone() }`.

```rust
/// Expired sessions only ever accumulate; sweeping them here keeps logout and
/// expiry cheap without a dedicated task. RFC 3339 UTC strings compare
/// lexicographically, so the string comparison is correct.
pub fn gc_sessions(conn: &Connection, now: jiff::Timestamp) -> Result<()> {
    conn.execute("DELETE FROM sessions WHERE expires_at < ?1", [now.to_string()])?;
    Ok(())
}

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tick.tick().await;
            let fired = {
                let conn = state.db.lock().unwrap();
                let now = jiff::Timestamp::now();
                let _ = gc_sessions(&conn, now);
                match fire_due(&conn, &state.config_dir, now) {
                    Ok(f) => f,
                    Err(e) => {
                        let _ = crate::log::record(&conn, None, "runner_error", &e.to_string());
                        Vec::new()
                    }
                }
            };
            if fired.is_empty() {
                continue;
            }
            let st = state.clone();
            let _ = tokio::task::spawn_blocking(move || {
                for ev in &fired {
                    crate::channels::deliver_event(&st.db, &st.channels, ev);
                }
            })
            .await;
        }
    });
}
```

`channels/mod.rs` additions:

```rust
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

/// Pure rendering of a fired event into a user-facing message; a non-empty
/// per-event `message` (set by `notify_send`) overrides the generic body.
pub fn render(conn: &Connection, ev: &crate::runner::FiredEvent) -> OutboundMessage {
    let (title, mut body, urgency) = if ev.kind.contains("checkin") {
        ("Check-in".to_string(), format!("{} at {}", ev.kind, ev.wall_time), Urgency::High)
    } else if ev.kind == "debrief" {
        let content: String = conn
            .query_row(
                "SELECT content FROM debriefs WHERE user_id = ?1 AND date = ?2",
                (ev.user_id, &ev.date),
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "(no debrief yet)".into());
        ("Good morning".to_string(), content, Urgency::Normal)
    } else {
        (ev.kind.clone(), format!("scheduled for {}", ev.wall_time), Urgency::Normal)
    };
    if !ev.message.is_empty() {
        body = ev.message.clone();
    }
    OutboundMessage { title, body, urgency, event_id: Some(ev.event_id) }
}

/// Walks the ladder until one channel delivers; every outcome is logged and
/// none propagates — a failed delivery is a plainer day, never an error.
pub fn deliver_event(
    db: &Mutex<Connection>,
    ladder: &[Arc<dyn Channel>],
    ev: &crate::runner::FiredEvent,
) {
    let msg = {
        let conn = db.lock().unwrap();
        if ev.channel == "voice" {
            let _ = crate::log::record(
                &conn,
                Some(ev.user_id),
                "voice_unavailable",
                &format!("event {}: voice not implemented, using push ladder", ev.event_id),
            );
        }
        render(&conn, ev)
    };
    let mut errors: Vec<String> = Vec::new();
    for ch in ladder {
        match ch.deliver(ev.user_id, &ev.username, &msg) {
            Ok(()) => {
                let conn = db.lock().unwrap();
                let _ = crate::log::record(
                    &conn,
                    Some(ev.user_id),
                    "delivery_ok",
                    &format!("event {} via {}", ev.event_id, ch.name()),
                );
                return;
            }
            Err(e) => errors.push(format!("{}: {e}", ch.name())),
        }
    }
    let conn = db.lock().unwrap();
    let _ = crate::log::record(
        &conn,
        Some(ev.user_id),
        "delivery_degraded",
        &format!("event {}: {}", ev.event_id, errors.join("; ")),
    );
}
```

Update the ripple sites listed above (runner tests, `full_day.rs`).

- [ ] **Step 3: Run all tests, commit** — `cargo test`; commit `feat: fired events deliver through the channel ladder`.

---

### Task 5: notify_send outreach tool and slide-on-decided guard

**Files:**
- Create: `server/src/tools/outreach_ops.rs`
- Modify: `server/src/tools/mod.rs` (registry, describe, run), `server/src/plan.rs` (`ShiftError::Decided` + guard), `server/src/tools/schedule_ops.rs` (map new error), `server/src/api.rs` (shift handler 409 arm)
- Test: inline in `outreach_ops.rs` + `plan.rs`

**Interfaces:**
- Consumes: `plan::generate`, `templates::Template`, `config::UserConfig`, `events.message` column (Task 1); delivery happens via the runner sweep (Task 4) — the tool only inserts, keeping `dispatch` free of network I/O.
- Produces:
  - `outreach_ops::SendArgs { pub text: String }` (Deserialize, JsonSchema, `deny_unknown_fields` like the other tool args).
  - `outreach_ops::send(conn, ctx, args) -> Result<serde_json::Value, ToolError>` — inserts an immediate droppable `nudge` event into today's plan (user-tz date, wall_time = current user-tz `HH:MM`, `channel='push'`, `message = text`); returns `{"event_id": …, "wall_time": …}`. The runner delivers it within one sweep (≤30 s).
  - Registry: `notify_send` appended to `NIGHTLY` only (subset invariant preserved). `describe` gains an arm: "Send the user a push nudge with this text (delivered within a minute)."
  - `plan::ShiftError` gains `#[error("event already {status}")] Decided { status: String }`; `plan::shift` returns it for `done`/`dropped` events instead of silently moving `wall_time`. `schedule_ops::slide` maps it to `ToolError::rejected`; the API shift handler maps it to `409 CONFLICT`.

- [ ] **Step 1: Write the failing tests**

Inline in `outreach_ops.rs`:

```rust
#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, SessionKind, ToolCtx};

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let write = |rel: &str, c: &str| {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        };
        write(
            "defaults/user.toml",
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n",
        );
        write("defaults/templates/default.toml", "events = []\n");
        (conn, tmp)
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir) -> ToolCtx<'a> {
        ToolCtx {
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            user_id: 1,
            username: "aki",
            embeddings: None,
        }
    }

    #[test]
    fn notify_send_inserts_an_immediate_droppable_nudge() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "notify_send",
            r#"{"text":"stretch break, you asked for it"}"#).unwrap();
        let id = out["event_id"].as_i64().unwrap();
        let (kind, flex, message, status): (String, String, String, String) = conn
            .query_row(
                "SELECT kind, flexibility, message, status FROM events WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(kind, "nudge");
        assert_eq!(flex, "drop");
        assert_eq!(message, "stretch break, you asked for it");
        assert_eq!(status, "pending");
    }

    #[test]
    fn notify_send_is_nightly_only_and_validates_text() {
        let (conn, tmp) = env();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "notify_send", r#"{"text":"x"}"#)
            .unwrap_err();
        assert_eq!(e.kind, "forbidden");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "notify_send", r#"{"text":"  "}"#)
            .unwrap_err();
        assert_eq!(e.kind, "rejected");
        let big = format!(r#"{{"text":"{}"}}"#, "x".repeat(16 * 1024 + 1));
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "notify_send", &big).unwrap_err();
        assert_eq!(e.kind, "rejected");
    }
}
```

(This task predates Task 8, so the test ctx uses the current `embeddings: None` field; Task 8's sweep renames it along with every other construction site.)

In `plan.rs` tests:

```rust
    #[test]
    fn shift_rejects_done_and_dropped_events() {
        use crate::templates::{Template, TemplateEvent};
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        let tmpl = Template {
            events: vec![TemplateEvent {
                kind: "nudge".into(),
                time: "09:00".into(),
                days: ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
                    .iter().map(|d| d.to_string()).collect(),
                flexibility: "slide".into(),
                slide_window_min: 120,
                channel: "push".into(),
            }],
        };
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, 1, &tmpl, date).unwrap();
        let id = events_for(&conn, 1, date).unwrap()[0].id;
        set_status(&conn, 1, id, "done").unwrap();
        let err = shift(&conn, 1, id, 30).unwrap_err();
        assert!(matches!(err, ShiftError::Decided { .. }), "got {err:?}");
        let wall: String = conn
            .query_row("SELECT wall_time FROM events WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        assert_eq!(wall, "09:00");
    }
```

(Mirror the surrounding tests' construction style if it differs; keep the two assertions — typed rejection AND unchanged `wall_time`.)

- [ ] **Step 2: Verify failure, implement**

`tools/outreach_ops.rs`:

```rust
use super::{check_text, ToolCtx, ToolError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendArgs {
    /// The nudge text shown to the user.
    pub text: String,
}

/// Inserts an immediate droppable nudge into today's plan; the runner delivers
/// it on its next sweep, so dispatch itself never touches the network.
pub fn send(conn: &Connection, ctx: &ToolCtx, args: SendArgs) -> Result<serde_json::Value, ToolError> {
    check_text("text", &args.text)?;
    if args.text.trim().is_empty() {
        return Err(ToolError::rejected("text must not be empty"));
    }
    let ucfg = crate::config::UserConfig::load(ctx.config_dir, ctx.username)
        .map_err(|e| ToolError::internal(e.to_string()))?;
    let tz = jiff::tz::TimeZone::get(&ucfg.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
    let local = jiff::Timestamp::now().to_zoned(tz);
    let wall = format!("{:02}:{:02}", local.hour(), local.minute());
    let tmpl = crate::templates::Template::load(ctx.config_dir, ctx.username, &ucfg.template)
        .map_err(|e| ToolError::internal(e.to_string()))?;
    let plan_id = crate::plan::generate(conn, ctx.user_id, &tmpl, local.date())
        .map_err(|e| ToolError::internal(e.to_string()))?;
    conn.execute(
        "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, flexibility, slide_window_min, channel, message)
         VALUES (?1, 'nudge', ?2, ?2, 'drop', 0, 'push', ?3)",
        (plan_id, &wall, args.text.trim()),
    )
    .map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(serde_json::json!({ "event_id": conn.last_insert_rowid(), "wall_time": wall }))
}
```

`tools/mod.rs`: `pub mod outreach_ops;`, append `"notify_send"` to `NIGHTLY`, add the `describe` arm and the `run` arm (`"notify_send" => outreach_ops::send(conn, ctx, parse(raw)?)`).

`plan.rs`: add the `Decided` variant; in `shift`, after reading the event's status, before mutating:

```rust
    if status == "done" || status == "dropped" {
        return Err(ShiftError::Decided { status });
    }
```

`schedule_ops.rs` slide: add `Err(ShiftError::Decided { status }) => Err(ToolError::rejected(format!("event already {status}")))`. `api.rs` `event_shift`: add `Err(crate::plan::ShiftError::Decided { .. }) => StatusCode::CONFLICT.into_response(),`.

**Ripple:** any phase-2 test asserting a done/dropped event can still be slid (status-preservation test) now expects rejection — update its assertion; that behavior change is this task's point (ledgered as the drop-hole's weaker cousin).

- [ ] **Step 3: Run all tests, commit** — `cargo test`; commit `feat: notify_send outreach tool; reject sliding decided events`.

---

### Task 6: Auth hardening

**Files:**
- Modify: `server/src/auth.rs` (login restructure, dummy hash, limiter, cookie helpers), `server/src/api.rs` (login/logout handlers), `server/src/lib.rs` (AppState fields), `server/src/main.rs` (secure_cookies)
- Test: inline in `auth.rs` + additions to `server/tests/auth.rs`

**Interfaces:**
- Consumes: existing `auth::login`, sessions table.
- Produces:
  - `auth::login(db: &Mutex<Connection>, username: &str, password: &str) -> Result<Option<String>>` — signature change (was `&Connection`): reads the row under a short lock, verifies argon2 with **no lock held** (unknown users verify against a process-wide dummy hash so the timing is the same), inserts the session under a fresh short lock. Ripple: every call site (api login handler, `tests/common`, any test calling `auth::login` directly).
  - `auth::LoginLimiter::new() -> LoginLimiter`; `allow(&self, username: &str, now: jiff::Timestamp) -> bool`; `record_failure(&self, username: &str, now: jiff::Timestamp)`; `clear(&self, username: &str)`. Fixed window: `MAX_FAILURES = 10` per `WINDOW_MINS = 15`; the window starts at the first failure and resets when it elapses.
  - `auth::session_cookie(token: &str, secure: bool) -> String` — `session={token}; HttpOnly; Path=/; SameSite=Lax; Max-Age=2592000` plus `; Secure` when `secure`.
  - `auth::clear_cookie(secure: bool) -> String` — same attributes with an empty value and `Max-Age=0`.
  - `AppState` gains `pub secure_cookies: bool` (default `false`) and `pub login_limiter: Arc<auth::LoginLimiter>`; `main.rs` sets `state.secure_cookies = cfg.public_base_url.starts_with("https://")`.
  - Routes: login handler moves argon2 into `spawn_blocking`, consults the limiter (429 when throttled), uses the cookie helpers; new `POST /api/logout` deletes the session row and clears the cookie.

- [ ] **Step 1: Write the failing tests**

Inline in `auth.rs`:

```rust
    #[test]
    fn unknown_user_and_wrong_password_both_return_none() {
        let db = std::sync::Mutex::new(crate::db::open_memory().unwrap());
        {
            let conn = db.lock().unwrap();
            create_user(&conn, "aki", "right", false).unwrap();
        }
        assert!(login(&db, "aki", "wrong").unwrap().is_none());
        assert!(login(&db, "nobody", "whatever").unwrap().is_none());
        assert!(login(&db, "aki", "right").unwrap().is_some());
    }

    #[test]
    fn limiter_blocks_after_max_failures_and_resets_after_window() {
        let lim = LoginLimiter::new();
        let t0: jiff::Timestamp = "2026-08-31T00:00:00Z".parse().unwrap();
        for _ in 0..MAX_FAILURES {
            assert!(lim.allow("aki", t0));
            lim.record_failure("aki", t0);
        }
        assert!(!lim.allow("aki", t0));
        // another user is unaffected
        assert!(lim.allow("other", t0));
        // window elapses → allowed again
        let later = t0 + jiff::Span::new().minutes(WINDOW_MINS + 1);
        assert!(lim.allow("aki", later));
        // success clears
        lim.record_failure("aki", later);
        lim.clear("aki");
        assert!(lim.allow("aki", later));
    }

    #[test]
    fn cookies_carry_hardened_attributes() {
        let c = session_cookie("tok", false);
        assert!(c.contains("HttpOnly") && c.contains("SameSite=Lax") && c.contains("Max-Age="));
        assert!(!c.contains("Secure"));
        let c = session_cookie("tok", true);
        assert!(c.contains("; Secure"));
        let c = clear_cookie(false);
        assert!(c.contains("session=;") && c.contains("Max-Age=0"));
    }
```

In `server/tests/auth.rs`, add (following that file's existing request-building style):

```rust
#[tokio::test]
async fn logout_invalidates_the_session() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    // sanity: authed
    let res = app.clone().oneshot(
        Request::get("/api/me").header("cookie", cookie.clone()).body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.clone().oneshot(
        Request::post("/api/logout").header("cookie", cookie.clone()).body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.oneshot(
        Request::get("/api/me").header("cookie", cookie).body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn repeated_login_failures_are_throttled() {
    let (app, _cookie, _cfg) = common::app_with_logged_in_user().await;
    let attempt = || {
        Request::post("/api/login")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"username":"aki","password":"wrong"}"#))
            .unwrap()
    };
    for _ in 0..10 {
        let res = app.clone().oneshot(attempt()).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
    let res = app.clone().oneshot(attempt()).await.unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
}
```

(Match the username `tests/common/mod.rs` actually creates; if the shared app state is reused across tests in one binary, keep this test self-contained by building its own app instance.)

- [ ] **Step 2: Verify failure, implement**

`auth.rs`:

```rust
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub const MAX_FAILURES: u32 = 10;
pub const WINDOW_MINS: i64 = 15;

/// Per-username fixed-window failure counter; keys are usernames (attackers
/// rotating usernames still pay the argon2 cost per attempt).
#[derive(Default)]
pub struct LoginLimiter {
    attempts: Mutex<HashMap<String, (u32, jiff::Timestamp)>>,
}

impl LoginLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow(&self, username: &str, now: jiff::Timestamp) -> bool {
        let mut a = self.attempts.lock().unwrap();
        match a.get(username) {
            Some((count, start)) => {
                if now - *start > jiff::Span::new().minutes(WINDOW_MINS) {
                    a.remove(username);
                    true
                } else {
                    *count < MAX_FAILURES
                }
            }
            None => true,
        }
    }

    pub fn record_failure(&self, username: &str, now: jiff::Timestamp) {
        let mut a = self.attempts.lock().unwrap();
        let entry = a.entry(username.to_string()).or_insert((0, now));
        if now - entry.1 > jiff::Span::new().minutes(WINDOW_MINS) {
            *entry = (0, now);
        }
        entry.0 += 1;
    }

    pub fn clear(&self, username: &str) {
        self.attempts.lock().unwrap().remove(username);
    }
}
```

(`jiff::Timestamp` subtraction yields a `Span`; comparing spans may need `Span::compare` or converting to seconds — use `(now.as_second() - start.as_second()) > WINDOW_MINS * 60` if the operator form fights the type system.)

Dummy-hash + restructured login:

```rust
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(b"timing-equalizer", &salt)
            .expect("static hash")
            .to_string()
    })
}

fn verify(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
        .unwrap_or(false)
}

/// Row lookup and session insert each take a short lock; the argon2 work runs
/// with no lock held. Unknown usernames verify against a dummy hash so the
/// response time does not reveal which usernames exist.
pub fn login(db: &Mutex<Connection>, username: &str, password: &str) -> Result<Option<String>> {
    let row: Option<(i64, String)> = {
        let conn = db.lock().unwrap();
        conn.query_row(
            "SELECT id, pass_hash FROM users WHERE username = ?1",
            [username],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    };
    let ok = match &row {
        Some((_, hash)) => verify(password, hash),
        None => {
            let _ = verify(password, dummy_hash());
            false
        }
    };
    if !ok {
        return Ok(None);
    }
    let (id, _) = row.expect("checked above");
    let token = uuid::Uuid::new_v4().to_string();
    let expires = jiff::Timestamp::now() + jiff::Span::new().hours(SESSION_LIFETIME_HOURS);
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT INTO sessions (token, user_id, expires_at) VALUES (?1, ?2, ?3)",
        (&token, id, expires.to_string()),
    )?;
    Ok(Some(token))
}

pub fn session_cookie(token: &str, secure: bool) -> String {
    let max_age = SESSION_LIFETIME_HOURS * 3600;
    let mut c = format!("session={token}; HttpOnly; Path=/; SameSite=Lax; Max-Age={max_age}");
    if secure {
        c.push_str("; Secure");
    }
    c
}

pub fn clear_cookie(secure: bool) -> String {
    let mut c = "session=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0".to_string();
    if secure {
        c.push_str("; Secure");
    }
    c
}
```

`api.rs` login/logout:

```rust
async fn login(State(state): State<AppState>, Json(req): Json<LoginReq>) -> impl IntoResponse {
    let now = jiff::Timestamp::now();
    if !state.login_limiter.allow(&req.username, now) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let db = state.db.clone();
    let (username, password) = (req.username.clone(), req.password);
    let result = tokio::task::spawn_blocking(move || auth::login(&db, &username, &password)).await;
    match result {
        Ok(Ok(Some(token))) => {
            state.login_limiter.clear(&req.username);
            (
                StatusCode::OK,
                [(header::SET_COOKIE, auth::session_cookie(&token, state.secure_cookies))],
            )
                .into_response()
        }
        Ok(Ok(None)) => {
            state.login_limiter.record_failure(&req.username, now);
            StatusCode::UNAUTHORIZED.into_response()
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn logout(State(state): State<AppState>, headers: axum::http::HeaderMap) -> impl IntoResponse {
    let jar = axum_extra::extract::CookieJar::from_headers(&headers);
    if let Some(c) = jar.get("session") {
        let conn = state.db.lock().unwrap();
        let _ = conn.execute("DELETE FROM sessions WHERE token = ?1", [c.value()]);
    }
    (StatusCode::OK, [(header::SET_COOKIE, auth::clear_cookie(state.secure_cookies))]).into_response()
}
```

Route: `.route("/api/logout", post(logout))`. `lib.rs`: `pub secure_cookies: bool` (false in `new`), `pub login_limiter: Arc<crate::auth::LoginLimiter>` (fresh in `new`). `main.rs`: `state.secure_cookies = cfg.public_base_url.starts_with("https://");` (make `state` mut).

**Ripple:** `auth::login` call sites — the api handler (rewritten above) and any direct test callers; `tests/common` logs in over HTTP so it should be unaffected, but check.

- [ ] **Step 3: Run all tests, commit** — `cargo test`; commit `feat: harden login path and add logout`.

---

### Task 7: Talk concurrency caps and canned empty reply

**Files:**
- Modify: `server/src/lib.rs` (TalkGate), `server/src/api.rs` (talk handler)
- Test: inline in `lib.rs` + additions to `server/tests/talk_api.rs`

**Interfaces:**
- Consumes: existing talk handler, `AppState`.
- Produces:
  - `lib.rs`: `pub const MAX_CONCURRENT_TALKS: usize = 4;`
  - `pub struct TalkGate { … }` with `new() -> Self`, `try_enter(self: &Arc<Self>, user_id: i64) -> Result<TalkPermit, TalkBusy>`; `pub enum TalkBusy { UserBusy, Full }`. `TalkPermit` holds an `OwnedSemaphorePermit` and removes the user from the active set on `Drop`.
  - `AppState` gains `pub talk_gate: Arc<TalkGate>` (fresh in `new`).
  - Talk handler: `TalkBusy::UserBusy → 409`, `TalkBusy::Full → 503`; the permit is acquired before `spawn_blocking` and held until the handler returns. A blank session reply becomes `EMPTY_REPLY_FALLBACK: &str = "(the assistant is not configured on this server)"`.

- [ ] **Step 1: Write the failing tests**

Inline in `lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn talk_gate_blocks_same_user_and_caps_total() {
        let gate = Arc::new(TalkGate::new());
        let p1 = gate.try_enter(1).unwrap();
        assert!(matches!(gate.try_enter(1), Err(TalkBusy::UserBusy)));
        let _p2 = gate.try_enter(2).unwrap();
        let _p3 = gate.try_enter(3).unwrap();
        let _p4 = gate.try_enter(4).unwrap();
        assert!(matches!(gate.try_enter(5), Err(TalkBusy::Full)));
        drop(p1);
        // released permit and user slot are reusable
        let _p5 = gate.try_enter(1).unwrap();
    }
}
```

In `server/tests/common/mod.rs`, add a variant of the existing logged-in helper that also returns the `AppState` it built (delegating so the two never drift), e.g. `pub async fn app_with_logged_in_user_and_state() -> (Router, String, AppState, TempDir)` — same body as the existing helper with the state returned before it is consumed by `api::router(state.clone())`. Then in `server/tests/talk_api.rs` (matching that file's request-building style):

```rust
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
                .header("content-type", "application/json")
                .header("cookie", cookie)
                .body(Body::from(r#"{"message":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn blank_reply_is_replaced_with_the_canned_line() {
    use note_server::providers::{mock::MockLLM, ChatResponse};
    use std::sync::Arc;
    let llm = Arc::new(MockLLM::scripted(vec![ChatResponse {
        text: "   ".into(),
        tool_calls: vec![],
    }]));
    let (app, cookie, _cfg) = common::app_with_logged_in_user_and_llm(llm).await;
    let res = app
        .oneshot(
            Request::post("/api/talk")
                .header("content-type", "application/json")
                .header("cookie", cookie)
                .body(Body::from(r#"{"message":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["reply"], note_server::EMPTY_REPLY_FALLBACK);
}
```

(Adapt helper names/signatures to what `tests/common/mod.rs` actually exports; keep both assertions.)

- [ ] **Step 2: Verify failure, implement**

`lib.rs`:

```rust
pub const MAX_CONCURRENT_TALKS: usize = 4;
pub const EMPTY_REPLY_FALLBACK: &str = "(the assistant is not configured on this server)";

pub enum TalkBusy {
    UserBusy,
    Full,
}

/// Caps concurrent talk sessions globally (each pins a blocking thread for up
/// to MAX_TURNS provider calls) and to one per user (interleaved tool calls
/// from two sessions of the same user would race).
pub struct TalkGate {
    semaphore: Arc<tokio::sync::Semaphore>,
    active: Mutex<std::collections::HashSet<i64>>,
}

pub struct TalkPermit {
    gate: Arc<TalkGate>,
    user_id: i64,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl Drop for TalkPermit {
    fn drop(&mut self) {
        self.gate.active.lock().unwrap().remove(&self.user_id);
    }
}

impl TalkGate {
    pub fn new() -> Self {
        Self {
            semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_TALKS)),
            active: Mutex::new(std::collections::HashSet::new()),
        }
    }

    pub fn try_enter(self: &Arc<Self>, user_id: i64) -> Result<TalkPermit, TalkBusy> {
        if !self.active.lock().unwrap().insert(user_id) {
            return Err(TalkBusy::UserBusy);
        }
        match self.semaphore.clone().try_acquire_owned() {
            Ok(permit) => Ok(TalkPermit { gate: self.clone(), user_id, _permit: permit }),
            Err(_) => {
                self.active.lock().unwrap().remove(&user_id);
                Err(TalkBusy::Full)
            }
        }
    }
}
```

`AppState` gains `pub talk_gate: Arc<TalkGate>` (fresh in `new`). Talk handler, before `spawn_blocking`:

```rust
    let _permit = match state.talk_gate.clone().try_enter(user.id) {
        Ok(p) => p,
        Err(crate::TalkBusy::UserBusy) => return StatusCode::CONFLICT.into_response(),
        Err(crate::TalkBusy::Full) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
```

and in the success arm:

```rust
        Ok(Ok(out)) => {
            let reply = if out.reply.trim().is_empty() {
                crate::EMPTY_REPLY_FALLBACK.to_string()
            } else {
                out.reply
            };
            Json(serde_json::json!({ "reply": reply })).into_response()
        }
```

- [ ] **Step 3: Run all tests, commit** — `cargo test`; commit `feat: cap concurrent talk sessions; canned reply when unconfigured`.

---

### Task 8: Embeddings out from under the DB lock

**Files:**
- Modify: `server/src/tools/mod.rs` (`PreparedVectors`, `prepare`, `ToolCtx`), `server/src/memory.rs` (signatures take precomputed vectors), `server/src/tools/memory_ops.rs`, `server/src/agent.rs` (prepare before locking), `server/src/providers/mod.rs` (doc comment on `embeddings_http_agent`)
- Modify (ripple): every `ToolCtx` construction site — `tools/mod.rs` tests, `tools/memory_ops.rs` tests, `tools/context_ops.rs` tests, `tools/schedule_ops.rs` tests, `tools/task_ops.rs` tests (if any construct one), `tools/outreach_ops.rs` tests (Task 5), `server/tests/tool_fuzz.rs`, `server/tests/tool_invariants.rs`; every `memory::add/update/supersede/query` caller — `memory.rs` tests, `memory_ops.rs`
- Test: inline in `tools/mod.rs` and `memory.rs`

**Interfaces:**
- Consumes: `EmbeddingsProvider`, existing memory fns.
- Produces:
  - `tools::PreparedVectors { pub content: Option<Vec<f32>>, pub query: Option<Vec<f32>>, pub error: Option<String> }` (Debug, Default).
  - `tools::prepare(emb: Option<&dyn EmbeddingsProvider>, name: &str, raw_args: &str) -> PreparedVectors` — the ONLY place tool-path embeddings happen; called by the agent loop with **no lock held**. For `memory_write` it embeds `memory::embed_text(summary, body)` parsed leniently from the args (malformed args → `Default`, dispatch produces the typed error); for `memory_query` it embeds the query string; for every other tool it returns `Default`. Oversized args (> `MAX_ARGS_BYTES`) return `Default`. An embed failure sets `error` and leaves the vector `None`.
  - `ToolCtx` **loses** `embeddings` and **gains** `pub vectors: PreparedVectors`.
  - `memory::embed_text(summary: &str, body: &str) -> String` (pub(crate)) — `format!("{}\n{}", one_line(summary), body.trim())`, the exact text `store_vector` used to embed, so prepared vectors match index-time vectors.
  - `memory::add(conn, data_dir, user, category, summary, body, vector: Option<&[f32]>) -> Result<String>`; `update`/`supersede` likewise take `vector: Option<&[f32]>` as their last parameter; `query(conn, user, q, limit, query_vec: Option<&[f32]>)`. `store_vector(conn, user, id: &str, vector: Option<&[f32]>)` just writes the blob (embed-failure logging moves to the dispatch path).
  - `memory_ops`: `write` passes `ctx.vectors.content.as_deref()`; `query` passes `ctx.vectors.query.as_deref()`; both first log `memory_embed_error` (with the detail from `ctx.vectors.error`) when `error` is `Some` — degradation stays visible.
  - `agent.rs` loop: per tool call, `let vectors = tools::prepare(deps.embeddings, &call.name, &call.args);` **before** `deps.db.lock()`, then builds the per-call `ToolCtx { …, vectors }` inside the lock scope.
  - `reindex_user`/`reindex_all` keep their current shape (no embedding at reindex — unchanged behavior: vectors for unchanged ids survive, stale ones are pruned).

- [ ] **Step 1: Write the failing tests**

Inline in `tools/mod.rs`:

```rust
    #[test]
    fn prepare_embeds_only_memory_tools_and_reports_failures() {
        use crate::providers::mock::MockEmbeddings;
        let emb = MockEmbeddings;

        let v = prepare(Some(&emb), "memory_query", r#"{"query":"abba"}"#);
        assert!(v.query.is_some());
        assert!(v.content.is_none() && v.error.is_none());

        let v = prepare(Some(&emb), "memory_write", r#"{"op":"add","category":"semantic","summary":"s","body":"b"}"#);
        assert!(v.content.is_some());
        assert!(v.query.is_none());

        // non-memory tools and malformed args cost nothing
        let v = prepare(Some(&emb), "task_create", r#"{"title":"x"}"#);
        assert!(v.content.is_none() && v.query.is_none() && v.error.is_none());
        let v = prepare(Some(&emb), "memory_query", "not json");
        assert!(v.query.is_none() && v.error.is_none());

        // no provider → all None
        let v = prepare(None, "memory_query", r#"{"query":"abba"}"#);
        assert!(v.query.is_none());

        struct FailingEmb;
        impl crate::providers::EmbeddingsProvider for FailingEmb {
            fn embed(&self, _: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
                anyhow::bail!("endpoint down")
            }
        }
        let v = prepare(Some(&FailingEmb), "memory_query", r#"{"query":"abba"}"#);
        assert!(v.query.is_none());
        assert!(v.error.as_deref().unwrap_or("").contains("endpoint down"));
    }

    #[test]
    fn prepared_vector_text_matches_index_text() {
        use crate::providers::{mock::MockEmbeddings, EmbeddingsProvider};
        let emb = MockEmbeddings;
        let v = prepare(Some(&emb), "memory_write",
            r#"{"op":"add","category":"semantic","summary":"line one\nline two","body":"  padded  "}"#);
        let direct = emb
            .embed(&[&crate::memory::embed_text("line one\nline two", "  padded  ")])
            .unwrap();
        assert_eq!(v.content.unwrap(), direct[0]);
    }
```

In `memory.rs` tests, the existing hybrid-retrieval tests change shape: where they passed `Some(&MockEmbeddings as &dyn …)` they now precompute, e.g.:

```rust
        let emb = crate::providers::mock::MockEmbeddings;
        let v = emb.embed(&[&embed_text("all about aaaa", "aaab")]).unwrap();
        let id = add(&conn, dir, "aki", "semantic", "all about aaaa", "aaab", Some(&v[0])).unwrap();
        // …
        let qv = emb.embed(&["aaab"]).unwrap();
        let hits = query(&conn, "aki", "aaab", 10, Some(&qv[0])).unwrap();
```

Keep every existing assertion; only the plumbing of vectors changes. Add one new test asserting a `query` with `query_vec: None` still returns lexical hits (this is the no-provider degradation, now explicit at the API).

- [ ] **Step 2: Verify failure, implement** per the interface list. Notes:
  - `prepare`'s lenient arg structs are private to `tools/mod.rs` and use `#[serde(default)]` + no `deny_unknown_fields` — they only exist to pull out the embeddable text; real validation stays in `memory_ops`:

```rust
#[derive(Default, Debug)]
pub struct PreparedVectors {
    pub content: Option<Vec<f32>>,
    pub query: Option<Vec<f32>>,
    pub error: Option<String>,
}

/// Embeds any text this tool call will need, so `dispatch` itself never does
/// network I/O. Malformed args embed nothing — dispatch will reject them with
/// a typed error anyway.
pub fn prepare(
    emb: Option<&dyn crate::providers::EmbeddingsProvider>,
    name: &str,
    raw_args: &str,
) -> PreparedVectors {
    let Some(emb) = emb else { return PreparedVectors::default() };
    if raw_args.len() > MAX_ARGS_BYTES {
        return PreparedVectors::default();
    }
    let mut out = PreparedVectors::default();
    match name {
        "memory_query" => {
            #[derive(serde::Deserialize, Default)]
            #[serde(default)]
            struct Q {
                query: String,
            }
            if let Ok(q) = serde_json::from_str::<Q>(raw_args) {
                if !q.query.is_empty() {
                    match emb.embed(&[&q.query]) {
                        Ok(vs) if !vs.is_empty() => out.query = Some(vs[0].clone()),
                        Ok(_) => {}
                        Err(e) => out.error = Some(e.to_string()),
                    }
                }
            }
        }
        "memory_write" => {
            #[derive(serde::Deserialize, Default)]
            #[serde(default)]
            struct W {
                summary: String,
                body: String,
            }
            if let Ok(w) = serde_json::from_str::<W>(raw_args) {
                if !w.summary.is_empty() || !w.body.is_empty() {
                    let text = crate::memory::embed_text(&w.summary, &w.body);
                    match emb.embed(&[&text]) {
                        Ok(vs) if !vs.is_empty() => out.content = Some(vs[0].clone()),
                        Ok(_) => {}
                        Err(e) => out.error = Some(e.to_string()),
                    }
                }
            }
        }
        _ => {}
    }
    out
}
```

  - `agent.rs`: move the `ToolCtx` construction into the per-call loop; `prepare` runs before the lock block:

```rust
        for call in calls {
            tool_calls += 1;
            let vectors = tools::prepare(deps.embeddings, &call.name, &call.args);
            let (content, is_error) = {
                let conn = deps.db.lock().unwrap();
                let ctx = ToolCtx {
                    config_dir: deps.config_dir,
                    data_dir: deps.data_dir,
                    user_id,
                    username,
                    vectors,
                };
                match tools::dispatch(&conn, &ctx, kind, &call.name, &call.args) {
                    // …unchanged…
                }
            };
            messages.push(Message::ToolResult { call_id: call.id, content, is_error });
        }
```

  - `memory_ops::write` (all three ops) and `query` gain, before their memory calls:

```rust
    if let Some(err) = &ctx.vectors.error {
        let _ = crate::log::record(conn, None, "memory_embed_error", err);
    }
```

  - `store_vector` simplifies to a blob write when `Some`, nothing when `None`; `supersede` keeps deleting the old id's vector.
  - `embeddings_http_agent` doc comment updates: it no longer runs under the DB lock (the tight 10s cap stays — it bounds a talk turn's stall, not a lock hold).
  - Sweep the ripple list top to bottom; the compiler enumerates the rest.

- [ ] **Step 3: Run the full suite** — every memory/tool/agent/fuzz/invariant test green.

- [ ] **Step 4: Commit** — `refactor: embed in a prepare pass outside the DB lock`.

---

### Task 9: Integration test — delivery day

**Files:**
- Create: `server/tests/delivery_day.rs`
- Test: itself

**Interfaces:**
- Consumes: everything above — `nightly::run_for_user`, `runner::fire_due` (`FiredEvent`), `channels::{deliver_event, ws::ClientHub, ws::WsChannel, mock::MockChannel}`, `providers::mock::MockLLM`, `push_subs`.
- Produces: the phase's end-to-end proof: a debrief generated overnight reaches a connected client over the WS hub; with no client connected delivery falls through to the next channel; total failure degrades loudly but harmlessly; voice falls back per spec.

- [ ] **Step 1: Write the test** (it passes only if Tasks 1–8 landed; it is the gate that the pieces compose):

```rust
mod common;

use note_server::channels::{self, mock::MockChannel, ws::{ClientHub, WsChannel}, Channel};
use note_server::providers::{mock::MockLLM, ChatResponse};
use note_server::tools::SessionKind;
use std::sync::{Arc, Mutex};

/// One simulated evening-to-day for a JST user: nightly debrief at 03:30,
/// morning debrief delivery over WS, a daytime nudge falling back to the mock
/// push channel, and a voice check-in degrading to the push ladder.
#[test]
fn delivery_reaches_the_user_through_the_ladder() {
    let conn = note_server::db::open_memory().unwrap();
    let uid = {
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    };
    let tmp = tempfile::tempdir().unwrap();
    let write = |rel: &str, c: &str| {
        let p = tmp.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, c).unwrap();
    };
    write(
        "defaults/user.toml",
        "display_name = \"Aki\"\ntimezone = \"Asia/Tokyo\"\ntemplate = \"default\"\nnightly_time = \"03:00\"\n",
    );
    write("defaults/prompts/persona.md", "you are note");
    write("defaults/prompts/planning.md", "plan the day");
    write(
        "defaults/templates/default.toml",
        concat!(
            "[[events]]\nkind = \"debrief\"\ntime = \"07:30\"\n",
            "days = [\"mon\",\"tue\",\"wed\",\"thu\",\"fri\",\"sat\",\"sun\"]\n",
            "flexibility = \"fixed\"\nchannel = \"push\"\n",
            "[[events]]\nkind = \"nudge\"\ntime = \"10:00\"\n",
            "days = [\"mon\",\"tue\",\"wed\",\"thu\",\"fri\",\"sat\",\"sun\"]\n",
            "flexibility = \"drop\"\nchannel = \"push\"\n",
            "[[events]]\nkind = \"checkin_call\"\ntime = \"16:00\"\n",
            "days = [\"mon\",\"tue\",\"wed\",\"thu\",\"fri\",\"sat\",\"sun\"]\n",
            "flexibility = \"fixed\"\nchannel = \"voice\"\n",
        ),
    );

    let db = Mutex::new(conn);
    let hub = Arc::new(ClientHub::new());
    let push = Arc::new(MockChannel::new("mockpush"));
    let ladder: Vec<Arc<dyn Channel>> =
        vec![Arc::new(WsChannel::new(hub.clone())), push.clone()];

    // --- 03:30 JST 2026-08-31 (= 18:30Z 08-30): nightly writes plan + debrief.
    let llm = MockLLM::scripted(vec![ChatResponse {
        text: "quiet day; dentist rolled forward".into(),
        tool_calls: vec![],
    }]);
    let deps = note_server::agent::SessionDeps {
        db: &db,
        config_dir: tmp.path(),
        data_dir: tmp.path(),
        llm: &llm,
        embeddings: None,
    };
    let nightly_now: jiff::Timestamp = "2026-08-30T18:30:00Z".parse().unwrap();
    note_server::nightly::run_for_user(&deps, uid, "aki", nightly_now).unwrap();

    // --- 07:31 JST: debrief event fires and reaches the connected client.
    let (_conn_id, mut rx) = hub.register(uid);
    let morning: jiff::Timestamp = "2026-08-30T22:31:00Z".parse().unwrap(); // 07:31 JST 08-31
    let fired = {
        let conn = db.lock().unwrap();
        note_server::runner::fire_due(&conn, tmp.path(), morning).unwrap()
    };
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].kind, "debrief");
    channels::deliver_event(&db, &ladder, &fired[0]);
    let text = rx.try_recv().unwrap();
    assert!(text.contains("dentist rolled forward"), "ws frame: {text}");
    assert!(push.seen().is_empty());

    // --- 10:01 JST, client gone: the nudge falls through to the mock channel.
    drop(rx);
    hub.unregister(uid, _conn_id);
    let midmorning: jiff::Timestamp = "2026-08-31T01:01:00Z".parse().unwrap(); // 10:01 JST
    let fired = {
        let conn = db.lock().unwrap();
        note_server::runner::fire_due(&conn, tmp.path(), midmorning).unwrap()
    };
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].kind, "nudge");
    channels::deliver_event(&db, &ladder, &fired[0]);
    assert_eq!(push.seen().len(), 1);
    assert_eq!(push.seen()[0].0, uid);

    // --- 16:01 JST: the voice check-in degrades to the push ladder, logged.
    let afternoon: jiff::Timestamp = "2026-08-31T07:01:00Z".parse().unwrap(); // 16:01 JST
    let fired = {
        let conn = db.lock().unwrap();
        note_server::runner::fire_due(&conn, tmp.path(), afternoon).unwrap()
    };
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].channel, "voice");
    channels::deliver_event(&db, &ladder, &fired[0]);
    assert_eq!(push.seen().len(), 2);
    {
        let conn = db.lock().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='voice_unavailable'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let ok: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='delivery_ok'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ok, 3);
    }

    // --- total failure degrades, never errors: fail the mock and re-nudge.
    push.set_fail(true);
    {
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, flexibility, slide_window_min, channel)
             SELECT plan_id, 'nudge', '16:30', '16:30', 'drop', 0, 'push' FROM events LIMIT 1",
            [],
        )
        .unwrap();
    }
    let late: jiff::Timestamp = "2026-08-31T07:31:00Z".parse().unwrap(); // 16:31 JST
    let fired = {
        let conn = db.lock().unwrap();
        note_server::runner::fire_due(&conn, tmp.path(), late).unwrap()
    };
    assert_eq!(fired.len(), 1);
    channels::deliver_event(&db, &ladder, &fired[0]);
    {
        let conn = db.lock().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='delivery_degraded'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
```

(JST arithmetic: 18:30Z Aug 30 = 03:30 JST Aug 31; 22:31Z Aug 30 = 07:31 JST Aug 31; 01:01Z / 07:01Z / 07:31Z Aug 31 = 10:01 / 16:01 / 16:31 JST Aug 31. The nightly plan is generated for the JST date 2026-08-31. If the template's `slide_window_min` default trips template validation, add `slide_window_min = 0` lines. `MockLLM` sees exactly one chat; if `nightly::run_for_user` consumes more, the scripted mock's blank fallback would corrupt the debrief assertion — the assertion on the WS frame content is the guard.)

- [ ] **Step 2: Run it** — `cargo test --test delivery_day`; fix only genuine integration seams it exposes (adjusting the test's mechanics to real signatures is fine; weakening its assertions is not).

- [ ] **Step 3: Full suite, commit** — `cargo test`; commit `test: full delivery day across the channel ladder`.

---

### Task 10: Starter config, README, release build

**Files:**
- Modify: `config/server.toml`, `README.md`
- Test: `cargo test` + `cargo build --release`

**Interfaces:**
- Consumes: `ChannelsConfig` (Task 3), the routes and behaviors of Tasks 1–7.
- Produces: admin-facing documentation; the phase's final green build.

- [ ] **Step 1: Append to `config/server.toml`:**

```toml
# --- Channels -----------------------------------------------------------
# Delivery ladder: connected WebSocket clients first, then Web Push.
# Without a [channels.webpush] section, only in-app WebSocket delivery is
# active and every push-channel event for a disconnected user is logged as
# delivery_degraded.
#
# Web Push needs a VAPID keypair (any P-256 EC key). Generate one with:
#   openssl ecparam -genkey -name prime256v1 -noout -out config/vapid.pem
# The public key is served to clients at GET /api/push/vapid_public_key.
#
# [channels.webpush]
# vapid_pem_file = "config/vapid.pem"
# subject = "mailto:admin@example.com"
```

- [ ] **Step 2: README section** — add "Channels & delivery" after the "Providers & the agent" section, covering: the ladder (WebSocket → Web Push, voice deferred with logged fallback), VAPID key generation (the openssl line above), the subscription API (`POST /api/push/subscribe` with the browser's `PushSubscription.toJSON()` shape, `POST /api/push/unsubscribe`, `GET /api/push/vapid_public_key`), `GET /api/ws` for in-app delivery, `POST /api/logout`, the login rate limit (10 failures / 15 min per username), talk concurrency limits (one session per user, 4 global), and that every delivery outcome lands in the admin log (`delivery_ok` / `delivery_degraded` / `voice_unavailable`). Every claim must match the shipped code — verify each against the source, not this plan.

- [ ] **Step 3: Verify and commit** — `cargo test` (full suite) and `cargo build --release` both clean; commit `docs: channel configuration and delivery in README`.
