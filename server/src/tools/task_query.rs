use super::{ToolCtx, ToolError};
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 200;
const MAX_KEYWORD_CHARS: usize = 200;
const MAX_QUERY_CHARS: usize = 200;
const SEARCH_HITS: usize = 20;
const MAX_BATCH: usize = 50;
const MAX_AGE_DAYS: u32 = 3650;

/// These tools survey or move the whole list, so a session pinned to one task
/// has no business in any of them.
pub(super) fn unscoped(ctx: &ToolCtx) -> Result<(), ToolError> {
    match ctx.task_scope {
        None => Ok(()),
        Some(scope) => Err(ToolError::rejected(format!(
            "this session works on task {scope} alone and cannot survey the whole list"
        ))),
    }
}

fn internal(e: impl std::fmt::Display) -> ToolError {
    ToolError::internal(e.to_string())
}

fn checked_date(field: &str, value: &str) -> Result<String, ToolError> {
    value
        .parse::<jiff::civil::Date>()
        .map(|d| d.to_string())
        .map_err(|_| ToolError::rejected(format!("{field} must be YYYY-MM-DD, got {value:?}")))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {
    /// open, in_progress, done, dropped, or any. Omitted: the live ones, open and in_progress.
    #[serde(default)]
    pub state: Option<String>,
    /// Case-insensitive substring of the title, the description or the notes.
    #[serde(default)]
    pub keyword: Option<String>,
    /// Only tasks in this category, matched exactly.
    #[serde(default)]
    pub category: Option<String>,
    /// Only tasks added on or after this day, YYYY-MM-DD.
    #[serde(default)]
    pub added_after: Option<String>,
    /// Only tasks added before this day, YYYY-MM-DD.
    #[serde(default)]
    pub added_before: Option<String>,
    /// Only tasks added more than this many days ago.
    #[serde(default)]
    pub older_than_days: Option<u32>,
    /// True for the Now list only, false for everything outside it.
    #[serde(default)]
    pub is_now: Option<bool>,
    /// Only tasks due before this day starts, YYYY-MM-DD in the user's timezone.
    #[serde(default)]
    pub due_before: Option<String>,
    /// Only tasks due on or after this day starts, YYYY-MM-DD in the user's timezone.
    #[serde(default)]
    pub due_after: Option<String>,
    /// True for tasks whose due date has passed, false for everything else.
    #[serde(default)]
    pub overdue: Option<bool>,
    /// Only tasks at this urgency: low, normal or high.
    #[serde(default)]
    pub urgency: Option<String>,
    /// added (default), newest first; due, soonest first with undated last; or
    /// urgency (high, then pressing, then normal, then low; soonest due inside each).
    #[serde(default)]
    pub sort: Option<String>,
    /// How many tasks to return, 1 to 200. Default 50.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// The category clause a share scope imposes on a task read; a category the
/// caller names itself must be one the link shares, and then needs no clause.
fn share_categories(
    ctx: &ToolCtx,
    requested: Option<&str>,
) -> Result<Option<(String, Vec<SqlValue>)>, ToolError> {
    let Some(scope) = &ctx.share else { return Ok(None) };
    match requested {
        Some(c) if !scope.allows_category(c.trim()) => Err(ToolError::rejected(format!(
            "category must be one of {}",
            scope.categories.join(", ")
        ))),
        Some(_) => Ok(None),
        None => Ok(scope.category_clause("category")),
    }
}

/// Whether a text match may reach descriptions and notes, which a link
/// without details keeps back.
fn matches_details(ctx: &ToolCtx) -> bool {
    ctx.share.as_ref().is_none_or(|scope| scope.details)
}

fn hides_goals(ctx: &ToolCtx) -> bool {
    ctx.share.as_ref().is_some_and(|scope| !scope.goals)
}

/// The filters, as SQL and its parameters, shared by the page and its count.
fn list_filters(ctx: &ToolCtx, args: &ListArgs) -> Result<(String, Vec<SqlValue>), ToolError> {
    let mut wheres = vec!["user_id = ?".to_string(), "parent_id IS NULL".to_string()];
    let mut params: Vec<SqlValue> = vec![ctx.user_id.into()];

    match (&ctx.share, args.state.as_deref()) {
        (_, None) => wheres.push("state IN ('open','in_progress')".into()),
        (Some(_), Some(s @ ("open" | "in_progress"))) => {
            wheres.push("state = ?".into());
            params.push(s.to_string().into());
        }
        (Some(scope), Some("done")) if scope.progress => {
            wheres.push("state = 'done' AND completed_at >= ?".into());
            params.push(crate::shares::done_since(jiff::Timestamp::now()).to_string().into());
        }
        (Some(scope), Some(s)) => {
            return Err(ToolError::rejected(if scope.progress {
                format!(
                    "this link shares live tasks and those done in the last {} days; state must be \
                     omitted, open, in_progress or done, got {s:?}",
                    crate::shares::RECENT_DAYS
                )
            } else {
                format!(
                    "this link shares live tasks alone; state must be omitted, open or in_progress, got {s:?}"
                )
            }))
        }
        (None, Some("any")) => {}
        (None, Some(s)) if crate::tasks::STATES.contains(&s) => {
            wheres.push("state = ?".into());
            params.push(s.to_string().into());
        }
        (None, Some(s)) => {
            return Err(ToolError::rejected(format!(
                "state must be one of {}, any, got {s:?}",
                crate::tasks::STATES.join(", ")
            )))
        }
    }
    if let Some(keyword) = &args.keyword {
        let needle = keyword.trim().to_lowercase();
        if needle.is_empty() || needle.chars().count() > MAX_KEYWORD_CHARS {
            return Err(ToolError::rejected(format!(
                "keyword must be 1 to {MAX_KEYWORD_CHARS} characters"
            )));
        }
        if matches_details(ctx) {
            wheres.push(
                "(instr(lower(title), ?) > 0 OR instr(lower(description), ?) > 0 \
                  OR instr(lower(notes), ?) > 0)"
                    .into(),
            );
            params.extend([needle.clone().into(), needle.clone().into(), needle.into()]);
        } else {
            wheres.push("instr(lower(title), ?) > 0".into());
            params.push(needle.into());
        }
    }
    if let Some(category) = &args.category {
        wheres.push("category = ?".into());
        params.push(category.trim().to_string().into());
    }
    if let Some(day) = &args.added_after {
        wheres.push("created_at >= ?".into());
        params.push(checked_date("added_after", day)?.into());
    }
    if let Some(day) = &args.added_before {
        wheres.push("created_at < ?".into());
        params.push(checked_date("added_before", day)?.into());
    }
    if let Some(days) = args.older_than_days {
        if days > MAX_AGE_DAYS {
            return Err(ToolError::rejected(format!(
                "older_than_days must be at most {MAX_AGE_DAYS}"
            )));
        }
        let cutoff = jiff::Timestamp::now()
            .checked_sub(jiff::Span::new().hours(24 * i64::from(days)))
            .map_err(internal)?;
        wheres.push("created_at < ?".into());
        params.push(cutoff.to_string().into());
    }
    if let Some(flag) = args.is_now {
        wheres.push("is_now = ?".into());
        params.push(i64::from(flag).into());
    }
    if let Some(day) = &args.due_before {
        wheres.push("due_at IS NOT NULL AND due_at < ?".into());
        params.push(day_start(ctx, "due_before", day)?.into());
    }
    if let Some(day) = &args.due_after {
        wheres.push("due_at IS NOT NULL AND due_at >= ?".into());
        params.push(day_start(ctx, "due_after", day)?.into());
    }
    if let Some(flag) = args.overdue {
        wheres.push(
            if flag { "(due_at IS NOT NULL AND due_at < ?)" } else { "(due_at IS NULL OR due_at >= ?)" }
                .into(),
        );
        params.push(jiff::Timestamp::now().to_string().into());
    }
    if let Some(u) = &args.urgency {
        if !crate::tasks::URGENCY.contains(&u.as_str()) {
            return Err(ToolError::rejected(format!(
                "urgency must be one of {}",
                crate::tasks::URGENCY.join(", ")
            )));
        }
        wheres.push("urgency = ?".into());
        params.push(u.clone().into());
    }
    let mut sql = wheres.join(" AND ");
    if let Some((clause, shared)) = share_categories(ctx, args.category.as_deref())? {
        sql.push_str(&clause);
        params.extend(shared);
    }
    Ok((sql, params))
}

/// The instant a local day begins, which is what a due-date filter compares
/// against.
fn day_start(ctx: &ToolCtx, field: &str, day: &str) -> Result<String, ToolError> {
    let day: jiff::civil::Date = day
        .parse()
        .map_err(|_| ToolError::rejected(format!("{field} must be YYYY-MM-DD, got {day:?}")))?;
    let tz = crate::config::UserConfig::load(ctx.config_dir, ctx.username)
        .ok()
        .and_then(|c| jiff::tz::TimeZone::get(&c.timezone).ok())
        .unwrap_or(jiff::tz::TimeZone::UTC);
    Ok(day.to_zoned(tz).map_err(internal)?.timestamp().to_string())
}

/// The user's top-level tasks, newest first, with how far their steps have got.
pub fn list(conn: &Connection, ctx: &ToolCtx, args: ListArgs) -> Result<serde_json::Value, ToolError> {
    unscoped(ctx)?;
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ToolError::rejected(format!("limit must be in 1..={MAX_LIMIT}")));
    }
    let now = jiff::Timestamp::now();
    let mut pressing_cutoff = None;
    let order = match args.sort.as_deref() {
        None | Some("added") => "created_at DESC, id DESC",
        Some("due") => "due_at IS NULL, due_at ASC, created_at DESC, id DESC",
        Some("urgency") => {
            pressing_cutoff =
                Some((now + jiff::Span::new().hours(crate::tasks::PRESSING_HOURS)).to_string());
            "CASE WHEN urgency = 'high' THEN 0
                  WHEN state IN ('open','in_progress') AND due_at IS NOT NULL AND due_at < ? THEN 1
                  WHEN urgency = 'normal' THEN 2 ELSE 3 END,
             due_at IS NULL, due_at ASC, created_at DESC, id DESC"
        }
        Some(other) => {
            return Err(ToolError::rejected(format!(
                "sort must be added, due or urgency, got {other:?}"
            )))
        }
    };
    let (wheres, params) = list_filters(ctx, &args)?;
    let goals_hidden = hides_goals(ctx);

    let total: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM tasks WHERE {wheres}"),
            rusqlite::params_from_iter(params.iter()),
            |r| r.get(0),
        )
        .map_err(internal)?;
    let mut page_params = params;
    page_params.extend(pressing_cutoff.map(SqlValue::from));
    let mut stmt = conn
        .prepare(&format!(
            "SELECT id, title, state, is_now, duration_min, created_at, updated_at, due_at,
                    (SELECT COUNT(*) FROM tasks s WHERE s.parent_id = tasks.id AND s.state != 'dropped'),
                    (SELECT COUNT(*) FROM tasks s WHERE s.parent_id = tasks.id AND s.state = 'done'),
                    progress, actual_min, category, goal_id, urgency
             FROM tasks WHERE {wheres}
             ORDER BY {order} LIMIT {limit}"
        ))
        .map_err(internal)?;
    let tasks = stmt
        .query_map(rusqlite::params_from_iter(page_params.iter()), |r| {
            let progress: u32 = r.get(10)?;
            let (expected_min, remaining_min) =
                crate::tasks::projection(progress, r.get(11)?);
            let state: String = r.get(2)?;
            let due_at: Option<String> = r.get(7)?;
            let pressing = crate::tasks::pressing_at(&state, due_at.as_deref(), now);
            Ok(serde_json::json!({
                "id": r.get::<_, i64>(0)?,
                "title": r.get::<_, String>(1)?,
                "state": state,
                "is_now": r.get::<_, bool>(3)?,
                "duration_min": r.get::<_, Option<i64>>(4)?,
                "created_at": r.get::<_, String>(5)?,
                "updated_at": r.get::<_, String>(6)?,
                "due_at": due_at,
                "steps": r.get::<_, i64>(8)?,
                "done_steps": r.get::<_, i64>(9)?,
                "progress": progress,
                "expected_min": expected_min,
                "remaining_min": remaining_min,
                "category": r.get::<_, String>(12)?,
                "goal_id": if goals_hidden { None } else { r.get::<_, Option<i64>>(13)? },
                "urgency": r.get::<_, String>(14)?,
                "pressing": pressing,
            }))
        })
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .map_err(internal)?;
    Ok(serde_json::json!({ "tasks": tasks, "total": total }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchArgs {
    /// The words to look for, 1 to 200 characters. Every word must match.
    pub query: String,
}

/// Every word of the query against the title first, then against the task's
/// whole text; live tasks before finished ones.
pub fn search(conn: &Connection, ctx: &ToolCtx, args: SearchArgs) -> Result<serde_json::Value, ToolError> {
    unscoped(ctx)?;
    let query = args.query.trim();
    if query.is_empty() || query.chars().count() > MAX_QUERY_CHARS {
        return Err(ToolError::rejected(format!(
            "query must be 1 to {MAX_QUERY_CHARS} characters"
        )));
    }
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut params: Vec<SqlValue> = vec![ctx.user_id.into()];
    let finished = match &ctx.share {
        None => "",
        Some(scope) if scope.progress => {
            params.push(crate::shares::done_since(jiff::Timestamp::now()).to_string().into());
            " AND (state != 'done' OR completed_at >= ?)"
        }
        Some(_) => " AND state != 'done'",
    };
    let clause = |column: &str, params: &mut Vec<SqlValue>| {
        params.extend(words.iter().map(|w| SqlValue::from(w.clone())));
        words
            .iter()
            .map(|_| format!("instr({column}, ?) > 0"))
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let text = if matches_details(ctx) {
        "lower(title) || ' ' || lower(description) || ' ' || lower(notes)"
    } else {
        "lower(title)"
    };
    // the parameters are pushed in the order the statement below binds them
    let text_match = clause(text, &mut params);
    let (shared, shared_params) = share_categories(ctx, None)?.unwrap_or_default();
    params.extend(shared_params);
    let title_match = clause("lower(title)", &mut params);

    let mut stmt = conn
        .prepare(&format!(
            "SELECT id, title, state, is_now, duration_min, due_at
             FROM tasks
             WHERE user_id = ? AND parent_id IS NULL AND state != 'dropped'{finished} AND ({text_match}){shared}
             ORDER BY CASE WHEN {title_match} THEN 0 ELSE 1 END,
                      CASE WHEN state = 'done' THEN 1 ELSE 0 END,
                      created_at DESC, id DESC
             LIMIT {SEARCH_HITS}"
        ))
        .map_err(internal)?;
    let tasks = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok(serde_json::json!({
                "id": r.get::<_, i64>(0)?,
                "title": r.get::<_, String>(1)?,
                "state": r.get::<_, String>(2)?,
                "is_now": r.get::<_, bool>(3)?,
                "duration_min": r.get::<_, Option<i64>>(4)?,
                "due_at": r.get::<_, Option<String>>(5)?,
            }))
        })
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .map_err(internal)?;
    Ok(serde_json::json!({ "tasks": tasks }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadArgs {
    pub task_id: i64,
}

/// One task in full, exactly as `GET /api/tasks` renders it, plus when it was
/// added.
pub fn read(conn: &Connection, ctx: &ToolCtx, args: ReadArgs) -> Result<serde_json::Value, ToolError> {
    unscoped(ctx)?;
    let Some(mut node) = crate::tasks::node(conn, ctx.user_id, args.task_id).map_err(internal)?
    else {
        return Err(ToolError::not_found(format!("no task {}", args.task_id)));
    };
    if let Some(scope) = &ctx.share {
        let finished_in_view = || {
            scope.progress
                && completed_at(conn, args.task_id)
                    .is_ok_and(|at| at.is_some_and(|at| at >= crate::shares::done_since(jiff::Timestamp::now()).to_string()))
        };
        let shared = scope.allows_category(&node.task.category)
            && match node.task.state.as_str() {
                "dropped" => false,
                "done" => finished_in_view(),
                _ => true,
            };
        if !shared {
            return Err(ToolError::not_found(format!("no task {}", args.task_id)));
        }
        if !scope.goals {
            for task in std::iter::once(&mut node.task).chain(node.children.iter_mut()) {
                task.goal_id = None;
                task.goal_title = None;
            }
        }
    }
    let tz = crate::triggers::timezone(ctx.config_dir, ctx.username);
    crate::tasks::stamp_schedule(conn, ctx.user_id, &tz, std::iter::once(&mut node.task))
        .map_err(internal)?;
    let mut out = serde_json::to_value(&node).map_err(internal)?;
    let mut stmt = conn
        .prepare("SELECT id, created_at FROM tasks WHERE id = ?1 OR parent_id = ?1")
        .map_err(internal)?;
    let added: std::collections::HashMap<i64, String> = stmt
        .query_map([args.task_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .and_then(|rows| rows.collect::<rusqlite::Result<_>>())
        .map_err(internal)?;
    stamp(&mut out, &added);
    if let Some(children) = out["children"].as_array_mut() {
        for child in children {
            stamp(child, &added);
        }
    }
    if ctx.share.as_ref().is_some_and(|scope| !scope.details) {
        titles_only(&mut out);
        if let Some(children) = out["children"].as_array_mut() {
            children.iter_mut().for_each(titles_only);
        }
    }
    Ok(out)
}

fn completed_at(conn: &Connection, id: i64) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT completed_at FROM tasks WHERE id = ?1", [id], |r| r.get(0))
}

fn titles_only(task: &mut serde_json::Value) {
    for field in ["description", "notes", "url"] {
        task[field] = serde_json::json!("");
    }
    task["external_id"] = serde_json::Value::Null;
}

fn stamp(task: &mut serde_json::Value, added: &std::collections::HashMap<i64, String>) {
    let Some(created) = task["id"].as_i64().and_then(|id| added.get(&id)) else { return };
    task["created_at"] = serde_json::json!(created);
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BulkUpdateArgs {
    /// 1 to 50 task ids. Every one must be the user's own, or nothing happens.
    pub task_ids: Vec<i64>,
    /// One of open, in_progress, done, dropped, for all of them.
    #[serde(default)]
    pub state: Option<String>,
    /// True moves them all into Now, false moves them all back to Later.
    #[serde(default)]
    pub is_now: Option<bool>,
    /// Puts them all in this category; an empty string clears it.
    #[serde(default)]
    pub category: Option<String>,
    /// Hangs them all from this goal; null detaches them.
    #[serde(default, deserialize_with = "crate::tasks::present")]
    pub goal_id: Option<Option<i64>>,
    /// Sets them all to this urgency: low, normal or high.
    #[serde(default)]
    pub urgency: Option<String>,
    /// True deletes them all, with their steps and their place on the day's plan.
    #[serde(default)]
    pub delete: Option<bool>,
}

/// One change across a batch of tasks: either every id takes it, or the call is
/// rejected and nothing moves.
pub fn bulk_update(
    conn: &Connection,
    ctx: &ToolCtx,
    args: BulkUpdateArgs,
) -> Result<serde_json::Value, ToolError> {
    unscoped(ctx)?;
    if args.task_ids.is_empty() || args.task_ids.len() > MAX_BATCH {
        return Err(ToolError::rejected(format!("task_ids must hold 1 to {MAX_BATCH} ids")));
    }
    let mut seen = std::collections::HashSet::new();
    if let Some(dup) = args.task_ids.iter().find(|id| !seen.insert(**id)) {
        return Err(ToolError::rejected(format!("task_ids names {dup} twice")));
    }
    let changes = [
        args.state.is_some(),
        args.is_now.is_some(),
        args.category.is_some(),
        args.goal_id.is_some(),
        args.urgency.is_some(),
        args.delete.is_some(),
    ]
    .iter()
    .filter(|c| **c)
    .count();
    if changes != 1 {
        return Err(ToolError::rejected(
            "set exactly one of state, is_now, category, goal_id, urgency or delete",
        ));
    }
    if args.delete == Some(false) {
        return Err(ToolError::rejected("delete only takes true; there is no undelete"));
    }
    for id in &args.task_ids {
        if crate::tasks::get(conn, ctx.user_id, *id).map_err(internal)?.is_none() {
            return Err(ToolError::not_found(format!("no task {id}; nothing was changed")));
        }
    }

    if args.delete == Some(true) {
        for id in &args.task_ids {
            // a step deleted with its parent earlier in the batch is already gone
            crate::tasks::delete_within(conn, ctx.user_id, *id).map_err(internal)?;
        }
        return Ok(serde_json::json!({ "deleted": args.task_ids }));
    }

    let mut demoted = Vec::new();
    for id in &args.task_ids {
        let patch = crate::tasks::TaskPatch {
            state: args.state.clone(),
            is_now: args.is_now,
            category: args.category.clone(),
            goal_id: args.goal_id,
            urgency: args.urgency.clone(),
            actor: crate::tasks::Actor::Agent,
            ..Default::default()
        };
        match crate::tasks::update(conn, ctx.user_id, *id, patch) {
            Ok(Some(t)) => demoted.extend(t.demoted_from_now),
            Ok(None) => return Err(ToolError::not_found(format!("no task {id}"))),
            Err(e) => return Err(task_error(e)),
        }
    }
    demoted.sort_unstable();
    demoted.dedup();
    let mut out = Vec::new();
    for id in demoted {
        let still_now: bool = conn
            .query_row("SELECT is_now FROM tasks WHERE id = ?1", [id], |r| r.get(0))
            .map_err(internal)?;
        if !still_now {
            out.push(id);
        }
    }
    Ok(serde_json::json!({ "updated": args.task_ids, "demoted_from_now": out }))
}

fn task_error(e: crate::tasks::UpdateError) -> ToolError {
    match e {
        crate::tasks::UpdateError::Db(e) => ToolError::internal(e.to_string()),
        other => ToolError::rejected(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, registry, PreparedVectors, SessionKind, ToolCtx, ToolError};
    use rusqlite::Connection;
    use serde_json::Value;

    fn env() -> (Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir, scope: Option<i64>) -> ToolCtx<'a> {
        ToolCtx {
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            user_id: 1,
            username: "aki",
            vectors: PreparedVectors::default(),
            task_scope: scope,
            inbox_source: None,
            memory_source: None,
            share: None,
            share_thread: None,
        }
    }

    fn call(
        conn: &Connection,
        tmp: &tempfile::TempDir,
        name: &str,
        args: &str,
    ) -> Result<Value, ToolError> {
        dispatch(conn, &ctx(tmp, None), SessionKind::Talk, name, args)
    }

    fn task(conn: &Connection, tmp: &tempfile::TempDir, args: &str) -> i64 {
        call(conn, tmp, "task_create", args).unwrap()["task_id"].as_i64().unwrap()
    }

    fn patch(conn: &Connection, tmp: &tempfile::TempDir, id: i64, fields: &str) {
        call(conn, tmp, "task_update", &format!(r#"{{"task_id":{id},{fields}}}"#)).unwrap();
    }

    fn ids(out: &Value, key: &str) -> Vec<i64> {
        out[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_i64().or_else(|| t.as_i64()).unwrap())
            .collect()
    }

    fn split_two(conn: &Connection, tmp: &tempfile::TempDir, id: i64) -> Vec<i64> {
        let out = call(
            conn,
            tmp,
            "task_split",
            &format!(
                r#"{{"task_id":{id},"steps":[{{"title":"find the thread","duration_min":5}},
                     {{"title":"write and send","duration_min":10}}]}}"#
            ),
        )
        .unwrap();
        out["step_ids"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect()
    }

    #[test]
    fn task_list_filters_and_sorts_by_urgency() {
        let (conn, tmp) = env();
        let mk = |title: &str, urgency: &str, due: Option<&str>| {
            let due = due.map(|d| format!(r#","due_at":"{d}""#)).unwrap_or_default();
            task(&conn, &tmp, &format!(r#"{{"title":"{title}","urgency":"{urgency}"{due}}}"#));
        };
        mk("low one", "low", None);
        mk("plain", "normal", None);
        mk("soon", "normal", Some("2026-01-02T00:00:00Z"));
        mk("top", "high", None);
        let out = call(&conn, &tmp, "task_list", r#"{"sort":"urgency"}"#).unwrap();
        let titles: Vec<&str> =
            out["tasks"].as_array().unwrap().iter().map(|t| t["title"].as_str().unwrap()).collect();
        assert_eq!(titles, vec!["top", "soon", "plain", "low one"]);
        assert_eq!(out["tasks"][0]["urgency"], "high");
        assert_eq!(out["tasks"][1]["pressing"], true);
        assert_eq!(out["tasks"][2]["pressing"], false);
        let out = call(&conn, &tmp, "task_list", r#"{"urgency":"low"}"#).unwrap();
        assert_eq!(out["total"], 1);
        let err = call(&conn, &tmp, "task_list", r#"{"urgency":"asap"}"#).unwrap_err();
        assert_eq!(err.kind, "rejected");
    }

    #[test]
    fn a_finished_task_with_a_past_due_date_is_not_ranked_pressing() {
        let (conn, tmp) = env();
        let done = task(&conn, &tmp, r#"{"title":"filed","due_at":"2026-01-02T00:00:00Z"}"#);
        patch(&conn, &tmp, done, r#""state":"done""#);
        let in_an_hour = jiff::Timestamp::now() + jiff::Span::new().hours(1);
        task(&conn, &tmp, &format!(r#"{{"title":"soon","due_at":"{in_an_hour}"}}"#));
        let out = call(&conn, &tmp, "task_list", r#"{"state":"any","sort":"urgency"}"#).unwrap();
        let titles: Vec<&str> =
            out["tasks"].as_array().unwrap().iter().map(|t| t["title"].as_str().unwrap()).collect();
        assert_eq!(titles, vec!["soon", "filed"]);
        assert_eq!(out["tasks"][0]["pressing"], true);
        assert_eq!(out["tasks"][1]["pressing"], false);
    }

    #[test]
    fn task_bulk_update_sets_urgency_on_every_task() {
        let (conn, tmp) = env();
        let a = task(&conn, &tmp, r#"{"title":"a"}"#);
        let b = task(&conn, &tmp, r#"{"title":"b"}"#);
        call(&conn, &tmp, "task_bulk_update", &format!(r#"{{"task_ids":[{a},{b}],"urgency":"high"}}"#))
            .unwrap();
        let out = call(&conn, &tmp, "task_list", r#"{"urgency":"high"}"#).unwrap();
        assert_eq!(out["total"], 2);
    }

    #[test]
    fn task_list_shows_the_live_tasks_newest_first_with_their_steps() {
        let (conn, tmp) = env();
        let dentist = task(&conn, &tmp, r#"{"title":"call dentist"}"#);
        let landlord = task(&conn, &tmp, r#"{"title":"email landlord"}"#);
        let steps = split_two(&conn, &tmp, landlord);
        patch(&conn, &tmp, steps[0], r#""state":"done""#);
        let gone = task(&conn, &tmp, r#"{"title":"an abandoned idea"}"#);
        patch(&conn, &tmp, gone, r#""state":"dropped""#);

        let out = call(&conn, &tmp, "task_list", "{}").unwrap();
        assert_eq!(ids(&out, "tasks"), vec![landlord, dentist]);
        assert_eq!(out["total"], 2);
        assert_eq!(out["tasks"][0]["steps"], 2);
        assert_eq!(out["tasks"][0]["done_steps"], 1);
        assert_eq!(out["tasks"][0]["duration_min"], 15);
        assert_eq!(out["tasks"][0]["state"], "open");
        assert_eq!(out["tasks"][0]["is_now"], false);
        assert_eq!(out["tasks"][1]["steps"], 0);
        assert!(out["tasks"][0]["created_at"].is_string());
        assert!(out["tasks"][0]["updated_at"].is_string());
    }

    #[test]
    fn task_list_filters_by_state_keyword_age_and_now() {
        let (conn, tmp) = env();
        let dentist =
            task(&conn, &tmp, r#"{"title":"call dentist","description":"about the MOLAR","is_now":true}"#);
        let landlord = task(&conn, &tmp, r#"{"title":"email landlord"}"#);
        patch(&conn, &tmp, landlord, r#""notes":"the broken window again""#);
        let taxes = task(&conn, &tmp, r#"{"title":"tax return"}"#);
        conn.execute("UPDATE tasks SET created_at = '2026-01-01T00:00:00Z' WHERE id = ?1", [taxes])
            .unwrap();
        patch(&conn, &tmp, taxes, r#""state":"done""#);

        let list = |args: &str| ids(&call(&conn, &tmp, "task_list", args).unwrap(), "tasks");
        assert_eq!(list(r#"{"state":"done"}"#), vec![taxes]);
        assert_eq!(list(r#"{"state":"any"}"#), vec![landlord, dentist, taxes]);
        assert_eq!(list(r#"{"keyword":"molar"}"#), vec![dentist]);
        assert_eq!(list(r#"{"keyword":"BROKEN window"}"#), vec![landlord]);
        assert_eq!(list(r#"{"older_than_days":30,"state":"any"}"#), vec![taxes]);
        assert_eq!(list(r#"{"added_before":"2026-06-01","state":"any"}"#), vec![taxes]);
        assert_eq!(list(r#"{"added_after":"2026-06-01","state":"any"}"#), vec![landlord, dentist]);
        assert_eq!(list(r#"{"is_now":true}"#), vec![dentist]);
        assert_eq!(list(r#"{"is_now":false}"#), vec![landlord]);
    }

    #[test]
    fn task_list_filters_and_sorts_by_due_date() {
        let (conn, tmp) = env();
        let tomorrow = jiff::Timestamp::now()
            .to_zoned(jiff::tz::TimeZone::UTC)
            .date()
            .tomorrow()
            .unwrap();
        let midnight = tomorrow.to_zoned(jiff::tz::TimeZone::UTC).unwrap().timestamp();
        let yesterday = (jiff::Timestamp::now() - jiff::Span::new().hours(30)).to_string();
        let soon = (midnight - jiff::Span::new().seconds(1)).to_string();
        let later = (midnight + jiff::Span::new().hours(24 * 10)).to_string();
        let late = task(&conn, &tmp, &format!(r#"{{"title":"late","due_at":"{yesterday}"}}"#));
        let today = task(&conn, &tmp, &format!(r#"{{"title":"today","due_at":"{soon}"}}"#));
        let far = task(&conn, &tmp, &format!(r#"{{"title":"far","due_at":"{later}"}}"#));
        let undated = task(&conn, &tmp, r#"{"title":"undated"}"#);

        let list = |args: &str| ids(&call(&conn, &tmp, "task_list", args).unwrap(), "tasks");
        assert_eq!(list("{}"), vec![undated, far, today, late], "added order is untouched");
        assert_eq!(list(r#"{"sort":"due"}"#), vec![late, today, far, undated], "nulls last");
        assert_eq!(list(r#"{"overdue":true}"#), vec![late]);
        assert_eq!(list(r#"{"overdue":false}"#), vec![undated, far, today]);

        assert_eq!(list(&format!(r#"{{"due_before":"{tomorrow}"}}"#)), vec![today, late]);
        assert_eq!(list(&format!(r#"{{"due_after":"{tomorrow}"}}"#)), vec![far]);

        let out = call(&conn, &tmp, "task_list", r#"{"sort":"due"}"#).unwrap();
        assert_eq!(out["tasks"][0]["due_at"], yesterday);
        assert!(out["tasks"][3]["due_at"].is_null());
    }

    #[test]
    fn a_bad_sort_or_filter_is_rejected_rather_than_ignored() {
        let (conn, tmp) = env();
        task(&conn, &tmp, r#"{"title":"x"}"#);
        for args in [r#"{"sort":"soonest"}"#, r#"{"due_before":"friday"}"#, r#"{"due_after":"2026-13-01"}"#] {
            assert_eq!(call(&conn, &tmp, "task_list", args).unwrap_err().kind, "rejected", "{args}");
        }
    }

    #[test]
    fn a_task_read_and_a_search_carry_where_the_task_came_from() {
        let (conn, tmp) = env();
        let id = task(&conn, &tmp, r#"{"title":"Biology ch.4","due_at":"2026-09-19T14:59:00Z"}"#);
        conn.execute(
            "UPDATE tasks SET external_id = 'canvas:12', url = 'https://canvas.example/a/12',
                              source = 'import' WHERE id = ?1",
            [id],
        )
        .unwrap();

        let out = call(&conn, &tmp, "task_read", &format!(r#"{{"task_id":{id}}}"#)).unwrap();
        assert_eq!(out["due_at"], "2026-09-19T14:59:00Z");
        assert_eq!(out["external_id"], "canvas:12");
        assert_eq!(out["url"], "https://canvas.example/a/12");
        assert_eq!(out["source"], "import");

        let hits = call(&conn, &tmp, "task_search", r#"{"query":"biology"}"#).unwrap();
        assert_eq!(hits["tasks"][0]["due_at"], "2026-09-19T14:59:00Z");
    }

    #[test]
    fn task_list_bounds_its_page_and_reports_the_whole_count() {
        let (conn, tmp) = env();
        for title in ["a", "b", "c", "d"] {
            task(&conn, &tmp, &format!(r#"{{"title":"{title}"}}"#));
        }
        let out = call(&conn, &tmp, "task_list", r#"{"limit":2}"#).unwrap();
        assert_eq!(out["tasks"].as_array().unwrap().len(), 2);
        assert_eq!(out["total"], 4);

        for (args, why) in [
            (r#"{"limit":0}"#, "no page at all"),
            (r#"{"limit":201}"#, "past the ceiling"),
            (r#"{"state":"maybe"}"#, "not a state"),
            (r#"{"keyword":"   "}"#, "blank keyword"),
            (r#"{"added_after":"last tuesday"}"#, "not a date"),
            (r#"{"added_before":"2026-13-01"}"#, "not a date"),
        ] {
            assert_eq!(
                call(&conn, &tmp, "task_list", args).unwrap_err().kind,
                "rejected",
                "{why}"
            );
        }
        let e = call(&conn, &tmp, "task_list", r#"{"surprise":1}"#).unwrap_err();
        assert_eq!(e.kind, "invalid_args");
    }

    #[test]
    fn task_list_leaves_steps_out() {
        let (conn, tmp) = env();
        let landlord = task(&conn, &tmp, r#"{"title":"email landlord"}"#);
        split_two(&conn, &tmp, landlord);
        let out = call(&conn, &tmp, "task_list", r#"{"state":"any"}"#).unwrap();
        assert_eq!(ids(&out, "tasks"), vec![landlord]);
    }

    #[test]
    fn task_search_puts_title_matches_first_and_finished_ones_last() {
        let (conn, tmp) = env();
        let by_title = task(&conn, &tmp, r#"{"title":"email the landlord"}"#);
        let by_body = task(
            &conn,
            &tmp,
            r#"{"title":"tuesday admin","description":"email the landlord about the window"}"#,
        );
        let finished = task(&conn, &tmp, r#"{"title":"email the landlord again"}"#);
        patch(&conn, &tmp, finished, r#""state":"done""#);
        let abandoned = task(&conn, &tmp, r#"{"title":"email the landlord once"}"#);
        patch(&conn, &tmp, abandoned, r#""state":"dropped""#);
        task(&conn, &tmp, r#"{"title":"call dentist"}"#);

        let out = call(&conn, &tmp, "task_search", r#"{"query":"LANDLORD  email"}"#).unwrap();
        assert_eq!(ids(&out, "tasks"), vec![by_title, finished, by_body]);
        assert_eq!(out["tasks"][0]["title"], "email the landlord");
        assert_eq!(out["tasks"][0]["state"], "open");
        assert_eq!(out["tasks"][0]["is_now"], false);
        assert!(out["tasks"][0].get("duration_min").is_some());

        let out = call(&conn, &tmp, "task_search", r#"{"query":"landlord dentist"}"#).unwrap();
        assert!(out["tasks"].as_array().unwrap().is_empty(), "every word must match");
    }

    #[test]
    fn task_search_needs_a_query_of_its_own() {
        let (conn, tmp) = env();
        let long = format!(r#"{{"query":"{}"}}"#, "x".repeat(201));
        for args in [r#"{"query":"   "}"#, &long] {
            assert_eq!(call(&conn, &tmp, "task_search", args).unwrap_err().kind, "rejected");
        }
    }

    #[test]
    fn task_read_returns_the_whole_task_with_its_steps() {
        let (conn, tmp) = env();
        let landlord =
            task(&conn, &tmp, r#"{"title":"email landlord","description":"about the window"}"#);
        patch(&conn, &tmp, landlord, r#""notes":"his number is on the fridge","is_now":true"#);
        let steps = split_two(&conn, &tmp, landlord);
        patch(&conn, &tmp, steps[0], r#""state":"done""#);

        let out = call(&conn, &tmp, "task_read", &format!(r#"{{"task_id":{landlord}}}"#)).unwrap();
        assert_eq!(out["id"], landlord);
        assert_eq!(out["title"], "email landlord");
        assert_eq!(out["description"], "about the window");
        assert_eq!(out["notes"], "his number is on the fridge");
        assert_eq!(out["state"], "open");
        assert_eq!(out["source"], "agent");
        assert_eq!(out["duration_min"], 15);
        assert_eq!(out["is_now"], true);
        assert!(out["parent_id"].is_null());
        assert!(out["created_at"].is_string());
        assert!(out["updated_at"].is_string());
        assert_eq!(out["children"].as_array().unwrap().len(), 2);
        assert_eq!(out["children"][0]["state"], "done");
        assert_eq!(out["children"][0]["duration_min"], 5);
        assert_eq!(out["children"][0]["parent_id"], landlord);
    }

    #[test]
    fn task_read_does_not_reach_another_users_task() {
        let (conn, tmp) = env();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('rin', 'x', 'member')",
            [],
        )
        .unwrap();
        let mut theirs = ctx(&tmp, None);
        theirs.user_id = 2;
        theirs.username = "rin";
        let id = dispatch(&conn, &theirs, SessionKind::Talk, "task_create", r#"{"title":"theirs"}"#)
            .unwrap()["task_id"]
            .as_i64()
            .unwrap();

        for target in [id, 9999] {
            let e = call(&conn, &tmp, "task_read", &format!(r#"{{"task_id":{target}}}"#))
                .unwrap_err();
            assert_eq!(e.kind, "not_found", "reached {target}");
        }
    }

    #[test]
    fn task_bulk_update_sets_one_state_on_many_tasks() {
        let (conn, tmp) = env();
        let a = task(&conn, &tmp, r#"{"title":"a"}"#);
        let b = task(&conn, &tmp, r#"{"title":"b"}"#);
        let out = call(
            &conn,
            &tmp,
            "task_bulk_update",
            &format!(r#"{{"task_ids":[{a},{b}],"state":"done"}}"#),
        )
        .unwrap();
        assert_eq!(ids(&out, "updated"), vec![a, b]);
        assert!(out["demoted_from_now"].as_array().unwrap().is_empty());
        for id in [a, b] {
            assert_eq!(crate::tasks::get(&conn, 1, id).unwrap().unwrap().state, "done");
        }
    }

    #[test]
    fn task_bulk_update_fills_now_and_reports_who_stepped_aside() {
        let (conn, tmp) = env();
        let all: Vec<i64> = ["a", "b", "c", "d"]
            .iter()
            .map(|t| task(&conn, &tmp, &format!(r#"{{"title":"{t}"}}"#)))
            .collect();
        let list = all.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        let out = call(
            &conn,
            &tmp,
            "task_bulk_update",
            &format!(r#"{{"task_ids":[{list}],"is_now":true}}"#),
        )
        .unwrap();
        assert_eq!(ids(&out, "updated"), all);
        assert_eq!(ids(&out, "demoted_from_now"), vec![all[2]]);
        let now: Vec<i64> = {
            let mut stmt = conn
                .prepare("SELECT id FROM tasks WHERE is_now = 1 ORDER BY id")
                .unwrap();
            stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(now, vec![all[0], all[1], all[3]]);
    }

    #[test]
    fn task_bulk_update_deletes_the_whole_batch() {
        let (conn, tmp) = env();
        let a = task(&conn, &tmp, r#"{"title":"a"}"#);
        let b = task(&conn, &tmp, r#"{"title":"b"}"#);
        split_two(&conn, &tmp, b);
        let keep = task(&conn, &tmp, r#"{"title":"keep me"}"#);

        let out = call(
            &conn,
            &tmp,
            "task_bulk_update",
            &format!(r#"{{"task_ids":[{a},{b}],"delete":true}}"#),
        )
        .unwrap();
        assert_eq!(ids(&out, "deleted"), vec![a, b]);
        let left: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 1);
        assert!(crate::tasks::get(&conn, 1, keep).unwrap().is_some());
    }

    #[test]
    fn task_bulk_update_needs_exactly_one_change_and_a_sane_batch() {
        let (conn, tmp) = env();
        let a = task(&conn, &tmp, r#"{"title":"a"}"#);
        let fifty_one = (0..51).map(|_| a.to_string()).collect::<Vec<_>>().join(",");
        for (args, why) in [
            (format!(r#"{{"task_ids":[{a}]}}"#), "nothing to change"),
            (format!(r#"{{"task_ids":[{a}],"state":"done","is_now":true}}"#), "two changes"),
            (format!(r#"{{"task_ids":[{a}],"delete":false}}"#), "delete only takes true"),
            (r#"{"task_ids":[],"state":"done"}"#.into(), "no tasks"),
            (format!(r#"{{"task_ids":[{fifty_one}],"state":"done"}}"#), "past the batch cap"),
            (format!(r#"{{"task_ids":[{a},{a}],"state":"done"}}"#), "the same id twice"),
            (format!(r#"{{"task_ids":[{a}],"state":"exploded"}}"#), "not a state"),
        ] {
            assert_eq!(
                call(&conn, &tmp, "task_bulk_update", &args).unwrap_err().kind,
                "rejected",
                "{why}"
            );
        }
        assert_eq!(crate::tasks::get(&conn, 1, a).unwrap().unwrap().state, "open");
    }

    #[test]
    fn one_bad_id_rejects_the_whole_batch() {
        let (conn, tmp) = env();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('rin', 'x', 'member')",
            [],
        )
        .unwrap();
        let mut theirs = ctx(&tmp, None);
        theirs.user_id = 2;
        theirs.username = "rin";
        let foreign =
            dispatch(&conn, &theirs, SessionKind::Talk, "task_create", r#"{"title":"theirs"}"#)
                .unwrap()["task_id"]
                .as_i64()
                .unwrap();
        let mine = task(&conn, &tmp, r#"{"title":"mine"}"#);

        for other in [foreign, 9999] {
            for change in [r#""state":"done""#, r#""delete":true"#] {
                let e = call(
                    &conn,
                    &tmp,
                    "task_bulk_update",
                    &format!(r#"{{"task_ids":[{mine},{other}],{change}}}"#),
                )
                .unwrap_err();
                assert_eq!(e.kind, "not_found", "{other} {change}");
                assert!(e.message.contains(&other.to_string()), "the id is named: {}", e.message);
            }
        }
        let mine_now = crate::tasks::get(&conn, 1, mine).unwrap().unwrap();
        assert_eq!(mine_now.state, "open", "a rejected batch changes nothing");
        assert!(crate::tasks::get(&conn, 2, foreign).unwrap().is_some());
    }

    #[test]
    fn task_bulk_update_never_puts_a_step_in_now() {
        let (conn, tmp) = env();
        let landlord = task(&conn, &tmp, r#"{"title":"email landlord"}"#);
        let steps = split_two(&conn, &tmp, landlord);
        let e = call(
            &conn,
            &tmp,
            "task_bulk_update",
            &format!(r#"{{"task_ids":[{landlord},{}],"is_now":true}}"#, steps[0]),
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");
        let flag: i64 = conn
            .query_row("SELECT is_now FROM tasks WHERE id = ?1", [landlord], |r| r.get(0))
            .unwrap();
        assert_eq!(flag, 0, "a rejected batch changes nothing");
    }

    #[test]
    fn the_survey_tools_are_closed_to_a_scoped_session() {
        let (conn, tmp) = env();
        let id = task(&conn, &tmp, r#"{"title":"biology ch.4"}"#);
        for (name, args) in [
            ("task_list", "{}".to_string()),
            ("task_search", r#"{"query":"biology"}"#.to_string()),
            ("task_read", format!(r#"{{"task_id":{id}}}"#)),
            ("task_bulk_update", format!(r#"{{"task_ids":[{id}],"state":"done"}}"#)),
        ] {
            let e = dispatch(&conn, &ctx(&tmp, Some(id)), SessionKind::Talk, name, &args)
                .unwrap_err();
            assert_eq!(e.kind, "rejected", "{name} answered a scoped session");
        }
    }

    #[test]
    fn the_survey_tools_sit_in_the_right_registries() {
        for name in ["task_list", "task_search", "task_read"] {
            for kind in [SessionKind::Checkin, SessionKind::Talk, SessionKind::Nightly] {
                assert!(registry(kind).contains(&name), "{name} missing from {kind:?}");
            }
        }
        assert!(!registry(SessionKind::Checkin).contains(&"task_bulk_update"));
        for kind in [SessionKind::Talk, SessionKind::Nightly] {
            assert!(registry(kind).contains(&"task_bulk_update"));
        }
        for name in ["task_list", "task_search", "task_read", "task_bulk_update"] {
            assert!(!registry(SessionKind::Import).contains(&name), "{name} reached import");
        }

        let (conn, tmp) = env();
        let e = dispatch(
            &conn,
            &ctx(&tmp, None),
            SessionKind::Checkin,
            "task_bulk_update",
            r#"{"task_ids":[1],"state":"done"}"#,
        )
        .unwrap_err();
        assert_eq!(e.kind, "forbidden");
    }

    fn share_ctx(tmp: &tempfile::TempDir, scope: crate::shares::ShareScope) -> ToolCtx<'_> {
        ToolCtx { share: Some(scope), ..ctx(tmp, None) }
    }

    fn out_id(v: &Value) -> i64 {
        v["tasks"][0]["id"].as_i64().unwrap()
    }

    fn school_only() -> crate::shares::ShareScope {
        crate::shares::ShareScope { categories: vec!["school".into()], ..Default::default() }
    }

    #[test]
    fn a_share_scope_confines_the_task_tools_to_its_categories() {
        let (conn, tmp) = env();
        call(&conn, &tmp, "task_create", r#"{"title":"lab report","category":"school","description":"secret grade talk"}"#).unwrap();
        call(&conn, &tmp, "task_create", r#"{"title":"therapy forms","category":"health"}"#).unwrap();
        let sctx = share_ctx(&tmp, school_only());
        let out = dispatch(&conn, &sctx, SessionKind::Share, "task_list", "{}").unwrap();
        assert_eq!(out["total"], 1);
        assert_eq!(out["tasks"][0]["title"], "lab report");
        let err = dispatch(&conn, &sctx, SessionKind::Share, "task_list", r#"{"category":"health"}"#).unwrap_err();
        assert_eq!(err.kind, "rejected");
        let out = dispatch(&conn, &sctx, SessionKind::Share, "task_search", r#"{"query":"forms"}"#).unwrap();
        assert_eq!(out["tasks"].as_array().unwrap().len(), 0);
        let out = dispatch(&conn, &sctx, SessionKind::Share, "task_search", r#"{"query":"lab"}"#).unwrap();
        assert_eq!(out["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(out["tasks"][0]["title"], "lab report");
        let id = out_id(&call(&conn, &tmp, "task_list", r#"{"category":"health"}"#).unwrap());
        let err = dispatch(&conn, &sctx, SessionKind::Share, "task_read", &format!(r#"{{"task_id":{id}}}"#)).unwrap_err();
        assert_eq!(err.kind, "not_found");
        let school = out_id(&call(&conn, &tmp, "task_list", r#"{"category":"school"}"#).unwrap());
        conn.execute(
            "UPDATE tasks SET url = 'https://lms.example/lab', external_id = 'lms-7' WHERE id = ?1",
            [school],
        )
        .unwrap();
        let read = dispatch(&conn, &sctx, SessionKind::Share, "task_read", &format!(r#"{{"task_id":{school}}}"#)).unwrap();
        assert_eq!(read["description"], "", "details are off");
        assert_eq!(read["url"], "");
        assert!(read["external_id"].is_null());
        let detailed = share_ctx(&tmp, crate::shares::ShareScope { details: true, ..school_only() });
        let read = dispatch(&conn, &detailed, SessionKind::Share, "task_read", &format!(r#"{{"task_id":{school}}}"#)).unwrap();
        assert_eq!(read["description"], "secret grade talk");
        assert_eq!(read["url"], "https://lms.example/lab");
        assert_eq!(read["external_id"], "lms-7");
    }

    #[test]
    fn without_details_a_share_matches_titles_alone() {
        let (conn, tmp) = env();
        call(&conn, &tmp, "task_create", r#"{"title":"lab report","description":"secret grade talk"}"#).unwrap();
        let plain = share_ctx(&tmp, crate::shares::ShareScope::default());
        let detailed = share_ctx(&tmp, crate::shares::ShareScope { details: true, ..Default::default() });
        for (sctx, hits) in [(&plain, 0), (&detailed, 1)] {
            let out = dispatch(&conn, sctx, SessionKind::Share, "task_list", r#"{"keyword":"grade"}"#).unwrap();
            assert_eq!(out["total"], hits);
            let out = dispatch(&conn, sctx, SessionKind::Share, "task_search", r#"{"query":"grade"}"#).unwrap();
            assert_eq!(out["tasks"].as_array().unwrap().len(), hits);
        }
        let out = dispatch(&conn, &plain, SessionKind::Share, "task_list", r#"{"keyword":"lab"}"#).unwrap();
        assert_eq!(out["total"], 1);
    }

    #[test]
    fn a_share_reaches_finished_tasks_only_with_progress_and_only_recent_ones() {
        let (conn, tmp) = env();
        let recent = task(&conn, &tmp, r#"{"title":"essay draft"}"#);
        let old = task(&conn, &tmp, r#"{"title":"essay outline"}"#);
        let dropped = task(&conn, &tmp, r#"{"title":"essay idea"}"#);
        patch(&conn, &tmp, recent, r#""state":"done""#);
        patch(&conn, &tmp, old, r#""state":"done""#);
        patch(&conn, &tmp, dropped, r#""state":"dropped""#);
        let at = |days: i64| (jiff::Timestamp::now() - jiff::Span::new().hours(24 * days)).to_string();
        conn.execute("UPDATE tasks SET completed_at = ?1 WHERE id = ?2", (at(1), recent)).unwrap();
        conn.execute("UPDATE tasks SET completed_at = ?1 WHERE id = ?2", (at(10), old)).unwrap();
        let read = |sctx: &ToolCtx, id: i64| {
            dispatch(&conn, sctx, SessionKind::Share, "task_read", &format!(r#"{{"task_id":{id}}}"#))
        };

        let closed = share_ctx(&tmp, crate::shares::ShareScope { progress: false, ..Default::default() });
        for state in ["done", "any", "dropped"] {
            let err = dispatch(&conn, &closed, SessionKind::Share, "task_list", &format!(r#"{{"state":"{state}"}}"#))
                .unwrap_err();
            assert_eq!(err.kind, "rejected", "{state}");
        }
        assert_eq!(read(&closed, recent).unwrap_err().kind, "not_found");
        let out = dispatch(&conn, &closed, SessionKind::Share, "task_search", r#"{"query":"essay"}"#).unwrap();
        assert!(out["tasks"].as_array().unwrap().is_empty(), "{out}");

        let open = share_ctx(&tmp, crate::shares::ShareScope::default());
        let out = dispatch(&conn, &open, SessionKind::Share, "task_list", r#"{"state":"done"}"#).unwrap();
        assert_eq!(ids(&out, "tasks"), vec![recent]);
        for state in ["any", "dropped"] {
            let err = dispatch(&conn, &open, SessionKind::Share, "task_list", &format!(r#"{{"state":"{state}"}}"#))
                .unwrap_err();
            assert_eq!(err.kind, "rejected", "{state}");
        }
        assert_eq!(read(&open, recent).unwrap()["id"], recent);
        assert_eq!(read(&open, old).unwrap_err().kind, "not_found");
        assert_eq!(read(&open, dropped).unwrap_err().kind, "not_found");
        let out = dispatch(&conn, &open, SessionKind::Share, "task_search", r#"{"query":"essay"}"#).unwrap();
        assert_eq!(ids(&out, "tasks"), vec![recent]);
    }

    #[test]
    fn a_share_without_goals_leaves_the_goal_off_its_tasks() {
        let (conn, tmp) = env();
        let goal = call(&conn, &tmp, "goal_create", r#"{"title":"GOAL-TITLE-SECRET"}"#).unwrap()["goal_id"]
            .as_i64()
            .unwrap();
        let id = task(&conn, &tmp, &format!(r#"{{"title":"essay","goal_id":{goal}}}"#));
        let hidden = share_ctx(&tmp, crate::shares::ShareScope { goals: false, ..Default::default() });
        let shown = share_ctx(&tmp, crate::shares::ShareScope::default());
        for (sctx, visible) in [(&hidden, false), (&shown, true)] {
            let list = dispatch(&conn, sctx, SessionKind::Share, "task_list", "{}").unwrap();
            assert_eq!(!list["tasks"][0]["goal_id"].is_null(), visible);
            let read = dispatch(&conn, sctx, SessionKind::Share, "task_read", &format!(r#"{{"task_id":{id}}}"#)).unwrap();
            assert_eq!(!read["goal_id"].is_null(), visible);
            assert_eq!(read.to_string().contains("GOAL-TITLE-SECRET"), visible, "{read}");
        }
    }
}
