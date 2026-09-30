use super::{ToolCtx, ToolError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

const MAX_SUMMARY: usize = 200;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryArgs {
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    8
}

pub fn query(
    conn: &Connection,
    ctx: &ToolCtx,
    args: &QueryArgs,
) -> Result<serde_json::Value, ToolError> {
    if !(1..=50).contains(&args.limit) {
        return Err(ToolError::rejected("limit must be in 1..=50"));
    }
    if let Some(err) = &ctx.vectors.error {
        let _ = crate::log::record(conn, None, "memory_embed_error", &format!("query: {err}"));
    }
    let hits = crate::memory::query(conn, ctx.username, &args.query, args.limit, ctx.vectors.query.as_deref())
        .map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(serde_json::json!({ "results": hits }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadArgs {
    pub id: String,
}

pub fn read(
    conn: &Connection,
    ctx: &ToolCtx,
    args: &ReadArgs,
) -> Result<serde_json::Value, ToolError> {
    let _ = conn;
    if !crate::memory::valid_id(&args.id) {
        return Err(ToolError::rejected("malformed memory id"));
    }
    match crate::memory::read(ctx.data_dir, ctx.username, &args.id) {
        Ok(Some(f)) => Ok(serde_json::json!({
            "id": f.id, "category": f.category, "summary": f.summary,
            "body": f.body, "archived": f.archived,
        })),
        Ok(None) => Err(ToolError::not_found(format!("no memory {}", args.id))),
        Err(e) => Err(ToolError::internal(e.to_string())),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WriteOp {
    Add,
    Update,
    Supersede,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Semantic,
    Episodic,
    Procedural,
}

impl Category {
    fn as_str(&self) -> &'static str {
        match self {
            Category::Semantic => "semantic",
            Category::Episodic => "episodic",
            Category::Procedural => "procedural",
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteArgs {
    pub op: WriteOp,
    pub id: Option<String>,
    pub category: Option<Category>,
    pub summary: String,
    pub body: String,
    /// `YYYY-MM-DD`, after which the nightly sweep archives the fact.
    #[serde(default)]
    pub until: Option<String>,
}

pub fn write(
    conn: &Connection,
    ctx: &ToolCtx,
    args: &WriteArgs,
) -> Result<serde_json::Value, ToolError> {
    if args.summary.trim().is_empty() || args.summary.len() > MAX_SUMMARY {
        return Err(ToolError::rejected(format!(
            "summary must be 1..={MAX_SUMMARY} bytes"
        )));
    }
    super::check_text("body", &args.body)?;
    if let Some(u) = &args.until {
        if u.parse::<jiff::civil::Date>().is_err() {
            return Err(ToolError::rejected(format!("until {u:?} is not a YYYY-MM-DD date")));
        }
    }
    if let Some(err) = &ctx.vectors.error {
        let _ = crate::log::record(conn, None, "memory_embed_error", &format!("write: {err}"));
    }
    let need_id = || -> Result<String, ToolError> {
        let id = args
            .id
            .clone()
            .ok_or_else(|| ToolError::rejected("this op requires an id"))?;
        if !crate::memory::valid_id(&id) {
            return Err(ToolError::rejected("malformed memory id"));
        }
        Ok(id)
    };
    match args.op {
        WriteOp::Add => {
            let cat = args
                .category
                .as_ref()
                .ok_or_else(|| ToolError::rejected("add requires a category"))?;
            let id = crate::memory::add_until(conn, ctx.data_dir, ctx.username, &crate::memory::Fact { category: cat.as_str(), summary: &args.summary, body: &args.body, until: args.until.as_deref() }, ctx.vectors.content.as_deref())
            .map_err(|e| ToolError::internal(e.to_string()))?;
            record_source(conn, ctx, &id)?;
            Ok(serde_json::json!({ "id": id }))
        }
        WriteOp::Update => {
            let id = need_id()?;
            match crate::memory::update(
                conn,
                ctx.data_dir,
                ctx.username,
                &id,
                &args.summary,
                &args.body,
                ctx.vectors.content.as_deref(),
            ) {
                Ok(Some(())) => {
                    record_source(conn, ctx, &id)?;
                    Ok(serde_json::json!({ "id": id }))
                }
                Ok(None) => Err(ToolError::not_found(format!("no memory {id}"))),
                Err(crate::memory::WriteError::Archived(id)) => Err(ToolError::rejected(format!(
                    "memory {id} is archived and immutable"
                ))),
                Err(e) => Err(ToolError::internal(e.to_string())),
            }
        }
        WriteOp::Supersede => {
            let id = need_id()?;
            match crate::memory::supersede(
                conn,
                ctx.data_dir,
                ctx.username,
                &id,
                &args.summary,
                &args.body,
                ctx.vectors.content.as_deref(),
            ) {
                Ok(Some(new_id)) => {
                    record_source(conn, ctx, &new_id)?;
                    Ok(serde_json::json!({ "id": new_id }))
                }
                Ok(None) => Err(ToolError::not_found(format!("no memory {id}"))),
                Err(crate::memory::WriteError::Archived(id)) => Err(ToolError::rejected(format!(
                    "memory {id} is archived and immutable"
                ))),
                Err(e) => Err(ToolError::internal(e.to_string())),
            }
        }
    }
}

/// Provenance for a session that writes on something else's behalf; a session
/// with no source of its own leaves the table alone.
fn record_source(conn: &Connection, ctx: &ToolCtx, memory_id: &str) -> Result<(), ToolError> {
    let Some(source) = &ctx.memory_source else { return Ok(()) };
    conn.execute(
        "INSERT OR IGNORE INTO memory_sources (user_id, source_id, memory_id) VALUES (?1, ?2, ?3)",
        (ctx.user_id, source, memory_id),
    )
    .map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(())
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

    fn ctx(tmp: &tempfile::TempDir) -> ToolCtx<'_> {
        ToolCtx { config_dir: tmp.path(), data_dir: tmp.path(), user_id: 1, username: "aki", vectors: PreparedVectors::default(), task_scope: None, inbox_source: None, memory_source: None, share: None, share_thread: None }
    }

    #[test]
    fn write_query_read_supersede_flow() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_write",
            r#"{"op":"add","category":"semantic","summary":"tea preference","body":"green, no sugar"}"#).unwrap();
        let id = out["id"].as_str().unwrap().to_string();

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_query",
            r#"{"query":"tea"}"#).unwrap();
        assert_eq!(out["results"][0]["id"], id.as_str());

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_read",
            &format!(r#"{{"id":"{id}"}}"#)).unwrap();
        assert_eq!(out["body"], "green, no sugar");

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_write",
            &format!(r#"{{"op":"supersede","id":"{id}","summary":"tea preference","body":"switched to coffee"}}"#)).unwrap();
        let new_id = out["id"].as_str().unwrap();
        assert_ne!(new_id, id);

        // superseding an archived fact is a typed rejection
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_write",
            &format!(r#"{{"op":"supersede","id":"{id}","summary":"x","body":"y"}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");
    }

    #[test]
    fn a_sourced_session_records_where_each_fact_came_from() {
        let (conn, tmp) = env();
        let sourced = ToolCtx { memory_source: Some("harvest:2026-09-17".into()), ..ctx(&tmp) };
        let out = dispatch(&conn, &sourced, SessionKind::Harvest, "memory_write",
            r#"{"op":"add","category":"semantic","summary":"runs on tuesdays","body":"with mira"}"#).unwrap();
        let id = out["id"].as_str().unwrap().to_string();
        let out = dispatch(&conn, &sourced, SessionKind::Harvest, "memory_write",
            &format!(r#"{{"op":"supersede","id":"{id}","summary":"runs on thursdays","body":"alone"}}"#)).unwrap();
        let new_id = out["id"].as_str().unwrap().to_string();

        let mut stmt = conn
            .prepare("SELECT memory_id FROM memory_sources WHERE source_id = 'harvest:2026-09-17' ORDER BY memory_id")
            .unwrap();
        let mut ids: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        ids.sort();
        let mut want = vec![id, new_id];
        want.sort();
        assert_eq!(ids, want);

        dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_write",
            r#"{"op":"add","category":"semantic","summary":"likes tea","body":"green"}"#).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "a session with no source records nothing");
    }

    #[test]
    fn an_added_fact_can_carry_the_date_it_stops_mattering() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Harvest, "memory_write",
            r#"{"op":"add","category":"episodic","summary":"Day 2026-09-17: the essay went in","body":"an hour of drafting","until":"2026-12-16"}"#).unwrap();
        let id = out["id"].as_str().unwrap();
        let f = crate::memory::read(tmp.path(), "aki", id).unwrap().unwrap();
        assert_eq!(f.until.as_deref(), Some("2026-12-16"));
        assert_eq!(f.category, "episodic");

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Harvest, "memory_write",
            r#"{"op":"add","category":"semantic","summary":"mira lives next door","body":"since september"}"#).unwrap();
        assert!(crate::memory::read(tmp.path(), "aki", out["id"].as_str().unwrap())
            .unwrap()
            .unwrap()
            .until
            .is_none());

        for bad in ["friday", "2026-13-01"] {
            let args = format!(
                r#"{{"op":"add","category":"episodic","summary":"s","body":"b","until":"{bad}"}}"#
            );
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Harvest, "memory_write", &args)
                .unwrap_err();
            assert_eq!(e.kind, "rejected", "{bad}");
        }
    }

    #[test]
    fn malformed_ids_categories_and_op_combos_are_typed() {
        let (conn, tmp) = env();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_read",
            r#"{"id":"../../../etc/passwd"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_write",
            r#"{"op":"add","category":"../../evil","summary":"s","body":"b"}"#).unwrap_err();
        assert_eq!(e.kind, "invalid_args");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_write",
            r#"{"op":"add","summary":"s","body":"b"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected"); // add without category
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_write",
            r#"{"op":"update","summary":"s","body":"b"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected"); // update without id
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_write",
            r#"{"op":"update","id":"00000000-0000-4000-8000-000000000000","summary":"s","body":"b"}"#).unwrap_err();
        assert_eq!(e.kind, "not_found");
    }
}
