use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::cell::Cell;

/// What provider calls spent. `cost` is in USD as `OpenRouter` bills it, 0
/// where the provider reports none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub calls: u64,
    pub prompt: u64,
    pub cached: u64,
    pub completion: u64,
    pub reasoning: u64,
    pub cost: f64,
}

impl Usage {
    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }

    pub fn add(&mut self, other: Self) {
        self.calls += other.calls;
        self.prompt += other.prompt;
        self.cached += other.cached;
        self.completion += other.completion;
        self.reasoning += other.reasoning;
        self.cost += other.cost;
    }

    /// One call, read from an OpenAI-shaped `usage` object; zero tokens where it is absent.
    pub fn openai_call(usage: &serde_json::Value) -> Self {
        let n = |v: &serde_json::Value| v.as_u64().unwrap_or(0);
        Self {
            calls: 1,
            prompt: n(&usage["prompt_tokens"]),
            cached: n(&usage["prompt_tokens_details"]["cached_tokens"]),
            completion: n(&usage["completion_tokens"]),
            reasoning: n(&usage["completion_tokens_details"]["reasoning_tokens"]),
            cost: usage["cost"].as_f64().unwrap_or(0.0),
        }
    }

    /// One call, read from an Anthropic `usage` object; `prompt` counts cache reads and writes too.
    pub fn anthropic_call(usage: &serde_json::Value) -> Self {
        let n = |key: &str| usage[key].as_u64().unwrap_or(0);
        let cached = n("cache_read_input_tokens");
        Self {
            calls: 1,
            prompt: n("input_tokens") + cached + n("cache_creation_input_tokens"),
            cached,
            completion: n("output_tokens"),
            ..Self::default()
        }
    }
}

thread_local! {
    static OPEN: Cell<Option<Usage>> = const { Cell::new(None) };
}

/// Adds a provider call to the meter open on this thread, if any.
pub fn record(usage: Usage) {
    OPEN.with(|open| {
        if let Some(mut total) = open.get() {
            total.add(usage);
            open.set(Some(total));
        }
    });
}

/// What this thread's open meter counted since it opened or was last taken from.
pub fn take() -> Usage {
    OPEN.with(|open| {
        let total = open.get();
        if total.is_some() {
            open.set(Some(Usage::default()));
        }
        total.unwrap_or_default()
    })
}

/// Counts every provider call made on this thread while it lives. A meter
/// opened inside another hides its calls from the outer one, which gets its
/// own count back once the inner meter drops.
pub struct Meter {
    outer: Option<Usage>,
}

impl Meter {
    #[must_use]
    pub fn open() -> Self {
        Self { outer: OPEN.with(|open| open.replace(Some(Usage::default()))) }
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        OPEN.with(|open| open.set(self.outer));
    }
}

/// Runs `f` under a meter of its own and logs what it spent under `kind`.
pub fn metered<T>(conn: &std::sync::Mutex<Connection>, user_id: i64, kind: &str, f: impl FnOnce() -> T) -> T {
    let meter = Meter::open();
    let out = f();
    let spent = take();
    drop(meter);
    let _ = log(&crate::db_guard(conn), user_id, kind, &spent);
    out
}

const LOG_KIND: &str = "llm_usage";

/// One `llm_usage` row in the event log, which is never pruned; nothing when nothing was spent.
pub fn log(conn: &Connection, user_id: i64, kind: &str, usage: &Usage) -> anyhow::Result<()> {
    if usage.calls == 0 {
        return Ok(());
    }
    let mut detail = serde_json::to_value(usage)?;
    detail["kind"] = kind.into();
    crate::log::record(conn, Some(user_id), LOG_KIND, &detail.to_string())
}

/// Spend per UTC day and session kind over the last `days` days, newest day
/// first, with a total per kind for the whole window.
pub fn summary(conn: &Connection, user_id: Option<i64>, days: u32, now: jiff::Timestamp) -> anyhow::Result<serde_json::Value> {
    let since = now - jiff::SignedDuration::from_hours(24 * i64::from(days));
    let mut stmt = conn.prepare(
        "SELECT substr(ts, 1, 10) AS day, json_extract(detail, '$.kind') AS k,
                SUM(json_extract(detail, '$.calls')), SUM(json_extract(detail, '$.prompt')),
                SUM(json_extract(detail, '$.cached')), SUM(json_extract(detail, '$.completion')),
                SUM(json_extract(detail, '$.reasoning')), SUM(json_extract(detail, '$.cost')), COUNT(*)
         FROM event_log
         WHERE kind = ?1 AND ts > ?2 AND (?3 IS NULL OR user_id = ?3)
         GROUP BY day, k ORDER BY day DESC, k",
    )?;
    let rows = stmt.query_map((LOG_KIND, since.to_string(), user_id), |r| {
        let usage = Usage {
            calls: r.get::<_, i64>(2)?.unsigned_abs(),
            prompt: r.get::<_, i64>(3)?.unsigned_abs(),
            cached: r.get::<_, i64>(4)?.unsigned_abs(),
            completion: r.get::<_, i64>(5)?.unsigned_abs(),
            reasoning: r.get::<_, i64>(6)?.unsigned_abs(),
            cost: r.get(7)?,
        };
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(8)?, usage))
    })?;
    let mut by_day = Vec::new();
    let mut by_kind = std::collections::BTreeMap::<String, (i64, Usage)>::new();
    let mut total = Usage::default();
    for row in rows {
        let (day, kind, sessions, usage) = row?;
        let entry = by_kind.entry(kind.clone()).or_default();
        entry.0 += sessions;
        entry.1.add(usage);
        total.add(usage);
        by_day.push(serde_json::json!({ "day": day, "kind": kind, "sessions": sessions, "usage": usage }));
    }
    let by_kind: Vec<_> = by_kind
        .into_iter()
        .map(|(kind, (sessions, usage))| serde_json::json!({ "kind": kind, "sessions": sessions, "usage": usage }))
        .collect();
    Ok(serde_json::json!({ "days": days, "since": since.to_string(), "total": total, "by_kind": by_kind, "by_day": by_day }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_openrouter_usage_with_cache_reasoning_and_cost() {
        let u = Usage::openai_call(&serde_json::json!({
            "prompt_tokens": 9000, "completion_tokens": 120, "cost": 0.0021,
            "prompt_tokens_details": {"cached_tokens": 8000},
            "completion_tokens_details": {"reasoning_tokens": 40}
        }));
        assert_eq!(u, Usage { calls: 1, prompt: 9000, cached: 8000, completion: 120, reasoning: 40, cost: 0.0021 });
        assert_eq!(Usage::openai_call(&serde_json::Value::Null), Usage { calls: 1, ..Usage::default() });
    }

    #[test]
    fn a_meter_counts_only_its_own_thread_and_an_inner_meter_hides_its_calls() {
        record(Usage { calls: 1, ..Usage::default() });
        assert!(take().is_zero(), "no meter is open");
        let _outer = Meter::open();
        record(Usage { calls: 1, prompt: 10, ..Usage::default() });
        std::thread::spawn(|| record(Usage { calls: 1, ..Usage::default() })).join().unwrap();
        {
            let _inner = Meter::open();
            record(Usage { calls: 1, prompt: 99, ..Usage::default() });
            assert_eq!(take().prompt, 99);
        }
        record(Usage { calls: 1, prompt: 5, ..Usage::default() });
        assert_eq!(take(), Usage { calls: 2, prompt: 15, ..Usage::default() });
        assert!(take().is_zero());
    }

    #[test]
    fn summary_sums_per_day_and_kind() {
        let conn = crate::db::open_memory().unwrap();
        let call = Usage { calls: 2, prompt: 1000, cached: 800, completion: 50, reasoning: 0, cost: 0.001 };
        log(&conn, 1, "Talk", &call).unwrap();
        log(&conn, 1, "Talk", &call).unwrap();
        log(&conn, 1, "Trigger", &call).unwrap();
        log(&conn, 2, "Talk", &call).unwrap();
        log(&conn, 1, "Talk", &Usage::default()).unwrap();
        let s = summary(&conn, Some(1), 7, jiff::Timestamp::now()).unwrap();
        assert_eq!(s["total"]["calls"], 6);
        assert_eq!(s["by_kind"][0]["kind"], "Talk");
        assert_eq!(s["by_kind"][0]["sessions"], 2);
        assert_eq!(s["by_kind"][0]["usage"]["prompt"], 2000);
        assert!((s["by_kind"][0]["usage"]["cost"].as_f64().unwrap() - 0.002).abs() < 1e-9);
        assert_eq!(s["by_day"].as_array().unwrap().len(), 2);
        assert_eq!(summary(&conn, None, 7, jiff::Timestamp::now()).unwrap()["total"]["calls"], 8);
    }
}
