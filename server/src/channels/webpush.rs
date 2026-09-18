use super::{Channel, OutboundMessage};
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

/// What the service worker decrypts: the notification's text, plus the app
/// route to open when it is clicked.
pub fn payload_json(msg: &OutboundMessage) -> String {
    let mut payload = serde_json::json!({ "title": msg.title, "body": msg.body });
    if let Some(id) = msg.conversation_id {
        payload["conversation_id"] = serde_json::json!(id);
        payload["url"] = serde_json::json!(super::conversation_path(id));
    }
    payload.to_string()
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

    let payload = payload_json(msg);
    let mut b = WebPushMessageBuilder::new(&info);
    b.set_payload(web_push::ContentEncoding::Aes128Gcm, payload.as_bytes());
    b.set_vapid_signature(signature);
    b.set_ttl(3600);
    let m = b.build().map_err(|e| anyhow::anyhow!("build push: {e}"))?;

    let p = m.payload.context("payload always set")?;
    let mut headers: Vec<(String, String)> = p
        .crypto_headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    if !headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-encoding"))
    {
        headers.push(("Content-Encoding".into(), "aes128gcm".into()));
    }
    headers.push(("TTL".into(), m.ttl.to_string()));
    headers.push(("Urgency".into(), msg.urgency.as_str().into()));
    Ok(BuiltPush {
        endpoint: m.endpoint.to_string(),
        headers,
        body: p.content,
    })
}

pub fn public_key_b64(vapid_pem: &[u8]) -> Result<String> {
    let partial = web_push::VapidSignatureBuilder::from_pem_no_sub(vapid_pem)
        .map_err(|e| anyhow::anyhow!("vapid key: {e}"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(partial.get_public_key()))
}

/// VAPID requires a contact the push service can reach if a key misbehaves.
fn valid_subject(subject: &str) -> bool {
    subject.starts_with("mailto:") || subject.starts_with("https://")
}

/// ureq's `Display` embeds the full endpoint URL, which is a per-device bearer
/// capability, and delivery errors are written to the event log.
fn describe(err: &ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, _) => format!("status {code}"),
        ureq::Error::Transport(t) => format!("transport error: {}", t.kind()),
    }
}

pub struct WebPushChannel {
    db: Arc<Mutex<Connection>>,
    vapid_pem: Vec<u8>,
    subject: String,
    agent: ureq::Agent,
}

impl WebPushChannel {
    pub fn new(db: Arc<Mutex<Connection>>, vapid_pem: Vec<u8>, subject: String) -> Result<Self> {
        // reject an unusable key or contact at startup, not at first delivery
        public_key_b64(&vapid_pem)?;
        anyhow::ensure!(
            valid_subject(&subject),
            "vapid subject must be a mailto: or https:// URL, got {subject:?}"
        );
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(15))
            .build();
        Ok(Self {
            db,
            vapid_pem,
            subject,
            agent,
        })
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
            let conn = crate::db_guard(&self.db);
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
                    Err(e) => last_err = describe(&e),
                },
            }
        }
        if !gone.is_empty() {
            let conn = crate::db_guard(&self.db);
            for endpoint in &gone {
                if let Err(e) = push_subs::remove_endpoint(&conn, endpoint) {
                    last_err = format!("pruning a gone subscription failed: {e}");
                }
            }
        }
        if delivered > 0 {
            return Ok(());
        }
        if last_err.is_empty() {
            anyhow::bail!("all push subscriptions gone (pruned {})", gone.len());
        }
        anyhow::bail!("no push delivery succeeded: {last_err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::{OutboundMessage, Urgency};
    use crate::push_subs::Subscription;

    // Throwaway P-256 key generated for these tests only — never a deployment key.
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
            conversation_id: None,
            actions: Vec::new(),
        }
    }

    #[test]
    fn the_payload_carries_the_thread_route_only_when_there_is_one() {
        let plain: serde_json::Value = serde_json::from_str(&payload_json(&msg())).unwrap();
        assert_eq!(plain, serde_json::json!({ "title": "Nudge", "body": "stretch break" }));

        let mut checkin = msg();
        checkin.conversation_id = Some(42);
        let v: serde_json::Value = serde_json::from_str(&payload_json(&checkin)).unwrap();
        assert_eq!(v["conversation_id"], 42);
        assert_eq!(v["url"], "/#/chat/42");
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
        assert_eq!(get("urgency").as_deref(), Some("normal"));
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
    fn new_rejects_a_subject_that_is_not_a_contact_url() {
        let db = Arc::new(Mutex::new(crate::db::open_memory().unwrap()));
        let bad = WebPushChannel::new(db.clone(), TEST_PEM.to_vec(), "admin@example.com".into());
        assert!(bad.is_err());
        assert!(
            WebPushChannel::new(db.clone(), TEST_PEM.to_vec(), "mailto:admin@example.com".into())
                .is_ok()
        );
        assert!(
            WebPushChannel::new(db, TEST_PEM.to_vec(), "https://example.com/contact".into()).is_ok()
        );
    }

    #[test]
    fn bad_pem_is_an_error() {
        assert!(public_key_b64(b"not a pem").is_err());
        assert!(build_push(&sub(), b"not a pem", "mailto:x@y", &msg()).is_err());
    }
}
