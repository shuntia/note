//! What the tool tests share: a database with one user, a config dir that
//! places them in a zone, and a call through `dispatch`.

use super::{dispatch, PreparedVectors, SessionKind, ToolCtx, ToolError};
use rusqlite::Connection;
use serde_json::Value;

pub fn env(tz: &str) -> (Connection, tempfile::TempDir) {
    let conn = crate::db::open_memory().unwrap();
    conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", []).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("defaults")).unwrap();
    std::fs::write(
        tmp.path().join("defaults/user.toml"),
        format!("display_name = \"X\"\ntimezone = \"{tz}\"\ntemplate = \"default\"\n"),
    )
    .unwrap();
    (conn, tmp)
}

pub fn ctx(tmp: &tempfile::TempDir) -> ToolCtx<'_> {
    ToolCtx {
        config_dir: tmp.path(),
        data_dir: tmp.path(),
        user_id: 1,
        username: "aki",
        vectors: PreparedVectors::default(),
        task_scope: None,
        inbox_source: None,
        memory_source: None,
        share: None,
        share_thread: None,
    }
}

pub fn call(conn: &Connection, tmp: &tempfile::TempDir, kind: SessionKind, name: &str, args: &str) -> Result<Value, ToolError> {
    dispatch(conn, &ctx(tmp), kind, name, args)
}

pub fn talk(conn: &Connection, tmp: &tempfile::TempDir, name: &str, args: impl std::fmt::Display) -> Value {
    call(conn, tmp, SessionKind::Talk, name, &args.to_string()).unwrap()
}

/// The id of the item saved at `r`.
pub fn saved(out: &Value, r: &str) -> i64 {
    out["saved"]
        .as_array()
        .and_then(|s| s.iter().find(|x| x["ref"] == r))
        .and_then(|x| x["id"].as_i64())
        .unwrap_or_else(|| panic!("nothing saved at {r}: {out}"))
}

pub fn task(conn: &Connection, id: i64) -> crate::tasks::Task {
    crate::tasks::get(conn, 1, id).unwrap().unwrap()
}
