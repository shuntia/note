use super::{Channel, OutboundMessage};
use crate::config::MatrixSettings;
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// How long a sync is asked to hold the connection open.
pub const SYNC_SECS: u64 = 30;
const SYNC_TIMEOUT_SECS: u64 = 45;

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub room_id: String,
    pub sender: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub next_batch: String,
    pub messages: Vec<Message>,
}

/// `Notice` is the msgtype Matrix clients do not notify for by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Notice,
}

impl Kind {
    fn msgtype(self) -> &'static str {
        match self {
            Kind::Text => "m.text",
            Kind::Notice => "m.notice",
        }
    }
}

pub struct MatrixChannel {
    db: Arc<Mutex<Connection>>,
    config_dir: PathBuf,
    base: String,
    token: String,
    user_id: OnceLock<String>,
    agent: ureq::Agent,
    sync_agent: ureq::Agent,
}

fn enc(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

fn agent(global_secs: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(5)))
        .timeout_global(Some(std::time::Duration::from_secs(global_secs)))
        .build()
        .into()
}

impl MatrixChannel {
    /// A token file that will not read fails here; the homeserver is first
    /// asked about the token by the first sync, so an outage never stops boot.
    pub fn new(db: Arc<Mutex<Connection>>, config_dir: PathBuf, cfg: &MatrixSettings) -> Result<Self> {
        let token = std::fs::read_to_string(&cfg.token_file)
            .with_context(|| format!("reading matrix token file {}", cfg.token_file.display()))?
            .trim()
            .to_string();
        anyhow::ensure!(!token.is_empty(), "matrix token file {} is empty", cfg.token_file.display());
        Ok(Self {
            db,
            config_dir,
            base: cfg.homeserver.trim_end_matches('/').to_string(),
            token,
            user_id: OnceLock::new(),
            agent: agent(10),
            sync_agent: agent(SYNC_TIMEOUT_SECS),
        })
    }

    /// The bot's own id, asked of the homeserver once.
    pub fn identify(&self) -> Result<&str> {
        if let Some(id) = self.user_id.get() {
            return Ok(id);
        }
        let who = self.get(&self.agent, "/_matrix/client/v3/account/whoami", &[]).context("whoami")?;
        let id = who["user_id"].as_str().context("whoami named no user_id")?.to_string();
        Ok(self.user_id.get_or_init(|| id))
    }

    fn get(&self, agent: &ureq::Agent, path: &str, query: &[(&str, &str)]) -> Result<Value> {
        let mut req = agent
            .get(format!("{}{path}", self.base))
            .header("Authorization", format!("Bearer {}", self.token));
        for (k, v) in query {
            req = req.query(*k, *v);
        }
        let mut res = req.call().map_err(|e| anyhow::anyhow!(super::describe_http_error(&e)))?;
        res.body_mut().read_json().map_err(|e| anyhow::anyhow!("unreadable reply: {e}"))
    }

    pub fn sync(&self, since: Option<&str>, timeout_secs: u64) -> Result<Batch> {
        let bot = self.identify()?;
        let filter = serde_json::json!({
            "presence": { "not_types": ["*"] },
            "account_data": { "not_types": ["*"] },
            "room": {
                "state": { "types": [] },
                "ephemeral": { "not_types": ["*"] },
                "account_data": { "not_types": ["*"] },
                "timeline": { "types": ["m.room.message"], "limit": 50 },
            },
        })
        .to_string();
        let timeout = (timeout_secs * 1000).to_string();
        let mut query = vec![("timeout", timeout.as_str()), ("filter", filter.as_str())];
        if let Some(since) = since {
            query.push(("since", since));
        }
        let body = self.get(&self.sync_agent, "/_matrix/client/v3/sync", &query)?;
        Ok(Batch {
            next_batch: body["next_batch"].as_str().context("sync without next_batch")?.to_string(),
            messages: parse_sync(&body, bot),
        })
    }

    pub fn send(&self, room_id: &str, text: &str, kind: Kind) -> Result<()> {
        let txn = uuid::Uuid::new_v4().simple().to_string();
        let url = format!("{}/_matrix/client/v3/rooms/{}/send/m.room.message/{txn}", self.base, enc(room_id));
        self.agent
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .send_json(serde_json::json!({ "msgtype": kind.msgtype(), "body": text }))
            .map_err(|e| anyhow::anyhow!(super::describe_http_error(&e)))?;
        Ok(())
    }
}

/// Text messages in joined rooms from anyone but the bot; an edit is not a new message.
fn parse_sync(body: &Value, bot: &str) -> Vec<Message> {
    let Some(rooms) = body["rooms"]["join"].as_object() else { return Vec::new() };
    let mut out = Vec::new();
    for (room, data) in rooms {
        for ev in data["timeline"]["events"].as_array().into_iter().flatten() {
            let sender = ev["sender"].as_str().unwrap_or_default();
            let edit = ev["content"]["m.relates_to"]["rel_type"] == "m.replace";
            if ev["type"] != "m.room.message" || ev["content"]["msgtype"] != "m.text" || sender == bot || edit {
                continue;
            }
            if let Some(text) = ev["content"]["body"].as_str() {
                out.push(Message { room_id: room.clone(), sender: sender.to_string(), text: text.to_string() });
            }
        }
    }
    out
}

impl Channel for MatrixChannel {
    fn name(&self) -> &'static str {
        "matrix"
    }

    fn companion(&self) -> bool {
        true
    }

    /// Posts only for a user who opted in and whose link is joined; anyone
    /// else is `Ok` with nothing sent.
    fn deliver(&self, user_id: i64, username: &str, msg: &OutboundMessage) -> Result<()> {
        let cfg = crate::config::UserConfig::load(&self.config_dir, username)?;
        if !cfg.matrix_send() {
            return Ok(());
        }
        let link = {
            let conn = crate::db_guard(&self.db);
            crate::voice::links::ringable(&conn, user_id)?
        };
        let Some(room_id) = link.and_then(|l| l.room_id) else { return Ok(()) };
        let text =
            if msg.title.trim().is_empty() { msg.body.clone() } else { format!("{}\n{}", msg.title, msg.body) };
        let kind = if cfg.matrix_ping() { Kind::Text } else { Kind::Notice };
        self.send(&room_id, &text, kind)?;
        if let Some(id) = msg.conversation_id {
            let conn = crate::db_guard(&self.db);
            let _ = crate::talk::stamp_matrix(&conn, id, jiff::Timestamp::now());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testhttp::{body_json, serve};
    use std::sync::mpsc::Receiver;

    const WHOAMI: &str = r#"{"user_id":"@note:t","device_id":"SRV"}"#;
    const SENT: &str = r#"{"event_id":"$1"}"#;

    fn took(rx: &Receiver<String>) -> String {
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap()
    }

    fn channel(base: &str) -> (MatrixChannel, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let token_file = tmp.path().join("matrix.token");
        std::fs::write(&token_file, "syt_secret\n").unwrap();
        let db = Arc::new(Mutex::new(crate::db::open_memory().unwrap()));
        let cfg = MatrixSettings { homeserver: base.into(), token_file };
        let ch = MatrixChannel::new(db, tmp.path().into(), &cfg).unwrap();
        ch.identify().unwrap();
        (ch, tmp)
    }

    fn timeline(events: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "next_batch": "s2",
            "rooms": { "join": { "!dm:t": { "timeline": { "events": events } } } },
        })
    }

    #[test]
    fn a_homeserver_that_is_down_does_not_stop_boot() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let tmp = tempfile::tempdir().unwrap();
        let token_file = tmp.path().join("matrix.token");
        std::fs::write(&token_file, "syt_secret\n").unwrap();
        let db = Arc::new(Mutex::new(crate::db::open_memory().unwrap()));
        let cfg = MatrixSettings { homeserver: base, token_file };
        let ch = MatrixChannel::new(db, tmp.path().into(), &cfg).unwrap();
        assert!(ch.sync(None, 0).is_err(), "the sync fails and the loop backs off");
    }

    #[test]
    fn identify_asks_who_the_token_is() {
        let (base, rx) = serve(vec![("200 OK", WHOAMI)]);
        let (ch, _tmp) = channel(&base);
        assert_eq!(ch.identify().unwrap(), "@note:t");
        let raw = took(&rx);
        assert!(raw.starts_with("GET /_matrix/client/v3/account/whoami"), "{raw}");
        assert!(raw.to_lowercase().contains("authorization: bearer syt_secret"), "{raw}");
    }

    #[test]
    fn parse_skips_the_bots_own_and_non_text_messages() {
        let body = timeline(serde_json::json!([
            { "type": "m.room.message", "sender": "@aki:t", "content": { "msgtype": "m.text", "body": "hi" } },
            { "type": "m.room.message", "sender": "@note:t", "content": { "msgtype": "m.text", "body": "echo" } },
            { "type": "m.room.message", "sender": "@aki:t", "content": { "msgtype": "m.image", "body": "pic" } },
            { "type": "m.reaction", "sender": "@aki:t", "content": {} },
        ]));
        assert_eq!(
            parse_sync(&body, "@note:t"),
            vec![Message { room_id: "!dm:t".into(), sender: "@aki:t".into(), text: "hi".into() }]
        );
    }

    #[test]
    fn parse_skips_edits() {
        let body = timeline(serde_json::json!([
            { "type": "m.room.message", "sender": "@aki:t", "content": {
                "msgtype": "m.text", "body": "* fixed",
                "m.new_content": { "msgtype": "m.text", "body": "fixed" },
                "m.relates_to": { "rel_type": "m.replace", "event_id": "$1" },
            } },
        ]));
        assert!(parse_sync(&body, "@note:t").is_empty());
    }

    #[test]
    fn sync_sends_since_and_timeout_and_reads_next_batch() {
        let reply: &'static str = timeline(serde_json::json!([])).to_string().leak();
        let (base, rx) = serve(vec![("200 OK", WHOAMI), ("200 OK", reply)]);
        let (ch, _tmp) = channel(&base);
        took(&rx);
        let batch = ch.sync(Some("s1"), 0).unwrap();
        assert_eq!(batch.next_batch, "s2");
        let raw = took(&rx);
        let line = raw.lines().next().unwrap();
        assert!(line.starts_with("GET /_matrix/client/v3/sync?"), "{line}");
        assert!(line.contains("since=s1") && line.contains("timeout=0"), "{line}");
    }

    #[test]
    fn send_puts_the_msgtype_asked_for() {
        let (base, rx) = serve(vec![("200 OK", WHOAMI), ("200 OK", SENT), ("200 OK", SENT)]);
        let (ch, _tmp) = channel(&base);
        took(&rx);
        ch.send("!dm:t", "hello", Kind::Notice).unwrap();
        let raw = took(&rx);
        assert!(raw.starts_with("PUT /_matrix/client/v3/rooms/%21dm%3At/send/m.room.message/"), "{raw}");
        assert_eq!(body_json(&raw), serde_json::json!({ "msgtype": "m.notice", "body": "hello" }));
        ch.send("!dm:t", "hello", Kind::Text).unwrap();
        assert_eq!(body_json(&took(&rx))["msgtype"], "m.text");
    }

    fn linked(ch: &MatrixChannel, tmp: &tempfile::TempDir, user_toml: &str) {
        let conn = crate::db_guard(&ch.db);
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", []).unwrap();
        conn.execute(
            "INSERT INTO matrix_links (user_id, mxid, room_id, state, created_at)
             VALUES (1, '@aki:t', '!dm:t', 'linked', 'x')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO conversations (user_id, title, created_at, updated_at) VALUES (1, 't', 'c', 'c')", [])
            .unwrap();
        let base = "display_name = \"A\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n";
        for (dir, extra) in [("defaults", ""), ("users/aki", user_toml)] {
            let dir = tmp.path().join(dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("user.toml"), format!("{base}{extra}")).unwrap();
        }
    }

    fn checkin() -> OutboundMessage {
        OutboundMessage {
            title: "Check-in".into(),
            body: "How is it going?".into(),
            urgency: crate::channels::Urgency::High,
            checkin: true,
            event_id: Some(1),
            conversation_id: Some(1),
            actions: crate::channels::event_actions(1),
        }
    }

    fn quiet(rx: &Receiver<String>) -> bool {
        rx.recv_timeout(std::time::Duration::from_millis(250)).is_err()
    }

    #[test]
    fn nothing_is_posted_until_the_user_opts_in() {
        let (base, rx) = serve(vec![("200 OK", WHOAMI), ("200 OK", SENT)]);
        let (ch, tmp) = channel(&base);
        took(&rx);
        linked(&ch, &tmp, "");
        ch.deliver(1, "aki", &checkin()).unwrap();
        assert!(quiet(&rx));
    }

    #[test]
    fn opted_in_posts_a_silent_notice_and_stamps_the_thread() {
        let (base, rx) = serve(vec![("200 OK", WHOAMI), ("200 OK", SENT)]);
        let (ch, tmp) = channel(&base);
        took(&rx);
        linked(&ch, &tmp, "matrix_send = true\n");
        ch.deliver(1, "aki", &checkin()).unwrap();
        assert_eq!(
            body_json(&took(&rx)),
            serde_json::json!({ "msgtype": "m.notice", "body": "Check-in\nHow is it going?" })
        );
        let stamped: bool = crate::db_guard(&ch.db)
            .query_row("SELECT matrix_at IS NOT NULL FROM conversations WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert!(stamped);
    }

    #[test]
    fn ping_posts_text_that_notifies() {
        let (base, rx) = serve(vec![("200 OK", WHOAMI), ("200 OK", SENT)]);
        let (ch, tmp) = channel(&base);
        took(&rx);
        linked(&ch, &tmp, "matrix_send = true\nmatrix_ping = true\n");
        ch.deliver(1, "aki", &checkin()).unwrap();
        assert_eq!(body_json(&took(&rx))["msgtype"], "m.text");
    }

    #[test]
    fn an_unlinked_user_gets_nothing_posted() {
        let (base, rx) = serve(vec![("200 OK", WHOAMI), ("200 OK", SENT)]);
        let (ch, tmp) = channel(&base);
        took(&rx);
        linked(&ch, &tmp, "matrix_send = true\n");
        crate::db_guard(&ch.db).execute("UPDATE matrix_links SET state = 'invited'", []).unwrap();
        ch.deliver(1, "aki", &checkin()).unwrap();
        assert!(quiet(&rx));
    }
}
