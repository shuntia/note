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
    args: QueryArgs,
) -> Result<serde_json::Value, ToolError> {
    if !(1..=50).contains(&args.limit) {
        return Err(ToolError::rejected("limit must be in 1..=50"));
    }
    let hits = crate::memory::query(conn, ctx.username, &args.query, args.limit, ctx.embeddings)
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
    args: ReadArgs,
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
}

pub fn write(
    conn: &Connection,
    ctx: &ToolCtx,
    args: WriteArgs,
) -> Result<serde_json::Value, ToolError> {
    if args.summary.trim().is_empty() || args.summary.len() > MAX_SUMMARY {
        return Err(ToolError::rejected(format!(
            "summary must be 1..={MAX_SUMMARY} bytes"
        )));
    }
    super::check_text("body", &args.body)?;
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
            crate::memory::add(
                conn,
                ctx.data_dir,
                ctx.username,
                cat.as_str(),
                &args.summary,
                &args.body,
                ctx.embeddings,
            )
            .map(|id| serde_json::json!({ "id": id }))
            .map_err(|e| ToolError::internal(e.to_string()))
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
                ctx.embeddings,
            ) {
                Ok(Some(())) => Ok(serde_json::json!({ "id": id })),
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
                ctx.embeddings,
            ) {
                Ok(Some(new_id)) => Ok(serde_json::json!({ "id": new_id })),
                Ok(None) => Err(ToolError::not_found(format!("no memory {id}"))),
                Err(crate::memory::WriteError::Archived(id)) => Err(ToolError::rejected(format!(
                    "memory {id} is archived and immutable"
                ))),
                Err(e) => Err(ToolError::internal(e.to_string())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, SessionKind, ToolCtx};

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir) -> ToolCtx<'a> {
        ToolCtx { config_dir: tmp.path(), data_dir: tmp.path(), user_id: 1, username: "aki", embeddings: None }
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
