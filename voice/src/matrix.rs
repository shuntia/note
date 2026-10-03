use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

pub const MEMBER_TYPE: &str = "org.matrix.msc3401.call.member";
pub const NOTIFICATION_TYPE: &str = "m.rtc.notification";
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
    CallMember { room: String, user: String, active: bool },
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
            events: parse_sync(&body),
        })
    }
}

fn parse_sync(body: &Value) -> Vec<RoomEvent> {
    let mut out = Vec::new();
    let Some(rooms) = body["rooms"]["join"].as_object() else { return out };
    for (room, data) in rooms {
        let lists = [&data["state"]["events"], &data["timeline"]["events"]];
        for ev in lists.into_iter().filter_map(|l| l.as_array()).flatten() {
            let kind = ev["type"].as_str().unwrap_or_default();
            let sender = ev["sender"].as_str().unwrap_or_default().to_string();
            if kind == "m.room.member" && ev["content"]["membership"] == "join" {
                if let Some(user) = ev["state_key"].as_str() {
                    out.push(RoomEvent::Joined { room: room.clone(), user: user.to_string() });
                }
            } else if kind == MEMBER_TYPE {
                let active = ev["content"].as_object().is_some_and(|c| !c.is_empty());
                out.push(RoomEvent::CallMember { room: room.clone(), user: sender, active });
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
}
