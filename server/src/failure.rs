//! Why a request failed, in terms the user can act on.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    ModelUnavailable,
    AuthInvalid,
    OutOfCredits,
    RateLimited,
    ProviderDown,
    ContextTooLong,
    Refused,
    Internal,
}

impl Reason {
    pub fn code(self) -> &'static str {
        match self {
            Reason::ModelUnavailable => "model_unavailable",
            Reason::AuthInvalid => "auth_invalid",
            Reason::OutOfCredits => "out_of_credits",
            Reason::RateLimited => "rate_limited",
            Reason::ProviderDown => "provider_down",
            Reason::ContextTooLong => "context_too_long",
            Reason::Refused => "refused",
            Reason::Internal => "internal",
        }
    }

    /// Whether the next request fails the same way until someone changes a
    /// setting, a key or a balance.
    pub fn lasts(self) -> bool {
        matches!(
            self,
            Reason::ModelUnavailable | Reason::AuthInvalid | Reason::OutOfCredits | Reason::ContextTooLong
        )
    }

    /// The reason of the first typed failure in `err`'s chain.
    pub fn of(err: &anyhow::Error) -> Reason {
        for cause in err.chain() {
            if let Some(p) = cause.downcast_ref::<crate::providers::ProviderError>() {
                return p.reason;
            }
            if cause.is::<crate::providers::FirstTokenTimeout>() {
                return Reason::ProviderDown;
            }
            if let Some(e) = cause.downcast_ref::<ureq::Error>() {
                return transport(e);
            }
        }
        Reason::Internal
    }
}

/// A provider's refusal by its status and body. A body read off a stream
/// carries no status.
pub fn classify(status: Option<u16>, body: &str) -> Reason {
    let text = body.to_ascii_lowercase();
    let says = |words: &[&str]| words.iter().any(|w| text.contains(w));
    if status == Some(402) || says(&["insufficient_quota", "insufficient credits", "credit balance", "key limit", "spend limit", "billing"]) {
        return Reason::OutOfCredits;
    }
    if status == Some(413) || says(&["context length", "context_length", "maximum context", "context window", "context too long", "prompt is too long", "too many tokens"]) {
        return Reason::ContextTooLong;
    }
    if status == Some(429) || says(&["rate limit", "rate-limit", "too many requests"]) {
        return Reason::RateLimited;
    }
    if says(&["flagged", "moderation", "content_filter", "content policy"]) {
        return Reason::Refused;
    }
    if says(&["more credits", "out of credits", "no credits"]) {
        return Reason::OutOfCredits;
    }
    match status {
        Some(401 | 403) => return Reason::AuthInvalid,
        Some(404) => return Reason::ModelUnavailable,
        Some(408 | 425 | 500..=599) => return Reason::ProviderDown,
        _ => {}
    }
    if says(&["no endpoints found", "model_not_found", "model not found", "not a valid model", "no allowed providers"]) {
        Reason::ModelUnavailable
    } else if says(&["invalid api key", "unauthorized", "authentication", "no auth credentials"]) {
        Reason::AuthInvalid
    } else if says(&["overloaded", "timed out", "timeout", "unavailable", "internal server error"]) {
        Reason::ProviderDown
    } else {
        Reason::Internal
    }
}

/// A request that never got a reply is the provider being unreachable; a
/// request Note built wrong is its own fault.
pub fn transport(err: &ureq::Error) -> Reason {
    match err {
        ureq::Error::BadUri(_) | ureq::Error::RequireHttpsOnly(_) | ureq::Error::Http(_) | ureq::Error::Json(_) => {
            Reason::Internal
        }
        _ => Reason::ProviderDown,
    }
}

/// A failed request: what the user is told, and the raw cause for logs and admins.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub reason: Reason,
    pub detail: String,
}

impl Failure {
    pub fn of(err: &anyhow::Error) -> Self {
        Self { reason: Reason::of(err), detail: format!("{err:#}") }
    }

    pub fn internal(detail: impl Into<String>) -> Self {
        Self { reason: Reason::Internal, detail: detail.into() }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

/// `detail` with URLs, absolute paths and key-shaped words masked, for
/// showing to an admin in the browser.
pub fn redact(detail: &str) -> String {
    let mut out = String::with_capacity(detail.len());
    let mut bearer = false;
    for (i, word) in detail.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let bare = word.trim_matches(|c: char| "\"'`()[]{}<>,;".contains(c));
        let masked = if bearer && !bare.is_empty() {
            Some("<key>")
        } else if bare.contains("://") {
            Some("<url>")
        } else if is_key(bare) {
            Some("<key>")
        } else if bare.starts_with('/') && bare[1..].contains('/') {
            Some("<path>")
        } else {
            None
        };
        bearer = bare.eq_ignore_ascii_case("bearer");
        match masked {
            Some(mask) => out.push_str(&word.replacen(bare, mask, 1)),
            None => out.push_str(word),
        }
    }
    out
}

fn is_key(word: &str) -> bool {
    const PREFIXES: [&str; 5] = ["sk-", "nvapi-", "xai-", "gsk_", "AIza"];
    PREFIXES.iter().any(|p| word.starts_with(p) && word.len() >= p.len() + 12)
}

/// Background work that failed tells its user once per reason in this window.
pub const NOTICE_WINDOW_MINS: i64 = 6 * 60;

/// Tells the user why work they were not watching failed, unless the same
/// reason was told within `NOTICE_WINDOW_MINS`. A fault inside Note is left to
/// the admin's log. The message walks the delivery ladder: the open web app,
/// else a push, with a copy to Matrix.
pub fn tell(state: &crate::AppState, user_id: i64, username: &str, failure: &Failure, now: jiff::Timestamp) {
    if failure.reason == Reason::Internal {
        return;
    }
    let fresh = {
        let conn = state.db();
        crate::log::record_throttled(&conn, Some(user_id), "failure_notice", failure.reason.code(), now, NOTICE_WINDOW_MINS)
            .unwrap_or(false)
    };
    if !fresh {
        return;
    }
    let lang = crate::text::Lang::for_user(&state.config_dir, username);
    let msg = crate::channels::OutboundMessage {
        title: crate::text::failure_title(lang),
        body: crate::text::failure_reason(lang, failure.reason),
        urgency: crate::channels::Urgency::Low,
        checkin: false,
        event_id: None,
        conversation_id: None,
        actions: Vec::new(),
    };
    crate::channels::deliver_via(&state.db, &state.channels, user_id, username, &msg);
}

thread_local! {
    static GATHERED: std::cell::RefCell<Option<Vec<Failure>>> = const { std::cell::RefCell::new(None) };
}

/// Runs `f`, returning with its result every failure `noted` on this thread meanwhile.
pub fn gather<T>(f: impl FnOnce() -> T) -> (T, Vec<Failure>) {
    let outer = GATHERED.with(|g| g.borrow_mut().replace(Vec::new()));
    let out = f();
    let gathered = GATHERED.with(|g| std::mem::replace(&mut *g.borrow_mut(), outer)).unwrap_or_default();
    (out, gathered)
}

/// Keeps `err` for the enclosing `gather`, if any, so a stage that swallows
/// its failure still lets the user hear why.
pub fn noted(err: &anyhow::Error) {
    GATHERED.with(|g| {
        if let Some(list) = g.borrow_mut().as_mut() {
            list.push(Failure::of(err));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_retired_openrouter_model_is_model_unavailable() {
        let body = r#"{"error":{"message":"No endpoints found for stealth/space-bunny-alpha.","code":404}}"#;
        assert_eq!(classify(Some(404), body), Reason::ModelUnavailable);
        assert_eq!(classify(None, body), Reason::ModelUnavailable, "the same body mid-stream");
    }

    #[test]
    fn provider_statuses_map_to_their_reasons() {
        let cases: &[(u16, &str, Reason)] = &[
            (401, r#"{"error":{"message":"No auth credentials found","code":401}}"#, Reason::AuthInvalid),
            (401, r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#, Reason::AuthInvalid),
            (403, r#"{"error":{"message":"Key limit exceeded (total limit). Manage it at the settings page.","code":403}}"#, Reason::OutOfCredits),
            (402, r#"{"error":{"message":"This request requires more credits, or fewer max_tokens.","code":402}}"#, Reason::OutOfCredits),
            (400, r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API."}}"#, Reason::OutOfCredits),
            (429, r#"{"error":{"message":"insufficient_quota: You exceeded your current quota","type":"insufficient_quota"}}"#, Reason::OutOfCredits),
            (429, r#"{"error":{"message":"Rate limit exceeded: limit_rpm/google/gemini","code":429}}"#, Reason::RateLimited),
            (400, r#"{"error":{"message":"This endpoint's maximum context length is 131072 tokens.","code":400}}"#, Reason::ContextTooLong),
            (400, r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 210000 tokens > 200000 maximum"}}"#, Reason::ContextTooLong),
            (403, r#"{"error":{"message":"Your chosen model requires moderation and your input was flagged for \"violence\"","code":403}}"#, Reason::Refused),
            (500, r#"{"error":{"message":"Internal Server Error","code":500}}"#, Reason::ProviderDown),
            (502, r#"{"error":{"message":"Provider returned error","code":502}}"#, Reason::ProviderDown),
            (529, r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#, Reason::ProviderDown),
            (400, r#"{"error":"something odd"}"#, Reason::Internal),
        ];
        for (status, body, want) in cases {
            assert_eq!(classify(Some(*status), body), *want, "{status} {body}");
        }
    }

    #[test]
    fn typed_failures_are_found_anywhere_in_the_chain() {
        let provider = crate::providers::ProviderError::new(Reason::RateLimited, "openai request failed: status 429");
        let err = anyhow::Error::new(provider).context("a talk turn");
        assert_eq!(Reason::of(&err), Reason::RateLimited);
        let timeout = anyhow::Error::new(crate::providers::FirstTokenTimeout(std::time::Duration::from_secs(4)));
        assert_eq!(Reason::of(&timeout), Reason::ProviderDown);
        let transport = anyhow::Error::new(ureq::Error::Timeout(ureq::Timeout::Global)).context("openai stream request failed");
        assert_eq!(Reason::of(&transport), Reason::ProviderDown);
        assert_eq!(Reason::of(&anyhow::anyhow!("database is locked")), Reason::Internal);
    }

    #[test]
    fn redaction_masks_urls_paths_and_keys() {
        let raw = "reading api key file /persist/secrets/openrouter: posting to https://openrouter.ai/api/v1/chat with Bearer abc123 sk-or-v1-0123456789abcdef failed (code 404)";
        assert_eq!(
            redact(raw),
            "reading api key file <path> posting to <url> with Bearer <key> <key> failed (code 404)"
        );
    }

    #[test]
    fn a_background_failure_is_told_once_per_reason_per_window() {
        let conn = crate::db::open_memory().unwrap();
        crate::auth::create_user(&conn, "aki", "pw", false).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let ch = std::sync::Arc::new(crate::channels::mock::MockChannel::new("mock"));
        let state = crate::AppState::new(conn, tmp.path().into(), tmp.path().into()).with_channels(vec![ch.clone()]);
        let gone = Failure { reason: Reason::ModelUnavailable, detail: "status 404".into() };
        let t0: jiff::Timestamp = "2026-10-06T03:00:00Z".parse().unwrap();
        tell(&state, 1, "aki", &gone, t0);
        tell(&state, 1, "aki", &gone, t0 + jiff::Span::new().hours(1));
        tell(&state, 1, "aki", &Failure::internal("database is locked"), t0);
        tell(&state, 1, "aki", &Failure { reason: Reason::RateLimited, detail: "429".into() }, t0);
        tell(&state, 1, "aki", &gone, t0 + jiff::Span::new().hours(7));
        let bodies: Vec<String> = ch.seen().into_iter().map(|(_, m)| m.body).collect();
        let en = |r| crate::text::failure_reason(crate::text::Lang::En, r);
        assert_eq!(bodies, vec![en(Reason::ModelUnavailable), en(Reason::RateLimited), en(Reason::ModelUnavailable)]);
    }

    #[test]
    fn gather_collects_only_what_is_noted_inside_it() {
        noted(&anyhow::anyhow!("outside"));
        let ((), failures) = gather(|| {
            noted(&anyhow::Error::new(crate::providers::ProviderError::new(Reason::ModelUnavailable, "gone")));
        });
        assert_eq!(failures.iter().map(|f| f.reason).collect::<Vec<_>>(), vec![Reason::ModelUnavailable]);
        assert!(gather(|| ()).1.is_empty());
    }
}
