use super::{Channel, OutboundMessage, Urgency};
use crate::config::{NtfySettings, UserConfig};
use anyhow::{Context, Result};
use std::path::PathBuf;

const MAX_TOPIC: usize = 64;

/// ntfy addresses a topic by URL path, so the name is restricted to what stays
/// unambiguous there.
pub fn valid_topic(topic: &str) -> bool {
    !topic.is_empty()
        && topic.len() <= MAX_TOPIC
        && topic.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// ntfy's 1-5 priority scale, where 4 and 5 are the ones that break through a
/// quiet phone.
fn priority(urgency: Urgency) -> u8 {
    match urgency {
        Urgency::Low => 2,
        Urgency::Normal => 3,
        Urgency::High => 5,
    }
}

pub struct NtfyChannel {
    config_dir: PathBuf,
    base_url: String,
    topic_prefix: String,
    token: Option<String>,
    click_url: String,
    agent: ureq::Agent,
}

impl NtfyChannel {
    /// A configured `token_file` that is unreadable or blank fails here rather
    /// than at the first delivery.
    pub fn new(config_dir: PathBuf, cfg: &NtfySettings, public_base_url: &str) -> Result<Self> {
        anyhow::ensure!(!cfg.base_url.is_empty(), "ntfy channel requires base_url");
        let token = if cfg.token_file.as_os_str().is_empty() {
            None
        } else {
            let token = std::fs::read_to_string(&cfg.token_file)
                .with_context(|| format!("reading ntfy token file {}", cfg.token_file.display()))?
                .trim()
                .to_string();
            anyhow::ensure!(
                !token.is_empty(),
                "ntfy token file {} is empty",
                cfg.token_file.display()
            );
            Some(token)
        };
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .build();
        Ok(Self {
            config_dir,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            topic_prefix: cfg.topic_prefix.clone(),
            token,
            click_url: public_base_url.trim_end_matches('/').to_string(),
            agent,
        })
    }

    /// A user config that will not load leaves the default topic, so a broken
    /// file costs the topic override and not the delivery.
    fn topic(&self, username: &str) -> String {
        UserConfig::load(&self.config_dir, username)
            .map(|cfg| cfg.ntfy_topic_for(&self.topic_prefix, username))
            .unwrap_or_else(|_| format!("{}{username}", self.topic_prefix))
    }
}

impl Channel for NtfyChannel {
    fn name(&self) -> &'static str {
        "ntfy"
    }

    /// The JSON publish form rather than the header form: a title is often the
    /// event's own words, and HTTP header values cannot carry non-ASCII bytes.
    fn deliver(&self, _user_id: i64, username: &str, msg: &OutboundMessage) -> Result<()> {
        let click = match msg.conversation_id {
            Some(id) => format!("{}{}", self.click_url, super::conversation_path(id)),
            None => self.click_url.clone(),
        };
        let body = serde_json::json!({
            "topic": self.topic(username),
            "title": msg.title,
            "message": msg.body,
            "priority": priority(msg.urgency),
            "tags": ["bell"],
            "click": click,
        });
        let mut req = self.agent.post(&self.base_url);
        if let Some(token) = &self.token {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        match req.send_json(body) {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(code, _)) => anyhow::bail!("status {code}"),
            Err(ureq::Error::Transport(t)) => anyhow::bail!("transport error: {}", t.kind()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::{Channel, OutboundMessage, Urgency};
    use crate::config::NtfySettings;
    use std::io::{Read, Write};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::Receiver;

    fn settings(base_url: &str) -> NtfySettings {
        NtfySettings {
            base_url: base_url.into(),
            token_file: PathBuf::new(),
            topic_prefix: "note-".into(),
        }
    }

    fn config_dir() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("defaults")).unwrap();
        std::fs::write(
            tmp.path().join("defaults/user.toml"),
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n",
        )
        .unwrap();
        tmp
    }

    fn write_user(dir: &Path, user: &str, content: &str) {
        let p = dir.join("users").join(user).join("user.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    /// One-shot HTTP server: answers the first request with `status` and hands
    /// the raw request back, so a delivery's wire shape can be asserted without
    /// a real ntfy.
    fn one_shot(status: &'static str) -> (String, Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                let n = sock.read(&mut buf).unwrap();
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
                    .find_map(|l| l.strip_prefix("content-length: ").or(l.strip_prefix("Content-Length: ")))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                if raw.len() >= head_end + 4 + want || n == 0 {
                    break;
                }
            }
            let _ = sock.write_all(
                format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            );
            let _ = tx.send(String::from_utf8_lossy(&raw).to_string());
        });
        (base, rx)
    }

    fn body_json(raw: &str) -> serde_json::Value {
        let (_, body) = raw.split_once("\r\n\r\n").expect("a request with a body");
        serde_json::from_str(body).unwrap_or_else(|e| panic!("body {body:?}: {e}"))
    }

    fn msg(urgency: Urgency) -> OutboundMessage {
        OutboundMessage {
            title: "Check-in".into(),
            body: "how is the day going?".into(),
            urgency,
            event_id: Some(7),
            conversation_id: None,
        }
    }

    #[test]
    fn a_checkin_clicks_through_to_its_thread() {
        let (base, rx) = one_shot("200 OK");
        let cfg = config_dir();
        let ch = NtfyChannel::new(
            cfg.path().to_path_buf(),
            &settings(&base),
            "https://note.example/",
        )
        .unwrap();
        let mut checkin = msg(Urgency::High);
        checkin.conversation_id = Some(5);
        ch.deliver(1, "aki", &checkin).unwrap();
        let raw = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(body_json(&raw)["click"], "https://note.example/#/chat/5");
    }

    #[test]
    fn topic_validation_follows_ntfys_rule() {
        assert!(valid_topic("note-aki"));
        assert!(valid_topic("A_b-9"));
        assert!(valid_topic(&"x".repeat(64)));
        assert!(!valid_topic(""));
        assert!(!valid_topic(&"x".repeat(65)));
        assert!(!valid_topic("has space"));
        assert!(!valid_topic("has/slash"));
        assert!(!valid_topic("dot.topic"));
    }

    #[test]
    fn posts_the_message_to_the_users_topic() {
        let (base, rx) = one_shot("200 OK");
        let cfg = config_dir();
        write_user(cfg.path(), "aki", "ntfy_topic = \"my-desk\"\n");
        let ch = NtfyChannel::new(
            cfg.path().to_path_buf(),
            &settings(&base),
            "https://note.example",
        )
        .unwrap();
        ch.deliver(1, "aki", &msg(Urgency::High)).unwrap();

        let raw = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(raw.starts_with("POST / "), "request line: {raw}");
        assert!(!raw.contains("Authorization"), "{raw}");
        let v = body_json(&raw);
        assert_eq!(v["topic"], "my-desk");
        assert_eq!(v["title"], "Check-in");
        assert_eq!(v["message"], "how is the day going?");
        assert_eq!(v["priority"], 5);
        assert_eq!(v["tags"], serde_json::json!(["bell"]));
        assert_eq!(v["click"], "https://note.example");
    }

    #[test]
    fn urgency_maps_onto_ntfys_scale() {
        for (urgency, priority) in [(Urgency::Low, 2), (Urgency::Normal, 3), (Urgency::High, 5)] {
            let (base, rx) = one_shot("200 OK");
            let cfg = config_dir();
            let ch =
                NtfyChannel::new(cfg.path().to_path_buf(), &settings(&base), "http://x").unwrap();
            ch.deliver(1, "aki", &msg(urgency)).unwrap();
            let raw = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            assert_eq!(body_json(&raw)["priority"], priority);
        }
    }

    #[test]
    fn an_unreadable_user_config_still_reaches_the_default_topic() {
        let (base, rx) = one_shot("200 OK");
        let cfg = config_dir();
        write_user(cfg.path(), "aki", "ntfy_topic = [broken\n");
        let ch = NtfyChannel::new(cfg.path().to_path_buf(), &settings(&base), "http://x").unwrap();
        ch.deliver(1, "aki", &msg(Urgency::Normal)).unwrap();
        let raw = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(body_json(&raw)["topic"], "note-aki");
    }

    /// A title is the event's own words, which a header value cannot carry.
    #[test]
    fn a_non_ascii_title_is_delivered_intact() {
        let (base, rx) = one_shot("200 OK");
        let cfg = config_dir();
        let ch = NtfyChannel::new(cfg.path().to_path_buf(), &settings(&base), "http://x").unwrap();
        let msg = OutboundMessage {
            title: "朝のチェックイン".into(),
            body: "今日はどう？".into(),
            urgency: Urgency::High,
            event_id: Some(7),
            conversation_id: None,
        };
        ch.deliver(1, "aki", &msg).unwrap();
        let raw = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let v = body_json(&raw);
        assert_eq!(v["title"], "朝のチェックイン");
        assert_eq!(v["message"], "今日はどう？");
    }

    #[test]
    fn a_configured_token_travels_as_a_bearer() {
        let (base, rx) = one_shot("200 OK");
        let cfg = config_dir();
        let key = cfg.path().join("ntfy.key");
        std::fs::write(&key, "  tk_secret\n").unwrap();
        let mut s = settings(&base);
        s.token_file = key;
        let ch = NtfyChannel::new(cfg.path().to_path_buf(), &s, "http://x").unwrap();
        ch.deliver(1, "aki", &msg(Urgency::Normal)).unwrap();
        let raw = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(raw.contains("Authorization: Bearer tk_secret\r\n"), "{raw}");
    }

    #[test]
    fn a_rejected_publish_is_an_error_the_ladder_can_fall_through() {
        let (base, _rx) = one_shot("500 Internal Server Error");
        let cfg = config_dir();
        let ch = NtfyChannel::new(cfg.path().to_path_buf(), &settings(&base), "http://x").unwrap();
        let err = ch.deliver(1, "aki", &msg(Urgency::Normal)).unwrap_err();
        assert!(err.to_string().contains("500"), "unexpected error: {err}");
    }

    #[test]
    fn a_dead_server_is_an_error_not_a_panic() {
        let cfg = config_dir();
        let ch = NtfyChannel::new(
            cfg.path().to_path_buf(),
            &settings("http://127.0.0.1:1"),
            "http://x",
        )
        .unwrap();
        assert!(ch.deliver(1, "aki", &msg(Urgency::Normal)).is_err());
    }

    #[test]
    fn startup_rejects_a_blank_base_url_or_an_unusable_token_file() {
        let cfg = config_dir();
        assert!(NtfyChannel::new(cfg.path().to_path_buf(), &settings(""), "http://x").is_err());

        let mut s = settings("http://x:2586");
        s.token_file = cfg.path().join("missing.key");
        assert!(NtfyChannel::new(cfg.path().to_path_buf(), &s, "http://x").is_err());

        let blank = cfg.path().join("blank.key");
        std::fs::write(&blank, "\n").unwrap();
        s.token_file = blank;
        assert!(NtfyChannel::new(cfg.path().to_path_buf(), &s, "http://x").is_err());
    }

    #[test]
    fn a_trailing_slash_on_the_base_url_does_not_double_up() {
        let (base, rx) = one_shot("200 OK");
        let cfg = config_dir();
        let ch = NtfyChannel::new(
            cfg.path().to_path_buf(),
            &settings(&format!("{base}/")),
            "http://x",
        )
        .unwrap();
        ch.deliver(1, "aki", &msg(Urgency::Normal)).unwrap();
        let raw = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(raw.starts_with("POST / "), "request line: {raw}");
        assert_eq!(body_json(&raw)["topic"], "note-aki");
    }
}
