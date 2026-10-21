use super::loose::{self, Obj, Target};
use super::{ToolCtx, ToolError};
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use serde_json::{json, Value};

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;
const MAX_QUERY_CHARS: usize = 200;
const SEARCH_HITS: i64 = 20;
const MAX_AGE_DAYS: i64 = 3650;

/// A session pinned to one task has no business surveying the whole list.
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

/// The soonest deadline still open anywhere in a root's tree, else the root's own.
const NEXT_DUE: &str = "COALESCE((SELECT MIN(d.due_at) FROM tasks d WHERE d.root_id = tasks.id
    AND d.state IN ('open','in_progress') AND d.due_at IS NOT NULL), tasks.due_at)";

fn hides_goals(ctx: &ToolCtx) -> bool {
    ctx.share.as_ref().is_some_and(|scope| !scope.goals)
}

fn matches_details(ctx: &ToolCtx) -> bool {
    ctx.share.as_ref().is_none_or(|scope| scope.details)
}

/// The category clause a share imposes, read off each node's root.
fn share_categories(ctx: &ToolCtx, requested: Option<&str>, column: &str) -> Result<Option<(String, Vec<SqlValue>)>, ToolError> {
    let Some(scope) = &ctx.share else { return Ok(None) };
    match requested {
        Some(c) if !scope.allows_category(c.trim()) => Err(ToolError::rejected(format!(
            "category must be one of {}",
            scope.categories.join(", ")
        ))),
        Some(_) => Ok(None),
        None => Ok(scope.category_clause(column)),
    }
}

fn tz(ctx: &ToolCtx) -> jiff::tz::TimeZone {
    crate::triggers::timezone(ctx.config_dir, ctx.username)
}

fn today(ctx: &ToolCtx) -> jiff::civil::Date {
    jiff::Timestamp::now().to_zoned(tz(ctx)).date()
}

fn day_start(ctx: &ToolCtx, day: jiff::civil::Date) -> Result<String, ToolError> {
    Ok(day.to_zoned(tz(ctx)).map_err(internal)?.timestamp().to_string())
}

/// What the filters read: the canonical fields plus the old list's own.
#[derive(Default)]
struct Filters {
    state: Option<String>,
    query: Option<String>,
    category: Option<String>,
    goal_id: Option<i64>,
    now: Option<bool>,
    due_before: Option<jiff::civil::Date>,
    due_after: Option<jiff::civil::Date>,
    overdue: Option<bool>,
    urgency: Option<&'static str>,
    sort: &'static str,
    sort_given: bool,
    depth: usize,
    depth_given: bool,
    limit: i64,
    added_after: Option<jiff::civil::Date>,
    added_before: Option<jiff::civil::Date>,
    older_than_days: Option<i64>,
}

fn read_filters(ctx: &ToolCtx, o: &mut Obj, warnings: &mut Vec<String>) -> Filters {
    let today = today(ctx);
    let mut day_of = |o: &mut Obj, names: &[&str]| {
        let v = o.take(names)?;
        let raw = loose::text(&v).unwrap_or_default();
        let d = loose::day(&raw, today);
        if d.is_none() {
            warnings.push(format!("{} {raw:?} was ignored: a day is {}", names[0], loose::DAY_FORMS));
        }
        d
    };
    let mut due_before = day_of(o, &["due_before", "before"]);
    let mut due_after = day_of(o, &["due_after", "after"]);
    if let Some(d) = day_of(o, &["due", "due_on", "due_date"]) {
        due_after = due_after.max(Some(d));
        due_before = d.tomorrow().ok().map(|next| due_before.map_or(next, |b| b.min(next)));
    }
    let added_after = day_of(o, &["added_after"]);
    let added_before = day_of(o, &["added_before"]);
    let state = o.take(&["state", "status"]).and_then(|v| loose::text(&v)).and_then(|s| {
        if s.trim().eq_ignore_ascii_case("any") || s.trim().eq_ignore_ascii_case("all") {
            return Some("any".to_string());
        }
        let state = loose::choice(&s, crate::tasks::STATES, loose::STATE_SYNONYMS);
        if state.is_none() {
            warnings.push(format!("state {s:?} was ignored: it is open, in_progress, done, dropped or any"));
        }
        state.map(str::to_string)
    });
    let urgency = o.take(&["urgency", "priority"]).and_then(|v| {
        let raw = loose::text(&v).unwrap_or_default();
        let u = loose::choice(&raw, crate::tasks::URGENCY, loose::URGENCY_SYNONYMS);
        if u.is_none() {
            warnings.push(format!("urgency {raw:?} was ignored: it is low, normal or high"));
        }
        u
    });
    let sort_raw = o.take(&["sort", "order", "sort_by"]).and_then(|v| loose::text(&v));
    let sort_given = sort_raw.as_deref().is_some_and(|s| !s.trim().is_empty());
    let sort = match sort_raw.as_deref().map(str::trim) {
        Some("due" | "due_at" | "deadline" | "soonest") => "due",
        Some("urgency" | "priority" | "urgent") => "urgency",
        Some("added" | "created" | "newest" | "") | None => "added",
        Some(other) => {
            warnings.push(format!("sort {other:?} was ignored: it is added, due or urgency"));
            "added"
        }
    };
    let limit = o.take(&["limit", "max", "count"]).and_then(|v| loose::integer(&v)).unwrap_or(DEFAULT_LIMIT);
    let depth = o.take(&["depth", "levels"]).and_then(|v| loose::integer(&v));
    let depth_given = depth.is_some();
    let depth = depth.unwrap_or(1);
    Filters {
        state,
        query: o.take(&["query", "keyword", "q", "search", "text", "words"]).and_then(|v| loose::text(&v)),
        category: o.take(&["category"]).and_then(|v| loose::text(&v)),
        goal_id: o.take(&["goal_id", "goal"]).and_then(|v| loose::id(&v)),
        now: o.take(&["now", "is_now"]).and_then(|v| loose::boolean(&v)),
        due_before,
        due_after,
        overdue: o.take(&["overdue"]).and_then(|v| loose::boolean(&v)),
        urgency,
        sort,
        sort_given,
        depth: depth.clamp(0, crate::tasks::MAX_DEPTH) as usize,
        depth_given,
        limit: limit.clamp(1, MAX_LIMIT),
        added_after,
        added_before,
        older_than_days: o.take(&["older_than_days"]).and_then(|v| loose::integer(&v)).map(|d| d.clamp(0, MAX_AGE_DAYS)),
    }
}

impl Filters {
    /// Asks for nodes by their own deadline, at any depth.
    fn by_due(&self) -> bool {
        self.due_before.is_some() || self.due_after.is_some() || self.overdue == Some(true)
    }
}

/// `task_find`: one node in full by id, a word search at any depth, the nodes
/// due in a window at any depth, or a survey of the top-level tasks.
pub fn find(conn: &Connection, ctx: &ToolCtx, raw: &str) -> Result<Value, ToolError> {
    unscoped(ctx)?;
    let mut o = Obj::new(loose::args(raw)?);
    let mut warnings = Vec::new();
    let ids = o.take(&["id", "ids", "task_id", "task_ids"]).map(loose::list).unwrap_or_default();
    let mut f = read_filters(ctx, &mut o, &mut warnings);
    for field in o.rest() {
        warnings.push(format!("{field} was ignored: not a task_find field"));
    }
    let targets: Vec<Target> = ids.iter().filter_map(loose::target).collect();
    let mut out = if targets.is_empty() {
        match f.query.take() {
            Some(q) => {
                for (given, field) in [(f.sort_given, "sort"), (f.depth_given, "depth")] {
                    if given {
                        warnings.push(format!("{field} was ignored: a query lists matching nodes, best title match first"));
                    }
                }
                search(conn, ctx, &q, &f, f.limit.min(SEARCH_HITS), &mut warnings)?
            }
            None if f.by_due() => due_nodes(conn, ctx, &f, &mut warnings)?,
            None => survey(conn, ctx, &f, &mut warnings)?,
        }
    } else {
        by_ids(conn, ctx, &targets, &mut warnings)?
    };
    if !warnings.is_empty() {
        out["warnings"] = warnings.into();
    }
    Ok(out)
}

fn by_ids(conn: &Connection, ctx: &ToolCtx, targets: &[Target], warnings: &mut Vec<String>) -> Result<Value, ToolError> {
    let mut items = Vec::new();
    for t in targets {
        match t {
            Target::Id(id) => match read_one(conn, ctx, *id)? {
                Some(v) => items.push(v),
                None => warnings.push(format!("no task {id}")),
            },
            Target::Title(title) => {
                let hits = search(conn, ctx, title, &Filters::default(), SEARCH_HITS, warnings)?;
                let found: Vec<i64> = hits["items"].as_array().into_iter().flatten().filter_map(|h| h["id"].as_i64()).collect();
                let exact: Vec<i64> = hits["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|h| h["title"].as_str().is_some_and(|x| loose::fold(x) == loose::fold(title)))
                    .filter_map(|h| h["id"].as_i64())
                    .collect();
                match (exact.as_slice(), found.as_slice()) {
                    ([one], _) | ([], [one]) => items.extend(read_one(conn, ctx, *one)?),
                    _ => items.extend(hits["items"].as_array().cloned().unwrap_or_default()),
                }
            }
        }
    }
    if items.len() == 1 && targets.len() == 1 {
        return Ok(json!({ "item": items.pop() }));
    }
    Ok(json!({ "total": items.len(), "items": items }))
}

fn compact(v: &mut Value) {
    if let Value::Object(o) = v {
        o.retain(|_, x| !(x.is_null() || x.as_str() == Some("") || x.as_array().is_some_and(Vec::is_empty)));
        if let Some(Value::Array(children)) = o.get_mut("children") {
            children.iter_mut().for_each(compact);
        }
    }
}

/// One node in full with its subtree, as `GET /api/tasks` renders it less its
/// bookkeeping, with its local due and its path from its root.
fn read_one(conn: &Connection, ctx: &ToolCtx, id: i64) -> Result<Option<Value>, ToolError> {
    let Some(mut node) = crate::tasks::node(conn, ctx.user_id, id).map_err(internal)? else { return Ok(None) };
    if let Some(scope) = &ctx.share {
        let finished_in_view = || {
            scope.progress
                && completed_at(conn, id)
                    .is_ok_and(|at| at.is_some_and(|at| at >= crate::shares::done_since(jiff::Timestamp::now()).to_string()))
        };
        let shared = scope.allows_category(&node.task.category)
            && match node.task.state.as_str() {
                "dropped" => false,
                "done" => finished_in_view(),
                _ => true,
            };
        if !shared {
            return Ok(None);
        }
    }
    let tz = tz(ctx);
    crate::tasks::stamp_schedule(conn, ctx.user_id, &tz, std::iter::once(&mut node.task)).map_err(internal)?;
    let path = path_of(conn, ctx.user_id, node.task.id)?;
    let mut out = serde_json::to_value(&node).map_err(internal)?;
    let hide_goals = hides_goals(ctx);
    let titles = !matches_details(ctx);
    each_node(&mut out, &mut |t| {
        if let Some(label) = t["effective_due_at"]
            .as_str()
            .and_then(|at| crate::due::local_label(at, t["effective_due_on"].as_str(), &tz))
        {
            t["due"] = label.into();
        }
        if hide_goals {
            t["goal_id"] = Value::Null;
            t["goal_title"] = Value::Null;
        }
        if titles {
            for field in ["description", "notes", "url", "external_id"] {
                t[field] = Value::Null;
            }
        }
        let Value::Object(o) = t else { return };
        for field in ["created_at", "updated_at", "duration_source", "source", "root_id", "position"] {
            o.remove(field);
        }
        for field in ["due_at", "due_on", "urgency"] {
            let effective = format!("effective_{field}");
            if o.get(&effective) == o.get(field) {
                o.remove(&effective);
            }
        }
    });
    if path.len() > 1 {
        out["path"] = path.join(" › ").into();
    }
    compact(&mut out);
    Ok(Some(out))
}

fn each_node(node: &mut Value, f: &mut impl FnMut(&mut Value)) {
    f(node);
    if let Some(children) = node["children"].as_array_mut() {
        for child in children {
            each_node(child, f);
        }
    }
}

fn completed_at(conn: &Connection, id: i64) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT completed_at FROM tasks WHERE id = ?1", [id], |r| r.get(0))
}

/// Titles from the root down to `id`.
pub(super) fn path_of(conn: &Connection, user_id: i64, id: i64) -> Result<Vec<String>, ToolError> {
    let mut out = Vec::new();
    let mut at = Some(id);
    while let Some(here) = at {
        let row: Option<(String, Option<i64>)> = conn
            .query_row("SELECT title, parent_id FROM tasks WHERE id = ?1 AND user_id = ?2", (here, user_id), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map(Some)
            .or_else(|e| if e == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(e) })
            .map_err(internal)?;
        let Some((title, parent)) = row else { break };
        out.push(title);
        at = parent;
    }
    out.reverse();
    Ok(out)
}

/// Every word of the query against titles first, then against each node's
/// whole text, at any depth; live nodes before finished ones.
fn search(conn: &Connection, ctx: &ToolCtx, query: &str, f: &Filters, limit: i64, warnings: &mut Vec<String>) -> Result<Value, ToolError> {
    let query = query.trim();
    if query.is_empty() || query.chars().count() > MAX_QUERY_CHARS {
        warnings.push(format!("query must be 1 to {MAX_QUERY_CHARS} characters"));
        return Ok(json!({ "total": 0, "items": [] }));
    }
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut wheres = vec!["user_id = ?".to_string()];
    let mut params: Vec<SqlValue> = vec![ctx.user_id.into()];
    if f.state.is_none() {
        wheres.push("state != 'dropped'".into());
        match &ctx.share {
            None => {}
            Some(scope) if scope.progress => {
                wheres.push("(state != 'done' OR completed_at >= ?)".into());
                params.push(crate::shares::done_since(jiff::Timestamp::now()).to_string().into());
            }
            Some(_) => wheres.push("state != 'done'".into()),
        }
    } else {
        state_filter(ctx, f.state.as_deref(), &mut wheres, &mut params)?;
    }
    let clause = |column: &str, params: &mut Vec<SqlValue>| {
        params.extend(words.iter().map(|w| SqlValue::from(w.clone())));
        words.iter().map(|_| format!("instr({column}, ?) > 0")).collect::<Vec<_>>().join(" AND ")
    };
    let text = if matches_details(ctx) {
        "lower(title) || ' ' || lower(description) || ' ' || lower(notes)"
    } else {
        "lower(title)"
    };
    wheres.push(format!("({})", clause(text, &mut params)));
    filters(ctx, f, Level::Any, &mut wheres, &mut params)?;
    let title_match = clause("lower(title)", &mut params);
    let mut stmt = conn
        .prepare(&format!(
            "SELECT id FROM tasks
             WHERE {}
             ORDER BY CASE WHEN {title_match} THEN 0 ELSE 1 END,
                      CASE WHEN state = 'done' THEN 1 ELSE 0 END,
                      depth, created_at DESC, id DESC
             LIMIT {limit}",
            wheres.join(" AND ")
        ))
        .map_err(internal)?;
    let ids: Vec<i64> = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| r.get(0))
        .and_then(|rows| rows.collect::<rusqlite::Result<_>>())
        .map_err(internal)?;
    let items = rows_with_paths(conn, ctx, &ids)?;
    if items.is_empty() {
        warnings.push(format!("nothing matched {query:?}; try fewer words, or no query to survey"));
    }
    Ok(json!({ "total": items.len(), "items": items }))
}

fn rows_with_paths(conn: &Connection, ctx: &ToolCtx, ids: &[i64]) -> Result<Vec<Value>, ToolError> {
    let tz = tz(ctx);
    let mut items = Vec::new();
    for id in ids {
        let Some(t) = crate::tasks::get(conn, ctx.user_id, *id).map_err(internal)? else { continue };
        let mut row = row_of(&t, &tz, hides_goals(ctx));
        let path = path_of(conn, ctx.user_id, *id)?;
        if path.len() > 1 {
            row["path"] = path.join(" › ").into();
        }
        items.push(row);
    }
    Ok(items)
}

/// The nodes whose own deadline falls in the window, at any depth, soonest first.
fn due_nodes(conn: &Connection, ctx: &ToolCtx, f: &Filters, warnings: &mut Vec<String>) -> Result<Value, ToolError> {
    let mut wheres = vec!["user_id = ?".to_string(), "due_at IS NOT NULL".to_string()];
    let mut params: Vec<SqlValue> = vec![ctx.user_id.into()];
    state_filter(ctx, f.state.as_deref(), &mut wheres, &mut params)?;
    filters(ctx, f, Level::Any, &mut wheres, &mut params)?;
    let wheres = wheres.join(" AND ");
    let total: i64 = conn
        .query_row(&format!("SELECT COUNT(*) FROM tasks WHERE {wheres}"), rusqlite::params_from_iter(params.iter()), |r| r.get(0))
        .map_err(internal)?;
    let mut stmt = conn
        .prepare(&format!("SELECT id FROM tasks WHERE {wheres} ORDER BY due_at, depth, id LIMIT {}", f.limit))
        .map_err(internal)?;
    let ids: Vec<i64> = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| r.get(0))
        .and_then(|rows| rows.collect::<rusqlite::Result<_>>())
        .map_err(internal)?;
    if total == 0 {
        warnings.push("nothing is due then; state any reaches finished ones".into());
    }
    Ok(json!({ "total": total, "items": rows_with_paths(conn, ctx, &ids)? }))
}

/// A node as one short line of fields, what lists carry.
fn row_of(t: &crate::tasks::Task, tz: &jiff::tz::TimeZone, goals_hidden: bool) -> Value {
    let mut row = json!({ "id": t.id, "title": t.title, "state": t.state });
    if t.is_now {
        row["now"] = true.into();
    }
    if let Some(m) = t.duration_min {
        row["minutes"] = m.into();
    }
    if let Some(label) = t.due_at.as_deref().and_then(|at| crate::due::local_label(at, t.due_on.as_deref(), tz)) {
        row["due"] = label.into();
    }
    if t.urgency != "normal" {
        row["urgency"] = t.urgency.clone().into();
    }
    if t.pressing {
        row["pressing"] = true.into();
    }
    if t.progress > 0 && t.state != "done" {
        row["progress"] = t.progress.into();
    }
    if t.parent_id.is_none() {
        if !t.category.is_empty() {
            row["category"] = t.category.clone().into();
        }
        if let (Some(g), false) = (t.goal_id, goals_hidden) {
            row["goal_id"] = g.into();
        }
    }
    row
}

/// Whether a filter reads a root's whole tree or each node on its own.
#[derive(Clone, Copy, PartialEq)]
enum Level {
    Roots,
    Any,
}

fn state_filter(ctx: &ToolCtx, state: Option<&str>, wheres: &mut Vec<String>, params: &mut Vec<SqlValue>) -> Result<(), ToolError> {
    match (&ctx.share, state) {
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
                format!("this link shares live tasks alone; state must be omitted, open or in_progress, got {s:?}")
            }))
        }
        (None, Some("any")) => {}
        (None, Some(s)) => {
            wheres.push("state = ?".into());
            params.push(s.to_string().into());
        }
    }
    Ok(())
}

/// Every filter but the state: at `Roots` a root matches through its tree,
/// at `Any` a node through its own fields and its root's category and goal.
fn filters(ctx: &ToolCtx, f: &Filters, level: Level, wheres: &mut Vec<String>, params: &mut Vec<SqlValue>) -> Result<(), ToolError> {
    let roots = level == Level::Roots;
    let (category, goal, due) = if roots {
        ("category", "goal_id", NEXT_DUE)
    } else {
        ("(SELECT r.category FROM tasks r WHERE r.id = tasks.root_id)", "(SELECT r.goal_id FROM tasks r WHERE r.id = tasks.root_id)", "due_at")
    };
    if let Some(c) = &f.category {
        wheres.push(format!("{category} = ?"));
        params.push(c.trim().to_string().into());
    }
    if let Some(g) = f.goal_id {
        wheres.push(format!("{goal} = ?"));
        params.push(g.into());
    }
    if let Some(day) = f.added_after {
        wheres.push("created_at >= ?".into());
        params.push(day.to_string().into());
    }
    if let Some(day) = f.added_before {
        wheres.push("created_at < ?".into());
        params.push(day.to_string().into());
    }
    if let Some(days) = f.older_than_days {
        let cutoff = jiff::Timestamp::now().checked_sub(jiff::Span::new().hours(24 * days)).map_err(internal)?;
        wheres.push("created_at < ?".into());
        params.push(cutoff.to_string().into());
    }
    if let Some(flag) = f.now {
        wheres.push(match (roots, flag) {
            (true, _) => format!(
                "{}EXISTS (SELECT 1 FROM tasks n WHERE n.root_id = tasks.id AND n.is_now = 1 AND n.state IN ('open','in_progress'))",
                if flag { "" } else { "NOT " }
            ),
            (false, true) => "is_now = 1".into(),
            (false, false) => "is_now = 0".into(),
        });
    }
    if let Some(day) = f.due_before {
        wheres.push(format!("{due} < ?"));
        params.push(day_start(ctx, day)?.into());
    }
    if let Some(day) = f.due_after {
        wheres.push(format!("{due} >= ?"));
        params.push(day_start(ctx, day)?.into());
    }
    if let Some(flag) = f.overdue {
        wheres.push(if flag { format!("({due} < ?)") } else { format!("({due} IS NULL OR {due} >= ?)") });
        params.push(jiff::Timestamp::now().to_string().into());
    }
    if let Some(u) = f.urgency {
        wheres.push(if roots {
            "EXISTS (SELECT 1 FROM tasks n WHERE n.root_id = tasks.id AND n.urgency = ? AND n.state != 'dropped')".into()
        } else {
            "urgency = ?".into()
        });
        params.push(u.to_string().into());
    }
    if let Some((clause, shared)) = share_categories(ctx, f.category.as_deref(), category)? {
        wheres.push(clause.trim_start().trim_start_matches("AND ").to_string());
        params.extend(shared);
    }
    Ok(())
}

fn survey_filters(ctx: &ToolCtx, f: &Filters) -> Result<(String, Vec<SqlValue>), ToolError> {
    let mut wheres = vec!["user_id = ?".to_string(), "parent_id IS NULL".to_string()];
    let mut params: Vec<SqlValue> = vec![ctx.user_id.into()];
    state_filter(ctx, f.state.as_deref(), &mut wheres, &mut params)?;
    filters(ctx, f, Level::Roots, &mut wheres, &mut params)?;
    Ok((wheres.join(" AND "), params))
}

/// The top-level tasks, each with its leaf counts, its next part due and
/// `depth` levels of children.
fn survey(conn: &Connection, ctx: &ToolCtx, f: &Filters, warnings: &mut Vec<String>) -> Result<Value, ToolError> {
    let now = jiff::Timestamp::now();
    let mut extra = Vec::new();
    let order = match f.sort {
        "due" => format!("{NEXT_DUE} IS NULL, {NEXT_DUE} ASC, created_at DESC, id DESC"),
        "urgency" => {
            extra.push(SqlValue::from((now + jiff::Span::new().hours(crate::tasks::PRESSING_HOURS)).to_string()));
            format!(
                "CASE WHEN EXISTS (SELECT 1 FROM tasks n WHERE n.root_id = tasks.id AND n.urgency = 'high'
                                   AND n.state IN ('open','in_progress')) THEN 0
                      WHEN state IN ('open','in_progress') AND {NEXT_DUE} < ? THEN 1
                      WHEN urgency = 'normal' THEN 2 ELSE 3 END,
                 {NEXT_DUE} IS NULL, {NEXT_DUE} ASC, created_at DESC, id DESC"
            )
        }
        _ => "created_at DESC, id DESC".to_string(),
    };
    let (wheres, params) = survey_filters(ctx, f)?;
    let total: i64 = conn
        .query_row(&format!("SELECT COUNT(*) FROM tasks WHERE {wheres}"), rusqlite::params_from_iter(params.iter()), |r| r.get(0))
        .map_err(internal)?;
    let mut page_params = params;
    page_params.extend(extra);
    let mut stmt = conn
        .prepare(&format!("SELECT id FROM tasks WHERE {wheres} ORDER BY {order} LIMIT {}", f.limit))
        .map_err(internal)?;
    let ids: Vec<i64> = stmt
        .query_map(rusqlite::params_from_iter(page_params.iter()), |r| r.get(0))
        .and_then(|rows| rows.collect::<rusqlite::Result<_>>())
        .map_err(internal)?;
    let tz = tz(ctx);
    let goals_hidden = hides_goals(ctx);
    let mut items = Vec::new();
    for id in ids {
        let Some(node) = crate::tasks::node(conn, ctx.user_id, id).map_err(internal)? else { continue };
        let mut row = row_of(&node.task, &tz, goals_hidden);
        if !node.children.is_empty() {
            let leaves = node.leaves();
            let done = leaves.iter().filter(|l| l.state == "done").count();
            row["leaves"] = format!("{done}/{}", leaves.len()).into();
            let next = node
                .walk()
                .into_iter()
                .skip(1)
                .filter(|t| matches!(t.state.as_str(), "open" | "in_progress") && t.due_at.is_some())
                .min_by(|a, b| a.due_at.cmp(&b.due_at));
            if let Some(n) = next {
                row["next_due"] = json!({
                    "id": n.id,
                    "title": n.title,
                    "due": n.due_at.as_deref().and_then(|at| crate::due::local_label(at, n.due_on.as_deref(), &tz)),
                });
            }
            if f.depth > 0 {
                row["children"] = children_rows(&node, &tz, f.depth);
            }
        }
        items.push(row);
    }
    if total == 0 {
        warnings.push("no task matched; state any reaches finished ones".into());
    }
    Ok(json!({ "total": total, "items": items }))
}

fn children_rows(node: &crate::tasks::TaskNode, tz: &jiff::tz::TimeZone, depth: usize) -> Value {
    node.children
        .iter()
        .map(|c| {
            let mut row = row_of(&c.task, tz, false);
            if !c.children.is_empty() {
                if depth > 1 {
                    row["children"] = children_rows(c, tz, depth - 1);
                } else {
                    let leaves = c.leaves();
                    let done = leaves.iter().filter(|l| l.state == "done").count();
                    row["leaves"] = format!("{done}/{}", leaves.len()).into();
                }
            }
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::tools::testkit::{ctx, env, saved, talk};
    use crate::tools::{dispatch, SessionKind, ToolCtx};
    use serde_json::{json, Value};

    fn titles(out: &Value) -> Vec<String> {
        out["items"].as_array().unwrap().iter().map(|t| t["title"].as_str().unwrap().to_string()).collect()
    }

    fn make(conn: &rusqlite::Connection, tmp: &tempfile::TempDir, node: &Value) -> i64 {
        saved(&talk(conn, tmp, "task_save", json!({"tasks": [node]})), "0")
    }

    #[test]
    fn a_survey_shows_the_top_level_with_leaf_counts_next_parts_due_and_children() {
        let (conn, tmp) = env("UTC");
        let course = make(&conn, &tmp, &json!({"title": "Visual Arts", "due": "2099-12-18", "children": [
            {"title": "Unit 1", "due": "2099-11-06", "children": [{"title": "L1", "state": "done"}, "L2"]},
            {"title": "Unit 2", "children": ["L3"]}
        ]}));
        make(&conn, &tmp, &json!({"title": "Dentist"}));
        let out = talk(&conn, &tmp, "task_find", json!({}));
        assert_eq!(out["total"], 2);
        let row = out["items"].as_array().unwrap().iter().find(|r| r["id"] == course).unwrap();
        assert_eq!(row["leaves"], "1/3");
        assert_eq!(row["next_due"]["title"], "Unit 1");
        assert_eq!(row["next_due"]["due"], "2099-11-06");
        assert_eq!(row["children"].as_array().unwrap().len(), 2);
        assert_eq!(row["children"][0]["leaves"], "1/2", "depth 1 stops at the units");
        let deep = talk(&conn, &tmp, "task_find", json!({"depth": 2}));
        let row = deep["items"].as_array().unwrap().iter().find(|r| r["id"] == course).unwrap();
        assert_eq!(row["children"][0]["children"][1]["title"], "L2");
        let flat = talk(&conn, &tmp, "task_find", json!({"depth": 0}));
        assert!(flat["items"][0].get("children").is_none());
    }

    #[test]
    fn a_query_matches_at_any_depth_with_the_path() {
        let (conn, tmp) = env("UTC");
        make(&conn, &tmp, &json!({"title": "Visual Arts", "children": [{"title": "Unit 2", "children": ["Lesson 3 shading"]}]}));
        make(&conn, &tmp, &json!({"title": "Shading pencils"}));
        let out = talk(&conn, &tmp, "task_find", json!({"query": "SHADING"}));
        assert_eq!(titles(&out), ["Shading pencils", "Lesson 3 shading"]);
        assert_eq!(out["items"][1]["path"], "Visual Arts › Unit 2 › Lesson 3 shading");
        let miss = talk(&conn, &tmp, "task_find", json!({"query": "pottery"}));
        assert_eq!(miss["total"], 0);
        assert!(miss["warnings"][0].as_str().unwrap().contains("nothing matched"));
    }

    #[test]
    fn an_id_reads_the_whole_subtree_in_full_and_a_miss_is_no_error() {
        let (conn, tmp) = env("UTC");
        let id = make(&conn, &tmp, &json!({"title": "email landlord", "description": "about the window", "notes": "fridge",
            "now": true, "due": "2099-01-02", "children": [{"title": "find the thread", "minutes": 5, "state": "done"}, {"title": "send", "minutes": 10}]}));
        let out = talk(&conn, &tmp, "task_find", json!({"id": id}));
        let item = &out["item"];
        assert_eq!(item["title"], "email landlord");
        assert_eq!(item["notes"], "fridge");
        assert_eq!(item["due"], "2099-01-02");
        assert_eq!(item["duration_min"], 15);
        for gone in ["created_at", "updated_at", "duration_source", "source", "root_id", "position", "effective_due_at", "effective_urgency"] {
            assert!(item.get(gone).is_none(), "{gone}: {item}");
        }
        assert!(item["children"][0].get("children").is_none());
        assert_eq!(item["children"][0]["state"], "done");
        assert_eq!(item["children"][1]["due"], "2099-01-02", "a child shows its effective due");
        let child = item["children"][1]["id"].as_i64().unwrap();
        assert_eq!(talk(&conn, &tmp, "task_find", json!({"id": child}))["item"]["path"], "email landlord › send");
        let both = talk(&conn, &tmp, "task_find", json!({"id": [id, 9999]}));
        assert_eq!(both["total"], 1);
        assert_eq!(both["warnings"][0], "no task 9999");
        let miss = talk(&conn, &tmp, "task_find", json!({"id": 9999}));
        assert_eq!(miss["total"], 0);
        let by_title = talk(&conn, &tmp, "task_find", json!({"id": "Email Landlord"}));
        assert_eq!(by_title["item"]["id"], id);
        assert_eq!(talk(&conn, &tmp, "task_find", json!({"task_id": id}))["item"]["id"], id);
    }

    #[test]
    fn the_filters_and_sorts_read_loosely() {
        let (conn, tmp) = env("UTC");
        let low = make(&conn, &tmp, &json!({"title": "low one", "urgency": "low"}));
        let plain = make(&conn, &tmp, &json!({"title": "plain"}));
        let soon = make(&conn, &tmp, &json!({"title": "soon", "due": "+30min"}));
        let top = make(&conn, &tmp, &json!({"title": "top", "urgency": "urgent", "category": "School"}));
        let late = make(&conn, &tmp, &json!({"title": "has a late part", "children": [{"title": "part", "due": "2020-01-02"}]}));
        let out = talk(&conn, &tmp, "task_find", json!({"sort": "urgency"}));
        let order: Vec<i64> = out["items"].as_array().unwrap().iter().map(|t| t["id"].as_i64().unwrap()).collect();
        assert_eq!(order, [top, late, soon, plain, low]);
        let overdue = talk(&conn, &tmp, "task_find", json!({"overdue": true}));
        assert_eq!(overdue["items"][0]["path"], "has a late part › part", "{overdue}");
        assert_eq!(talk(&conn, &tmp, "task_find", json!({"overdue": false}))["items"].as_array().unwrap().len(), 4);
        assert_eq!(talk(&conn, &tmp, "task_find", json!({"sort": "due"}))["items"][0]["id"], late, "a root is as due as its parts");
        assert_eq!(talk(&conn, &tmp, "task_find", json!({"urgency": "HIGH"}))["total"], 1);
        assert_eq!(talk(&conn, &tmp, "task_find", json!({"category": "School"}))["total"], 1);
        assert_eq!(talk(&conn, &tmp, "task_find", json!({"due_before": "tomorrow"}))["total"], 2);
        assert_eq!(talk(&conn, &tmp, "task_find", json!({"limit": 2}))["items"].as_array().unwrap().len(), 2);
        let bad = talk(&conn, &tmp, "task_find", json!({"sort": "random", "due_before": "friday-ish", "colour": 1}));
        assert_eq!(bad["total"], 5);
        assert_eq!(bad["warnings"].as_array().unwrap().len(), 3);
        talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": plain, "state": "done"}]}));
        assert_eq!(talk(&conn, &tmp, "task_find", json!({"state": "complete"}))["items"][0]["id"], plain);
        assert_eq!(talk(&conn, &tmp, "task_find", json!({"state": "any"}))["total"], 5);
        assert_eq!(talk(&conn, &tmp, "task_find", json!({"is_now": false}))["total"], 4);
    }

    #[test]
    fn a_due_filter_lists_the_nodes_due_then_at_any_depth() {
        let (conn, tmp) = env("UTC");
        let unit = make(&conn, &tmp, &json!({"title": "Unit 4", "due": "2099-11-13", "children": ["Read", {"title": "Problem set 4", "due": "2099-10-23"}]}));
        let essay = make(&conn, &tmp, &json!({"title": "Essay", "due": "2099-10-23", "category": "History"}));
        make(&conn, &tmp, &json!({"title": "Gym bag", "due": "2099-10-22"}));
        let friday = talk(&conn, &tmp, "task_find", json!({"due": "2099-10-23"}));
        assert_eq!(titles(&friday), ["Essay", "Problem set 4"], "{friday}");
        assert_eq!(friday["items"][1]["path"], "Unit 4 › Problem set 4");
        assert!(!friday["items"].as_array().unwrap().iter().any(|i| i["id"] == unit));
        let window = talk(&conn, &tmp, "task_find", json!({"due_after": "2099-10-23", "due_before": "2099-10-24"}));
        assert_eq!(titles(&window), titles(&friday));
        let before = talk(&conn, &tmp, "task_find", json!({"due_before": "2099-10-23"}));
        assert_eq!(titles(&before), ["Gym bag"], "due_before leaves its own day out");
        let history = talk(&conn, &tmp, "task_find", json!({"due": "2099-10-23", "category": "History"}));
        assert_eq!(history["items"][0]["id"], essay);
        assert_eq!(history["total"], 1);
    }

    #[test]
    fn a_query_keeps_the_state_due_and_category_filters() {
        let (conn, tmp) = env("UTC");
        make(&conn, &tmp, &json!({"title": "Essay draft", "category": "History", "due": "2099-10-23"}));
        make(&conn, &tmp, &json!({"title": "Essay outline", "category": "English", "state": "done"}));
        assert_eq!(talk(&conn, &tmp, "task_find", json!({"query": "essay"}))["total"], 2);
        assert_eq!(titles(&talk(&conn, &tmp, "task_find", json!({"query": "essay", "state": "done"}))), ["Essay outline"]);
        assert_eq!(titles(&talk(&conn, &tmp, "task_find", json!({"query": "essay", "category": "History"}))), ["Essay draft"]);
        assert_eq!(talk(&conn, &tmp, "task_find", json!({"query": "essay", "due": "2099-10-24"}))["total"], 0);
        let sorted = talk(&conn, &tmp, "task_find", json!({"query": "essay", "sort": "due"}));
        assert!(sorted["warnings"][0].as_str().unwrap().starts_with("sort was ignored"), "{sorted}");
    }

    fn share_ctx(tmp: &tempfile::TempDir, scope: crate::shares::ShareScope) -> ToolCtx<'_> {
        ToolCtx { share: Some(scope), ..ctx(tmp) }
    }

    #[test]
    fn a_share_confines_task_find_to_its_categories_details_and_recent_finishes() {
        let (conn, tmp) = env("UTC");
        let lab = make(&conn, &tmp, &json!({"title": "lab report", "category": "school", "description": "secret grade talk", "children": ["graph"]}));
        let forms = make(&conn, &tmp, &json!({"title": "therapy forms", "category": "health"}));
        let school = share_ctx(&tmp, crate::shares::ShareScope { categories: vec!["school".into()], ..Default::default() });
        let find = |c: &ToolCtx, args: Value| dispatch(&conn, c, SessionKind::Share, "task_find", &args.to_string());
        assert_eq!(titles(&find(&school, json!({})).unwrap()), ["lab report"]);
        assert_eq!(find(&school, json!({"category": "health"})).unwrap_err().kind, "rejected");
        assert_eq!(find(&school, json!({"query": "forms"})).unwrap()["total"], 0);
        assert_eq!(titles(&find(&school, json!({"query": "graph"})).unwrap()), ["graph"], "a child reads its root's category");
        assert_eq!(find(&school, json!({"id": forms})).unwrap()["total"], 0);
        let read = find(&school, json!({"id": lab})).unwrap();
        assert!(read["item"].get("description").is_none(), "details are off");
        assert_eq!(find(&school, json!({"query": "grade"})).unwrap()["total"], 0);
        let detailed = share_ctx(&tmp, crate::shares::ShareScope { details: true, ..Default::default() });
        assert_eq!(find(&detailed, json!({"id": lab})).unwrap()["item"]["description"], "secret grade talk");
        let closed = share_ctx(&tmp, crate::shares::ShareScope { progress: false, ..Default::default() });
        assert_eq!(find(&closed, json!({"state": "done"})).unwrap_err().kind, "rejected");
        let hidden = share_ctx(&tmp, crate::shares::ShareScope { goals: false, ..Default::default() });
        let goal = saved(&talk(&conn, &tmp, "goal_save", json!({"goals": [{"title": "GOAL-TITLE-SECRET", "tasks": [{"id": lab}]}]})), "0");
        assert_eq!(crate::tasks::get(&conn, 1, lab).unwrap().unwrap().goal_id, Some(goal));
        let read = find(&hidden, json!({"id": lab})).unwrap();
        assert!(!read.to_string().contains("GOAL-TITLE-SECRET"), "{read}");
        assert!(find(&hidden, json!({})).unwrap()["items"][0].get("goal_id").is_none());
    }

    #[test]
    fn a_scoped_session_surveys_nothing() {
        let (conn, tmp) = env("UTC");
        let id = make(&conn, &tmp, &json!({"title": "biology"}));
        let scoped = ToolCtx { task_scope: Some(id), ..ctx(&tmp) };
        let e = dispatch(&conn, &scoped, SessionKind::Talk, "task_find", "{}").unwrap_err();
        assert_eq!(e.kind, "rejected");
    }
}
