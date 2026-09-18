use crate::tools::SessionKind;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// The cap on every stored piece of model or tool text, so one runaway argument
/// cannot make a trace row unreadable.
pub const MAX_FIELD_BYTES: usize = 16 * 1024;
/// Traces kept per user; older ones are dropped as each new one lands.
pub const KEEP_PER_USER: usize = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Call {
    pub name: String,
    pub ms: u64,
    pub is_error: bool,
    pub error_kind: Option<String>,
    pub args: String,
    pub result: String,
}

/// One provider call and the tool calls it asked for. A round whose provider
/// call failed carries `error` and no calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Round {
    pub ms: u64,
    pub error: Option<String>,
    pub calls: Vec<Call>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Detail {
    opening: String,
    reply: String,
    rounds: Vec<Round>,
}

/// Cuts `s` to `MAX_FIELD_BYTES` on a char boundary, marking what was dropped.
fn clip(s: &str) -> String {
    if s.len() <= MAX_FIELD_BYTES {
        return s.to_string();
    }
    let mut end = MAX_FIELD_BYTES;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn error_kind_of(result: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(result)
        .ok()
        .and_then(|v| v["kind"].as_str().map(String::from))
}

/// Collects one agent session as it runs. Every exit of the session — a reply,
/// a terminal tool, the turn cap, or a provider failure — finalizes the same
/// builder, so no session ends without leaving a row.
pub struct Builder {
    ts: String,
    kind: String,
    started: std::time::Instant,
    opening: String,
    reply: String,
    outcome: &'static str,
    error: Option<String>,
    rounds: Vec<Round>,
    turns: usize,
    tool_calls: usize,
}

impl Builder {
    pub fn new(kind: SessionKind, opening: &str) -> Self {
        Self {
            ts: jiff::Timestamp::now().to_string(),
            kind: format!("{kind:?}"),
            started: std::time::Instant::now(),
            opening: clip(opening),
            reply: String::new(),
            outcome: "error",
            error: None,
            rounds: Vec::new(),
            turns: 0,
            tool_calls: 0,
        }
    }

    /// Opens a round the provider answered; the calls recorded next are its own.
    pub fn round(&mut self, ms: u64) {
        self.turns += 1;
        self.rounds.push(Round { ms, error: None, calls: Vec::new() });
    }

    /// The round the session died in: it holds no calls and ends the trace.
    pub fn round_failed(&mut self, ms: u64, error: &str) {
        self.rounds.push(Round { ms, error: Some(error.to_string()), calls: Vec::new() });
    }

    pub fn call(&mut self, name: &str, args: &str, result: &str, is_error: bool, ms: u64) {
        self.tool_calls += 1;
        let error_kind = is_error.then(|| error_kind_of(result)).flatten();
        let call = Call {
            name: name.to_string(),
            ms,
            is_error,
            error_kind,
            args: clip(args),
            result: clip(result),
        };
        match self.rounds.last_mut() {
            Some(round) => round.calls.push(call),
            None => self.rounds.push(Round { ms: 0, error: None, calls: vec![call] }),
        }
    }

    pub fn ok(&mut self, reply: &str) {
        self.outcome = "ok";
        self.reply = clip(reply);
    }

    pub fn max_turns(&mut self, reply: &str) {
        self.outcome = "max_turns";
        self.reply = clip(reply);
    }

    pub fn failed(&mut self, error: &str) {
        self.outcome = "error";
        self.error = Some(error.to_string());
    }

    /// Writes the row and prunes the user's oldest traces.
    pub fn insert(&self, conn: &Connection, user_id: i64) -> Result<()> {
        let detail = serde_json::to_string(&Detail {
            opening: self.opening.clone(),
            reply: self.reply.clone(),
            rounds: self.rounds.clone(),
        })?;
        conn.execute(
            "INSERT INTO agent_traces
                 (ts, user_id, kind, outcome, turns, tool_calls, duration_ms, error, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            (
                &self.ts,
                user_id,
                &self.kind,
                self.outcome,
                self.turns as i64,
                self.tool_calls as i64,
                self.started.elapsed().as_millis() as i64,
                self.error.as_deref(),
                detail,
            ),
        )?;
        prune(conn, user_id)?;
        Ok(())
    }
}

fn prune(conn: &Connection, user_id: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM agent_traces WHERE user_id = ?1 AND id <= (
             SELECT id FROM agent_traces WHERE user_id = ?1 ORDER BY id DESC LIMIT 1 OFFSET ?2
         )",
        (user_id, KEEP_PER_USER as i64),
    )
}

#[derive(Debug, Default)]
pub struct Filter {
    pub limit: i64,
    pub user_id: Option<i64>,
    pub kind: Option<String>,
    pub outcome: Option<String>,
    pub before_id: Option<i64>,
}

const ROW_COLUMNS: &str = "t.id, t.ts, t.user_id, u.username, t.kind, t.outcome,
                           t.turns, t.tool_calls, t.duration_ms, t.error";

fn row_json(r: &rusqlite::Row) -> rusqlite::Result<serde_json::Value> {
    Ok(serde_json::json!({
        "id": r.get::<_, i64>(0)?,
        "ts": r.get::<_, String>(1)?,
        "user_id": r.get::<_, i64>(2)?,
        "username": r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        "kind": r.get::<_, String>(4)?,
        "outcome": r.get::<_, String>(5)?,
        "turns": r.get::<_, i64>(6)?,
        "tool_calls": r.get::<_, i64>(7)?,
        "duration_ms": r.get::<_, i64>(8)?,
        "error": r.get::<_, Option<String>>(9)?,
    }))
}

/// Newest first, with every kind the table holds so a caller can offer them as
/// a filter.
pub fn list(conn: &Connection, f: &Filter) -> rusqlite::Result<serde_json::Value> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ROW_COLUMNS} FROM agent_traces t LEFT JOIN users u ON u.id = t.user_id
         WHERE (?1 IS NULL OR t.user_id = ?1)
           AND (?2 IS NULL OR t.kind = ?2)
           AND (?3 IS NULL OR t.outcome = ?3)
           AND (?4 IS NULL OR t.id < ?4)
         ORDER BY t.id DESC LIMIT ?5"
    ))?;
    let rows = stmt
        .query_map((f.user_id, &f.kind, &f.outcome, f.before_id, f.limit), row_json)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut kinds = conn.prepare("SELECT DISTINCT kind FROM agent_traces ORDER BY kind")?;
    let kinds = kinds
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(serde_json::json!({ "rows": rows, "kinds": kinds }))
}

/// One trace in full. Without `full` the user's own words are left behind: the
/// shape of the session survives, its content does not.
pub fn detail(conn: &Connection, id: i64, full: bool) -> rusqlite::Result<Option<serde_json::Value>> {
    let row: Option<(serde_json::Value, String)> = conn
        .query_row(
            &format!(
                "SELECT {ROW_COLUMNS}, t.detail FROM agent_traces t
                 LEFT JOIN users u ON u.id = t.user_id WHERE t.id = ?1"
            ),
            [id],
            |r| Ok((row_json(r)?, r.get::<_, String>(10)?)),
        )
        .optional()?;
    let Some((mut out, raw)) = row else { return Ok(None) };
    let detail: Detail = serde_json::from_str(&raw).unwrap_or_default();
    let rounds: Vec<serde_json::Value> = detail
        .rounds
        .iter()
        .map(|round| {
            let calls: Vec<serde_json::Value> = round
                .calls
                .iter()
                .map(|c| {
                    let mut call = serde_json::json!({
                        "name": c.name,
                        "ms": c.ms,
                        "is_error": c.is_error,
                        "error_kind": c.error_kind,
                    });
                    if full {
                        call["args"] = c.args.clone().into();
                        call["result"] = c.result.clone().into();
                    }
                    call
                })
                .collect();
            serde_json::json!({ "ms": round.ms, "error": round.error, "calls": calls })
        })
        .collect();
    out["rounds"] = rounds.into();
    out["full"] = full.into();
    if full {
        out["opening"] = detail.opening.into();
        out["reply"] = detail.reply.into();
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let conn = crate::db::open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aki','x','member')", [])
            .unwrap();
        conn
    }

    #[test]
    fn oversized_text_is_cut_on_a_char_boundary() {
        let wide = "あ".repeat(MAX_FIELD_BYTES);
        let cut = clip(&wide);
        assert!(cut.ends_with('…'));
        assert!(cut.len() <= MAX_FIELD_BYTES + '…'.len_utf8());
        assert!(cut.chars().take_while(|c| *c == 'あ').count() > 0);
        assert_eq!(clip("short"), "short");
    }

    #[test]
    fn retention_keeps_the_newest_traces_of_each_user() {
        let conn = conn();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('bo','x','member')", [])
            .unwrap();
        for _ in 0..KEEP_PER_USER + 5 {
            let mut b = Builder::new(SessionKind::Talk, "hi");
            b.ok("there");
            b.insert(&conn, 1).unwrap();
        }
        let mut b = Builder::new(SessionKind::Talk, "hi");
        b.ok("there");
        b.insert(&conn, 2).unwrap();
        let count = |user: i64| -> i64 {
            conn.query_row("SELECT COUNT(*) FROM agent_traces WHERE user_id = ?1", [user], |r| {
                r.get(0)
            })
            .unwrap()
        };
        assert_eq!(count(1), KEEP_PER_USER as i64);
        assert_eq!(count(2), 1);
        let oldest: i64 = conn
            .query_row("SELECT MIN(id) FROM agent_traces WHERE user_id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(oldest, 6);
    }

    #[test]
    fn a_detail_without_full_carries_the_shape_and_none_of_the_words() {
        let conn = conn();
        let mut b = Builder::new(SessionKind::Talk, "add buy milk");
        b.round(120);
        b.call("task_create", r#"{"title":"buy milk"}"#, r#"{"task_id":1}"#, false, 4);
        b.call("task_update", "{}", r#"{"kind":"not_found","message":"no task 9"}"#, true, 2);
        b.round(80);
        b.ok("added");
        b.insert(&conn, 1).unwrap();

        let full = detail(&conn, 1, true).unwrap().unwrap();
        assert_eq!(full["opening"], "add buy milk");
        assert_eq!(full["reply"], "added");
        assert_eq!(full["rounds"][0]["calls"][0]["args"], r#"{"title":"buy milk"}"#);
        assert_eq!(full["rounds"][0]["calls"][1]["error_kind"], "not_found");

        let bare = detail(&conn, 1, false).unwrap().unwrap();
        assert_eq!(bare["full"], false);
        assert!(bare["opening"].is_null() && bare["reply"].is_null());
        assert_eq!(bare["rounds"][0]["ms"], 120);
        assert_eq!(bare["rounds"][0]["calls"][0]["name"], "task_create");
        assert_eq!(bare["rounds"][0]["calls"][0]["ms"], 4);
        assert_eq!(bare["rounds"][0]["calls"][1]["error_kind"], "not_found");
        for call in bare["rounds"][0]["calls"].as_array().unwrap() {
            assert!(call["args"].is_null() && call["result"].is_null(), "{call}");
        }
        assert!(detail(&conn, 99, true).unwrap().is_none());
    }

    #[test]
    fn a_failing_round_ends_the_trace_and_keeps_what_ran() {
        let conn = conn();
        let mut b = Builder::new(SessionKind::Nightly, "night");
        b.round(10);
        b.call("memory_query", "{}", "[]", false, 1);
        b.round_failed(4_000, "timed out reading response");
        b.failed("nightly session: timed out reading response");
        b.insert(&conn, 1).unwrap();

        let v = detail(&conn, 1, true).unwrap().unwrap();
        assert_eq!(v["outcome"], "error");
        assert_eq!(v["turns"], 1, "only the answered round counts as a turn");
        assert_eq!(v["error"], "nightly session: timed out reading response");
        assert_eq!(v["rounds"].as_array().unwrap().len(), 2);
        assert_eq!(v["rounds"][1]["error"], "timed out reading response");
        assert_eq!(v["rounds"][1]["ms"], 4_000);
        assert!(v["rounds"][1]["calls"].as_array().unwrap().is_empty());
    }

    #[test]
    fn the_list_filters_by_user_kind_and_outcome_and_pages_backwards() {
        let conn = conn();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('bo','x','member')", [])
            .unwrap();
        for (user, kind, ok) in [
            (1, SessionKind::Talk, true),
            (1, SessionKind::Nightly, false),
            (2, SessionKind::Talk, true),
            (1, SessionKind::Talk, true),
        ] {
            let mut b = Builder::new(kind, "x");
            match ok {
                true => b.ok("y"),
                false => b.failed("down"),
            }
            b.insert(&conn, user).unwrap();
        }
        let all = list(&conn, &Filter { limit: 10, ..Filter::default() }).unwrap();
        let rows = all["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0]["id"], 4, "newest first");
        assert_eq!(rows[0]["username"], "aki");
        assert_eq!(all["kinds"], serde_json::json!(["Nightly", "Talk"]));

        let mine = list(&conn, &Filter { limit: 10, user_id: Some(2), ..Filter::default() }).unwrap();
        assert_eq!(mine["rows"].as_array().unwrap().len(), 1);
        let failed =
            list(&conn, &Filter { limit: 10, outcome: Some("error".into()), ..Filter::default() })
                .unwrap();
        assert_eq!(failed["rows"][0]["id"], 2);
        let talk =
            list(&conn, &Filter { limit: 10, kind: Some("Talk".into()), ..Filter::default() })
                .unwrap();
        assert_eq!(talk["rows"].as_array().unwrap().len(), 3);
        let page = list(&conn, &Filter { limit: 2, before_id: Some(4), ..Filter::default() }).unwrap();
        assert_eq!(page["rows"][0]["id"], 3);
        assert_eq!(page["rows"].as_array().unwrap().len(), 2);
    }
}
