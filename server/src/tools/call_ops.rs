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
    /// Why, in one line; the call is told it.
    #[serde(default)]
    pub reason: Option<String>,
    /// The user asked to be called, which lets it through while they seem busy.
    #[serde(default)]
    pub asked: bool,
    /// Their own words asking.
    #[serde(default)]
    pub quote: Option<String>,
    /// What the first call returned.
    #[serde(default)]
    pub token: Option<String>,
    /// Your first words when they pick up.
    #[serde(default)]
    pub opening: Option<String>,
    /// What the call needs to know in short plain lines: facts, ids, times, what to settle.
    #[serde(default)]
    pub brief: Option<String>,
    /// The tasks it pushes on; default the wake-up's own.
    #[serde(default)]
    #[schemars(with = "Vec<i64>", extend("items" = { "type": ["integer", "string"] }))]
    pub task_ids: Vec<serde_json::Value>,
}

pub const MAX_BRIEF_BYTES: usize = 1536;

struct Pending {
    token: String,
    reason: String,
    asked: Option<String>,
    issued: Instant,
}

/// The tokens one session's checks issued; they die with the session.
#[derive(Default)]
pub struct Calls(Vec<Pending>);

impl Calls {
    /// Whether a token is still good for the confirming call.
    pub fn held(&self, now: Instant) -> bool {
        self.0.iter().any(|p| now.duration_since(p.issued) < TOKEN_TTL)
    }
}

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
    let cfg = crate::config::UserConfig::load(config_dir, username).ok();
    let (start, until) = cfg.as_ref().map_or_else(
        || (crate::config::DEFAULT_DAY_START.to_string(), crate::config::DEFAULT_CALLS_UNTIL.to_string()),
        crate::wake::call_hours,
    );
    let busy = crate::wake::seems_busy(conn, user_id, &tz, (&start, &until), now).map_err(|e| internal(&e))?;
    let ring_for = cfg.as_ref().map_or(crate::config::RING_FOR_ALLOWED, |c| c.ring_for()).to_string();
    let live_call = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM voice_calls WHERE user_id = ?1 AND state != 'ended')",
            [user_id],
            |r| r.get(0),
        )
        .map_err(|e| internal(&e))?;
    Ok(Situation { busy, reach, ring_for, live_call })
}

/// What stands between this moment and a call: `cannot` is what nothing
/// overrides, `unwelcome` what the user's own asking for the call does.
pub struct Gate {
    pub cannot: Vec<String>,
    pub unwelcome: Vec<String>,
}

impl Gate {
    pub fn open(&self, asked: bool) -> bool {
        self.cannot.is_empty() && (asked || self.unwelcome.is_empty())
    }
}

/// The one gate every call passes, `call_user` and a trigger laid to ring alike.
pub fn gate(s: &Situation) -> Gate {
    let mut cannot = Vec::new();
    if !s.reach.web && !s.reach.phone {
        cannot.push("nothing can ring: the web app is not open and no phone is linked".to_string());
    }
    if s.live_call {
        cannot.push("a call is already live".to_string());
    }
    if s.ring_for == crate::config::RING_FOR_NEVER {
        cannot.push("the user's settings say never to ring".to_string());
    }
    Gate { cannot, unwelcome: s.busy.iter().map(crate::wake::Busy::reason).collect() }
}

const PREPARE: &str = "Prepare the call before it rings: anticipate what it will cover — the reason, \
    the likely follow-ups, the tasks, events, goals and people involved — and fetch that now, in one \
    round (memory_find, task_find, calendar_find, day_find). Judge \
    from what comes back whether you could hold the conversation without looking anything up; if \
    not, gather more, or write to the user instead.";

fn trimmed(field: Option<&String>) -> &str {
    field.map_or("", |s| s.trim())
}

fn normalised(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_alphanumeric() { c.to_lowercase().next().unwrap_or(c) } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

const MIN_QUOTE_CHARS: usize = 8;
const MIN_QUOTE_WORDS: usize = 3;

/// Whether `quote` is words the user really wrote: part of one of `heard`, the
/// messages this session holds from the user. Spaced text must match whole
/// words, at least `MIN_QUOTE_WORDS` of them; unspaced text (Japanese) is a
/// plain substring. Either way it holds at least `MIN_QUOTE_CHARS` characters.
fn heard_from_user(quote: &str, heard: &[String]) -> bool {
    let quote = normalised(quote);
    if quote.chars().count() < MIN_QUOTE_CHARS {
        return false;
    }
    if quote.contains(' ') && quote.chars().any(|c| c.is_ascii_alphanumeric()) {
        if quote.split(' ').count() < MIN_QUOTE_WORDS {
            return false;
        }
        let quote = format!(" {quote} ");
        return heard.iter().any(|h| format!(" {} ", normalised(h)).contains(&quote));
    }
    heard.iter().any(|h| normalised(h).contains(&quote))
}

/// The first `call_user` call: never rings. Refuses as busy when no call may
/// ring now, else issues this session a token for the second call. `heard`
/// holds what the user wrote that this session has in front of it; an `asked`
/// whose quote is not among it is refused.
pub fn check(
    calls: &mut Calls,
    args: &CallArgs,
    s: &Situation,
    heard: &[String],
    lang: Lang,
    now: Instant,
) -> Result<serde_json::Value, ToolError> {
    if args.opening.is_some() || args.brief.is_some() {
        return Err(ToolError::rejected("opening and brief go on the second call, with the token"));
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
    if asked.is_some() && !heard_from_user(quote, heard) {
        return Err(ToolError::rejected(
            "quote must be the user's own words from a message in front of you; without them the call is not one they asked for",
        ));
    }

    let gate = gate(s);
    if !gate.open(asked.is_some()) {
        let bypass = gate.cannot.is_empty();
        let mut why = gate.cannot;
        why.extend(gate.unwelcome.iter().cloned());
        let mut message = format!("not a moment to call: {}; write to the user instead", why.join("; "));
        if bypass {
            message.push_str(", unless they asked to be called: then check again with asked and their words in quote");
        }
        return Err(ToolError::busy(message));
    }
    let unwelcome = gate.unwelcome;

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
        "prepare": PREPARE,
        "opening_style": crate::model_text::call_opening_style(lang),
        "next": format!(
            "Then call call_user again with this token, your opening line, and brief: what the call \
             needs to know from what you gathered, in short plain lines (at most {MAX_BRIEF_BYTES} bytes)."
        ),
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
    /// What the session gathered for the call to talk from.
    pub brief: String,
    /// The user's words asking for the call, when the call overrode their seeming busy.
    pub asked: Option<String>,
    pub task_ids: Vec<i64>,
}

impl Confirmed {
    /// The result a trigger session ends on; its firing path rings.
    pub fn decided(&self) -> serde_json::Value {
        serde_json::json!({
            "said": self.opening, "ring": true, "reason": self.reason, "brief": self.brief, "asked": self.asked,
            "task_ids": self.task_ids,
        })
    }

    pub fn from_decided(v: &serde_json::Value) -> Option<Self> {
        Some(Self {
            opening: v["said"].as_str()?.to_string(),
            reason: v["reason"].as_str()?.to_string(),
            brief: v["brief"].as_str().unwrap_or_default().to_string(),
            asked: v["asked"].as_str().map(str::to_string),
            task_ids: serde_json::from_value(v["task_ids"].clone()).unwrap_or_default(),
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

/// The tasks a confirming call names, each title read as the one open task
/// carrying it.
pub fn resolve_task_ids(conn: &Connection, user_id: i64, given: &[serde_json::Value]) -> Result<Vec<serde_json::Value>, ToolError> {
    let mut out = Vec::new();
    for v in given {
        match super::loose::target(v) {
            Some(super::loose::Target::Id(id)) => out.push(id.into()),
            Some(super::loose::Target::Title(title)) => {
                let mut stmt = conn
                    .prepare("SELECT id, title FROM tasks WHERE user_id = ?1 AND state IN ('open','in_progress') ORDER BY id")
                    .map_err(|e| ToolError::internal(e.to_string()))?;
                let open: Vec<(i64, String)> = stmt
                    .query_map([user_id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .and_then(|rows| rows.collect::<rusqlite::Result<_>>())
                    .map_err(|e| ToolError::internal(e.to_string()))?;
                let id = super::loose::by_title(&title, &open, "task").map_err(|why| ToolError::rejected(format!("task_ids: {why}")))?;
                out.push(id.into());
            }
            None => return Err(ToolError::rejected(format!("task_ids holds {v}, which is neither an id nor a title"))),
        }
    }
    Ok(out)
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
    let brief = trimmed(args.brief.as_ref());
    if brief.is_empty() || brief.len() > MAX_BRIEF_BYTES {
        return Err(ToolError::rejected(format!(
            "brief must be 1 to {MAX_BRIEF_BYTES} bytes: what you gathered, in short plain lines"
        )));
    }
    let pending = calls.0.remove(at);
    Ok(Confirmed {
        opening: opening.to_string(),
        reason: pending.reason,
        brief: brief.to_string(),
        asked: pending.asked,
        task_ids: args.task_ids.iter().filter_map(super::loose::id).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wake::Busy;

    #[test]
    fn a_confirming_call_names_its_tasks_by_id_or_title() {
        let conn = crate::db::open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", []).unwrap();
        for title in ["Lab report", "Essay", "essay"] {
            crate::tasks::create(&conn, 1, crate::tasks::NewTask { title: title.into(), ..Default::default() }, "manual", crate::tasks::Actor::User).unwrap();
        }
        let ids = resolve_task_ids(&conn, 1, &[serde_json::json!("lab report"), serde_json::json!("#2")]).unwrap();
        assert_eq!(ids, [serde_json::json!(1), serde_json::json!(2)]);
        let e = resolve_task_ids(&conn, 1, &[serde_json::json!("Essay")]).unwrap_err();
        assert!(e.message.contains("2, 3"), "{}", e.message);
        let confirmed = args(r#"{"task_ids":[3,"x"]}"#).task_ids.iter().filter_map(super::super::loose::id).collect::<Vec<_>>();
        assert_eq!(confirmed, [3]);
    }

    fn free() -> Situation {
        Situation { busy: None, reach: Reach { web: true, phone: false }, ring_for: "allowed".into(), live_call: false }
    }

    fn args(json: &str) -> CallArgs {
        serde_json::from_str(json).unwrap()
    }

    fn heard() -> Vec<String> {
        vec!["Can you CALL me at 11, please?".into(), "ring me".into()]
    }

    fn checked(calls: &mut Calls, s: &Situation, json: &str) -> Result<serde_json::Value, ToolError> {
        check(calls, &args(json), s, &heard(), Lang::En, Instant::now())
    }

    fn token(out: &serde_json::Value) -> String {
        out["token"].as_str().unwrap().to_string()
    }

    fn confirmed(calls: &mut Calls, token: &str, at: Instant) -> Result<Confirmed, ToolError> {
        let json = serde_json::json!({
            "token": token, "opening": "It's eleven — time to head out.", "brief": "bus 11:20 from Elm St", "task_ids": [3],
        });
        confirm(calls, &args(&json.to_string()), at)
    }

    #[test]
    fn the_check_never_rings_and_asks_for_preparation_before_it_hands_a_token() {
        let mut calls = Calls::default();
        let out = checked(&mut calls, &free(), r#"{"reason":"the 11:00 they asked to be called for"}"#).unwrap();
        assert_eq!(out["allowed"], true);
        assert_eq!(out["would_ring"], serde_json::json!(["the web app"]));
        let prepare = out["prepare"].as_str().unwrap();
        assert!(prepare.contains("anticipate") && prepare.contains("task_find"), "{prepare}");
        assert!(out["next"].as_str().unwrap().contains("brief"), "{out}");
        assert_eq!(out["opening_style"], crate::model_text::call_opening_style(Lang::En));
        assert!(out.get("said").is_none() && !is_confirmed(&out.to_string()));
    }

    #[test]
    fn the_second_call_carries_a_capped_brief() {
        let mut calls = Calls::default();
        let t = token(&checked(&mut calls, &free(), r#"{"reason":"x"}"#).unwrap());
        for brief in [serde_json::json!(null), serde_json::json!("  "), serde_json::json!("x".repeat(MAX_BRIEF_BYTES + 1))] {
            let json = serde_json::json!({ "token": t, "opening": "hi", "brief": brief }).to_string();
            assert_eq!(confirm(&mut calls, &args(&json), Instant::now()).unwrap_err().kind, "rejected", "{json}");
        }
        let c = confirmed(&mut calls, &t, Instant::now()).unwrap();
        assert_eq!(c.brief, "bus 11:20 from Elm St");
        assert_eq!(Confirmed::from_decided(&c.decided()).unwrap().brief, c.brief);
        let early = r#"{"reason":"x","brief":"b"}"#;
        assert_eq!(checked(&mut calls, &free(), early).unwrap_err().kind, "rejected");
    }

    #[test]
    fn asked_holds_only_with_words_the_user_really_wrote() {
        let busy = Situation { busy: Some(Busy::Working { title: "essay".into() }), ..free() };
        let mut calls = Calls::default();
        let invented = r#"{"reason":"x","asked":true,"quote":"please call me now"}"#;
        assert_eq!(checked(&mut calls, &busy, invented).unwrap_err().kind, "rejected");
        let theirs = r#"{"reason":"x","asked":true,"quote":"call me at 11"}"#;
        checked(&mut calls, &busy, theirs).unwrap();
        let none = check(&mut calls, &args(theirs), &busy, &[], Lang::En, Instant::now()).unwrap_err();
        assert_eq!(none.kind, "rejected", "nothing the user wrote is in front of the session");
    }

    #[test]
    fn a_confirmed_call_carries_the_checked_reason_and_spends_its_token() {
        let mut calls = Calls::default();
        let t = token(&checked(&mut calls, &free(), r#"{"reason":"the 11:00 call"}"#).unwrap());
        let c = confirmed(&mut calls, &t, Instant::now()).unwrap();
        assert_eq!(c, Confirmed {
            opening: "It's eleven — time to head out.".into(),
            reason: "the 11:00 call".into(),
            brief: "bus 11:20 from Elm St".into(),
            asked: None,
            task_ids: vec![3],
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
        let blank = serde_json::json!({ "token": t, "opening": "  ", "brief": "b" }).to_string();
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
    fn a_quote_must_be_whole_words_and_long_enough() {
        let heard = vec!["Can you CALL me at 11, please?".to_string(), "あとで電話してください。".to_string()];
        for short in ["a", "the", "call me", "all me at 1", "can you"] {
            assert!(!heard_from_user(short, &heard), "{short}");
        }
        assert!(heard_from_user("call me at 11", &heard));
        assert!(heard_from_user("you call me", &heard));
        assert!(heard_from_user("電話してください", &heard));
        assert!(!heard_from_user("電話して", &heard), "too short");
        assert!(!heard_from_user("今すぐ電話してください", &heard));
    }

    #[test]
    fn ring_for_never_holds_even_an_asked_call() {
        let never = Situation { ring_for: "never".into(), ..free() };
        let mut calls = Calls::default();
        assert_eq!(checked(&mut calls, &never, r#"{"reason":"x"}"#).unwrap_err().kind, "busy");
        let e = checked(&mut calls, &never, r#"{"reason":"x","asked":true,"quote":"call me at 11"}"#).unwrap_err();
        assert_eq!(e.kind, "busy");
        assert!(!e.message.contains("unless"), "{}", e.message);
    }

    #[test]
    fn nothing_to_ring_or_a_live_call_refuses_even_an_asked_call() {
        let asked = r#"{"reason":"x","asked":true,"quote":"call me at 11"}"#;
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
        let now: jiff::Timestamp = "2026-10-07T12:00:00Z".parse().unwrap();
        let s = situation(&conn, tmp.path(), "aki", 1, Reach::default(), now).unwrap();
        assert_eq!((s.busy, s.ring_for.as_str(), s.live_call), (None, "never", false));

        conn.execute(
            "INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at)
             VALUES ('c1', 1, 'outbound', 'answered', 'x', 'x')",
            [],
        )
        .unwrap();
        let evening: jiff::Timestamp = "2026-10-07T22:00:00Z".parse().unwrap();
        let s = situation(&conn, tmp.path(), "aki", 1, Reach::default(), evening).unwrap();
        assert_eq!((s.busy, s.live_call), (None, true), "past the close of the day, before calls_until");
        let late: jiff::Timestamp = "2026-10-07T23:30:00Z".parse().unwrap();
        let s = situation(&conn, tmp.path(), "aki", 1, Reach::default(), late).unwrap();
        let night = Busy::Night { start: "08:00".into(), end: "23:00".into() };
        assert_eq!((s.busy, s.live_call), (Some(night), true));
    }
}
