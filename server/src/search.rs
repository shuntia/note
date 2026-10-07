use crate::agent::SessionDeps;
use crate::providers::{ChatRequest, Message};
use crate::tools::ToolError;
use crate::model_text as mt;
use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use std::fmt::Write as _;

/// How much of one result's text the model is shown.
pub const MAX_SNIPPET_CHARS: usize = 500;
/// How many hits the model gets raw when the summarizer has nothing to say.
const FALLBACK_HITS: usize = 5;

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

pub trait SearchProvider: Send + Sync {
    fn search(&self, query: &str) -> Result<Vec<SearchHit>>;
}

/// A `SearXNG` instance's JSON API.
pub struct SearxngSearch {
    agent: ureq::Agent,
    url: String,
    max_results: usize,
}

impl SearxngSearch {
    pub fn new(cfg: &crate::config::SearchConfig) -> Self {
        Self {
            agent: crate::providers::http_agent(cfg.timeout_secs),
            url: format!("{}/search", cfg.searxng_url.trim_end_matches('/')),
            max_results: cfg.max_results,
        }
    }
}

impl SearchProvider for SearxngSearch {
    fn search(&self, query: &str) -> Result<Vec<SearchHit>> {
        let call = self.agent.get(&self.url).query("q", query).query("format", "json").call();
        let body: serde_json::Value = match call {
            Ok(mut resp) if resp.status().is_success() => resp
                .body_mut()
                .read_json()
                .map_err(|e| anyhow::anyhow!("searxng: reading the response: {e}"))?,
            Ok(mut resp) => {
                let code = resp.status().as_u16();
                let head: String =
                    resp.body_mut().read_to_string().unwrap_or_default().chars().take(200).collect();
                anyhow::bail!("searxng: status {code}: {head}")
            }
            Err(e) => anyhow::bail!("searxng: {e}"),
        };
        let results = body["results"].as_array().map(Vec::as_slice).unwrap_or_default();
        Ok(results
            .iter()
            .take(self.max_results)
            .map(|r| SearchHit {
                title: text(&r["title"]),
                url: text(&r["url"]),
                snippet: clip(&text(&r["content"]), MAX_SNIPPET_CHARS),
            })
            .collect())
    }
}

fn text(v: &serde_json::Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// At most `chars` characters, cut on a character boundary.
fn clip(s: &str, chars: usize) -> String {
    s.chars().take(chars).collect()
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchArgs {
    pub query: String,
    /// What you want to learn from the results; it steers the summary.
    #[serde(default)]
    pub question: Option<String>,
}

/// Runs the `web_search` tool: the search, then one tool-less provider call
/// that answers `question` from the hits alone. Network work, so the session
/// runs it outside the DB lock; the lock is taken for the log row only.
pub fn run_tool(
    deps: &SessionDeps,
    user_id: i64,
    username: &str,
    background: bool,
    raw_args: &str,
) -> Result<serde_json::Value, ToolError> {
    let Some(provider) = deps.search else {
        return Err(ToolError::rejected("web search is not configured on this server"));
    };
    let args: SearchArgs =
        serde_json::from_str(raw_args).map_err(|e| ToolError::invalid_args(e.to_string()))?;
    let query = args.query.trim();
    if query.is_empty() {
        return Err(ToolError::invalid_args("query must not be empty"));
    }
    crate::tools::check_text("query", query)?;
    crate::tools::check_text("question", args.question.as_deref().unwrap_or_default())?;
    let hits = provider.search(query).map_err(|e| ToolError::internal(e.to_string()))?;
    if hits.is_empty() {
        log(deps, user_id, 0, "none");
        let l = crate::text::Lang::for_user(deps.config_dir, username);
        return Ok(serde_json::json!({ "summary": mt::no_results(l), "sources": [] }));
    }
    if let Some(summary) = summarize(deps, username, background, &args, &hits) {
        log(deps, user_id, hits.len(), "ok");
        Ok(serde_json::json!({ "summary": summary, "sources": sources(&hits) }))
    } else {
        log(deps, user_id, hits.len(), "fallback");
        Ok(serde_json::json!({ "summary": null, "hits": raw_hits(&hits) }))
    }
}

/// `None` where the summarizer cannot be reached or answers blank, which the
/// caller falls back from rather than failing the tool call.
fn summarize(
    deps: &SessionDeps,
    username: &str,
    background: bool,
    args: &SearchArgs,
    hits: &[SearchHit],
) -> Option<String> {
    let l = crate::text::Lang::for_user(deps.config_dir, username);
    let system = crate::prompts::load_in(deps.config_dir, username, "search", l).ok()?;
    let mut prompt = format!("{}: {}\n", mt::search_query(l), args.query.trim());
    if let Some(q) = args.question.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let _ = writeln!(prompt, "{}: {q}", mt::search_question(l));
    }
    let _ = write!(prompt, "\n{}:\n", mt::search_results(l));
    for (i, h) in hits.iter().enumerate() {
        let _ = write!(prompt, "{}. {}\n{}\n{}\n\n", i + 1, h.title, h.url, h.snippet);
    }
    let messages = [Message::User(prompt)];
    let req = ChatRequest { system: &system, messages: &messages, tools: &[], background };
    let text = deps.llm.chat(&req).ok()?.text;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn sources(hits: &[SearchHit]) -> Vec<serde_json::Value> {
    hits.iter()
        .enumerate()
        .map(|(i, h)| serde_json::json!({ "n": i + 1, "title": h.title, "url": h.url }))
        .collect()
}

fn raw_hits(hits: &[SearchHit]) -> Vec<serde_json::Value> {
    hits.iter()
        .take(FALLBACK_HITS)
        .enumerate()
        .map(|(i, h)| {
            serde_json::json!({ "n": i + 1, "title": h.title, "url": h.url, "snippet": h.snippet })
        })
        .collect()
}

/// One row per search. The query is the user's own words, so it never reaches
/// the admin log.
fn log(deps: &SessionDeps, user_id: i64, hits: usize, summary: &str) {
    let conn = crate::db_guard(deps.db);
    let _ = crate::log::record(
        &conn,
        Some(user_id),
        "web_search",
        &format!("hits={hits} summary={summary}"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(url: &str, max_results: usize) -> crate::config::SearchConfig {
        crate::config::SearchConfig { searxng_url: url.into(), max_results, timeout_secs: 5 }
    }

    #[test]
    fn searxng_results_are_parsed_capped_and_clipped() {
        let body = Box::leak(
            serde_json::json!({"results": [
                {"title": "one", "url": "http://a", "content": "x".repeat(MAX_SNIPPET_CHARS + 40)},
                {"title": "two", "url": "http://b", "content": "short"},
                {"title": "three", "url": "http://c"},
            ]})
            .to_string()
            .into_boxed_str(),
        );
        let (base, rx) = crate::testhttp::serve(vec![("200 OK", body)]);
        let hits = SearxngSearch::new(&config(&base, 2)).search("tokyo rain").unwrap();
        assert_eq!(hits.len(), 2, "max_results caps the hits");
        assert_eq!(hits[0].title, "one");
        assert_eq!(hits[0].snippet.chars().count(), MAX_SNIPPET_CHARS);
        assert_eq!(hits[1].snippet, "short");
        let raw = rx.recv().unwrap();
        let line = raw.lines().next().unwrap();
        assert!(line.contains("q=tokyo%20rain") || line.contains("q=tokyo+rain"), "{line}");
        assert!(line.contains("format=json"), "{line}");
    }

    #[test]
    fn a_multibyte_snippet_is_cut_on_a_character_boundary() {
        let body = Box::leak(
            serde_json::json!({"results": [
                {"title": "t", "url": "http://a", "content": "。".repeat(MAX_SNIPPET_CHARS + 10)},
            ]})
            .to_string()
            .into_boxed_str(),
        );
        let (base, _rx) = crate::testhttp::serve(vec![("200 OK", body)]);
        let hits = SearxngSearch::new(&config(&base, 8)).search("q").unwrap();
        assert_eq!(hits[0].snippet.chars().count(), MAX_SNIPPET_CHARS);
    }

    #[test]
    fn a_failing_instance_says_what_went_wrong() {
        let (base, _rx) = crate::testhttp::serve(vec![("503 Unavailable", r#"{"error":"engines"}"#)]);
        let err = SearxngSearch::new(&config(&base, 8)).search("q").unwrap_err().to_string();
        assert!(err.contains("503") && err.contains("engines"), "{err}");

        let err = SearxngSearch::new(&config("http://127.0.0.1:1", 8))
            .search("q")
            .unwrap_err()
            .to_string();
        assert!(err.contains("searxng"), "{err}");
    }

    #[test]
    fn a_reply_without_results_is_no_hits_rather_than_an_error() {
        let (base, _rx) = crate::testhttp::serve(vec![("200 OK", r#"{"query":"q"}"#)]);
        assert!(SearxngSearch::new(&config(&base, 8)).search("q").unwrap().is_empty());
    }
}
