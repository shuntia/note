use super::loose::{self, Envelope, Obj, Ref, Target, When};
use super::task_ops::{user_tz, Saver};
use super::{SessionKind, ToolCtx, ToolError};
use crate::goals::{GoalPatch, NewGoal};
use rusqlite::Connection;
use serde_json::{json, Value};

const MAX_TITLE_BYTES: usize = 200;
const MAX_DESCRIPTION_BYTES: usize = 8 * 1024;

fn internal(e: impl std::fmt::Display) -> ToolError {
    ToolError::internal(e.to_string())
}

fn row(g: &crate::goals::Goal, tz: &jiff::tz::TimeZone, details: bool) -> Value {
    let mut row = json!({
        "id": g.id,
        "title": g.title,
        "state": g.state,
        "tasks": g.tasks,
        "done_tasks": g.done_tasks,
    });
    if let Some(due) = g.due_at.as_deref().and_then(|at| crate::due::local_label(at, g.due_on.as_deref(), tz)) {
        row["due"] = due.into();
    }
    if let (Some(id), Some(title)) = (g.next_task_id, &g.next_task_title) {
        row["next_task"] = json!({
            "id": id,
            "title": title,
            "due": g.next_due_at.as_deref().and_then(|at| crate::due::local_label(at, g.next_due_on.as_deref(), tz)),
        });
    }
    if details && !g.description.is_empty() {
        row["description"] = g.description.clone().into();
    }
    row
}

/// `goal_find`: the goals, soonest due first, or one with its tasks.
pub fn find(conn: &Connection, ctx: &ToolCtx, raw: &str) -> Result<Value, ToolError> {
    super::task_query::unscoped(ctx)?;
    let mut o = Obj::new(loose::args(raw)?);
    let mut warnings = Vec::new();
    let id = o.take(&["id", "goal_id", "ids"]).map(loose::list).unwrap_or_default();
    let state = o.take(&["state", "status"]).and_then(|v| loose::text(&v)).map(|s| {
        let s = s.trim().to_lowercase();
        if s == "any" || s == "all" {
            s.replace("all", "any")
        } else {
            loose::choice(&s, crate::goals::STATES, loose::STATE_SYNONYMS).map_or(s, str::to_string)
        }
    });
    for field in o.rest() {
        warnings.push(format!("{field} was ignored: not a goal_find field"));
    }
    let state = match state.as_deref() {
        Some(s) if s != "any" && !crate::goals::STATES.contains(&s) => {
            warnings.push(format!("state {s:?} was ignored: it is open, done, dropped or any"));
            None
        }
        _ => state,
    };
    let wanted: Vec<i64> = id.iter().filter_map(loose::id).collect();
    let mut goals = crate::goals::list(conn, ctx.user_id, if wanted.is_empty() { state.as_deref() } else { Some("any") })
        .map_err(|e| internal(e.to_string()))?;
    if !wanted.is_empty() {
        goals.retain(|g| wanted.contains(&g.id));
    }
    if let Some(scope) = ctx.share.as_ref().filter(|s| !s.categories.is_empty()) {
        goals = confined(conn, goals, scope)?;
    }
    let tz = user_tz(ctx);
    let details = ctx.share.as_ref().is_none_or(|s| s.details);
    let mut items: Vec<Value> = goals.iter().map(|g| row(g, &tz, details)).collect();
    for id in &wanted {
        if !goals.iter().any(|g| g.id == *id) {
            warnings.push(format!("no goal {id}"));
        }
    }
    let mut out = if wanted.len() == 1 && items.len() == 1 {
        let mut item = items.remove(0);
        item["task_list"] = goal_tasks(conn, ctx, wanted[0], &tz)?;
        json!({ "item": item })
    } else {
        json!({ "total": items.len(), "items": items })
    };
    if !warnings.is_empty() {
        out["warnings"] = warnings.into();
    }
    Ok(out)
}

/// The goal's top-level tasks, each with its leaf count, as one line apiece.
fn goal_tasks(conn: &Connection, ctx: &ToolCtx, goal: i64, tz: &jiff::tz::TimeZone) -> Result<Value, ToolError> {
    let mut sql = "SELECT id FROM tasks WHERE user_id = ?1 AND goal_id = ?2 AND parent_id IS NULL AND state != 'dropped'".to_string();
    let mut params: Vec<rusqlite::types::Value> = vec![ctx.user_id.into(), goal.into()];
    if let Some((clause, shared)) = ctx.share.as_ref().and_then(|s| s.category_clause("category")) {
        sql.push_str(&clause);
        params.extend(shared);
    }
    sql.push_str(" ORDER BY due_at IS NULL, due_at, id");
    let mut stmt = conn.prepare(&sql).map_err(internal)?;
    let ids: Vec<i64> = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| r.get(0))
        .and_then(|rows| rows.collect::<rusqlite::Result<_>>())
        .map_err(internal)?;
    let mut out = Vec::new();
    for id in ids {
        let Some(node) = crate::tasks::node(conn, ctx.user_id, id).map_err(internal)? else { continue };
        let t = &node.task;
        let mut row = json!({ "id": t.id, "title": t.title, "state": t.state });
        if let Some(due) = t.due_at.as_deref().and_then(|at| crate::due::local_label(at, t.due_on.as_deref(), tz)) {
            row["due"] = due.into();
        }
        if let Some(m) = t.duration_min {
            row["minutes"] = m.into();
        }
        if !node.children.is_empty() {
            let leaves = node.leaves();
            row["leaves"] = format!("{}/{}", leaves.iter().filter(|l| l.state == "done").count(), leaves.len()).into();
        }
        out.push(row);
    }
    Ok(out.into())
}

/// Recounts each goal, and picks its next task, from the shared categories
/// alone, dropping the goals none of them reach.
fn confined(
    conn: &Connection,
    goals: Vec<crate::goals::Goal>,
    scope: &crate::shares::ShareScope,
) -> Result<Vec<crate::goals::Goal>, ToolError> {
    let mut out = Vec::with_capacity(goals.len());
    for mut g in goals {
        let (tasks, done_tasks) = crate::shares::goal_counts(conn, g.id, scope).map_err(internal)?;
        if tasks == 0 {
            continue;
        }
        let next = crate::shares::goal_next_task(conn, g.id, scope).map_err(internal)?;
        g.tasks = tasks;
        g.done_tasks = done_tasks;
        (g.next_task_id, g.next_task_title, g.next_due_at) = match next {
            Some((id, title, due)) => (Some(id), Some(title), due),
            None => (None, None, None),
        };
        g.next_due_on = None;
        out.push(g);
    }
    Ok(out)
}

fn open_goals(conn: &Connection, user_id: i64) -> Result<Vec<(i64, String)>, ToolError> {
    Ok(crate::goals::list(conn, user_id, None)
        .map_err(|e| internal(e.to_string()))?
        .into_iter()
        .map(|g| (g.id, g.title))
        .collect())
}

fn open_roots(conn: &Connection, user_id: i64, goal: i64) -> Result<Vec<(i64, String)>, ToolError> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT id, title FROM tasks WHERE user_id = ?1 AND goal_id = ?2 AND parent_id IS NULL
             AND state IN ('open','in_progress') ORDER BY due_at IS NULL, due_at, id",
        )
        .map_err(internal)?;
    let rows = stmt
        .query_map((user_id, goal), |r| Ok((r.get(0)?, r.get(1)?)))
        .and_then(|rows| rows.collect::<rusqlite::Result<_>>())
        .map_err(internal)?;
    Ok(rows)
}

/// A goal finished or dropped takes its open tasks the same way.
fn close_work(conn: &Connection, ctx: &ToolCtx, goal: &crate::goals::Goal, env: &mut Envelope) -> Result<(), ToolError> {
    let why = if goal.state == "done" { "its goal was finished" } else { "its goal was dropped" };
    for (id, title) in open_roots(conn, ctx.user_id, goal.id)? {
        let patch = crate::tasks::TaskPatch { state: Some(goal.state.clone()), actor: crate::tasks::Actor::Agent, ..Default::default() };
        match crate::tasks::update(conn, ctx.user_id, id, &patch) {
            Ok(_) => env.adjusted.push(json!({ "id": id, "title": title, "field": "state", "to": goal.state, "why": why })),
            Err(crate::tasks::UpdateError::Db(e)) => return Err(internal(e)),
            Err(e) => env.warn(format!("task {id} stayed open: {e}")),
        }
    }
    Ok(())
}

/// `goal_save`: goals made or changed, each with its task nodes hung from it.
pub fn save(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, raw: &str) -> Result<Value, ToolError> {
    super::task_query::unscoped(ctx)?;
    let mut obj = Obj::new(loose::args(raw)?);
    let (goals, warning) = loose::items(&mut obj, "goals", &["items", "list", "data", "goal"], &["title", "id", "goal_id", "due", "tasks"]);
    let mut saver = Saver::new(conn, ctx, kind)?;
    if let Some(w) = warning {
        saver.env.warn(w);
    }
    let rest = obj.rest();
    for field in &rest {
        saver.env.ignored(None, field, "not a goal_save field");
    }
    if goals.is_empty() {
        if rest.is_empty() {
            saver.env.warn("nothing to save: goals was empty");
        } else {
            saver.env.refuse_call("nothing was saved: the goals go in goals, a list");
        }
        return Ok(saver.env.into_value());
    }
    let tz = user_tz(ctx);
    let now = jiff::Timestamp::now();
    for (i, v) in goals.into_iter().enumerate() {
        let r = Ref::top(i);
        let Value::Object(map) = v else {
            saver.env.refused(&r, "a goal is an object with a title");
            continue;
        };
        let mut goal_obj = Obj::new(map);
        let env = &mut saver.env;
        let target = goal_obj.take(&["id", "goal_id"]).and_then(|v| loose::target(&v));
        let mut title = goal_obj.take(&["title", "name"]).and_then(|v| loose::text(&v)).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        let mut description = goal_obj.take(&["description", "desc", "details"]).and_then(|v| loose::text(&v));
        if let Some(full) = title.clone() {
            if let Some(cut) = loose::cut_bytes(&full, MAX_TITLE_BYTES) {
                env.adjusted(Some(&r), "title", cut.clone().into(), "a goal's title stops at 200 bytes; the whole text leads the description");
                description = Some(match description {
                    Some(d) => format!("{full}\n\n{d}"),
                    None => full,
                });
                title = Some(cut);
            }
        }
        if let Some(d) = description.as_deref().and_then(|d| loose::truncate(d, MAX_DESCRIPTION_BYTES)) {
            env.warn(format!("{} description was cut to {MAX_DESCRIPTION_BYTES} bytes", r.0));
            description = Some(d);
        }
        let mut due: Option<Option<When>> = None;
        if let Some(v) = goal_obj.take(&["due", "due_at", "due_on", "deadline", "due_date"]) {
            match loose::text(&v) {
                Some(s) if s.trim().is_empty() => due = Some(None),
                Some(s) => match loose::when(&s, &tz, now) {
                    Some(w) => {
                        if let Some(d) = loose::rolled(&s, now.to_zoned(tz.clone()).date()) {
                            env.adjusted(Some(&r), "due", d.to_string().into(), loose::rolled_why(&s, d));
                        }
                        due = Some(Some(w));
                    }
                    None => env.ignored(Some(&r), "due", format!("a due is {} or {}", loose::DAY_FORMS, loose::MOMENT_FORMS)),
                },
                None => env.ignored(Some(&r), "due", "a due is text such as 2026-12-18"),
            }
        }
        let state = goal_obj.take(&["state", "status"]).and_then(|v| loose::text(&v)).and_then(|s| {
            let state = loose::choice(&s, crate::goals::STATES, loose::STATE_SYNONYMS);
            if state.is_none() {
                env.ignored(Some(&r), "state", "a goal's state is open, done or dropped");
            }
            state
        });
        let new = goal_obj.take(&["new", "also"]).and_then(|v| loose::boolean(&v)).unwrap_or(false);
        let tasks = goal_obj.take(&["tasks", "children", "milestones", "items", "steps"]).map(loose::list).unwrap_or_default();
        for field in goal_obj.rest() {
            env.ignored(Some(&r), &field, "not a goal field");
        }
        let (due_at, due_on) = match due {
            None => (None, None),
            Some(None) => (Some(None), None),
            Some(Some(When::Day(d))) => (None, Some(Some(d.to_string()))),
            Some(Some(When::At(t))) => (Some(Some(t.to_string())), None),
        };
        let state_only = state.is_some() && description.is_none() && due.is_none() && tasks.is_empty();
        let mut matched = false;
        let id = match target {
            Some(Target::Id(id)) => {
                if crate::goals::get(conn, ctx.user_id, id).map_err(internal)?.is_none() {
                    env.refused(&r, format!("no goal {id}"));
                    continue;
                }
                Some(id)
            }
            Some(Target::Title(t)) => match loose::by_title(&t, &open_goals(conn, ctx.user_id)?, "goal") {
                Ok(id) => {
                    env.adjusted(Some(&r), "id", id.into(), format!("{t:?} is goal {id}"));
                    Some(id)
                }
                Err(why) => {
                    env.refused(&r, why);
                    continue;
                }
            },
            None => match (&title, new) {
                (Some(t), false) => {
                    let want = loose::fold(t);
                    let open = open_goals(conn, ctx.user_id)?;
                    let hits: Vec<i64> = open.iter().filter(|(_, x)| loose::fold(x) == want).map(|(id, _)| *id).collect();
                    match hits.as_slice() {
                        [] if state_only => {
                            let near: Vec<String> = loose::nearest(t, &open, 3).iter().map(|(id, x)| format!("{x:?} ({id})")).collect();
                            let near = if near.is_empty() { String::new() } else { format!("; nearest: {}", near.join(", ")) };
                            env.refused(&r, format!("no open goal is titled {t:?}{near}; give its id, or new: true to make it"));
                            continue;
                        }
                        [] => None,
                        [one] => {
                            matched = true;
                            Some(*one)
                        }
                        many => {
                            env.refused(&r, format!("{} open goals are titled {t:?}: {}; give the id", many.len(), many.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")));
                            continue;
                        }
                    }
                }
                _ => None,
            },
        };
        let mut was = None;
        let saved = if let Some(id) = id {
            {
                let before = crate::goals::get(conn, ctx.user_id, id).map_err(internal)?;
                was = before.as_ref().map(|b| b.state.clone());
                let patch = GoalPatch { title, description, state: state.map(str::to_string), due_at, due_on, zone: Some(tz.clone()) };
                let changed = before.is_none_or(|b| {
                    patch.title.as_ref().is_some_and(|t| *t != b.title)
                        || patch.description.as_ref().is_some_and(|d| *d != b.description)
                        || patch.state.as_ref().is_some_and(|s| *s != b.state)
                        || patch.due_at.is_some()
                        || patch.due_on.is_some()
                });
                match crate::goals::update(conn, ctx.user_id, id, &patch) {
                    Ok(Some(goal)) => Some((goal, if changed { "updated" } else { "unchanged" })),
                    Ok(None) => {
                        env.refused(&r, format!("no goal {id}"));
                        None
                    }
                    Err(crate::tasks::UpdateError::Db(e)) => return Err(internal(e)),
                    Err(e) => {
                        env.refused(&r, e.to_string());
                        None
                    }
                }
            }
        } else {
            {
                let Some(title) = title else {
                    env.refused(&r, "a new goal needs a title; to change one, give its id");
                    continue;
                };
                let made = crate::goals::create(conn, ctx.user_id, NewGoal { title, description, due_at, due_on, zone: Some(tz.clone()) });
                match made {
                    Ok(goal) => {
                        let goal = if let Some(s) = state.filter(|s| *s != "open") {
                            crate::goals::update(conn, ctx.user_id, goal.id, &GoalPatch { state: Some(s.into()), ..GoalPatch::default() })
                                .map_err(|e| internal(e.to_string()))?
                                .unwrap_or(goal)
                        } else {
                            goal
                        };
                        Some((goal, "created"))
                    }
                    Err(crate::tasks::UpdateError::Db(e)) => return Err(internal(e)),
                    Err(e) => {
                        env.refused(&r, e.to_string());
                        None
                    }
                }
            }
        };
        let Some((goal, op)) = saved else { continue };
        let row = saver.env.saved(&r, goal.id, &goal.title, op);
        row["kind"] = "goal".into();
        if matched {
            row["matched"] = "title".into();
        }
        if matches!(goal.state.as_str(), "done" | "dropped") && was.as_deref().is_some_and(|w| w != goal.state) {
            close_work(conn, ctx, &goal, &mut saver.env)?;
        }
        if !tasks.is_empty() {
            let items = tasks.into_iter().enumerate().map(|(j, v)| (r.child(j), v)).collect();
            saver.set_goal(Some(goal.id), op == "created");
            saver.save(items)?;
            saver.set_goal(None, false);
        }
    }
    Ok(saver.env.into_value())
}

/// `goal_remove`: the goals go, their tasks stay, named in a warning.
pub fn remove(conn: &Connection, ctx: &ToolCtx, raw: &str) -> Result<Value, ToolError> {
    super::task_query::unscoped(ctx)?;
    let mut o = Obj::new(loose::args(raw)?);
    let mut env = Envelope::default();
    let ids = loose::ids(&mut o, &["ids", "id", "goal_ids", "goal_id", "goals"], &mut env);
    for field in o.rest() {
        env.ignored(None, &field, "not a goal_remove field");
    }
    if ids.is_empty() {
        loose::nothing_removed(&mut env);
        return Ok(env.into_value());
    }
    for (i, v) in ids.into_iter().enumerate() {
        let r = Ref::top(i);
        let id = match loose::target(&v) {
            Some(Target::Id(id)) => id,
            Some(Target::Title(t)) => match loose::by_title(&t, &open_goals(conn, ctx.user_id)?, "goal") {
                Ok(id) => id,
                Err(why) => {
                    env.refused(&r, why);
                    continue;
                }
            },
            None => {
                env.refused(&r, "a goal is named by its id");
                continue;
            }
        };
        let Some(goal) = crate::goals::get(conn, ctx.user_id, id).map_err(internal)? else {
            env.refused(&r, format!("no goal {id}"));
            continue;
        };
        let left = open_roots(conn, ctx.user_id, id)?;
        if crate::goals::delete_within(conn, ctx.user_id, id).map_err(internal)? {
            env.removed(id, &goal.title);
            if !left.is_empty() {
                let names: Vec<String> = left.iter().take(8).map(|(id, t)| format!("{t:?} ({id})")).collect();
                let more = if left.len() > 8 { format!(" and {} more", left.len() - 8) } else { String::new() };
                env.warn(format!(
                    "the open tasks of {:?} stay, under no goal: {}{more}; drop them with task_save if they go too",
                    goal.title,
                    names.join(", ")
                ));
            }
        }
    }
    Ok(env.into_value())
}

#[cfg(test)]
mod tests {
    use crate::tools::testkit::{call, ctx, env, saved, talk, task};
    use crate::tools::{dispatch, registry, SessionKind, ToolCtx};
    use serde_json::{json, Value};

    fn md(days: i64) -> String {
        let d = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::get("America/Los_Angeles").unwrap()).date()
            .checked_add(jiff::Span::new().days(days)).unwrap();
        format!("{}/{}", d.month(), d.day())
    }

    fn iso(days: i64) -> String {
        jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::get("America/Los_Angeles").unwrap()).date()
            .checked_add(jiff::Span::new().days(days)).unwrap().to_string()
    }

    /// The course the eval's visual-arts case hands a model, in one call.
    #[test]
    fn a_whole_course_lands_in_one_goal_save() {
        let (conn, tmp) = env("America/Los_Angeles");
        let sizes = [json!("1h"), json!("45m"), json!(30), json!("1:15"), json!("50 min"), json!("40"), json!(25), json!("1.5h"), json!(20)];
        let units: Vec<(i64, usize)> = vec![(17, 8), (29, 7), (44, 9), (58, 8)];
        let mut tasks: Vec<Value> = units
            .iter()
            .enumerate()
            .map(|(u, (due, n))| {
                let lessons: Vec<Value> = (0..*n)
                    .map(|l| json!({"title": format!("Lesson {}.{}", u + 1, l + 1), "minutes": sizes[(u + l) % sizes.len()]}))
                    .collect();
                json!({"title": format!("Unit {}", u + 1), "due": md(*due), "children": lessons})
            })
            .collect();
        tasks.insert(2, json!({"title": "Unit 2b", "due": md(36), "minutes": "2h", "steps": ["Sketch", "Shade", {"title": "Critique", "duration": "1h", "due": md(90)}]}));
        tasks.push(json!({"title": "Cumulative exam", "due": md(59), "children": [{"title": "Review all units", "minutes": "2h"}]}));
        let out = talk(&conn, &tmp, "goal_save", json!({"goals": [{"title": "Visual Arts", "due": md(59), "tasks": tasks}]}));
        assert_eq!(out["ok"], true, "{out}");
        let goal = saved(&out, "0");
        let g = crate::goals::get(&conn, 1, goal).unwrap().unwrap();
        assert_eq!(g.due_on.as_deref(), Some(iso(59).as_str()));
        let roots: Vec<i64> = conn
            .prepare("SELECT id FROM tasks WHERE goal_id = ?1 AND parent_id IS NULL ORDER BY id").unwrap()
            .query_map([goal], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(roots.len(), 6);
        let mut lessons = 0;
        for (i, root) in roots.iter().enumerate() {
            let node = crate::tasks::node(&conn, 1, *root).unwrap().unwrap();
            let sum: u32 = node.children.iter().map(|c| c.task.duration_min.unwrap()).sum();
            assert_eq!(node.task.duration_min, Some(sum), "{} sums its lessons", node.task.title);
            for child in &node.children {
                assert_eq!(child.task.duration_min.unwrap() % 5, 0);
                if let (Some(own), Some(limit)) = (&child.task.due_at, &node.task.due_at) {
                    assert!(own <= limit, "{} is due after {}", child.task.title, node.task.title);
                }
                lessons += 1;
            }
            let due = [17, 29, 36, 44, 58, 59][i];
            assert_eq!(node.task.due_on.as_deref(), Some(iso(due).as_str()), "{}", node.task.title);
        }
        assert!(lessons >= 35, "{lessons} lessons");
        assert!(out["adjusted"].as_array().unwrap().iter().any(|a| a["field"] == "due" && a["ref"] == "0.2.2"), "the critique was clamped");
        assert_eq!(task(&conn, saved(&out, "0.0.3")).duration_min, Some(75));
        assert_eq!(task(&conn, saved(&out, "0.2.0")).duration_min, Some(30), "the unsized parts share what is left of 2h");

        let again = talk(&conn, &tmp, "goal_save", json!({"goals": [{"title": "Visual Arts", "tasks": [{"title": "Unit 1", "children": [{"title": "Lesson 1.1", "state": "done"}]}]}]}));
        assert_eq!(again["saved"][0]["op"], "unchanged");
        assert_eq!(saved(&again, "0.0"), roots[0], "the same course sent twice updates, not doubles");
        assert_eq!(task(&conn, saved(&again, "0.0.0")).state, "done");
    }

    #[test]
    fn a_second_course_with_the_same_unit_names_keeps_its_own_tasks() {
        let (conn, tmp) = env("UTC");
        let course = |title: &str| json!({"goals": [{"title": title, "tasks": [
            {"title": "Unit 1", "children": ["Lesson 1", "Lesson 2"]}, {"title": "Unit 2", "children": ["Lesson 1"]}]}]});
        let arts = talk(&conn, &tmp, "goal_save", course("Visual Arts"));
        let music = talk(&conn, &tmp, "goal_save", course("Music Theory"));
        assert!(music["saved"].as_array().unwrap().iter().all(|s| s["op"] == "created"), "{music}");
        assert_ne!(saved(&music, "0.0"), saved(&arts, "0.0"));
        assert_eq!(task(&conn, saved(&arts, "0.0")).goal_id, Some(saved(&arts, "0")));
        let again = talk(&conn, &tmp, "goal_save", course("Visual Arts"));
        assert_eq!(saved(&again, "0.0"), saved(&arts, "0.0"), "a retry lands on the course's own units");
        assert_eq!(again["saved"][1]["matched"], "title");
        assert!(again.get("adjusted").is_none(), "{again}");
    }

    #[test]
    fn dropping_a_goal_drops_its_open_work_and_a_loose_title_is_never_a_new_goal() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "goal_save", json!({"goals": [{"title": "AP Chemistry final", "tasks": [
            {"title": "Unit 4", "children": ["Read", "Problem set"]}, {"title": "Unit 5", "state": "done"}]}]}));
        let (goal, unit4, unit5) = (saved(&out, "0"), saved(&out, "0.0"), saved(&out, "0.1"));
        let out = talk(&conn, &tmp, "goal_save", json!({"goals": [{"title": "chemistry", "state": "dropped"}]}));
        assert_eq!(out["ok"], false);
        let why = out["refused"][0]["why"].as_str().unwrap();
        assert!(why.contains(&format!("\"AP Chemistry final\" ({goal})")), "{why}");
        assert_eq!(crate::goals::list(&conn, 1, Some("any")).unwrap().len(), 1, "no goal was made");
        let out = talk(&conn, &tmp, "goal_save", json!({"goals": [{"id": goal, "state": "dropped"}]}));
        assert_eq!(out["adjusted"].as_array().unwrap().len(), 1, "{out}");
        assert_eq!(out["adjusted"][0]["id"], unit4);
        assert_eq!(out["adjusted"][0]["why"], "its goal was dropped");
        assert_eq!(task(&conn, unit4).state, "dropped");
        assert_eq!(task(&conn, unit5).state, "done", "finished work stays finished");
        let node = crate::tasks::node(&conn, 1, unit4).unwrap().unwrap();
        assert!(node.children.iter().all(|c| c.task.state == "dropped"));
    }

    #[test]
    fn removing_a_goal_names_the_open_tasks_it_leaves() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "goal_save", json!({"goals": [{"title": "Visual Arts", "tasks": ["Unit 1", "Unit 2"]}]}));
        let out = talk(&conn, &tmp, "goal_remove", json!({"ids": [saved(&out, "0")]}));
        let warning = out["warnings"][0].as_str().unwrap();
        assert!(warning.contains("\"Unit 1\"") && warning.contains("\"Unit 2\""), "{warning}");
    }

    #[test]
    fn a_goal_is_made_read_changed_and_removed_with_its_tasks_left_behind() {
        let (conn, tmp) = env("Asia/Tokyo");
        let out = talk(&conn, &tmp, "goal_save", json!({"goals": [{"title": "get into the programme", "description": "the whole application", "due": "2099-11-01",
            "tasks": [{"title": "essay", "due": "2099-10-25"}, {"title": "form", "due": "2099-10-02"}]}]}));
        let goal = saved(&out, "0");
        assert_eq!(out["saved"][0]["kind"], "goal");
        assert_eq!(crate::goals::get(&conn, 1, goal).unwrap().unwrap().due_at.as_deref(), Some("2099-11-01T14:59:00Z"));
        let found = talk(&conn, &tmp, "goal_find", json!({}));
        assert_eq!(found["total"], 1);
        assert_eq!(found["items"][0]["tasks"], 2);
        assert_eq!(found["items"][0]["next_task"]["title"], "form");
        let one = talk(&conn, &tmp, "goal_find", json!({"id": goal}));
        assert_eq!(one["item"]["task_list"].as_array().unwrap().len(), 2);
        let out = talk(&conn, &tmp, "goal_save", json!({"goals": [{"id": "Get into the  programme", "state": "reached"}]}));
        assert_eq!(out["saved"][0]["op"], "updated");
        assert_eq!(talk(&conn, &tmp, "goal_find", json!({}))["total"], 0);
        assert_eq!(talk(&conn, &tmp, "goal_find", json!({"state": "any"}))["total"], 1);
        let essay = saved(&talk(&conn, &tmp, "task_find", json!({"query": "essay"})).pipe_items(), "x");
        let out = talk(&conn, &tmp, "goal_remove", json!({"ids": [goal, 999]}));
        assert_eq!(out["removed"][0]["id"], goal);
        assert_eq!(out["refused"][0]["why"], "no goal 999");
        assert_eq!(task(&conn, essay).goal_id, None, "its tasks stay, detached");
    }

    trait Items {
        fn pipe_items(self) -> Value;
    }

    impl Items for Value {
        fn pipe_items(self) -> Value {
            json!({ "saved": self["items"].as_array().unwrap().iter().map(|i| json!({"ref": "x", "id": i["id"]})).collect::<Vec<_>>() })
        }
    }

    #[test]
    fn a_goal_save_forgives_what_it_can_and_refuses_what_it_cannot() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "goal_save", json!({"title": "bare goal", "due": "someday", "colour": "red"}));
        assert_eq!(out["saved"][0]["title"], "bare goal");
        assert!(out["ignored"].as_array().unwrap().iter().any(|i| i["field"] == "due"));
        assert!(out["ignored"].as_array().unwrap().iter().any(|i| i["field"] == "colour"));
        let out = talk(&conn, &tmp, "goal_save", json!({"goals": [{"id": 99, "title": "x"}, {"description": "no title"}, {"title": "bare goal"}]}));
        assert_eq!(out["refused"][0]["why"], "no goal 99");
        assert_eq!(out["refused"][1]["ref"], "1");
        assert_eq!(out["saved"][0]["op"], "unchanged", "the same title is the same goal");
        let out = talk(&conn, &tmp, "goal_save", json!({"goals": []}));
        assert_eq!(out["ok"], true);
        let long = "goal ".repeat(60);
        let out = talk(&conn, &tmp, "goal_save", json!({"goals": [{"title": long}]}));
        let g = crate::goals::get(&conn, 1, saved(&out, "0")).unwrap().unwrap();
        assert!(g.title.len() <= 200 && g.description.starts_with("goal goal"));
        let out = talk(&conn, &tmp, "goal_save", json!({"goals": {"title": "bare object", "due_at": "2099-01-01"}}));
        let id = saved(&out, "0");
        talk(&conn, &tmp, "goal_save", json!({"goals": [{"goal_id": id, "state": "complete"}]}));
        assert_eq!(crate::goals::get(&conn, 1, id).unwrap().unwrap().state, "done");
        assert_eq!(talk(&conn, &tmp, "goal_find", json!({"state": "done"}))["total"], 1);
    }

    #[test]
    fn the_goal_tools_sit_where_the_user_can_be_asked() {
        for name in ["goal_find", "goal_save", "goal_remove"] {
            for kind in [SessionKind::Talk, SessionKind::Nightly, SessionKind::Call] {
                assert!(registry(kind).contains(&name), "{name} missing from {kind:?}");
            }
            for kind in [SessionKind::Import, SessionKind::Inbox] {
                assert!(!registry(kind).contains(&name), "{name} reached {kind:?}");
            }
        }
        assert!(registry(SessionKind::Trigger).contains(&"goal_find"));
        assert!(!registry(SessionKind::Trigger).contains(&"goal_save"), "a wake-up reads goals only");
        let (conn, tmp) = env("UTC");
        let scoped = ToolCtx { task_scope: Some(1), ..ctx(&tmp) };
        let e = dispatch(&conn, &scoped, SessionKind::Talk, "goal_find", "{}").unwrap_err();
        assert_eq!(e.kind, "rejected", "a scoped session surveys nothing");
        let e = call(&conn, &tmp, SessionKind::Trigger, "goal_save", r#"{"goals":[{"title":"x"}]}"#).unwrap_err();
        assert_eq!(e.kind, "forbidden");
    }

    #[test]
    fn a_share_counts_only_allowed_tasks_and_hides_empty_goals() {
        let (conn, tmp) = env("UTC");
        talk(&conn, &tmp, "goal_save", json!({"goals": [
            {"title": "pass chemistry", "tasks": [{"title": "lab", "category": "school"}, {"title": "quiz", "category": "school", "state": "done"}, {"title": "gym", "category": "health"}]},
            {"title": "get healthy", "description": "private", "tasks": [{"title": "forms", "category": "health"}]}
        ]}));
        let sctx = ToolCtx { share: Some(crate::shares::ShareScope { categories: vec!["school".into()], ..Default::default() }), ..ctx(&tmp) };
        let out = dispatch(&conn, &sctx, SessionKind::Share, "goal_find", "{}").unwrap();
        assert_eq!(out["total"], 1, "{out}");
        assert_eq!(out["items"][0]["tasks"], 2);
        assert_eq!(out["items"][0]["done_tasks"], 1);
        assert!(out["items"][0].get("description").is_none());
        assert_eq!(out["items"][0]["next_task"]["title"], "lab");
    }
}
