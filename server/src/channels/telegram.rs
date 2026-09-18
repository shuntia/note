use super::{Action, Channel, OutboundMessage};
use crate::config::TelegramSettings;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

/// Telegram refuses a message past its own cap; this leaves room under it.
pub const MAX_CHARS: usize = 4000;
/// How long `getUpdates` is asked to hold the connection open, and how long the
/// client waits for it to come back.
const POLL_SECS: u64 = 30;
const POLL_TIMEOUT_SECS: u64 = 40;

/// One text message from a chat, all a linked conversation needs of an update.
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub update_id: i64,
    pub chat_id: i64,
    pub handle: String,
    pub text: String,
}

/// A button pressed on a message Note sent, carrying what it needs to apply the
/// answer and to write the outcome back onto the message it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Callback {
    pub update_id: i64,
    pub callback_id: String,
    pub chat_id: i64,
    pub message_id: i64,
    pub text: String,
    pub data: String,
}

/// `last_update_id` counts every update in the batch, including the ones that
/// carried nothing to answer, so the cursor never stalls on them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Batch {
    pub last_update_id: Option<i64>,
    pub messages: Vec<Update>,
    pub callbacks: Vec<Callback>,
}

pub struct TelegramChannel {
    db: Arc<Mutex<Connection>>,
    base_url: String,
    token: String,
    bot: String,
    agent: ureq::Agent,
    poll_agent: ureq::Agent,
}

impl TelegramChannel {
    /// A token file that will not read, or a token the API refuses, fails here
    /// rather than at the first delivery.
    pub fn new(db: Arc<Mutex<Connection>>, cfg: &TelegramSettings) -> Result<Self> {
        anyhow::ensure!(!cfg.base_url.is_empty(), "telegram channel requires base_url");
        let token = std::fs::read_to_string(&cfg.token_file)
            .with_context(|| format!("reading telegram token file {}", cfg.token_file.display()))?
            .trim()
            .to_string();
        anyhow::ensure!(
            !token.is_empty(),
            "telegram token file {} is empty",
            cfg.token_file.display()
        );
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .build();
        let poll_agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(POLL_TIMEOUT_SECS))
            .build();
        let mut ch = Self {
            db,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            token,
            bot: String::new(),
            agent,
            poll_agent,
        };
        ch.bot = ch.get_me().with_context(|| {
            format!("the bot token in {} was refused", cfg.token_file.display())
        })?;
        Ok(ch)
    }

    /// The bot's @name, which the deep link a user follows is addressed to.
    pub fn bot(&self) -> &str {
        &self.bot
    }

    fn call(&self, agent: &ureq::Agent, method: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let url = format!("{}/bot{}/{method}", self.base_url, self.token);
        let res = match agent.post(&url).send_json(body) {
            Ok(res) => res,
            Err(ureq::Error::Status(code, _)) => anyhow::bail!("status {code}"),
            Err(ureq::Error::Transport(t)) => anyhow::bail!("transport error: {}", t.kind()),
        };
        let body: serde_json::Value =
            res.into_json().map_err(|e| anyhow::anyhow!("unreadable reply: {e}"))?;
        if body["ok"] != serde_json::Value::Bool(true) {
            let why = body["description"].as_str().unwrap_or("refused").to_string();
            anyhow::bail!("{method}: {why}");
        }
        Ok(body["result"].clone())
    }

    fn get_me(&self) -> Result<String> {
        let result = self.call(&self.agent, "getMe", serde_json::json!({}))?;
        Ok(result["username"].as_str().context("getMe named no username")?.to_string())
    }

    /// The buttons ride on the last part, so a reply that travels as several
    /// messages still ends with one keyboard.
    pub fn send_message(&self, chat_id: i64, text: &str, actions: &[Action]) -> Result<()> {
        let parts = split(text);
        let last = parts.len() - 1;
        for (i, part) in parts.into_iter().enumerate() {
            let mut body = serde_json::json!({ "chat_id": chat_id, "text": part });
            if i == last && !actions.is_empty() {
                body["reply_markup"] = keyboard(actions);
            }
            self.call(&self.agent, "sendMessage", body)?;
        }
        Ok(())
    }

    pub fn answer_callback(&self, callback_id: &str, text: &str) -> Result<()> {
        self.call(
            &self.agent,
            "answerCallbackQuery",
            serde_json::json!({ "callback_query_id": callback_id, "text": text }),
        )?;
        Ok(())
    }

    /// Takes the buttons off a message that has been answered.
    pub fn clear_keyboard(&self, chat_id: i64, message_id: i64) -> Result<()> {
        self.call(
            &self.agent,
            "editMessageReplyMarkup",
            serde_json::json!({ "chat_id": chat_id, "message_id": message_id }),
        )?;
        Ok(())
    }

    pub fn edit_text(&self, chat_id: i64, message_id: i64, text: &str) -> Result<()> {
        self.call(
            &self.agent,
            "editMessageText",
            serde_json::json!({ "chat_id": chat_id, "message_id": message_id, "text": text }),
        )?;
        Ok(())
    }

    /// The reply Note sends back into a chat of its own accord; `Err` when the
    /// account has no chat.
    pub fn send_to_user(&self, user_id: i64, text: &str) -> Result<()> {
        let chat_id = self.chat_for(user_id)?;
        self.send_message(chat_id, text, &[])
    }

    pub fn get_updates(&self, offset: i64) -> Result<Batch> {
        let result = self.call(
            &self.poll_agent,
            "getUpdates",
            serde_json::json!({
                "offset": offset,
                "timeout": POLL_SECS,
                "allowed_updates": ["message", "callback_query"],
            }),
        )?;
        let updates = result.as_array().context("getUpdates returned no list")?;
        let mut batch = Batch::default();
        for raw in updates {
            if let Some(id) = raw["update_id"].as_i64() {
                batch.last_update_id = Some(batch.last_update_id.map_or(id, |seen| seen.max(id)));
            }
            if let Some(update) = parse_update(raw) {
                batch.messages.push(update);
            }
            if let Some(callback) = parse_callback(raw) {
                batch.callbacks.push(callback);
            }
        }
        Ok(batch)
    }

    fn chat_for(&self, user_id: i64) -> Result<i64> {
        let chat_id = {
            let conn = crate::db_guard(&self.db);
            crate::telegram::chat_for_user(&conn, user_id)?
        };
        chat_id.context("not linked")
    }
}

/// One row of buttons; Telegram's `data` is capped at 64 bytes, and a label
/// whose data will not fit is dropped rather than refused by the API.
fn keyboard(actions: &[Action]) -> serde_json::Value {
    let row: Vec<serde_json::Value> = actions
        .iter()
        .filter(|a| a.data.len() <= super::MAX_ACTION_DATA)
        .map(|a| serde_json::json!({ "text": a.label, "callback_data": a.data }))
        .collect();
    serde_json::json!({ "inline_keyboard": [row] })
}

fn parse_callback(raw: &serde_json::Value) -> Option<Callback> {
    let query = raw.get("callback_query")?;
    let message = query.get("message")?;
    Some(Callback {
        update_id: raw["update_id"].as_i64()?,
        callback_id: query["id"].as_str()?.to_string(),
        chat_id: message["chat"]["id"].as_i64()?,
        message_id: message["message_id"].as_i64()?,
        text: message["text"].as_str().unwrap_or_default().to_string(),
        data: query["data"].as_str()?.to_string(),
    })
}

fn parse_update(raw: &serde_json::Value) -> Option<Update> {
    let message = raw.get("message")?;
    Some(Update {
        update_id: raw["update_id"].as_i64()?,
        chat_id: message["chat"]["id"].as_i64()?,
        handle: message["from"]["username"].as_str().unwrap_or_default().to_string(),
        text: message["text"].as_str()?.to_string(),
    })
}

/// A reply longer than one message travels as several, cut at a line break
/// where there is one within reach of the cap.
pub fn split(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = text;
    while rest.chars().count() > MAX_CHARS {
        let limit = rest.char_indices().nth(MAX_CHARS).map(|(i, _)| i).unwrap_or(rest.len());
        let cut = rest[..limit].rfind('\n').map_or(limit, |i| i + 1);
        parts.push(rest[..cut].trim_end().to_string());
        rest = &rest[cut..];
    }
    if parts.is_empty() || !rest.trim().is_empty() {
        parts.push(rest.to_string());
    }
    parts
}

impl Channel for TelegramChannel {
    fn name(&self) -> &'static str {
        "telegram"
    }

    /// An account with no chat is an `Err`, so the ladder falls through to the
    /// channels the browser and the phone are reached by.
    fn deliver(&self, user_id: i64, _username: &str, msg: &OutboundMessage) -> Result<()> {
        let chat_id = self.chat_for(user_id)?;
        let text = if msg.title.trim().is_empty() {
            msg.body.clone()
        } else {
            format!("{}\n{}", msg.title, msg.body)
        };
        self.send_message(chat_id, &text, &msg.actions)?;
        if let Some(id) = msg.conversation_id {
            let conn = crate::db_guard(&self.db);
            let _ = crate::talk::stamp_telegram(&conn, id, jiff::Timestamp::now());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::Urgency;
    use std::io::{Read, Write};
    use std::sync::mpsc::Receiver;

    const GET_ME: &str = r#"{"ok":true,"result":{"id":7,"username":"note_bot"}}"#;
    const SENT: &str = r#"{"ok":true,"result":{"message_id":1}}"#;

    /// Answers each request in turn with the next `(status, body)` and hands the
    /// raw requests back, so the wire a delivery puts out can be asserted
    /// without a real Telegram.
    fn serve(responses: Vec<(&'static str, &'static str)>) -> (String, Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let Ok((mut sock, _)) = listener.accept() else { return };
                let mut raw = Vec::new();
                let mut buf = [0u8; 1024];
                loop {
                    let Ok(n) = sock.read(&mut buf) else { return };
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw).to_string();
                    let Some(head_end) = text.find("\r\n\r\n") else {
                        if n == 0 {
                            break;
                        }
                        continue;
                    };
                    let want: usize = text[..head_end]
                        .lines()
                        .find_map(|l| {
                            l.strip_prefix("content-length: ")
                                .or(l.strip_prefix("Content-Length: "))
                        })
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    if raw.len() >= head_end + 4 + want || n == 0 {
                        break;
                    }
                }
                let _ = sock.write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = tx.send(String::from_utf8_lossy(&raw).to_string());
            }
        });
        (base, rx)
    }

    fn body_json(raw: &str) -> serde_json::Value {
        let (_, body) = raw.split_once("\r\n\r\n").expect("a request with a body");
        serde_json::from_str(body).unwrap_or_else(|e| panic!("body {body:?}: {e}"))
    }

    fn took(rx: &Receiver<String>) -> String {
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap()
    }

    fn db() -> Arc<Mutex<Connection>> {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn settings(dir: &std::path::Path, base_url: &str, token: &str) -> TelegramSettings {
        let token_file = dir.join("telegram.token");
        std::fs::write(&token_file, token).unwrap();
        TelegramSettings { token_file, base_url: base_url.into() }
    }

    fn channel(db: Arc<Mutex<Connection>>, base: &str) -> (TelegramChannel, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        match TelegramChannel::new(db, &settings(tmp.path(), base, "  bot:secret\n")) {
            Ok(ch) => (ch, tmp),
            Err(e) => panic!("the channel refused to boot: {e}"),
        }
    }

    fn refusal(built: Result<TelegramChannel>) -> String {
        match built {
            Ok(_) => panic!("expected the boot to be refused"),
            Err(e) => e.to_string(),
        }
    }

    fn link(db: &Arc<Mutex<Connection>>, user_id: i64, chat_id: i64) {
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO telegram_links (user_id, chat_id, handle, linked_at)
             VALUES (?1, ?2, 'aki_t', 'now')",
            (user_id, chat_id),
        )
        .unwrap();
    }

    fn msg() -> OutboundMessage {
        OutboundMessage {
            title: "Check-in".into(),
            body: "how is the day going?".into(),
            urgency: Urgency::High,
            event_id: Some(7),
            conversation_id: None,
            actions: Vec::new(),
        }
    }

    #[test]
    fn boot_learns_the_bot_name_from_the_token() {
        let (base, rx) = serve(vec![("200 OK", GET_ME)]);
        let (ch, _tmp) = channel(db(), &base);
        assert_eq!(ch.bot(), "note_bot");
        let raw = took(&rx);
        assert!(raw.starts_with("POST /botbot:secret/getMe "), "request line: {raw}");
    }

    #[test]
    fn boot_refuses_a_missing_blank_or_rejected_token() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = settings(tmp.path(), "http://x", "t");
        cfg.base_url = String::new();
        assert!(TelegramChannel::new(db(), &cfg).is_err());

        let mut cfg = settings(tmp.path(), "http://127.0.0.1:1", "t");
        cfg.token_file = tmp.path().join("missing.token");
        let err = refusal(TelegramChannel::new(db(), &cfg));
        assert!(err.contains("missing.token"), "unexpected error: {err}");

        let cfg = settings(tmp.path(), "http://127.0.0.1:1", "\n  \n");
        let err = refusal(TelegramChannel::new(db(), &cfg));
        assert!(err.contains("telegram.token"), "unexpected error: {err}");

        let (base, _rx) =
            serve(vec![("401 Unauthorized", r#"{"ok":false,"description":"Unauthorized"}"#)]);
        let cfg = settings(tmp.path(), &base, "bot:wrong");
        let err = refusal(TelegramChannel::new(db(), &cfg));
        assert!(err.contains("telegram.token"), "unexpected error: {err}");
    }

    #[test]
    fn a_delivery_names_the_chat_and_leads_with_the_title() {
        let db = db();
        link(&db, 1, 4242);
        let (base, rx) = serve(vec![("200 OK", GET_ME), ("200 OK", SENT)]);
        let (ch, _tmp) = channel(db, &base);
        took(&rx);
        ch.deliver(1, "aki", &msg()).unwrap();

        let raw = took(&rx);
        assert!(raw.starts_with("POST /botbot:secret/sendMessage "), "request line: {raw}");
        let v = body_json(&raw);
        assert_eq!(v["chat_id"], 4242);
        assert_eq!(v["text"], "Check-in\nhow is the day going?");
    }

    #[test]
    fn a_checkin_stamps_the_thread_it_opened() {
        let db = db();
        link(&db, 1, 4242);
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO conversations (user_id, title, created_at, updated_at)
                 VALUES (1, 'morning', 'c', 'c')",
                [],
            )
            .unwrap();
        }
        let (base, rx) = serve(vec![("200 OK", GET_ME), ("200 OK", SENT)]);
        let (ch, _tmp) = channel(db.clone(), &base);
        took(&rx);
        let mut checkin = msg();
        checkin.conversation_id = Some(1);
        ch.deliver(1, "aki", &checkin).unwrap();
        took(&rx);

        let conn = db.lock().unwrap();
        let at: Option<String> = conn
            .query_row("SELECT telegram_at FROM conversations WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert!(at.is_some(), "the thread carries no telegram stamp");
    }

    #[test]
    fn an_unlinked_account_is_an_error_the_ladder_falls_through() {
        let (base, rx) = serve(vec![("200 OK", GET_ME)]);
        let (ch, _tmp) = channel(db(), &base);
        took(&rx);
        let err = ch.deliver(1, "aki", &msg()).unwrap_err();
        assert_eq!(err.to_string(), "not linked");
        assert_eq!(ch.name(), "telegram");
    }

    #[test]
    fn a_refused_send_is_an_error_not_a_panic() {
        let blocked = db();
        link(&blocked, 1, 4242);
        let (base, rx) = serve(vec![
            ("200 OK", GET_ME),
            ("403 Forbidden", r#"{"ok":false,"description":"bot was blocked by the user"}"#),
        ]);
        let (ch, _tmp) = channel(blocked, &base);
        took(&rx);
        let err = ch.deliver(1, "aki", &msg()).unwrap_err();
        assert!(err.to_string().contains("403"), "unexpected error: {err}");

        let (base, rx) = serve(vec![("200 OK", GET_ME)]);
        let live = db();
        link(&live, 1, 4242);
        let (ch, _tmp) = channel(live, &base);
        took(&rx);
        assert!(ch.deliver(1, "aki", &msg()).is_err(), "a server that has gone is an error");
    }

    #[test]
    fn a_long_reply_travels_as_several_messages() {
        let line = format!("{}\n", "x".repeat(99));
        let long = line.repeat(60);
        let parts = split(&long);
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|p| p.chars().count() <= MAX_CHARS));
        assert!(parts[0].ends_with('x'), "a part is cut at a line break");
        assert_eq!(
            parts.join("\n").replace('\n', ""),
            long.replace('\n', ""),
            "nothing is lost in the split"
        );

        let unbroken = "y".repeat(MAX_CHARS + 10);
        let parts = split(&unbroken);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].chars().count(), MAX_CHARS);
        assert_eq!(parts[1].chars().count(), 10);

        assert_eq!(split("short"), vec!["short".to_string()]);
        assert_eq!(split(""), vec![String::new()]);
        let wide = "日".repeat(MAX_CHARS);
        assert_eq!(split(&wide), vec![wide.clone()], "the cap counts characters");
    }

    #[test]
    fn a_batch_keeps_text_messages_and_still_counts_the_rest() {
        let updates = r#"{"ok":true,"result":[
            {"update_id":11,"message":{"chat":{"id":42},"from":{"username":"aki_t"},"text":"hi"}},
            {"update_id":12,"message":{"chat":{"id":42},"sticker":{"file_id":"s"}}},
            {"update_id":13,"edited_message":{"chat":{"id":42},"text":"no"}}
        ]}"#;
        let (base, rx) = serve(vec![("200 OK", GET_ME), ("200 OK", updates)]);
        let (ch, _tmp) = channel(db(), &base);
        took(&rx);
        let batch = ch.get_updates(11).unwrap();

        let raw = took(&rx);
        assert!(raw.starts_with("POST /botbot:secret/getUpdates "), "request line: {raw}");
        let v = body_json(&raw);
        assert_eq!(v["offset"], 11);
        assert_eq!(v["timeout"], 30);
        assert_eq!(v["allowed_updates"], serde_json::json!(["message", "callback_query"]));

        assert_eq!(batch.last_update_id, Some(13));
        assert_eq!(
            batch.messages,
            vec![Update { update_id: 11, chat_id: 42, handle: "aki_t".into(), text: "hi".into() }]
        );
        assert!(batch.callbacks.is_empty());
    }

    #[test]
    fn a_batch_carries_the_buttons_that_were_pressed() {
        let updates = r#"{"ok":true,"result":[
            {"update_id":21,"callback_query":{"id":"q1","data":"ev:done:7",
                "from":{"username":"aki_t"},
                "message":{"message_id":90,"chat":{"id":42},"text":"Check-in\nhow is it going?"}}},
            {"update_id":22,"callback_query":{"id":"q2","data":"ev:drop:7"}}
        ]}"#;
        let (base, rx) = serve(vec![("200 OK", GET_ME), ("200 OK", updates)]);
        let (ch, _tmp) = channel(db(), &base);
        took(&rx);
        let batch = ch.get_updates(0).unwrap();
        took(&rx);

        assert_eq!(batch.last_update_id, Some(22));
        assert!(batch.messages.is_empty());
        assert_eq!(
            batch.callbacks,
            vec![Callback {
                update_id: 21,
                callback_id: "q1".into(),
                chat_id: 42,
                message_id: 90,
                text: "Check-in\nhow is it going?".into(),
                data: "ev:done:7".into(),
            }],
            "a press with no message to write back onto is counted, not kept"
        );
    }

    #[test]
    fn a_message_with_actions_carries_one_row_of_buttons() {
        let db = db();
        link(&db, 1, 4242);
        let (base, rx) = serve(vec![("200 OK", GET_ME), ("200 OK", SENT)]);
        let (ch, _tmp) = channel(db, &base);
        took(&rx);
        let mut m = msg();
        m.actions = crate::channels::event_actions(7);
        ch.deliver(1, "aki", &m).unwrap();

        let v = body_json(&took(&rx));
        assert_eq!(
            v["reply_markup"],
            serde_json::json!({ "inline_keyboard": [[
                { "text": "Done", "callback_data": "ev:done:7" },
                { "text": "Snooze 15", "callback_data": "ev:snooze:7:15" },
                { "text": "Drop", "callback_data": "ev:drop:7" },
            ]] })
        );
    }

    #[test]
    fn only_the_last_of_a_split_message_holds_the_buttons() {
        let db = db();
        link(&db, 1, 4242);
        let (base, rx) = serve(vec![("200 OK", GET_ME), ("200 OK", SENT), ("200 OK", SENT)]);
        let (ch, _tmp) = channel(db, &base);
        took(&rx);
        let long = format!("{}\n", "x".repeat(99)).repeat(60);
        ch.send_message(4242, &long, &crate::channels::event_actions(7)).unwrap();

        assert!(body_json(&took(&rx)).get("reply_markup").is_none());
        assert!(body_json(&took(&rx))["reply_markup"]["inline_keyboard"][0].is_array());
    }

    #[test]
    fn a_label_whose_data_will_not_fit_is_left_off() {
        let long = Action { label: "Carry".into(), data: "carry:".to_string() + &"x".repeat(64) };
        let row = keyboard(&[long, Action { label: "Done".into(), data: "ev:done:7".into() }]);
        assert_eq!(
            row,
            serde_json::json!({ "inline_keyboard": [[
                { "text": "Done", "callback_data": "ev:done:7" },
            ]] })
        );
    }

    #[test]
    fn an_answered_press_is_toasted_disarmed_and_written_back() {
        let ok = r#"{"ok":true,"result":true}"#;
        let (base, rx) = serve(vec![("200 OK", GET_ME), ("200 OK", ok), ("200 OK", ok), ("200 OK", ok)]);
        let (ch, _tmp) = channel(db(), &base);
        took(&rx);
        ch.answer_callback("q1", "Done").unwrap();
        ch.clear_keyboard(42, 90).unwrap();
        ch.edit_text(42, 90, "Check-in\n✓ Done").unwrap();

        let raw = took(&rx);
        assert!(raw.starts_with("POST /botbot:secret/answerCallbackQuery "), "{raw}");
        let v = body_json(&raw);
        assert_eq!(v["callback_query_id"], "q1");
        assert_eq!(v["text"], "Done");

        let raw = took(&rx);
        assert!(raw.starts_with("POST /botbot:secret/editMessageReplyMarkup "), "{raw}");
        let v = body_json(&raw);
        assert_eq!(v["chat_id"], 42);
        assert_eq!(v["message_id"], 90);
        assert!(v.get("reply_markup").is_none(), "the keyboard is removed, not replaced");

        let raw = took(&rx);
        assert!(raw.starts_with("POST /botbot:secret/editMessageText "), "{raw}");
        assert_eq!(body_json(&raw)["text"], "Check-in\n✓ Done");
    }

    #[test]
    fn an_empty_batch_moves_nothing() {
        let (base, rx) = serve(vec![("200 OK", GET_ME), ("200 OK", r#"{"ok":true,"result":[]}"#)]);
        let (ch, _tmp) = channel(db(), &base);
        took(&rx);
        assert_eq!(ch.get_updates(0).unwrap(), Batch::default());
    }
}
