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

/// Someone's reaction to an event in a joined room.
#[derive(Debug, Clone, PartialEq)]
pub struct Reaction {
    pub room_id: String,
    pub sender: String,
    pub event_id: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub next_batch: String,
    pub messages: Vec<Message>,
    pub reactions: Vec<Reaction>,
}

/// The reaction a button is pressed with.
pub fn emoji(data: &str) -> Option<&'static str> {
    const BY_PREFIX: [(&str, &str); 5] =
        [("ev:done:", "✅"), ("ev:snooze:", "💤"), ("ev:drop:", "✖"), ("carry:", "➡"), ("block:start:", "▶")];
    BY_PREFIX.iter().find(|(p, _)| data.starts_with(p)).map(|(_, e)| *e)
}

/// The key a reaction presses: clients send an emoji with or without its
/// variation selector, and either presses the same button.
pub fn reaction_key(event_id: &str, emoji: &str) -> String {
    format!("mx:{event_id}:{}", emoji.replace('\u{fe0f}', ""))
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
                "timeline": { "types": ["m.room.message", "m.reaction"], "limit": 50 },
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
            reactions: parse_reactions(&body, bot),
        })
    }

    /// Posts `content` as an event of `kind` and returns the event's id.
    fn put_event(&self, room_id: &str, kind: &str, content: &Value) -> Result<String> {
        let txn = uuid::Uuid::new_v4().simple().to_string();
        let url = format!("{}/_matrix/client/v3/rooms/{}/send/{kind}/{txn}", self.base, enc(room_id));
        let mut res = self
            .agent
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .send_json(content)
            .map_err(|e| anyhow::anyhow!(super::describe_http_error(&e)))?;
        let sent: Value = res.body_mut().read_json().unwrap_or_default();
        Ok(sent["event_id"].as_str().unwrap_or_default().to_string())
    }

    pub fn send(&self, room_id: &str, text: &str, kind: Kind) -> Result<String> {
        self.put_event(room_id, "m.room.message", &serde_json::json!({ "msgtype": kind.msgtype(), "body": text }))
    }

    pub fn react(&self, room_id: &str, event_id: &str, key: &str) -> Result<String> {
        let content = serde_json::json!({ "m.relates_to": { "rel_type": "m.annotation", "event_id": event_id, "key": key } });
        self.put_event(room_id, "m.reaction", &content)
    }

    /// Puts each button under the posted message as the bot's own reaction, so
    /// the user presses one by tapping it. A reaction that will not post costs
    /// only its button.
    fn offer(&self, user_id: i64, room_id: &str, event_id: &str, actions: &[super::Action]) -> Result<()> {
        let offered: Vec<(&super::Action, &str)> =
            actions.iter().filter_map(|a| emoji(&a.data).map(|e| (a, e))).collect();
        if event_id.is_empty() || offered.is_empty() {
            return Ok(());
        }
        let keys: Vec<String> = offered.iter().map(|(_, e)| reaction_key(event_id, e)).collect();
        let kept: Vec<super::Action> = offered.iter().map(|(a, _)| (*a).clone()).collect();
        {
            let conn = crate::db_guard(&self.db);
            crate::actions::keep(&conn, user_id, event_id, &keys, &kept, jiff::Timestamp::now())?;
        }
        for (_, e) in &offered {
            if self.react(room_id, event_id, e).is_err() {
                break;
            }
        }
        Ok(())
    }
}

/// The legend under a message that carries buttons: each reaction and what it does.
fn legend(actions: &[super::Action]) -> Option<String> {
    let parts: Vec<String> =
        actions.iter().filter_map(|a| emoji(&a.data).map(|e| format!("{e} {}", a.label))).collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Reactions in joined rooms from anyone but the bot.
fn parse_reactions(body: &Value, bot: &str) -> Vec<Reaction> {
    let Some(rooms) = body["rooms"]["join"].as_object() else { return Vec::new() };
    let mut out = Vec::new();
    for (room, data) in rooms {
        for ev in data["timeline"]["events"].as_array().into_iter().flatten() {
            let sender = ev["sender"].as_str().unwrap_or_default();
            let rel = &ev["content"]["m.relates_to"];
            if ev["type"] != "m.reaction" || sender == bot || rel["rel_type"] != "m.annotation" {
                continue;
            }
            if let (Some(event_id), Some(key)) = (rel["event_id"].as_str(), rel["key"].as_str()) {
                out.push(Reaction {
                    room_id: room.clone(),
                    sender: sender.to_string(),
                    event_id: event_id.to_string(),
                    key: key.to_string(),
                });
            }
        }
    }
    out
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
        let mut text =
            if msg.title.trim().is_empty() { msg.body.clone() } else { format!("{}\n{}", msg.title, msg.body) };
        if let Some(legend) = legend(&msg.actions) {
            text = format!("{text}\n{legend}");
        }
        let kind = if cfg.matrix_ping() { Kind::Text } else { Kind::Notice };
        let event_id = self.send(&room_id, &text, kind)?;
        if let Err(e) = self.offer(user_id, &room_id, &event_id, &msg.actions) {
            let conn = crate::db_guard(&self.db);
            let _ = crate::log::record(&conn, Some(user_id), "matrix_send_error", &format!("offering buttons: {e}"));
        }
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

    fn timeline(events: &serde_json::Value) -> serde_json::Value {
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
        let body = timeline(&serde_json::json!([
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
        let body = timeline(&serde_json::json!([
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
        let reply: &'static str = timeline(&serde_json::json!([])).to_string().leak();
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
            reason: None,
            brief: None,
            body: "How is it going?".into(),
            urgency: crate::channels::Urgency::High,
            checkin: true,
            event_id: Some(1),
            conversation_id: Some(1),
            actions: crate::channels::event_actions(1, crate::text::Lang::En),
            wait: None,
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
        ch.deliver(1, "aki", &OutboundMessage { actions: Vec::new(), ..checkin() }).unwrap();
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
    fn buttons_are_offered_as_the_bots_own_reactions_and_kept_by_them() {
        let (base, rx) = serve(vec![("200 OK", WHOAMI), ("200 OK", SENT), ("200 OK", SENT), ("200 OK", SENT), ("200 OK", SENT)]);
        let (ch, tmp) = channel(&base);
        took(&rx);
        linked(&ch, &tmp, "matrix_send = true\n");
        ch.deliver(1, "aki", &checkin()).unwrap();
        assert_eq!(body_json(&took(&rx))["body"], "Check-in\nHow is it going?\n✅ Done · 💤 Snooze 15 · ✖ Drop");
        let mut keys = Vec::new();
        for _ in 0..3 {
            let raw = took(&rx);
            assert!(raw.contains("/send/m.reaction/"), "{raw}");
            let rel = body_json(&raw)["m.relates_to"].clone();
            assert_eq!((rel["rel_type"].as_str(), rel["event_id"].as_str()), (Some("m.annotation"), Some("$1")));
            keys.push(rel["key"].as_str().unwrap().to_string());
        }
        assert_eq!(keys, ["✅", "💤", "✖"]);
        let conn = crate::db_guard(&ch.db);
        let now = jiff::Timestamp::now();
        let pressed = crate::actions::take(&conn, &reaction_key("$1", "✖\u{fe0f}"), now).unwrap();
        assert_eq!(pressed, Some((1, "ev:drop:1".to_string())));
        assert_eq!(crate::actions::take(&conn, &reaction_key("$1", "✅"), now).unwrap(), None, "one press per message");
    }

    #[test]
    fn parse_reads_reactions_from_others_only() {
        let body = timeline(&serde_json::json!([
            { "type": "m.reaction", "sender": "@aki:t", "content": {
                "m.relates_to": { "rel_type": "m.annotation", "event_id": "$1", "key": "✅" } } },
            { "type": "m.reaction", "sender": "@note:t", "content": {
                "m.relates_to": { "rel_type": "m.annotation", "event_id": "$1", "key": "✅" } } },
            { "type": "m.room.message", "sender": "@aki:t", "content": { "msgtype": "m.text", "body": "hi" } },
        ]));
        assert_eq!(
            parse_reactions(&body, "@note:t"),
            vec![Reaction { room_id: "!dm:t".into(), sender: "@aki:t".into(), event_id: "$1".into(), key: "✅".into() }]
        );
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
