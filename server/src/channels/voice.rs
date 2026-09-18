use super::{Channel, OutboundMessage};
use crate::config::{UserConfig, VoiceSettings};
use anyhow::{Context, Result};
use argon2::password_hash::rand_core::{OsRng, RngCore};
use base64::Engine;
use hmac::{Hmac, Mac};
use rusqlite::{Connection, OptionalExtension};
use sha1::Sha1;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use thiserror::Error;

const TOKEN_BYTES: usize = 32;
/// How long a callback token stays answerable; a call Twilio never completed
/// cannot reopen a decision the next day.
pub const TOKEN_TTL_SECS: i64 = 2 * 60 * 60;
pub const KEYPAD_SNOOZE_MINUTES: i64 = 15;
const RING_SECONDS: &str = "25";

/// E.164: a plus, a non-zero country digit, and 7 to 15 digits in all.
pub fn valid_phone(number: &str) -> bool {
    let Some(digits) = number.strip_prefix('+') else {
        return false;
    };
    (7..=15).contains(&digits.len())
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !digits.starts_with('0')
}

pub fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// `application/x-www-form-urlencoded` as Twilio sends it, in wire order; the
/// signature needs every name and value exactly as they were decoded.
pub fn parse_form(body: &str) -> Vec<(String, String)> {
    body.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(&text[i + 1..i + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Twilio's request signature: HMAC-SHA1, keyed by the account's auth token,
/// over the full public URL followed by every form parameter sorted by name,
/// each name immediately followed by its value.
pub fn sign(auth_token: &str, url: &str, params: &[(String, String)]) -> String {
    let mut sorted: Vec<&(String, String)> = params.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut data = url.to_string();
    for (name, value) in sorted {
        data.push_str(name);
        data.push_str(value);
    }
    let mut mac = Hmac::<Sha1>::new_from_slice(auth_token.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(data.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

fn same_signature(expected: &str, given: &str) -> bool {
    expected.len() == given.len()
        && expected
            .bytes()
            .zip(given.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

#[derive(Debug, Error)]
pub enum CallRefused {
    #[error("no phone number")]
    NoPhone,
    #[error("calls are off")]
    Disabled,
    #[error("{0}")]
    Upstream(String),
}

/// One placed call, as the inbound routes read it.
pub struct VoiceCall {
    pub id: i64,
    pub user_id: i64,
    pub username: String,
    pub event_id: Option<i64>,
    pub message: String,
    pub digit: Option<String>,
}

pub struct VoiceChannel {
    config_dir: PathBuf,
    db: Arc<Mutex<Connection>>,
    account_sid: String,
    auth_token: String,
    from_number: String,
    base_url: String,
    public_base_url: String,
    agent: ureq::Agent,
}

impl VoiceChannel {
    /// Everything Twilio needs is settled here rather than at the first call: a
    /// blank account, an unusable token file, a caller id that is not E.164, or
    /// a public base URL Twilio would refuse to fetch TwiML from.
    pub fn new(
        config_dir: PathBuf,
        db: Arc<Mutex<Connection>>,
        cfg: &VoiceSettings,
        public_base_url: &str,
    ) -> Result<Self> {
        anyhow::ensure!(!cfg.account_sid.trim().is_empty(), "voice channel requires account_sid");
        anyhow::ensure!(!cfg.base_url.trim().is_empty(), "voice channel requires base_url");
        anyhow::ensure!(
            valid_phone(&cfg.from_number),
            "voice from_number {:?} is not E.164",
            cfg.from_number
        );
        let public = public_base_url.trim_end_matches('/').to_string();
        anyhow::ensure!(
            public.starts_with("https://"),
            "voice channel needs an https public_base_url; Twilio will not fetch TwiML from {public}"
        );
        let auth_token = std::fs::read_to_string(&cfg.auth_token_file)
            .with_context(|| {
                format!("reading twilio token file {}", cfg.auth_token_file.display())
            })?
            .trim()
            .to_string();
        anyhow::ensure!(
            !auth_token.is_empty(),
            "twilio token file {} is empty",
            cfg.auth_token_file.display()
        );
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(15))
            .build();
        Ok(Self {
            config_dir,
            db,
            account_sid: cfg.account_sid.trim().to_string(),
            auth_token,
            from_number: cfg.from_number.clone(),
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            public_base_url: public,
            agent,
        })
    }

    /// Whether Twilio signed this request. The URL it signed is the public one
    /// this server is reachable at plus the path, never a host header: the
    /// tunnel in front of us terminates TLS and rewrites both.
    pub fn signed(&self, path_and_query: &str, params: &[(String, String)], header: &str) -> bool {
        let url = format!("{}{path_and_query}", self.public_base_url);
        same_signature(&sign(&self.auth_token, &url, params), header)
    }

    /// Rings the user whether or not check-in calls are switched on, for the
    /// test call they asked for themselves.
    pub fn call_now(
        &self,
        user_id: i64,
        username: &str,
        msg: &OutboundMessage,
    ) -> Result<i64, CallRefused> {
        self.ring(user_id, username, msg, false)
    }

    fn ring(
        &self,
        user_id: i64,
        username: &str,
        msg: &OutboundMessage,
        require_enabled: bool,
    ) -> Result<i64, CallRefused> {
        let cfg = UserConfig::load(&self.config_dir, username)
            .map_err(|e| CallRefused::Upstream(e.to_string()))?;
        let Some(phone) = cfg.phone().map(str::to_string) else {
            return Err(CallRefused::NoPhone);
        };
        let token = new_token();
        let id = {
            let conn = crate::db_guard(&self.db);
            let category: String = conn
                .query_row("SELECT category FROM users WHERE id = ?1", [user_id], |r| r.get(0))
                .unwrap_or_else(|_| crate::config::CATEGORY_MEMBER.to_string());
            if require_enabled && !cfg.features(&category).calls {
                return Err(CallRefused::Disabled);
            }
            let now = jiff::Timestamp::now().to_string();
            conn.execute(
                "INSERT INTO voice_calls (token, user_id, event_id, message, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                (&token, user_id, msg.event_id, &msg.body, &now),
            )
            .map_err(|e| CallRefused::Upstream(e.to_string()))?;
            conn.last_insert_rowid()
        };
        match self.place(&phone, &token) {
            Ok(sid) => {
                let conn = crate::db_guard(&self.db);
                let _ = conn.execute(
                    "UPDATE voice_calls SET call_sid = ?1, updated_at = ?2 WHERE id = ?3",
                    (&sid, jiff::Timestamp::now().to_string(), id),
                );
                Ok(id)
            }
            Err(e) => {
                let conn = crate::db_guard(&self.db);
                let _ = conn.execute(
                    "UPDATE voice_calls SET status = 'failed', updated_at = ?1 WHERE id = ?2",
                    (jiff::Timestamp::now().to_string(), id),
                );
                let _ = crate::log::record(
                    &conn,
                    Some(user_id),
                    "voice_failed",
                    &format!("call {id}: {e}"),
                );
                Err(CallRefused::Upstream(e.to_string()))
            }
        }
    }

    fn place(&self, to: &str, token: &str) -> Result<String> {
        let url =
            format!("{}/2010-04-01/Accounts/{}/Calls.json", self.base_url, self.account_sid);
        let auth = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", self.account_sid, self.auth_token));
        let twiml = format!("{}/api/voice/twiml/{token}", self.public_base_url);
        let status = format!("{}/api/voice/status/{token}", self.public_base_url);
        let sent = self
            .agent
            .post(&url)
            .set("Authorization", &format!("Basic {auth}"))
            .send_form(&[
                ("To", to),
                ("From", &self.from_number),
                ("Url", &twiml),
                ("StatusCallback", &status),
                ("StatusCallbackEvent", "answered completed"),
                ("Timeout", RING_SECONDS),
            ]);
        match sent {
            Ok(res) if res.status() == 201 => {
                let body: serde_json::Value = res.into_json().unwrap_or(serde_json::Value::Null);
                Ok(body["sid"].as_str().unwrap_or_default().to_string())
            }
            Ok(res) => anyhow::bail!("status {}", res.status()),
            Err(ureq::Error::Status(code, _)) => anyhow::bail!("status {code}"),
            Err(ureq::Error::Transport(t)) => anyhow::bail!("transport error: {}", t.kind()),
        }
    }
}

impl Channel for VoiceChannel {
    fn name(&self) -> &'static str {
        "voice"
    }

    fn deliver(&self, user_id: i64, username: &str, msg: &OutboundMessage) -> Result<()> {
        self.ring(user_id, username, msg, true).map(|_| ()).map_err(|e| anyhow::anyhow!("{e}"))
    }
}

fn new_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The call a callback token names, or `None` when the token is unknown or the
/// call is older than `TOKEN_TTL_SECS`.
pub fn call_by_token(conn: &Connection, token: &str, now: jiff::Timestamp) -> Option<VoiceCall> {
    let row: Option<(i64, i64, String, Option<i64>, String, Option<String>, String)> = conn
        .query_row(
            "SELECT c.id, c.user_id, u.username, c.event_id, c.message, c.digit, c.created_at
             FROM voice_calls c JOIN users u ON u.id = c.user_id
             WHERE c.token = ?1",
            [token],
            |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
            },
        )
        .optional()
        .ok()
        .flatten();
    let (id, user_id, username, event_id, message, digit, created_at) = row?;
    let created: jiff::Timestamp = created_at.parse().ok()?;
    if now.as_second() - created.as_second() > TOKEN_TTL_SECS {
        return None;
    }
    Some(VoiceCall { id, user_id, username, event_id, message, digit })
}

pub fn set_call_status(conn: &Connection, id: i64, status: &str) -> Result<()> {
    conn.execute(
        "UPDATE voice_calls SET status = ?1, updated_at = ?2 WHERE id = ?3",
        (status, jiff::Timestamp::now().to_string(), id),
    )?;
    Ok(())
}

pub fn record_digit(conn: &Connection, id: i64, digit: &str) -> Result<()> {
    conn.execute(
        "UPDATE voice_calls SET digit = ?1, updated_at = ?2 WHERE id = ?3",
        (digit, jiff::Timestamp::now().to_string(), id),
    )?;
    Ok(())
}

/// Twilio's `CallStatus` in the row's own vocabulary; `None` for a status the
/// schema has no place for.
pub fn call_status_from(twilio: &str) -> Option<&'static str> {
    Some(match twilio {
        "ringing" | "queued" | "initiated" => "ringing",
        "in-progress" | "answered" => "answered",
        "completed" => "completed",
        "no-answer" => "no_answer",
        "busy" => "busy",
        "failed" | "canceled" => "failed",
        _ => return None,
    })
}

const KEYPAD_PROMPT: &str =
    "Press 1 if it is done, 2 to snooze it for fifteen minutes, 3 to drop it.";
const UNANSWERED: &str = "No answer taken. I will check in again later.";

/// The question a check-in call asks, with the keypad open under it.
pub fn twiml_gather(token: &str, display_name: &str, message: &str) -> String {
    format!(
        "<Response><Gather numDigits=\"1\" timeout=\"8\" action=\"/api/voice/gather/{}\">\
         <Say>Hi {}. This is Note. {} {KEYPAD_PROMPT}</Say></Gather><Say>{UNANSWERED}</Say></Response>",
        xml_escape(token),
        xml_escape(display_name),
        xml_escape(message),
    )
}

/// A call with nothing to decide: one line, then the line hangs up on its own.
pub fn twiml_say(display_name: &str, message: &str) -> String {
    format!(
        "<Response><Say>Hi {}. This is Note. {}</Say></Response>",
        xml_escape(display_name),
        xml_escape(message),
    )
}

pub fn twiml_done() -> String {
    "<Response><Say>Got it.</Say><Hangup/></Response>".to_string()
}

pub fn twiml_giving_up() -> String {
    format!("<Response><Say>{UNANSWERED}</Say><Hangup/></Response>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::{Channel, OutboundMessage, Urgency};
    use std::io::{Read, Write};
    use std::path::Path;
    use std::sync::mpsc::Receiver;

    /// Twilio's documented example: the auth token, URL and parameters of their
    /// own validation walkthrough, and the signature they publish for it.
    const DOC_TOKEN: &str = "12345";
    const DOC_URL: &str = "https://mycompany.com/myapp.php?foo=1&bar=2";
    const DOC_SIGNATURE: &str = "0/KCTR6DLpKmkAf8muzZqo1nDgQ=";

    fn doc_params() -> Vec<(String, String)> {
        [
            ("CallSid", "CA1234567890ABCDE"),
            ("Caller", "+12349013030"),
            ("Digits", "1234"),
            ("From", "+12349013030"),
            ("To", "+18005551212"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    fn settings(base_url: &str, token_file: &Path) -> VoiceSettings {
        VoiceSettings {
            account_sid: "AC0000000000000000000000000000000d".into(),
            auth_token_file: token_file.to_path_buf(),
            from_number: "+15005550006".into(),
            base_url: base_url.into(),
        }
    }

    struct Env {
        _tmp: tempfile::TempDir,
        dir: PathBuf,
        token_file: PathBuf,
        db: Arc<Mutex<Connection>>,
    }

    fn env() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("defaults")).unwrap();
        std::fs::write(
            dir.join("defaults/user.toml"),
            "display_name = \"Aki\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n",
        )
        .unwrap();
        let token_file = dir.join("twilio.token");
        std::fs::write(&token_file, "  tok_secret\n").unwrap();
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO plans (user_id, date, created_at) VALUES (1, '2026-09-17', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time) VALUES (1, 'checkin', '09:00', '09:00')",
            [],
        )
        .unwrap();
        Env { _tmp: tmp, dir, token_file, db: Arc::new(Mutex::new(conn)) }
    }

    fn write_user(dir: &Path, content: &str) {
        let p = dir.join("users/aki/user.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    /// One-shot HTTP server: answers the first request with `status` and `body`
    /// and hands the raw request back, so a placed call's wire shape can be
    /// asserted without a real Twilio.
    fn one_shot(status: &'static str, body: &'static str) -> (String, Receiver<String>) {
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
                    .find_map(|l| {
                        l.strip_prefix("content-length: ").or(l.strip_prefix("Content-Length: "))
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
        });
        (base, rx)
    }

    fn form(raw: &str) -> Vec<(String, String)> {
        let (_, body) = raw.split_once("\r\n\r\n").expect("a request with a body");
        parse_form(body)
    }

    fn field<'a>(params: &'a [(String, String)], name: &str) -> &'a str {
        params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("no {name} in {params:?}"))
    }

    fn msg(event_id: Option<i64>) -> OutboundMessage {
        OutboundMessage {
            title: "Check-in".into(),
            body: "Time for your 09:00 check-in.".into(),
            urgency: Urgency::High,
            event_id,
            conversation_id: None,
        }
    }

    fn channel(env: &Env, base: &str) -> VoiceChannel {
        VoiceChannel::new(
            env.dir.clone(),
            env.db.clone(),
            &settings(base, &env.token_file),
            "https://note.example/",
        )
        .unwrap()
    }

    #[test]
    fn e164_is_the_only_shape_a_number_may_take() {
        assert!(valid_phone("+15005550006"));
        assert!(valid_phone("+819012345678"));
        assert!(!valid_phone("15005550006"));
        assert!(!valid_phone("+0123456789"));
        assert!(!valid_phone("+1234"));
        assert!(!valid_phone("+1234567890123456"));
        assert!(!valid_phone("+1 500 555 0006"));
        assert!(!valid_phone(""));
    }

    #[test]
    fn the_documented_twilio_vector_signs_to_the_documented_value() {
        assert_eq!(sign(DOC_TOKEN, DOC_URL, &doc_params()), DOC_SIGNATURE);
    }

    #[test]
    fn a_tampered_parameter_or_url_changes_the_signature() {
        let mut tampered = doc_params();
        tampered[2].1 = "9999".into();
        assert_ne!(sign(DOC_TOKEN, DOC_URL, &tampered), DOC_SIGNATURE);
        assert_ne!(sign(DOC_TOKEN, "https://evil.example/myapp.php", &doc_params()), DOC_SIGNATURE);
        assert_ne!(sign("54321", DOC_URL, &doc_params()), DOC_SIGNATURE);
        let mut extra = doc_params();
        extra.push(("Extra".into(), "1".into()));
        assert_ne!(sign(DOC_TOKEN, DOC_URL, &extra), DOC_SIGNATURE);
    }

    /// The order parameters arrive in is Twilio's business, not ours.
    #[test]
    fn parameter_order_does_not_change_the_signature() {
        let mut shuffled = doc_params();
        shuffled.reverse();
        assert_eq!(sign(DOC_TOKEN, DOC_URL, &shuffled), DOC_SIGNATURE);
    }

    #[test]
    fn a_form_body_decodes_to_the_parameters_that_were_signed() {
        let params = parse_form("CallStatus=no-answer&Called=%2B12349013030&Msg=a+b&Empty=");
        assert_eq!(field(&params, "CallStatus"), "no-answer");
        assert_eq!(field(&params, "Called"), "+12349013030");
        assert_eq!(field(&params, "Msg"), "a b");
        assert_eq!(field(&params, "Empty"), "");
        assert!(parse_form("").is_empty());
    }

    #[test]
    fn the_signature_is_checked_against_the_public_url_not_the_request_host() {
        let env = env();
        let ch = channel(&env, "http://127.0.0.1:1");
        let params = vec![("Digits".to_string(), "1".to_string())];
        let good = sign("tok_secret", "https://note.example/api/voice/gather/abc", &params);
        assert!(ch.signed("/api/voice/gather/abc", &params, &good));
        assert!(!ch.signed("/api/voice/gather/abc", &params, "nope"));
        assert!(!ch.signed("/api/voice/status/abc", &params, &good));
    }

    #[test]
    fn a_placed_call_carries_the_callbacks_and_stores_the_sid() {
        let env = env();
        let (base, rx) = one_shot("201 Created", r#"{"sid":"CA99"}"#);
        write_user(&env.dir, "phone_number = \"+819012345678\"\n");
        let ch = channel(&env, &base);
        let id = ch.call_now(1, "aki", &msg(Some(1))).unwrap();

        let raw = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(
            raw.starts_with("POST /2010-04-01/Accounts/AC0000000000000000000000000000000d/Calls.json "),
            "request line: {raw}"
        );
        let auth = base64::engine::general_purpose::STANDARD
            .encode("AC0000000000000000000000000000000d:tok_secret");
        assert!(raw.contains(&format!("Authorization: Basic {auth}\r\n")), "{raw}");
        let params = form(&raw);
        assert_eq!(field(&params, "To"), "+819012345678");
        assert_eq!(field(&params, "From"), "+15005550006");
        assert_eq!(field(&params, "StatusCallbackEvent"), "answered completed");
        assert_eq!(field(&params, "Timeout"), RING_SECONDS);
        let token = field(&params, "Url")
            .strip_prefix("https://note.example/api/voice/twiml/")
            .expect("the TwiML url is the public one")
            .to_string();
        assert_eq!(
            field(&params, "StatusCallback"),
            format!("https://note.example/api/voice/status/{token}")
        );

        let conn = crate::db_guard(&env.db);
        let (stored, sid, event, message): (String, Option<String>, Option<i64>, String) = conn
            .query_row(
                "SELECT token, call_sid, event_id, message FROM voice_calls WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(stored, token);
        assert_eq!(sid.as_deref(), Some("CA99"));
        assert_eq!(event, Some(1));
        assert_eq!(message, "Time for your 09:00 check-in.");
        assert!(call_by_token(&conn, &token, jiff::Timestamp::now()).is_some());
    }

    #[test]
    fn a_refused_call_marks_the_row_failed_and_falls_through() {
        let env = env();
        let (base, _rx) = one_shot("400 Bad Request", "{}");
        write_user(&env.dir, "phone_number = \"+819012345678\"\n");
        let ch = channel(&env, &base);
        let err = ch.call_now(1, "aki", &msg(Some(1))).unwrap_err();
        assert!(err.to_string().contains("400"), "unexpected error: {err}");
        let conn = crate::db_guard(&env.db);
        let status: String =
            conn.query_row("SELECT status FROM voice_calls WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "failed");
    }

    #[test]
    fn a_dead_twilio_is_an_error_not_a_panic() {
        let env = env();
        write_user(&env.dir, "phone_number = \"+819012345678\"\n");
        let ch = channel(&env, "http://127.0.0.1:1");
        assert!(ch.call_now(1, "aki", &msg(None)).is_err());
    }

    #[test]
    fn without_a_number_nothing_reaches_the_network() {
        let env = env();
        let ch = channel(&env, "http://127.0.0.1:1");
        assert!(matches!(ch.call_now(1, "aki", &msg(Some(1))), Err(CallRefused::NoPhone)));
        let conn = crate::db_guard(&env.db);
        let n: i64 =
            conn.query_row("SELECT COUNT(*) FROM voice_calls", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn calls_switched_off_refuse_the_ladder_but_not_the_test_call() {
        let env = env();
        let (base, rx) = one_shot("201 Created", r#"{"sid":"CA1"}"#);
        write_user(&env.dir, "phone_number = \"+819012345678\"\ncalls_enabled = false\n");
        let ch = channel(&env, &base);
        let err = ch.deliver(1, "aki", &msg(Some(1))).unwrap_err();
        assert!(err.to_string().contains("calls are off"), "unexpected error: {err}");
        assert!(ch.call_now(1, "aki", &msg(None)).is_ok());
        assert!(rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok());
    }

    /// A real channel re-locks the same mutex, so the dispatcher must have let
    /// go of it before calling `deliver`.
    #[test]
    fn placing_a_call_never_holds_the_db_guard_across_the_request() {
        let env = env();
        let (base, rx) = one_shot("201 Created", r#"{"sid":"CA1"}"#);
        write_user(&env.dir, "phone_number = \"+819012345678\"\n");
        let ch = channel(&env, &base);
        let db = env.db.clone();
        let probe = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                if let Ok(conn) = db.try_lock() {
                    let n: i64 = conn
                        .query_row("SELECT COUNT(*) FROM voice_calls", [], |r| r.get(0))
                        .unwrap();
                    if n == 1 {
                        return true;
                    }
                }
                std::thread::yield_now();
            }
            false
        });
        ch.deliver(1, "aki", &msg(Some(1))).unwrap();
        assert!(probe.join().unwrap(), "the row was never visible while the call was in flight");
        assert!(rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok());
    }

    #[test]
    fn a_token_older_than_its_window_names_nothing() {
        let env = env();
        let conn = crate::db_guard(&env.db);
        let now = jiff::Timestamp::now();
        let old = now - std::time::Duration::from_secs((TOKEN_TTL_SECS + 60) as u64);
        conn.execute(
            "INSERT INTO voice_calls (token, user_id, message, created_at, updated_at)
             VALUES ('stale', 1, 'hi', ?1, ?1)",
            [old.to_string()],
        )
        .unwrap();
        assert!(call_by_token(&conn, "stale", now).is_none());
        assert!(call_by_token(&conn, "never-issued", now).is_none());
    }

    #[test]
    fn startup_refuses_an_unusable_account_number_or_public_url() {
        let env = env();
        let blank = env.dir.join("blank.token");
        std::fs::write(&blank, "\n").unwrap();
        let build = |s: VoiceSettings, public: &str| {
            VoiceChannel::new(env.dir.clone(), env.db.clone(), &s, public)
        };
        assert!(build(settings("https://api.twilio.com", &env.token_file), "https://x").is_ok());
        assert!(build(settings("https://api.twilio.com", &env.token_file), "http://x").is_err());
        assert!(build(settings("https://api.twilio.com", &blank), "https://x").is_err());
        assert!(
            build(settings("https://api.twilio.com", &env.dir.join("missing")), "https://x")
                .is_err()
        );
        let mut s = settings("https://api.twilio.com", &env.token_file);
        s.account_sid = "  ".into();
        assert!(build(s, "https://x").is_err());
        let mut s = settings("https://api.twilio.com", &env.token_file);
        s.from_number = "5005550006".into();
        assert!(build(s, "https://x").is_err());
        let mut s = settings("", &env.token_file);
        s.base_url = String::new();
        assert!(build(s, "https://x").is_err());
    }

    #[test]
    fn spoken_text_is_escaped_and_the_keypad_names_its_own_route() {
        let xml = twiml_gather("tok_1", "Aki & Co", "Ship the <draft> by 5");
        assert!(xml.contains("action=\"/api/voice/gather/tok_1\""), "{xml}");
        assert!(xml.contains("Hi Aki &amp; Co."), "{xml}");
        assert!(xml.contains("Ship the &lt;draft&gt; by 5"), "{xml}");
        assert!(xml.contains("Press 1 if it is done"), "{xml}");
        assert!(xml.ends_with("<Say>No answer taken. I will check in again later.</Say></Response>"));

        let plain = twiml_say("Aki", "Your phone is set up.");
        assert!(!plain.contains("<Gather"), "{plain}");
        assert!(plain.contains("Your phone is set up."), "{plain}");
    }

    #[test]
    fn twilio_call_statuses_land_on_the_rows_vocabulary() {
        for (twilio, ours) in [
            ("ringing", "ringing"),
            ("in-progress", "answered"),
            ("completed", "completed"),
            ("no-answer", "no_answer"),
            ("busy", "busy"),
            ("failed", "failed"),
            ("canceled", "failed"),
        ] {
            assert_eq!(call_status_from(twilio), Some(ours));
        }
        assert_eq!(call_status_from("whatever"), None);
    }
}
