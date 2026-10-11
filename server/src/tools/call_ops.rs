use super::{check_text, ToolError};
use crate::channels::calling::Reach;
use crate::text::Lang;
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;
use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

pub const TOKEN_TTL: Duration = Duration::from_mins(5);

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CallArgs {
    /// First call: why you want to call, in one line. The call is told it.
    #[serde(default)]
    pub reason: Option<String>,
    /// First call: the user asked to be called. Only then does a call go
    /// through while they seem busy.
    #[serde(default)]
    pub asked: bool,
    /// First call, with asked: the user's own words asking to be called.
    #[serde(default)]
    pub quote: Option<String>,
    /// Second call: the token the first call returned.
    #[serde(default)]
    pub token: Option<String>,
    /// Second call: the first thing you say when they pick up.
    #[serde(default)]
    pub opening: Option<String>,
}

struct Pending {
    token: String,
    reason: String,
    asked: Option<String>,
    issued: Instant,
}

/// The tokens one session's checks issued; they die with the session.
#[derive(Default)]
pub struct Calls(Vec<Pending>);

pub struct Situation {
    pub busy: Option<crate::wake::Busy>,
    pub reach: Reach,
    pub ring_for: String,
    pub live_call: bool,
}

pub fn situation(
    conn: &Connection,
    config_dir: &Path,
    username: &str,
    user_id: i64,
    reach: Reach,
    now: jiff::Timestamp,
) -> Result<Situation, ToolError> {
    let internal = |e: &dyn std::fmt::Display| ToolError::internal(e.to_string());
    let tz = crate::triggers::timezone(config_dir, username);
    let busy = crate::wake::seems_busy(conn, user_id, &tz, now).map_err(|e| internal(&e))?;
    let ring_for = crate::config::UserConfig::load(config_dir, username)
        .map_or_else(|_| crate::config::RING_FOR_URGENT.to_string(), |c| c.ring_for().to_string());
    let live_call = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM voice_calls WHERE user_id = ?1 AND state != 'ended')",
            [user_id],
            |r| r.get(0),
        )
        .map_err(|e| internal(&e))?;
    Ok(Situation { busy, reach, ring_for, live_call })
}

const BEFORE_YOU_RING: [&str; 3] = [
    "Do you really want to interrupt the user with a call, rather than a message they read when they can?",
    "Is it allowed: did the user ask for this call, or is a short conversation right now clearly worth more than a message?",
    "Do you have enough information to inform the user — why you are calling and what you will say first?",
];

fn trimmed(field: Option<&String>) -> &str {
    field.map_or("", |s| s.trim())
}

/// The first `call_user` call: never rings. Refuses as busy when no call may
/// ring now, else issues this session a token for the second call.
pub fn check(
    calls: &mut Calls,
    args: &CallArgs,
    s: &Situation,
    lang: Lang,
    now: Instant,
) -> Result<serde_json::Value, ToolError> {
    if args.opening.is_some() {
        return Err(ToolError::rejected("opening goes on the second call, with the token"));
    }
    let reason = trimmed(args.reason.as_ref());
    if reason.is_empty() {
        return Err(ToolError::rejected("reason must say why you want to call"));
    }
    check_text("reason", reason)?;
    let quote = trimmed(args.quote.as_ref());
    let asked = match (args.asked, quote.is_empty()) {
        (true, true) => return Err(ToolError::rejected("asked needs quote: the user's own words asking to be called")),
        (true, false) => Some(quote.to_string()),
        (false, true) => None,
        (false, false) => return Err(ToolError::rejected("quote goes with asked")),
    };
    check_text("quote", quote)?;

    let mut cannot = Vec::new();
    if !s.reach.web && !s.reach.phone {
        cannot.push("nothing can ring: the web app is not open and no phone is linked".to_string());
    }
    if s.live_call {
        cannot.push("a call is already live".to_string());
    }
    let mut unwelcome = Vec::new();
    if let Some(busy) = &s.busy {
        unwelcome.push(busy.reason());
    }
    if s.ring_for == crate::config::RING_FOR_NEVER {
        unwelcome.push("the user's settings say never to ring".to_string());
    }
    if !cannot.is_empty() || (asked.is_none() && !unwelcome.is_empty()) {
        let bypass = cannot.is_empty();
        let mut why = cannot;
        why.extend(unwelcome);
        let mut message = format!("not a moment to call: {}; write to the user instead", why.join("; "));
        if bypass {
            message.push_str(", unless they asked to be called: then check again with asked and their words in quote");
        }
        return Err(ToolError::busy(message));
    }

    let token = uuid::Uuid::new_v4().simple().to_string();
    calls.0.retain(|p| now.duration_since(p.issued) < TOKEN_TTL);
    calls.0.push(Pending { token: token.clone(), reason: reason.to_string(), asked, issued: now });
    let rings: Vec<&str> =
        [(s.reach.web, "the web app"), (s.reach.phone, "the phone")].into_iter().filter(|r| r.0).map(|r| r.1).collect();
    let mut out = serde_json::json!({
        "allowed": true,
        "token": token,
        "expires_in_min": TOKEN_TTL.as_secs() / 60,
        "would_ring": rings,
        "before_you_ring": BEFORE_YOU_RING,
        "opening_style": crate::model_text::call_opening_style(lang),
        "next": "Answer each question to yourself. Only if every answer is yes, call call_user again \
                 with this token and your opening line; otherwise write to the user instead.",
    });
    if !unwelcome.is_empty() {
        out["overriding"] = serde_json::json!(unwelcome);
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Confirmed {
    pub opening: String,
    pub reason: String,
    /// The user's words asking for the call, when the call overrode their seeming busy.
    pub asked: Option<String>,
}

impl Confirmed {
    /// The result a trigger session ends on; its firing path rings.
    pub fn decided(&self) -> serde_json::Value {
        serde_json::json!({ "said": self.opening, "ring": true, "reason": self.reason, "asked": self.asked })
    }

    pub fn from_decided(v: &serde_json::Value) -> Option<Self> {
        Some(Self {
            opening: v["said"].as_str()?.to_string(),
            reason: v["reason"].as_str()?.to_string(),
            asked: v["asked"].as_str().map(str::to_string),
        })
    }

    pub fn log_line(&self, subject: &str, outcome: &str) -> String {
        let mut line = format!("{subject}: {outcome}: {}", self.reason);
        if let Some(quote) = &self.asked {
            let _ = write!(line, "; asked: \"{quote}\"");
        }
        line
    }
}

/// Whether a successful `call_user` result is the confirmed second call.
pub fn is_confirmed(result: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(result).is_ok_and(|v| v.get("said").is_some())
}

/// The second `call_user` call: spends a token this session's check issued.
pub fn confirm(calls: &mut Calls, args: &CallArgs, now: Instant) -> Result<Confirmed, ToolError> {
    if args.reason.is_some() || args.asked || args.quote.is_some() {
        return Err(ToolError::rejected(
            "the second call takes only token and opening; the reason is the one you checked",
        ));
    }
    let token = trimmed(args.token.as_ref());
    let Some(at) = calls.0.iter().position(|p| p.token == token) else {
        return Err(ToolError::rejected(
            "that token was not issued in this session, or was already used; check again first",
        ));
    };
    if now.duration_since(calls.0[at].issued) >= TOKEN_TTL {
        calls.0.remove(at);
        return Err(ToolError::rejected("that token has expired; check again first"));
    }
    let opening = trimmed(args.opening.as_ref());
    if opening.is_empty() || opening.len() > crate::triggers::MAX_SAY_BYTES {
        return Err(ToolError::rejected(format!(
            "opening must be 1 to {} bytes",
            crate::triggers::MAX_SAY_BYTES
        )));
    }
    let pending = calls.0.remove(at);
    Ok(Confirmed { opening: opening.to_string(), reason: pending.reason, asked: pending.asked })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wake::Busy;

    fn free() -> Situation {
        Situation { busy: None, reach: Reach { web: true, phone: false }, ring_for: "urgent".into(), live_call: false }
    }

    fn args(json: &str) -> CallArgs {
        serde_json::from_str(json).unwrap()
    }

    fn checked(calls: &mut Calls, s: &Situation, json: &str) -> Result<serde_json::Value, ToolError> {
        check(calls, &args(json), s, Lang::En, Instant::now())
    }

    fn token(out: &serde_json::Value) -> String {
        out["token"].as_str().unwrap().to_string()
    }

    fn confirmed(calls: &mut Calls, token: &str, at: Instant) -> Result<Confirmed, ToolError> {
        let json = serde_json::json!({ "token": token, "opening": "It's eleven — time to head out." });
        confirm(calls, &args(&json.to_string()), at)
    }

    #[test]
    fn the_check_never_rings_and_asks_before_it_hands_a_token() {
        let mut calls = Calls::default();
        let out = checked(&mut calls, &free(), r#"{"reason":"the 11:00 they asked to be called for"}"#).unwrap();
        assert_eq!(out["allowed"], true);
        assert_eq!(out["would_ring"], serde_json::json!(["the web app"]));
        let questions = out["before_you_ring"].to_string();
        assert!(questions.contains("rather than a message"), "{questions}");
        assert!(questions.contains("allowed"), "{questions}");
        assert!(questions.contains("enough information to inform the user"), "{questions}");
        assert_eq!(out["opening_style"], crate::model_text::call_opening_style(Lang::En));
        assert!(out.get("said").is_none() && !is_confirmed(&out.to_string()));
    }

    #[test]
    fn a_confirmed_call_carries_the_checked_reason_and_spends_its_token() {
        let mut calls = Calls::default();
        let t = token(&checked(&mut calls, &free(), r#"{"reason":"the 11:00 call"}"#).unwrap());
        let c = confirmed(&mut calls, &t, Instant::now()).unwrap();
        assert_eq!(c, Confirmed {
            opening: "It's eleven — time to head out.".into(),
            reason: "the 11:00 call".into(),
            asked: None,
        });
        assert!(is_confirmed(&c.decided().to_string()));
        assert_eq!(Confirmed::from_decided(&c.decided()).as_ref(), Some(&c));
        assert_eq!(confirmed(&mut calls, &t, Instant::now()).unwrap_err().kind, "rejected", "a token rings once");
    }

    #[test]
    fn a_token_from_another_session_or_past_its_time_is_refused() {
        let (mut mine, mut theirs) = (Calls::default(), Calls::default());
        let t = token(&checked(&mut theirs, &free(), r#"{"reason":"x"}"#).unwrap());
        assert_eq!(confirmed(&mut mine, &t, Instant::now()).unwrap_err().kind, "rejected");
        let later = Instant::now() + TOKEN_TTL;
        let e = confirmed(&mut theirs, &t, later).unwrap_err();
        assert!(e.message.contains("expired"), "{}", e.message);
    }

    #[test]
    fn a_bad_opening_keeps_the_token_for_another_try() {
        let mut calls = Calls::default();
        let t = token(&checked(&mut calls, &free(), r#"{"reason":"x"}"#).unwrap());
        let blank = serde_json::json!({ "token": t, "opening": "  " }).to_string();
        assert_eq!(confirm(&mut calls, &args(&blank), Instant::now()).unwrap_err().kind, "rejected");
        confirmed(&mut calls, &t, Instant::now()).unwrap();
    }

    #[test]
    fn the_second_call_takes_no_new_reason() {
        let mut calls = Calls::default();
        let t = token(&checked(&mut calls, &free(), r#"{"reason":"x"}"#).unwrap());
        let swapped = serde_json::json!({ "token": t, "opening": "hi", "reason": "something else" }).to_string();
        assert_eq!(confirm(&mut calls, &args(&swapped), Instant::now()).unwrap_err().kind, "rejected");
        let early = r#"{"reason":"x","opening":"hi"}"#;
        assert_eq!(checked(&mut calls, &free(), early).unwrap_err().kind, "rejected");
    }

    #[test]
    fn a_busy_user_is_written_to_unless_they_asked_for_the_call() {
        let busy = Situation { busy: Some(Busy::Working { title: "essay".into() }), ..free() };
        let mut calls = Calls::default();
        let e = checked(&mut calls, &busy, r#"{"reason":"x"}"#).unwrap_err();
        assert_eq!(e.kind, "busy");
        assert!(e.message.contains("essay") && e.message.contains("asked"), "{}", e.message);
        assert!(calls.0.is_empty());

        let e = checked(&mut calls, &busy, r#"{"reason":"x","asked":true}"#).unwrap_err();
        assert_eq!(e.kind, "rejected", "an override names the user's words");
        let out = checked(&mut calls, &busy, r#"{"reason":"x","asked":true,"quote":"call me at 11"}"#).unwrap();
        assert!(out["overriding"][0].as_str().unwrap().contains("essay"));
        let c = confirmed(&mut calls, &token(&out), Instant::now()).unwrap();
        assert_eq!(c.asked.as_deref(), Some("call me at 11"));
        assert_eq!(c.log_line("talk", "web"), "talk: web: x; asked: \"call me at 11\"");
    }

    #[test]
    fn ring_for_never_is_an_override_too() {
        let never = Situation { ring_for: "never".into(), ..free() };
        let mut calls = Calls::default();
        assert_eq!(checked(&mut calls, &never, r#"{"reason":"x"}"#).unwrap_err().kind, "busy");
        checked(&mut calls, &never, r#"{"reason":"x","asked":true,"quote":"ring me"}"#).unwrap();
    }

    #[test]
    fn nothing_to_ring_or_a_live_call_refuses_even_an_asked_call() {
        let asked = r#"{"reason":"x","asked":true,"quote":"call me"}"#;
        for s in [
            Situation { reach: Reach::default(), ..free() },
            Situation { live_call: true, ..free() },
        ] {
            let e = checked(&mut Calls::default(), &s, asked).unwrap_err();
            assert_eq!(e.kind, "busy");
            assert!(!e.message.contains("unless"), "{}", e.message);
        }
    }

    #[test]
    fn the_situation_reads_busy_ring_for_and_live_calls() {
        let conn = crate::db::open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", []).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("defaults/user.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\nring_for = \"never\"\n")
            .unwrap();
        let now = jiff::Timestamp::now();
        crate::presence::touch(&conn, 1, now).unwrap();
        let s = situation(&conn, tmp.path(), "aki", 1, Reach::default(), now).unwrap();
        assert_eq!((s.busy, s.ring_for.as_str(), s.live_call), (None, "never", false));

        conn.execute(
            "INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at)
             VALUES ('c1', 1, 'outbound', 'answered', 'x', 'x')",
            [],
        )
        .unwrap();
        conn.execute("UPDATE users SET last_active_at = '2000-01-01T00:00:00Z'", []).unwrap();
        let s = situation(&conn, tmp.path(), "aki", 1, Reach::default(), now).unwrap();
        assert_eq!((s.busy, s.live_call), (Some(Busy::Away), true));
    }
}
