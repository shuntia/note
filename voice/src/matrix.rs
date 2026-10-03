use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

pub const MEMBER_TYPE: &str = "org.matrix.msc3401.call.member";
pub const NOTIFICATION_TYPE: &str = "m.rtc.notification";
const RING_TYPES: [&str; 2] = [NOTIFICATION_TYPE, "org.matrix.msc4075.rtc.notification"];
const DECLINE_TYPES: [&str; 2] = ["org.matrix.msc4310.rtc.decline", "m.rtc.decline"];
const USER_AGENT: &str = concat!("note-voice/", env!("CARGO_PKG_VERSION"));

#[derive(Debug)]
pub struct HomeserverError {
    pub status: reqwest::StatusCode,
    pub errcode: String,
}

impl std::fmt::Display for HomeserverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "homeserver said {}: {}", self.status, self.errcode)
    }
}

impl std::error::Error for HomeserverError {}

fn is_transient(e: &anyhow::Error) -> bool {
    match e.downcast_ref::<HomeserverError>() {
        Some(h) => h.status.is_server_error() || h.status == reqwest::StatusCode::TOO_MANY_REQUESTS,
        None => e.downcast_ref::<reqwest::Error>().is_some(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RoomEvent {
    Joined { room: String, user: String },
    /// `device` is read from the state key `_<user>_<device>_m.call`.
    CallMember { room: String, user: String, device: String, active: bool, event_id: String, ts_ms: i64 },
    /// A ring that mentions the bot.
    RingForBot { room: String, sender: String, event_id: String, ts_ms: i64 },
    Declined { room: String, notification: String },
}

#[derive(Debug, Clone)]
pub struct SyncBatch {
    pub next_batch: String,
    pub events: Vec<RoomEvent>,
}

pub struct Matrix {
    http: reqwest::Client,
    sync_http: reqwest::Client,
    base: String,
    token: String,
    pub user_id: String,
    pub device_id: String,
}

fn enc(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

impl Matrix {
    pub async fn connect(homeserver: &str, token: &str) -> Result<Matrix> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()?;
        let sync_http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(45))
            .build()?;
        let mut m = Matrix {
            http,
            sync_http,
            base: homeserver.trim_end_matches('/').to_string(),
            token: token.trim().to_string(),
            user_id: String::new(),
            device_id: String::new(),
        };
        let who = m.whoami().await.context("whoami")?;
        m.user_id = who["user_id"].as_str().context("whoami without user_id")?.to_string();
        m.device_id = who["device_id"].as_str().context("whoami without device_id")?.to_string();
        Ok(m)
    }

    /// Waits out a homeserver (or the tunnel in front of it) that is still
    /// starting; a refused token fails at once.
    async fn whoami(&self) -> Result<Value> {
        const DELAYS_S: [u64; 6] = [1, 2, 4, 8, 15, 30];
        let mut delays = DELAYS_S.iter();
        loop {
            match self.get("/_matrix/client/v3/account/whoami").await {
                Err(e) if is_transient(&e) => {
                    let Some(delay) = delays.next() else { return Err(e) };
                    eprintln!("voice: homeserver not ready ({e:#}), retrying in {delay}s");
                    tokio::time::sleep(Duration::from_secs(*delay)).await;
                }
                r => return r,
            }
        }
    }

    async fn check(resp: reqwest::Response) -> Result<Value> {
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(HomeserverError { status, errcode: body["errcode"].as_str().unwrap_or("?").to_string() }.into());
        }
        Ok(body)
    }

    async fn get(&self, path: &str) -> Result<Value> {
        Self::check(self.http.get(format!("{}{path}", self.base)).bearer_auth(&self.token).send().await?).await
    }

    async fn put(&self, path: &str, body: &Value) -> Result<Value> {
        Self::check(self.http.put(format!("{}{path}", self.base)).bearer_auth(&self.token).json(body).send().await?)
            .await
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        Self::check(self.http.post(format!("{}{path}", self.base)).bearer_auth(&self.token).json(body).send().await?)
            .await
    }

    pub fn member_key(&self) -> String {
        format!("_{}_{}_m.call", self.user_id, self.device_id)
    }

    /// Whether `user`'s call membership from `device` is still set in `room`.
    pub async fn member_active(&self, room: &str, user: &str, device: &str) -> Result<bool> {
        let path =
            format!("/_matrix/client/v3/rooms/{}/state/{MEMBER_TYPE}/{}", enc(room), enc(&format!("_{user}_{device}_m.call")));
        match self.get(&path).await {
            Ok(content) => Ok(content.as_object().is_some_and(|c| !c.is_empty())),
            Err(e) if e.downcast_ref::<HomeserverError>().is_some_and(|h| h.errcode == "M_NOT_FOUND") => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// An unencrypted DM inviting `mxid`. Recording it in the bot's
    /// `m.direct` is best-effort: only room creation can fail.
    pub async fn create_dm(&self, mxid: &str) -> Result<String> {
        let created = self
            .post(
                "/_matrix/client/v3/createRoom",
                &json!({ "preset": "trusted_private_chat", "is_direct": true, "invite": [mxid], "name": "Note" }),
            )
            .await?;
        let room = created["room_id"].as_str().context("createRoom without room_id")?.to_string();
        if let Err(e) = self.mark_direct(mxid, &room).await {
            eprintln!("voice: recording {room} in m.direct failed: {e:#}");
        }
        Ok(room)
    }

    async fn mark_direct(&self, mxid: &str, room: &str) -> Result<()> {
        let path = format!("/_matrix/client/v3/user/{}/account_data/m.direct", enc(&self.user_id));
        let mut direct = match self.get(&path).await {
            Ok(v) if v.is_object() => v,
            Ok(_) => json!({}),
            Err(e) if e.downcast_ref::<HomeserverError>().is_some_and(|h| h.errcode == "M_NOT_FOUND") => json!({}),
            Err(e) => return Err(e.context("reading m.direct")),
        };
        let rooms = direct[mxid].as_array().cloned().unwrap_or_default();
        let mut rooms: Vec<Value> = rooms.into_iter().filter(|r| r != &json!(room)).collect();
        rooms.push(json!(room));
        direct[mxid] = json!(rooms);
        self.put(&path, &direct).await.map(|_| ())
    }

    pub async fn joined_members(&self, room: &str) -> Result<Vec<String>> {
        let got = self.get(&format!("/_matrix/client/v3/rooms/{}/joined_members", enc(room))).await?;
        Ok(got["joined"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default())
    }

    pub async fn invite(&self, room: &str, mxid: &str) -> Result<()> {
        self.post(&format!("/_matrix/client/v3/rooms/{}/invite", enc(room)), &json!({ "user_id": mxid }))
            .await
            .map(|_| ())
    }

    pub async fn put_member(&self, room: &str, expires_ms: u64, livekit_url: &str) -> Result<String> {
        let path = format!(
            "/_matrix/client/v3/rooms/{}/state/{MEMBER_TYPE}/{}",
            enc(room),
            enc(&self.member_key())
        );
        let body = json!({
            "application": "m.call",
            "call_id": "",
            "scope": "m.room",
            "device_id": self.device_id,
            "expires": expires_ms,
            "focus_active": { "type": "livekit", "focus_selection": "oldest_membership" },
            "foci_preferred": [{ "type": "livekit", "livekit_service_url": livekit_url, "livekit_alias": room }],
            "m.call.intent": "audio",
        });
        let got = self.put(&path, &body).await?;
        Ok(got["event_id"].as_str().context("state put without event_id")?.to_string())
    }

    pub async fn clear_member(&self, room: &str) -> Result<()> {
        let path = format!(
            "/_matrix/client/v3/rooms/{}/state/{MEMBER_TYPE}/{}",
            enc(room),
            enc(&self.member_key())
        );
        self.put(&path, &json!({})).await.map(|_| ())
    }

    pub async fn ring(&self, room: &str, target: &str, member_event_id: &str, lifetime_ms: u64) -> Result<String> {
        let sender_ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as u64;
        let path = format!(
            "/_matrix/client/v3/rooms/{}/send/{NOTIFICATION_TYPE}/{}",
            enc(room),
            uuid::Uuid::new_v4().simple()
        );
        let body = json!({
            "sender_ts": sender_ts,
            "lifetime": lifetime_ms,
            "notification_type": "ring",
            "m.call.intent": "audio",
            "m.mentions": { "user_ids": [target], "room": true },
            "m.relates_to": { "rel_type": "m.reference", "event_id": member_event_id },
        });
        let got = self.put(&path, &body).await?;
        Ok(got["event_id"].as_str().context("send without event_id")?.to_string())
    }

    pub async fn openid_token(&self) -> Result<Value> {
        self.post(&format!("/_matrix/client/v3/user/{}/openid/request_token", enc(&self.user_id)), &json!({})).await
    }

    /// Returns `(url, jwt)` for the `LiveKit` SFU behind the `MatrixRTC` JWT service.
    pub async fn livekit_jwt(&self, service_url: &str, room_id: &str) -> Result<(String, String)> {
        let body = json!({ "room": room_id, "openid_token": self.openid_token().await?, "device_id": self.device_id });
        let url = format!("{}/sfu/get", service_url.trim_end_matches('/'));
        let got = Self::check(self.http.post(url).json(&body).send().await?).await.context("sfu/get")?;
        Ok((
            got["url"].as_str().context("sfu/get without url")?.to_string(),
            got["jwt"].as_str().context("sfu/get without jwt")?.to_string(),
        ))
    }

    pub async fn sync(&self, since: Option<&str>, timeout_ms: u64) -> Result<SyncBatch> {
        let filter = json!({
            "presence": { "not_types": ["*"] },
            "account_data": { "not_types": ["*"] },
            "room": {
                "account_data": { "not_types": ["*"] },
                "ephemeral": { "not_types": ["*"] },
                "timeline": { "limit": 50 },
            },
        })
        .to_string();
        let mut query: Vec<(&str, String)> = vec![("timeout", timeout_ms.to_string()), ("filter", filter)];
        if let Some(s) = since {
            query.push(("since", s.to_string()));
        }
        let resp = self
            .sync_http
            .get(format!("{}/_matrix/client/v3/sync", self.base))
            .bearer_auth(&self.token)
            .query(&query)
            .send()
            .await?;
        let body = Self::check(resp).await?;
        Ok(SyncBatch {
            next_batch: body["next_batch"].as_str().context("sync without next_batch")?.to_string(),
            events: parse_sync(&body, &self.user_id),
        })
    }
}

fn parse_sync(body: &Value, bot: &str) -> Vec<RoomEvent> {
    let mut out = Vec::new();
    let Some(rooms) = body["rooms"]["join"].as_object() else { return out };
    for (room, data) in rooms {
        let lists = [&data["state"]["events"], &data["timeline"]["events"]];
        for ev in lists.into_iter().filter_map(|l| l.as_array()).flatten() {
            let kind = ev["type"].as_str().unwrap_or_default();
            let sender = ev["sender"].as_str().unwrap_or_default().to_string();
            let event_id = ev["event_id"].as_str().unwrap_or_default().to_string();
            let ts_ms = ev["origin_server_ts"].as_i64().unwrap_or(0);
            if kind == "m.room.member" && ev["content"]["membership"] == "join" {
                if let Some(user) = ev["state_key"].as_str() {
                    out.push(RoomEvent::Joined { room: room.clone(), user: user.to_string() });
                }
            } else if kind == MEMBER_TYPE {
                let active = ev["content"].as_object().is_some_and(|c| !c.is_empty());
                let device = ev["state_key"]
                    .as_str()
                    .and_then(|k| k.strip_prefix(&format!("_{sender}_"))?.strip_suffix("_m.call"))
                    .unwrap_or_default()
                    .to_string();
                out.push(RoomEvent::CallMember { room: room.clone(), user: sender, device, active, event_id, ts_ms });
            } else if RING_TYPES.contains(&kind) {
                let mentions = &ev["content"]["m.mentions"];
                let for_bot = mentions["room"] == true
                    || mentions["user_ids"].as_array().is_some_and(|ids| ids.iter().any(|id| id == bot));
                if for_bot {
                    out.push(RoomEvent::RingForBot { room: room.clone(), sender, event_id, ts_ms });
                }
            } else if DECLINE_TYPES.contains(&kind) {
                if let Some(id) = ev["content"]["m.relates_to"]["event_id"].as_str() {
                    out.push(RoomEvent::Declined { room: room.clone(), notification: id.to_string() });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(code: u16) -> anyhow::Error {
        HomeserverError { status: reqwest::StatusCode::from_u16(code).unwrap(), errcode: "?".into() }.into()
    }

    #[test]
    fn a_starting_homeserver_is_waited_out_and_a_bad_token_is_not() {
        assert!(is_transient(&said(530)));
        assert!(is_transient(&said(502)));
        assert!(is_transient(&said(429)));
        assert!(!is_transient(&said(401)));
        assert!(!is_transient(&anyhow::anyhow!("whoami without user_id")));
    }

    #[test]
    fn element_x_call_members_and_rings_for_the_bot_are_read() {
        let body = json!({ "rooms": { "join": { "!r:t": { "timeline": { "events": [
            {
                "type": MEMBER_TYPE,
                "state_key": "_@shuntia:matrix.example.org_ALICEPHONE_m.call",
                "sender": "@shuntia:matrix.example.org",
                "event_id": "$m1",
                "origin_server_ts": 1_700_000_000_000_i64,
                "content": { "application": "m.call", "call_id": "", "device_id": "ALICEPHONE", "scope": "m.room" },
            },
            {
                "type": "org.matrix.msc4075.rtc.notification",
                "sender": "@shuntia:matrix.example.org",
                "event_id": "$n1",
                "origin_server_ts": 1_700_000_000_100_i64,
                "content": { "notification_type": "ring", "m.mentions": { "user_ids": ["@note:t"] } },
            },
            {
                "type": NOTIFICATION_TYPE,
                "sender": "@shuntia:matrix.example.org",
                "event_id": "$n2",
                "content": { "notification_type": "ring", "m.mentions": { "user_ids": ["@someone:t"] } },
            },
            {
                "type": NOTIFICATION_TYPE,
                "sender": "@shuntia:matrix.example.org",
                "event_id": "$n3",
                "origin_server_ts": 1_700_000_000_150_i64,
                "content": { "notification_type": "ring", "m.mentions": { "user_ids": [], "room": true } },
            },
            {
                "type": MEMBER_TYPE,
                "state_key": "_@shuntia:matrix.example.org_ALICEPHONE_m.call",
                "sender": "@shuntia:matrix.example.org",
                "event_id": "$m2",
                "origin_server_ts": 1_700_000_000_200_i64,
                "content": {},
            },
        ] } } } } });
        let user = "@shuntia:matrix.example.org".to_string();
        assert_eq!(
            parse_sync(&body, "@note:t"),
            vec![
                RoomEvent::CallMember {
                    room: "!r:t".into(),
                    user: user.clone(),
                    device: "ALICEPHONE".into(),
                    active: true,
                    event_id: "$m1".into(),
                    ts_ms: 1_700_000_000_000,
                },
                RoomEvent::RingForBot {
                    room: "!r:t".into(),
                    sender: user.clone(),
                    event_id: "$n1".into(),
                    ts_ms: 1_700_000_000_100,
                },
                RoomEvent::RingForBot {
                    room: "!r:t".into(),
                    sender: user.clone(),
                    event_id: "$n3".into(),
                    ts_ms: 1_700_000_000_150,
                },
                RoomEvent::CallMember {
                    room: "!r:t".into(),
                    user,
                    device: "ALICEPHONE".into(),
                    active: false,
                    event_id: "$m2".into(),
                    ts_ms: 1_700_000_000_200,
                },
            ]
        );
    }
}
