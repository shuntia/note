use super::{ToolCtx, ToolError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

const MAX_REASON_CHARS: usize = 200;
const MAX_FACT_SUMMARY_CHARS: usize = 120;
const MAX_FACT_BODY: usize = 2 * 1024;
const MAX_FACTS: usize = 10;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    /// One short line naming what the fact is.
    pub summary: String,
    /// The durable statement itself, naming the course and citing the item.
    pub body: String,
    /// The date after which the fact stops mattering, as YYYY-MM-DD. Omit it
    /// for standing rules and resources.
    #[serde(default)]
    pub until: Option<String>,
}

#[derive(Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Remembered,
    Nothing,
    Task,
}

impl Outcome {
    fn as_str(&self) -> &'static str {
        match self {
            Outcome::Remembered => "remembered",
            Outcome::Nothing => "nothing",
            Outcome::Task => "task",
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecideArgs {
    /// The source id of the item this session was opened for.
    pub source_id: String,
    pub outcome: Outcome,
    /// Why this outcome, in one line.
    pub reason: String,
    /// The facts to remember; only with outcome "remembered", 1 to 10 of them.
    #[serde(default)]
    pub facts: Option<Vec<Fact>>,
}

/// The whole inbox session in one call: what the source held before is
/// archived, and — for "remembered" — its facts are written afresh, so a
/// re-sent item never leaves a stale copy behind.
pub fn decide(
    conn: &Connection,
    ctx: &ToolCtx,
    args: DecideArgs,
) -> Result<serde_json::Value, ToolError> {
    let scope = ctx
        .inbox_source
        .as_deref()
        .ok_or_else(|| ToolError::rejected("this session has no inbox source"))?;
    if args.source_id != scope {
        return Err(ToolError::rejected(format!(
            "this session is scoped to source {scope}, not {}",
            args.source_id
        )));
    }
    let reason = args.reason.trim();
    if reason.is_empty() || reason.chars().count() > MAX_REASON_CHARS {
        return Err(ToolError::rejected(format!(
            "reason must be 1 to {MAX_REASON_CHARS} characters"
        )));
    }
    let facts = args.facts.unwrap_or_default();
    if args.outcome == Outcome::Remembered {
        if !(1..=MAX_FACTS).contains(&facts.len()) {
            return Err(ToolError::rejected(format!(
                "outcome remembered needs 1 to {MAX_FACTS} facts"
            )));
        }
    } else if !facts.is_empty() {
        return Err(ToolError::rejected(format!(
            "outcome {} writes no facts",
            args.outcome.as_str()
        )));
    }
    for f in &facts {
        let summary = f.summary.trim();
        if summary.is_empty() || summary.chars().count() > MAX_FACT_SUMMARY_CHARS {
            return Err(ToolError::rejected(format!(
                "each fact summary must be 1 to {MAX_FACT_SUMMARY_CHARS} characters"
            )));
        }
        if f.body.trim().is_empty() || f.body.len() > MAX_FACT_BODY {
            return Err(ToolError::rejected(format!(
                "each fact body must be 1 to {MAX_FACT_BODY} bytes"
            )));
        }
        if let Some(u) = &f.until {
            if u.parse::<jiff::civil::Date>().is_err() {
                return Err(ToolError::rejected(format!("until {u:?} is not a YYYY-MM-DD date")));
            }
        }
    }
    if let Some(err) = &ctx.vectors.error {
        let _ = crate::log::record(conn, None, "memory_embed_error", &format!("inbox: {err}"));
    }

    let superseded = supersede_source(conn, ctx, scope)?;
    let mut memory_ids = Vec::with_capacity(facts.len());
    for (i, f) in facts.iter().enumerate() {
        let id = crate::memory::add_until(conn, ctx.data_dir, ctx.username, &crate::memory::Fact { category: "semantic", summary: &f.summary, body: &f.body, until: f.until.as_deref() }, ctx.vectors.facts.get(i).and_then(|v| v.as_deref()))
        .map_err(|e| ToolError::internal(e.to_string()))?;
        conn.execute(
            "INSERT INTO memory_sources (user_id, source_id, memory_id) VALUES (?1, ?2, ?3)",
            (ctx.user_id, scope, &id),
        )
        .map_err(|e| ToolError::internal(e.to_string()))?;
        memory_ids.push(id);
    }
    crate::inbox::record_decision(
        conn,
        ctx.user_id,
        scope,
        args.outcome.as_str(),
        reason,
        jiff::Timestamp::now(),
    )
    .map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(serde_json::json!({
        "outcome": args.outcome.as_str(),
        "reason": reason,
        "memory_ids": memory_ids,
        "superseded": superseded,
    }))
}

fn supersede_source(conn: &Connection, ctx: &ToolCtx, source: &str) -> Result<usize, ToolError> {
    let internal = |e: rusqlite::Error| ToolError::internal(e.to_string());
    let mut stmt = conn
        .prepare("SELECT memory_id FROM memory_sources WHERE user_id = ?1 AND source_id = ?2")
        .map_err(internal)?;
    let ids: Vec<String> = stmt
        .query_map((ctx.user_id, source), |r| r.get(0))
        .map_err(internal)?
        .collect::<rusqlite::Result<_>>()
        .map_err(internal)?;
    drop(stmt);
    let mut n = 0;
    for id in &ids {
        if crate::memory::archive(conn, ctx.data_dir, ctx.username, id)
            .map_err(|e| ToolError::internal(e.to_string()))?
        {
            n += 1;
        }
    }
    conn.execute(
        "DELETE FROM memory_sources WHERE user_id = ?1 AND source_id = ?2",
        (ctx.user_id, source),
    )
    .map_err(internal)?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, PreparedVectors, SessionKind, ToolCtx};

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir, source: &str) -> ToolCtx<'a> {
        ToolCtx {
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            user_id: 1,
            username: "aki",
            vectors: PreparedVectors::default(),
            task_scope: None,
            inbox_source: Some(source.to_string()),
            memory_source: None,
            share: None,
            share_thread: None,
        }
    }

    fn decide(
        conn: &rusqlite::Connection,
        tmp: &tempfile::TempDir,
        source: &str,
        args: &str,
    ) -> Result<serde_json::Value, crate::tools::ToolError> {
        dispatch(conn, &ctx(tmp, source), SessionKind::Inbox, "inbox_decide", args)
    }

    const TWO_FACTS: &str = r#"{"source_id":"s1","outcome":"remembered","reason":"quiz and rule",
        "facts":[{"summary":"quiz","body":"Biology: quiz on 2026-09-25.","until":"2026-09-25"},
                 {"summary":"rule","body":"Biology: late work loses 10% a day."}]}"#;

    #[test]
    fn remembered_writes_facts_and_records_them_under_the_source() {
        let (conn, tmp) = env();
        let out = decide(&conn, &tmp, "s1", TWO_FACTS).unwrap();
        assert_eq!(out["outcome"], "remembered");
        assert_eq!(out["superseded"], 0);
        let ids: Vec<String> = out["memory_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i.as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids.len(), 2);
        let f = crate::memory::read(tmp.path(), "aki", &ids[0]).unwrap().unwrap();
        assert_eq!(f.category, "semantic");
        assert_eq!(f.until.as_deref(), Some("2026-09-25"));
        assert!(crate::memory::read(tmp.path(), "aki", &ids[1]).unwrap().unwrap().until.is_none());
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_sources WHERE user_id = 1 AND source_id = 's1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
        // the facts are searchable at once
        assert_eq!(crate::memory::query(&conn, "aki", "Biology", 10, None).unwrap().len(), 2);
    }

    #[test]
    fn a_re_send_archives_what_the_source_held_before() {
        let (conn, tmp) = env();
        let first = decide(&conn, &tmp, "s1", TWO_FACTS).unwrap();
        let old: Vec<String> = first["memory_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i.as_str().unwrap().to_string())
            .collect();

        let out = decide(
            &conn,
            &tmp,
            "s1",
            r#"{"source_id":"s1","outcome":"remembered","reason":"it moved",
                "facts":[{"summary":"quiz","body":"Biology: quiz moved to 2026-09-28."}]}"#,
        )
        .unwrap();
        assert_eq!(out["superseded"], 2);
        for id in &old {
            assert!(crate::memory::read(tmp.path(), "aki", id).unwrap().unwrap().archived);
        }
        let live: Vec<String> =
            crate::memory::list(&conn, "aki", None, 50).unwrap().into_iter().map(|h| h.id).collect();
        assert_eq!(live, vec![out["memory_ids"][0].as_str().unwrap().to_string()]);

        // an outcome that writes nothing still clears the source
        let out = decide(
            &conn,
            &tmp,
            "s1",
            r#"{"source_id":"s1","outcome":"nothing","reason":"nothing left"}"#,
        )
        .unwrap();
        assert_eq!(out["superseded"], 1);
        assert!(crate::memory::list(&conn, "aki", None, 50).unwrap().is_empty());
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn a_source_id_outside_the_session_scope_is_rejected() {
        let (conn, tmp) = env();
        let e = decide(
            &conn,
            &tmp,
            "s1",
            r#"{"source_id":"s2","outcome":"nothing","reason":"not mine"}"#,
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");
        assert!(e.message.contains("s1"), "{}", e.message);

        // and a session with no scope at all cannot decide
        let mut c = ctx(&tmp, "s1");
        c.inbox_source = None;
        let e = dispatch(
            &conn,
            &c,
            SessionKind::Inbox,
            "inbox_decide",
            r#"{"source_id":"s1","outcome":"nothing","reason":"x"}"#,
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");
    }

    #[test]
    fn every_argument_rule_is_a_typed_rejection_that_writes_nothing() {
        let (conn, tmp) = env();
        let long_body = "b".repeat(2 * 1024 + 1);
        let bad = [
            // remembered with no facts
            r#"{"source_id":"s1","outcome":"remembered","reason":"r"}"#.to_string(),
            r#"{"source_id":"s1","outcome":"remembered","reason":"r","facts":[]}"#.to_string(),
            // eleven facts
            format!(
                r#"{{"source_id":"s1","outcome":"remembered","reason":"r","facts":[{}]}}"#,
                [r#"{"summary":"s","body":"b"}"#; 11].join(",")
            ),
            // nothing / task with facts
            r#"{"source_id":"s1","outcome":"nothing","reason":"r","facts":[{"summary":"s","body":"b"}]}"#.to_string(),
            r#"{"source_id":"s1","outcome":"task","reason":"r","facts":[{"summary":"s","body":"b"}]}"#.to_string(),
            // blank and over-long reason
            r#"{"source_id":"s1","outcome":"nothing","reason":"  "}"#.to_string(),
            format!(
                r#"{{"source_id":"s1","outcome":"nothing","reason":"{}"}}"#,
                "r".repeat(201)
            ),
            // fact field limits
            format!(
                r#"{{"source_id":"s1","outcome":"remembered","reason":"r","facts":[{{"summary":"{}","body":"b"}}]}}"#,
                "s".repeat(121)
            ),
            r#"{"source_id":"s1","outcome":"remembered","reason":"r","facts":[{"summary":" ","body":"b"}]}"#.to_string(),
            format!(
                r#"{{"source_id":"s1","outcome":"remembered","reason":"r","facts":[{{"summary":"s","body":"{long_body}"}}]}}"#
            ),
            r#"{"source_id":"s1","outcome":"remembered","reason":"r","facts":[{"summary":"s","body":" "}]}"#.to_string(),
            // an until that is not a civil date
            r#"{"source_id":"s1","outcome":"remembered","reason":"r","facts":[{"summary":"s","body":"b","until":"friday"}]}"#.to_string(),
            r#"{"source_id":"s1","outcome":"remembered","reason":"r","facts":[{"summary":"s","body":"b","until":"2026-13-01"}]}"#.to_string(),
        ];
        for args in &bad {
            let e = decide(&conn, &tmp, "s1", args).unwrap_err();
            assert_eq!(e.kind, "rejected", "{args} gave {e:?}");
        }
        // unknown fields and unknown outcomes never reach the handler
        for args in [
            r#"{"source_id":"s1","outcome":"nothing","reason":"r","extra":1}"#,
            r#"{"source_id":"s1","outcome":"maybe","reason":"r"}"#,
            r#"{"source_id":"s1","outcome":"remembered","reason":"r","facts":[{"summary":"s","body":"b","note":"x"}]}"#,
        ] {
            let e = decide(&conn, &tmp, "s1", args).unwrap_err();
            assert_eq!(e.kind, "invalid_args", "{args}");
        }
        assert!(crate::memory::list(&conn, "aki", None, 50).unwrap().is_empty());
        let n: i64 =
            conn.query_row("SELECT COUNT(*) FROM memory_sources", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn the_inbox_surface_is_three_tools_and_the_decision_ends_the_session() {
        assert_eq!(
            crate::tools::registry(SessionKind::Inbox),
            ["memory_query", "memory_read", "inbox_decide"]
        );
        assert!(crate::tools::is_terminal(SessionKind::Inbox, "inbox_decide"));
        assert!(!crate::tools::is_terminal(SessionKind::Inbox, "memory_query"));
        assert!(!crate::tools::is_terminal(SessionKind::Talk, "inbox_decide"));

        let (conn, tmp) = env();
        for name in ["memory_write", "task_create", "context_edit"] {
            let e = dispatch(&conn, &ctx(&tmp, "s1"), SessionKind::Inbox, name, "{}").unwrap_err();
            assert!(matches!(e.kind, "forbidden" | "unknown_tool"), "{name} gave {e:?}");
        }
        let e = dispatch(
            &conn,
            &ctx(&tmp, "s1"),
            SessionKind::Talk,
            "inbox_decide",
            r#"{"source_id":"s1","outcome":"nothing","reason":"r"}"#,
        )
        .unwrap_err();
        assert_eq!(e.kind, "unknown_tool");
    }

    #[test]
    fn each_fact_carries_its_own_embedding() {
        use crate::providers::EmbeddingsProvider;
        let (conn, tmp) = env();
        let vectors = crate::tools::prepare(
            Some(&crate::providers::mock::MockEmbeddings),
            "inbox_decide",
            TWO_FACTS,
        );
        assert_eq!(vectors.facts.len(), 2);
        assert!(vectors.facts.iter().all(std::option::Option::is_some));
        assert_ne!(vectors.facts[0], vectors.facts[1]);

        let mut c = ctx(&tmp, "s1");
        c.vectors = vectors;
        let out = dispatch(&conn, &c, SessionKind::Inbox, "inbox_decide", TWO_FACTS).unwrap();
        let n: i64 =
            conn.query_row("SELECT COUNT(*) FROM memory_vectors", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2, "every fact must be embedded, not just the first");

        let e = crate::providers::mock::MockEmbeddings;
        let want = e.embed(&[&crate::memory::embed_text("quiz", "Biology: quiz on 2026-09-25.")])
            .unwrap();
        let stored: Vec<u8> = conn
            .query_row(
                "SELECT vector FROM memory_vectors WHERE id = ?1",
                [out["memory_ids"][0].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        let stored: Vec<f32> =
            stored.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        assert_eq!(stored, want[0]);
    }
    #[test]
    fn the_decision_is_recorded_on_the_inbox_row() {
        let (conn, tmp) = env();
        let now = jiff::Timestamp::now();
        crate::inbox::upsert(&conn, 1, "s1", "announcement", "Quiz Friday", now).unwrap();
        decide(&conn, &tmp, "s1", TWO_FACTS).unwrap();
        let (outcome, reason, decided): (Option<String>, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT outcome, reason, decided_at FROM inbox_items WHERE user_id = 1 AND source_id = 's1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(outcome.as_deref(), Some("remembered"));
        assert_eq!(reason.as_deref(), Some("quiz and rule"));
        assert!(decided.unwrap() >= crate::inbox::stamp(now));

        // a rejected call leaves the row as it was
        crate::inbox::upsert(&conn, 1, "s1", "announcement", "Quiz moved", now).unwrap();
        decide(&conn, &tmp, "s1", r#"{"source_id":"s1","outcome":"nothing","reason":"  "}"#).unwrap_err();
        let outcome: Option<String> = conn
            .query_row("SELECT outcome FROM inbox_items WHERE source_id = 's1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(outcome, None);
    }
}
