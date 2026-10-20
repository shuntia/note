use crate::cases::{self, Case, Kind};
use crate::checks::{self, Fixture, Verdict};
use crate::client::Client;
use crate::models::Model;
use crate::scratch::{Scratch, Spec};
use crate::score::{self, Score};
use crate::snapshot::{self, Snapshot};
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_secs(3);
const TALK_WAIT: Duration = Duration::from_mins(15);
const TRIGGER_WAIT: Duration = Duration::from_mins(12);
const NIGHTLY_WAIT: Duration = Duration::from_mins(20);
/// The longest one 429 is waited out, and all of a case's waits together.
pub const MAX_WAIT: Duration = Duration::from_mins(5);
pub const MAX_TOTAL_WAIT: Duration = Duration::from_mins(15);

pub struct Env<'a> {
    pub repo: &'a Path,
    pub server_bin: &'a Path,
    pub key_file: &'a Path,
    pub models: &'a [Model],
    pub base_url: &'a str,
    pub keep_dir: &'a Path,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunResult {
    pub case: String,
    pub model: String,
    pub model_id: String,
    pub repeat: u32,
    /// `ok` when the case ran to its end, `error` when the turn failed,
    /// `unavailable` when the model could not be reached in time.
    pub status: String,
    pub success: bool,
    pub failures: Vec<String>,
    pub score: Score,
    pub wall_secs: u64,
    pub waited_secs: u64,
    pub attempts: u32,
    pub error: Option<String>,
    pub fixture: Fixture,
    /// The state the checker judged, so a verdict can be read back later.
    pub snapshot: Option<Snapshot>,
}

enum Drive {
    Done,
    RateLimited(Option<Duration>),
    Failed(String),
}

struct Attempt {
    drive: Drive,
    verdict: Verdict,
    score: Score,
    fixture: Fixture,
    snapshot: Snapshot,
}

/// One case on one model, retried from a fresh scratch server while the
/// provider answers 429, within `MAX_WAIT` per wait and `MAX_TOTAL_WAIT` in all.
/// The flag is set when the provider asked for a wait past `MAX_WAIT`, such as
/// a daily cap, so later cases on the model would only wait again.
pub fn run(env: &Env, case: &Case, model: &Model, repeat: u32) -> (RunResult, bool) {
    let started = Instant::now();
    let mut waited = Duration::ZERO;
    let mut attempts = 0;
    let mut backoff = Duration::from_secs(30);
    let stem = format!("{}-{}-{repeat}", case.name, model.short);
    loop {
        attempts += 1;
        let result = attempt(env, case, model, &format!("{stem}-a{attempts}"));
        let base = |status: &str, a: Option<&Attempt>, error: Option<String>| RunResult {
            case: case.name.clone(),
            model: model.short.clone(),
            model_id: model.id.clone(),
            repeat,
            status: status.to_string(),
            success: status == "ok" && a.is_some_and(|a| a.verdict.pass),
            failures: a.filter(|_| status != "unavailable").map(|a| a.verdict.failures.clone()).unwrap_or_default(),
            score: a.map(|a| a.score.clone()).unwrap_or_default(),
            wall_secs: started.elapsed().as_secs(),
            waited_secs: waited.as_secs(),
            attempts,
            error,
            fixture: a.map(|a| a.fixture.clone()).unwrap_or_default(),
            snapshot: a.map(|a| a.snapshot.clone()),
        };
        match result {
            Err(e) => return (base("error", None, Some(format!("{e:#}"))), false),
            Ok(a) => match &a.drive {
                Drive::Done => return (base("ok", Some(&a), None), false),
                Drive::Failed(why) => return (base("error", Some(&a), Some(why.clone())), false),
                Drive::RateLimited(after) => {
                    let wait = after.unwrap_or(backoff).max(Duration::from_secs(5));
                    if wait > MAX_WAIT || waited + wait > MAX_TOTAL_WAIT {
                        let why = format!("rate limited; the next try would wait {}s", wait.as_secs());
                        return (base("unavailable", Some(&a), Some(why)), wait > MAX_WAIT);
                    }
                    eprintln!("  {stem}: rate limited, waiting {}s", wait.as_secs());
                    std::thread::sleep(wait);
                    waited += wait;
                    backoff = (backoff * 2).min(MAX_WAIT);
                }
            },
        }
    }
}

fn attempt(env: &Env, case: &Case, model: &Model, stem: &str) -> Result<Attempt> {
    let now = jiff::Timestamp::now();
    let zone = cases::zone_for(case.local_hour, now);
    let spec = Spec {
        repo: env.repo,
        server_bin: env.server_bin,
        key_file: env.key_file,
        model,
        models: env.models,
        base_url: env.base_url,
        user_toml: cases::user_toml(case, &zone, now),
    };
    let scratch = Scratch::start(&spec)?;
    let outcome = drive_case(&scratch, case, model, &zone);
    let _ = scratch.keep(env.keep_dir, stem);
    outcome
}

fn drive_case(scratch: &Scratch, case: &Case, model: &Model, zone: &str) -> Result<Attempt> {
    let client = scratch.login()?;
    let fixture = {
        let db = scratch.db()?;
        cases::seed(case, &client, &db, zone)?
    };
    let (trace_mark, event_mark) = {
        let db = scratch.db()?;
        (snapshot::max_id(&db, "agent_traces")?, snapshot::max_id(&db, "events")?)
    };
    let today = fixture.today.context("fixture has no today")?;
    let mut drive = match case.kind {
        Kind::Talk => talk(&client, &cases::render(&case.request, today), &model.id)?,
        Kind::Trigger => wait_for_trace(scratch, trace_mark, "Trigger", TRIGGER_WAIT)?,
        Kind::Nightly => wait_for_nightly(scratch, fixture.dates.get("target").copied().unwrap_or(today))?,
    };
    let snap = read_snapshot(scratch, &client, zone, &fixture, trace_mark, event_mark)?;
    if matches!(drive, Drive::Done) {
        if let Some(s) = snap.sessions.iter().find(|s| s.kind == case.kind.trace_kind() && s.outcome == "error") {
            let text = s.error.clone().unwrap_or_default();
            drive = if rate_limited(&text) {
                Drive::RateLimited(retry_after(&text, jiff::Timestamp::now()))
            } else {
                Drive::Failed(format!("session failed: {}", clip(&text)))
            };
        }
    }
    let verdict = checks::check(&case.name, &snap, &fixture);
    let score = score::score(&snap.sessions, case.kind.trace_kind());
    Ok(Attempt { drive, verdict, score, fixture, snapshot: snap })
}

fn talk(client: &Client, message: &str, model: &str) -> Result<Drive> {
    let body = serde_json::json!({ "message": message, "model": model });
    let started = Instant::now();
    let reply = loop {
        let r = client.send("POST", "/api/talk", &body)?;
        if r.status == 409 && started.elapsed() < Duration::from_secs(120) {
            std::thread::sleep(Duration::from_secs(5));
            continue;
        }
        break r;
    };
    match reply.status {
        200 => Ok(Drive::Done),
        202 => {
            let started = Instant::now();
            while started.elapsed() < TALK_WAIT {
                std::thread::sleep(POLL);
                if client.get("/api/talk/live")?.body.is_null() {
                    return Ok(Drive::Done);
                }
            }
            if let Some(id) = reply.body["conversation_id"].as_i64() {
                let _ = client.send("POST", "/api/talk/stop", &serde_json::json!({ "conversation_id": id }));
            }
            Ok(Drive::Failed(format!("the turn ran past {}s", TALK_WAIT.as_secs())))
        }
        _ if reply.body["reason"] == "rate_limited" => Ok(Drive::RateLimited(None)),
        status => Ok(Drive::Failed(format!("talk answered {status}: {}", clip(&reply.body.to_string())))),
    }
}

fn wait_for_trace(scratch: &Scratch, mark: i64, kind: &str, cap: Duration) -> Result<Drive> {
    let started = Instant::now();
    while started.elapsed() < cap {
        std::thread::sleep(POLL);
        let db = scratch.db()?;
        let n: i64 = db.query_row("SELECT COUNT(*) FROM agent_traces WHERE id > ?1 AND kind = ?2", (mark, kind), |r| r.get(0))?;
        if n > 0 {
            return Ok(Drive::Done);
        }
    }
    Ok(Drive::Failed(format!("no {kind} session within {}s", cap.as_secs())))
}

fn wait_for_nightly(scratch: &Scratch, target: jiff::civil::Date) -> Result<Drive> {
    let started = Instant::now();
    while started.elapsed() < NIGHTLY_WAIT {
        std::thread::sleep(POLL);
        let db = scratch.db()?;
        let n: i64 = db.query_row("SELECT COUNT(*) FROM debriefs WHERE date = ?1", [target.to_string()], |r| r.get(0))?;
        if n > 0 {
            return Ok(Drive::Done);
        }
    }
    Ok(Drive::Failed(format!("no nightly debrief for {target} within {}s", NIGHTLY_WAIT.as_secs())))
}

fn read_snapshot(scratch: &Scratch, c: &Client, zone: &str, fx: &Fixture, trace_mark: i64, event_mark: i64) -> Result<Snapshot> {
    let tz = jiff::tz::TimeZone::get(zone).unwrap_or(jiff::tz::TimeZone::UTC);
    let today = fx.today.context("fixture has no today")?;
    let dates = [today, cases::plus(today, 1)];
    let mut snap = Snapshot {
        today: Some(today),
        nodes: snapshot::nodes_from(&c.must_get("/api/tasks")?, &tz),
        goals: snapshot::goals_from(&c.must_get("/api/goals")?, &tz),
        calendar: snapshot::calendar_from(&c.must_get("/api/calendar")?),
        ..Default::default()
    };
    let db = scratch.db()?;
    snap.days = snapshot::order_from_db(&db, &dates)?;
    for d in dates {
        let day = c.must_get(&format!("/api/day/{d}"))?;
        snap.days.entry(d).or_default().extend(snapshot::day_events_from(&day));
        snap.occurrences.insert(d, snapshot::occurrences_from(&day));
    }
    snap.sessions = snapshot::sessions_from_db(&db, trace_mark)?;
    snap.wakes_laid = snapshot::wakes_laid_after(&db, event_mark)?;
    Ok(snap)
}

pub fn rate_limited(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    ["429", "rate limit", "rate-limit", "rate_limit", "too many requests"].iter().any(|w| t.contains(w))
}

/// The wait a 429 asks for: `OpenRouter`'s `X-RateLimit-Reset` (epoch ms) or a
/// `retry_after`/`Retry-After` in seconds, wherever the error text carries it.
pub fn retry_after(text: &str, now: jiff::Timestamp) -> Option<Duration> {
    let number_after = |key: &str| -> Option<u64> {
        let lower = text.to_ascii_lowercase();
        let at = lower.find(&key.to_ascii_lowercase())? + key.len();
        let digits: String = text[at..].chars().skip_while(|c| !c.is_ascii_digit()).take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    if let Some(reset_ms) = number_after("x-ratelimit-reset") {
        let now_ms = now.as_millisecond().max(0) as u64;
        return Some(Duration::from_millis(reset_ms.saturating_sub(now_ms)));
    }
    number_after("retry-after").or_else(|| number_after("retry_after")).map(Duration::from_secs)
}

fn clip(s: &str) -> String {
    s.chars().take(400).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_limit_is_recognised_in_provider_words() {
        assert!(rate_limited("openai stream request failed: status 429: {\"error\":{\"message\":\"Rate limit exceeded\"}}"));
        assert!(rate_limited("Too Many Requests"));
        assert!(!rate_limited("model not found"));
    }

    #[test]
    fn the_reset_header_or_retry_after_sets_the_wait() {
        let now: jiff::Timestamp = "2026-10-20T10:00:00Z".parse().unwrap();
        let reset = now.as_millisecond() + 42_000;
        let text = format!(r#"status 429: {{"error":{{"metadata":{{"headers":{{"X-RateLimit-Reset":"{reset}"}}}}}}}}"#);
        assert_eq!(retry_after(&text, now), Some(Duration::from_secs(42)));
        assert_eq!(retry_after("429, Retry-After: 17", now), Some(Duration::from_secs(17)));
        assert_eq!(retry_after(r#"{"retry_after": 9}"#, now), Some(Duration::from_secs(9)));
        assert_eq!(retry_after("429", now), None);
    }
}
