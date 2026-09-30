use crate::auth::CurrentUser;
use crate::AppState;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing;
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const KINDS: [&str; 2] = ["announcement", "material"];
pub const LIST_LIMIT_DEFAULT: usize = 50;
pub const LIST_LIMIT_MAX: usize = 100;
const MAX_TITLE_CHARS: usize = 200;

#[derive(Debug, Serialize, PartialEq)]
pub struct InboxRow {
    pub id: i64,
    pub source_id: String,
    pub kind: String,
    pub title: String,
    pub received_at: String,
    pub outcome: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ProducedMemory {
    pub id: String,
    pub summary: String,
    pub archived: bool,
}

#[derive(Debug, Serialize)]
pub struct InboxItem {
    pub id: i64,
    pub source_id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub received_at: String,
    pub outcome: Option<String>,
    pub reason: Option<String>,
    pub decided_at: Option<String>,
    pub memories: Vec<ProducedMemory>,
}

/// Fixed-width UTC, so stored times sort as text and a client can compare them
/// as strings.
pub fn stamp(ts: jiff::Timestamp) -> String {
    format!("{ts:.6}")
}

/// The first non-blank line of the context, or the source id when it has none.
pub fn title_of(context: &str, source_id: &str) -> String {
    let line = context.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(source_id);
    line.chars().take(MAX_TITLE_CHARS).collect::<String>().trim_end().to_string()
}

/// A re-sent source keeps its row and reads as undecided until its new session
/// decides.
pub fn upsert(
    conn: &Connection,
    user_id: i64,
    source_id: &str,
    kind: &str,
    context: &str,
    now: jiff::Timestamp,
) -> rusqlite::Result<i64> {
    conn.query_row(
        "INSERT INTO inbox_items (user_id, source_id, kind, title, body, received_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (user_id, source_id) DO UPDATE SET
             kind = excluded.kind,
             title = excluded.title,
             body = excluded.body,
             received_at = excluded.received_at,
             outcome = NULL,
             reason = NULL,
             decided_at = NULL
         RETURNING id",
        (user_id, source_id, kind, title_of(context, source_id), context, stamp(now)),
        |r| r.get(0),
    )
}

pub fn record_decision(
    conn: &Connection,
    user_id: i64,
    source_id: &str,
    outcome: &str,
    reason: &str,
    now: jiff::Timestamp,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE inbox_items SET outcome = ?3, reason = ?4, decided_at = ?5
         WHERE user_id = ?1 AND source_id = ?2",
        (user_id, source_id, outcome, reason, stamp(now)),
    )?;
    Ok(())
}

/// Newest first; `before` is a stamp and is exclusive.
pub fn list(
    conn: &Connection,
    user_id: i64,
    before: Option<&str>,
    limit: usize,
) -> rusqlite::Result<Vec<InboxRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, source_id, kind, title, received_at, outcome FROM inbox_items
         WHERE user_id = ?1 AND (?2 IS NULL OR received_at < ?2)
         ORDER BY received_at DESC, id DESC
         LIMIT ?3",
    )?;
    let rows = stmt.query_map((user_id, before, i64::try_from(limit).unwrap_or(i64::MAX)), |r| {
        Ok(InboxRow {
            id: r.get(0)?,
            source_id: r.get(1)?,
            kind: r.get(2)?,
            title: r.get(3)?,
            received_at: r.get(4)?,
            outcome: r.get(5)?,
        })
    })?;
    rows.collect()
}

pub fn latest(conn: &Connection, user_id: i64) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT MAX(received_at) FROM inbox_items WHERE user_id = ?1",
        [user_id],
        |r| r.get(0),
    )
}

/// The memories are the ones the source holds now; a memory whose file is gone
/// is left out.
pub fn get(
    conn: &Connection,
    data_dir: &Path,
    username: &str,
    user_id: i64,
    id: i64,
) -> anyhow::Result<Option<InboxItem>> {
    let item = conn
        .query_row(
            "SELECT id, source_id, kind, title, body, received_at, outcome, reason, decided_at
             FROM inbox_items WHERE user_id = ?1 AND id = ?2",
            (user_id, id),
            |r| {
                Ok(InboxItem {
                    id: r.get(0)?,
                    source_id: r.get(1)?,
                    kind: r.get(2)?,
                    title: r.get(3)?,
                    body: r.get(4)?,
                    received_at: r.get(5)?,
                    outcome: r.get(6)?,
                    reason: r.get(7)?,
                    decided_at: r.get(8)?,
                    memories: Vec::new(),
                })
            },
        )
        .optional()?;
    let Some(mut item) = item else {
        return Ok(None);
    };
    let ids: Vec<String> = conn
        .prepare(
            "SELECT memory_id FROM memory_sources WHERE user_id = ?1 AND source_id = ?2
             ORDER BY memory_id",
        )?
        .query_map((user_id, &item.source_id), |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for mid in ids {
        if !crate::memory::valid_id(&mid) {
            continue;
        }
        if let Some(f) = crate::memory::read(data_dir, username, &mid)? {
            item.memories.push(ProducedMemory { id: f.id, summary: f.summary, archived: f.archived });
        }
    }
    Ok(Some(item))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/inbox", routing::get(list_route))
        .route("/api/inbox/refresh", routing::post(refresh_route))
        .route("/api/inbox/{id}", routing::get(read_route))
}

#[derive(Deserialize)]
struct ListQuery {
    before: Option<String>,
    limit: Option<String>,
}

fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": message }))).into_response()
}

fn non_blank(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

async fn list_route(
    user: CurrentUser,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Response {
    let limit = match non_blank(q.limit.as_deref()) {
        Some(raw) => match raw.parse::<usize>() {
            Ok(n) => n.clamp(1, LIST_LIMIT_MAX),
            Err(_) => return bad_request("limit must be a number"),
        },
        None => LIST_LIMIT_DEFAULT,
    };
    let before = match non_blank(q.before.as_deref()) {
        Some(raw) => match raw.parse::<jiff::Timestamp>() {
            Ok(ts) => Some(stamp(ts)),
            Err(_) => return bad_request("before must be a timestamp"),
        },
        None => None,
    };
    let conn = state.db();
    match (list(&conn, user.id, before.as_deref(), limit), latest(&conn, user.id)) {
        (Ok(items), Ok(latest)) => {
            Json(serde_json::json!({
                "items": items,
                "latest": latest,
                "refresh": state.inbox_refresh.is_some(),
            }))
            .into_response()
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Another user's item is indistinguishable from one that does not exist.
async fn read_route(
    user: CurrentUser,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let conn = state.db();
    match get(&conn, &state.data_dir, &user.username, user.id, id) {
        Ok(Some(item)) => Json(item).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// The host starts a sync when the file changes; Note never runs it itself.
async fn refresh_route(user: CurrentUser, State(state): State<AppState>) -> Response {
    let Some(path) = state.inbox_refresh.clone() else {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "refresh is not configured" })),
        )
            .into_response();
    };
    let requested_at = stamp(jiff::Timestamp::now());
    match std::fs::write(&path, format!("{requested_at}\n")) {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "requested_at": requested_at })),
        )
            .into_response(),
        Err(e) => {
            let conn = state.db();
            let _ = crate::log::record(
                &conn,
                Some(user.id),
                "inbox_refresh_error",
                &format!("{}: {e}", path.display()),
            );
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "refresh is unavailable" })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> (Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        for name in ["aki", "bo"] {
            conn.execute(
                "INSERT INTO users (username, pass_hash, role) VALUES (?1, 'x', 'member')",
                [name],
            )
            .unwrap();
        }
        (conn, tempfile::tempdir().unwrap())
    }

    fn at(second: i64) -> jiff::Timestamp {
        jiff::Timestamp::from_second(1_790_000_000 + second).unwrap()
    }

    #[test]
    fn stamps_are_fixed_width_so_text_order_is_time_order() {
        assert_eq!(stamp(jiff::Timestamp::from_second(0).unwrap()), "1970-01-01T00:00:00.000000Z");
        let half = jiff::Timestamp::new(0, 500_000_000).unwrap();
        assert_eq!(stamp(half), "1970-01-01T00:00:00.500000Z");
        assert!(stamp(half) > stamp(jiff::Timestamp::from_second(0).unwrap()));
        assert!(stamp(at(1)) > stamp(half));
    }

    #[test]
    fn title_is_the_first_non_blank_line_cut_to_200_chars() {
        assert_eq!(title_of("  \n\n  Quiz Friday  \nLate work loses 10%", "s1"), "Quiz Friday");
        assert_eq!(title_of("", "lms:post:1"), "lms:post:1");
        assert_eq!(title_of(" \n\t\n ", "lms:post:1"), "lms:post:1");
        let long = "é".repeat(300);
        let t = title_of(&long, "s1");
        assert_eq!(t.chars().count(), 200);
        assert!(t.chars().all(|c| c == 'é'));
        assert_eq!(title_of(&format!("{} tail", "a".repeat(199)), "s1"), "a".repeat(199));
    }

    #[test]
    fn a_re_send_updates_the_row_in_place_and_clears_the_decision() {
        let (conn, _tmp) = env();
        let id = upsert(&conn, 1, "s1", "announcement", "Quiz Friday\nbody", at(0)).unwrap();
        record_decision(&conn, 1, "s1", "remembered", "a dated quiz", at(1)).unwrap();
        let again = upsert(&conn, 1, "s1", "material", "Quiz moved\nnew body", at(2)).unwrap();
        assert_eq!(id, again);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM inbox_items", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let (kind, title, body, received, outcome, reason, decided): (
            String, String, String, String, Option<String>, Option<String>, Option<String>,
        ) = conn
            .query_row(
                "SELECT kind, title, body, received_at, outcome, reason, decided_at FROM inbox_items",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .unwrap();
        assert_eq!((kind.as_str(), title.as_str(), body.as_str()), ("material", "Quiz moved", "Quiz moved\nnew body"));
        assert_eq!(received, stamp(at(2)));
        assert_eq!((outcome, reason, decided), (None, None, None));
    }

    #[test]
    fn a_decision_lands_on_its_own_row_and_nowhere_else() {
        type Decided = (i64, Option<String>, Option<String>, Option<String>);
        let (conn, _tmp) = env();
        upsert(&conn, 1, "s1", "announcement", "x", at(0)).unwrap();
        upsert(&conn, 2, "s1", "announcement", "x", at(0)).unwrap();
        record_decision(&conn, 1, "s1", "task", "a study guide", at(5)).unwrap();
        record_decision(&conn, 1, "unknown", "nothing", "no row", at(5)).unwrap();
        let rows: Vec<Decided> = conn
            .prepare("SELECT user_id, outcome, reason, decided_at FROM inbox_items ORDER BY user_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (1, Some("task".into()), Some("a study guide".into()), Some(stamp(at(5)))),
                (2, None, None, None),
            ]
        );
    }

    #[test]
    fn the_list_is_newest_first_per_user_with_a_strict_cursor() {
        let (conn, _tmp) = env();
        for (i, s) in ["a", "b", "c"].iter().enumerate() {
            upsert(&conn, 1, s, "announcement", s, at(i64::try_from(i).unwrap())).unwrap();
        }
        upsert(&conn, 2, "theirs", "material", "theirs", at(10)).unwrap();
        let all: Vec<String> = list(&conn, 1, None, 50).unwrap().into_iter().map(|r| r.source_id).collect();
        assert_eq!(all, ["c", "b", "a"]);
        let first = list(&conn, 1, None, 2).unwrap();
        assert_eq!(first.len(), 2);
        let rest: Vec<String> = list(&conn, 1, Some(&first[1].received_at), 2)
            .unwrap()
            .into_iter()
            .map(|r| r.source_id)
            .collect();
        assert_eq!(rest, ["a"]);
        assert_eq!(latest(&conn, 1).unwrap(), Some(stamp(at(2))));
        assert_eq!(latest(&conn, 2).unwrap(), Some(stamp(at(10))));
        let (empty, _t) = env();
        assert_eq!(latest(&empty, 1).unwrap(), None);
    }

    #[test]
    fn an_item_reads_back_with_the_memories_its_source_holds() {
        let (conn, tmp) = env();
        let id = upsert(&conn, 1, "s1", "announcement", "Quiz Friday\nLate work loses 10%", at(0)).unwrap();
        let mem = crate::memory::add_until(&conn, tmp.path(), "aki", &crate::memory::Fact { category: "semantic", summary: "Biology quiz", body: "Biology: quiz Friday.", until: None }, None)
        .unwrap();
        conn.execute(
            "INSERT INTO memory_sources (user_id, source_id, memory_id) VALUES (1, 's1', ?1)",
            [&mem],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_sources (user_id, source_id, memory_id) VALUES (1, 's1', 'gone')",
            [],
        )
        .unwrap();
        record_decision(&conn, 1, "s1", "remembered", "a dated quiz", at(1)).unwrap();

        let item = get(&conn, tmp.path(), "aki", 1, id).unwrap().unwrap();
        assert_eq!(item.title, "Quiz Friday");
        assert_eq!(item.body, "Quiz Friday\nLate work loses 10%");
        assert_eq!(item.outcome.as_deref(), Some("remembered"));
        assert_eq!(item.reason.as_deref(), Some("a dated quiz"));
        assert_eq!(
            item.memories,
            vec![ProducedMemory { id: mem, summary: "Biology quiz".into(), archived: false }]
        );
        assert!(get(&conn, tmp.path(), "bo", 2, id).unwrap().is_none());
        assert!(get(&conn, tmp.path(), "aki", 1, id + 100).unwrap().is_none());
    }
}
