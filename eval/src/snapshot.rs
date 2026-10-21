//! The state a checker judges, read from the scratch server into a shape that
//! does not depend on how deep the task tree goes or where a day's list lives.

use anyhow::Result;
use jiff::civil::{Date, Time};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub today: Option<Date>,
    pub nodes: Vec<Node>,
    pub goals: Vec<Goal>,
    pub calendar: Vec<Entry>,
    pub days: BTreeMap<Date, Vec<DayItem>>,
    /// Each read day's calendar occurrences.
    pub occurrences: BTreeMap<Date, Vec<Occurrence>>,
    pub sessions: Vec<Session>,
    /// Wake-ups the agent laid during the case.
    pub wakes_laid: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Node {
    pub id: i64,
    pub parent: Option<i64>,
    pub goal: Option<i64>,
    pub title: String,
    pub state: String,
    pub due: Option<Date>,
    pub minutes: Option<u32>,
    pub progress: u32,
    pub urgency: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Goal {
    pub id: i64,
    pub title: String,
    pub due: Option<Date>,
    pub state: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Entry {
    pub id: i64,
    pub title: String,
    pub kind: String,
    pub quiet: bool,
    pub start: Option<Time>,
    pub end: Option<Time>,
    /// Mon = 1 … Sun = 64; 0 for a one-off.
    pub days: i64,
    pub on_date: Option<Date>,
    pub until: Option<Date>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct DayItem {
    pub task_id: Option<i64>,
    pub kind: String,
    pub start: Option<Time>,
    pub end: Option<Time>,
    pub status: String,
    /// Place in the day's ordered list; `None` for a timed block or routine.
    pub position: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Occurrence {
    pub title: String,
    pub kind: String,
    pub start: Option<Time>,
    pub end: Option<Time>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Session {
    pub kind: String,
    pub outcome: String,
    pub turns: u64,
    pub error: Option<String>,
    pub calls: Vec<Call>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Call {
    pub name: String,
    pub args: Value,
    pub result: Value,
    pub is_error: bool,
}

impl Snapshot {
    pub fn node(&self, id: i64) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn children(&self, id: i64) -> Vec<&Node> {
        self.nodes.iter().filter(|n| n.parent == Some(id)).collect()
    }

    pub fn is_leaf(&self, id: i64) -> bool {
        !self.nodes.iter().any(|n| n.parent == Some(id) && n.open())
    }

    pub fn root_of(&self, id: i64) -> i64 {
        let mut at = id;
        while let Some(p) = self.node(at).and_then(|n| n.parent) {
            at = p;
        }
        at
    }

    /// The tasks directly under a goal: roots tied to a goal row, or the
    /// children of a node that stands as the goal.
    pub fn under_goal(&self, goal: GoalRef) -> Vec<&Node> {
        match goal {
            GoalRef::Row(id) => self.nodes.iter().filter(|n| n.parent.is_none() && n.goal == Some(id)).collect(),
            GoalRef::Node(id) => self.children(id),
        }
    }

    /// Every goal: goal rows, then root nodes that hold children and carry a
    /// due date, which is how a tree can stand for one.
    pub fn goal_refs(&self) -> Vec<(GoalRef, String, Option<Date>)> {
        let rows = self.goals.iter().filter(|g| g.state != "dropped").map(|g| (GoalRef::Row(g.id), g.title.clone(), g.due));
        let nodes = self
            .nodes
            .iter()
            .filter(|n| n.parent.is_none() && n.due.is_some() && !self.children(n.id).is_empty())
            .map(|n| (GoalRef::Node(n.id), n.title.clone(), n.due));
        rows.chain(nodes).collect()
    }

    /// Every task on a day: the ordered list first, then timed blocks.
    pub fn day_tasks(&self, date: Date) -> Vec<i64> {
        let mut out = Vec::new();
        let Some(items) = self.days.get(&date) else { return out };
        let mut listed: Vec<&DayItem> = items.iter().filter(|i| i.position.is_some()).collect();
        listed.sort_by_key(|i| i.position);
        let timed = items.iter().filter(|i| i.position.is_none() && i.status != "dropped");
        for i in listed.into_iter().chain(timed) {
            if let Some(t) = i.task_id {
                if !out.contains(&t) {
                    out.push(t);
                }
            }
        }
        out
    }

    pub fn blocks_for(&self, date: Date, task: i64) -> Vec<&DayItem> {
        self.days
            .get(&date)
            .map(|items| {
                items
                    .iter()
                    .filter(|i| i.task_id == Some(task) && i.start.is_some() && !matches!(i.status.as_str(), "dropped" | "moved"))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GoalRef {
    Row(i64),
    Node(i64),
}

impl Node {
    pub fn open(&self) -> bool {
        matches!(self.state.as_str(), "open" | "in_progress")
    }
}

pub fn fold(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn date_of(due_on: &Value, due_at: &Value, tz: &jiff::tz::TimeZone) -> Option<Date> {
    if let Some(d) = due_on.as_str().and_then(|s| s.parse::<Date>().ok()) {
        return Some(d);
    }
    due_at.as_str().and_then(|s| s.parse::<jiff::Timestamp>().ok()).map(|t| t.to_zoned(tz.clone()).date())
}

fn time_of(v: &Value) -> Option<Time> {
    v.as_str().and_then(|s| s.parse::<Time>().ok())
}

/// Flattens `GET /api/tasks`, whatever depth the `children` nest to.
pub fn nodes_from(tasks: &Value, tz: &jiff::tz::TimeZone) -> Vec<Node> {
    fn walk(v: &Value, parent: Option<i64>, tz: &jiff::tz::TimeZone, out: &mut Vec<Node>) {
        let Some(id) = v["id"].as_i64() else { return };
        let parent = v["parent_id"].as_i64().or(parent);
        out.push(Node {
            id,
            parent,
            goal: v["goal_id"].as_i64(),
            title: v["title"].as_str().unwrap_or_default().to_string(),
            state: v["state"].as_str().unwrap_or("open").to_string(),
            due: date_of(&v["due_on"], &v["due_at"], tz),
            minutes: v["duration_min"].as_u64().map(|m| m as u32),
            progress: v["progress"].as_u64().unwrap_or(0) as u32,
            urgency: v["urgency"].as_str().unwrap_or("normal").to_string(),
        });
        for c in v["children"].as_array().into_iter().flatten() {
            walk(c, Some(id), tz, out);
        }
    }
    let mut out = Vec::new();
    for t in tasks.as_array().into_iter().flatten() {
        walk(t, None, tz, &mut out);
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|n| seen.insert(n.id));
    out
}

pub fn goals_from(goals: &Value, tz: &jiff::tz::TimeZone) -> Vec<Goal> {
    goals
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| {
            Some(Goal {
                id: g["id"].as_i64()?,
                title: g["title"].as_str().unwrap_or_default().to_string(),
                due: date_of(&g["due_on"], &g["due_at"], tz),
                state: g["state"].as_str().unwrap_or("open").to_string(),
            })
        })
        .collect()
}

pub fn calendar_from(v: &Value) -> Vec<Entry> {
    v["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            Some(Entry {
                id: e["id"].as_i64()?,
                title: e["title"].as_str().unwrap_or_default().to_string(),
                kind: e["kind"].as_str().unwrap_or_default().to_string(),
                quiet: e["quiet"].as_bool().unwrap_or(false),
                start: time_of(&e["start_time"]),
                end: time_of(&e["end_time"]),
                days: e["days"].as_i64().unwrap_or(0),
                on_date: e["on_date"].as_str().and_then(|s| s.parse().ok()),
                until: e["until_date"].as_str().and_then(|s| s.parse().ok()),
            })
        })
        .collect()
}

/// A day's timed events from `GET /api/day/{date}`.
pub fn day_events_from(v: &Value) -> Vec<DayItem> {
    v["events"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| DayItem {
            task_id: e["task"]["id"].as_i64().or_else(|| e["task_id"].as_i64()),
            kind: e["kind"].as_str().unwrap_or_default().to_string(),
            start: time_of(&e["wall_time"]).or_else(|| time_of(&e["at"])),
            end: time_of(&e["end_wall_time"]),
            status: e["status"].as_str().unwrap_or("pending").to_string(),
            position: None,
        })
        .collect()
}

pub fn occurrences_from(v: &Value) -> Vec<Occurrence> {
    v["calendar"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| Occurrence {
            title: o["title"].as_str().unwrap_or_default().to_string(),
            kind: o["kind"].as_str().unwrap_or_default().to_string(),
            start: time_of(&o["start"]),
            end: time_of(&o["end"]),
        })
        .collect()
}

/// The agent sessions written after `after_trace`, from `agent_traces`.
pub fn sessions_from_db(conn: &rusqlite::Connection, after_trace: i64) -> Result<Vec<Session>> {
    let mut stmt = conn.prepare(
        "SELECT kind, outcome, turns, error, detail FROM agent_traces WHERE id > ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([after_trace], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, String>(4)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (kind, outcome, turns, error, detail) = row?;
        out.push(session_from(&kind, &outcome, turns as u64, error, &detail));
    }
    Ok(out)
}

pub fn session_from(kind: &str, outcome: &str, turns: u64, error: Option<String>, detail: &str) -> Session {
    let d: Value = serde_json::from_str(detail).unwrap_or(Value::Null);
    let parse = |s: &Value| -> Value {
        s.as_str().map_or(Value::Null, |t| serde_json::from_str(t).unwrap_or_else(|_| Value::String(t.to_string())))
    };
    let calls = d["rounds"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|r| r["calls"].as_array().cloned().unwrap_or_default())
        .map(|c| Call {
            name: c["name"].as_str().unwrap_or_default().to_string(),
            args: parse(&c["args"]),
            result: parse(&c["result"]),
            is_error: c["is_error"].as_bool().unwrap_or(false),
        })
        .collect();
    let usage = &d["usage"];
    let round_errors = d["rounds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["error"].as_str().map(String::from))
        .collect::<Vec<_>>();
    let error = error.or_else(|| round_errors.last().cloned());
    Session {
        kind: kind.to_string(),
        outcome: outcome.to_string(),
        turns,
        error,
        calls,
        prompt_tokens: usage["prompt"].as_u64().unwrap_or(0),
        completion_tokens: usage["completion"].as_u64().unwrap_or(0),
        cost: usage["cost"].as_f64().unwrap_or(0.0),
    }
}

pub fn max_id(conn: &rusqlite::Connection, table: &str) -> Result<i64> {
    Ok(conn.query_row(&format!("SELECT COALESCE(MAX(id), 0) FROM {table}"), [], |r| r.get(0))?)
}

/// The ordered list of each date, from `run_order`.
pub fn order_from_db(conn: &rusqlite::Connection, dates: &[Date]) -> Result<BTreeMap<Date, Vec<DayItem>>> {
    let mut out = BTreeMap::new();
    let has_table: i64 =
        conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'run_order'", [], |r| r.get(0))?;
    for d in dates {
        let mut items = Vec::new();
        if has_table > 0 {
            let mut stmt = conn.prepare(
                "SELECT o.position, o.task_id, t.state FROM run_order o JOIN tasks t ON t.id = o.task_id
                 WHERE o.date = ?1 ORDER BY o.position",
            )?;
            let rows = stmt.query_map([d.to_string()], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))?;
            for row in rows {
                let (position, task, state) = row?;
                items.push(DayItem { task_id: Some(task), kind: "list".into(), start: None, end: None, status: state, position: Some(position) });
            }
        }
        out.insert(*d, items);
    }
    Ok(out)
}

pub fn wakes_laid_after(conn: &rusqlite::Connection, after_event: i64) -> Result<usize> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE id > ?1 AND kind = 'trigger' AND origin = 'agent'",
        [after_event],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_children_flatten_at_any_depth() {
        let tz = jiff::tz::TimeZone::UTC;
        let tasks = serde_json::json!([
            { "id": 1, "title": "Course", "state": "open", "due_on": "2026-12-01", "due_at": "2026-12-01T23:59:59Z",
              "children": [
                { "id": 2, "parent_id": 1, "title": "Unit", "state": "open",
                  "children": [ { "id": 3, "parent_id": 2, "title": "Lesson", "state": "done", "duration_min": 30 } ] }
              ] }
        ]);
        let nodes = nodes_from(&tasks, &tz);
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[2].parent, Some(2));
        assert_eq!(nodes[0].due, Some(jiff::civil::date(2026, 12, 1)));
        let snap = Snapshot { nodes, ..Default::default() };
        assert_eq!(snap.root_of(3), 1);
        assert!(snap.is_leaf(2), "a node whose children are all done is a leaf");
    }

    #[test]
    fn a_due_moment_reads_as_the_users_local_date() {
        let tz = jiff::tz::TimeZone::get("Etc/GMT+8").unwrap();
        let d = date_of(&Value::Null, &serde_json::json!("2026-10-21T03:00:00Z"), &tz);
        assert_eq!(d, Some(jiff::civil::date(2026, 10, 20)));
    }

    #[test]
    fn a_trace_row_reads_into_calls_and_usage() {
        let detail = serde_json::json!({
            "opening": "", "reply": "ok",
            "rounds": [ { "ms": 1, "error": null, "usage": {}, "calls": [
                { "name": "task_create", "ms": 1, "is_error": true, "error_kind": "rejected",
                  "args": "{\"title\":\"x\"}", "result": "{\"kind\":\"rejected\"}" } ] } ],
            "usage": { "calls": 1, "prompt": 1200, "cached": 0, "completion": 80, "reasoning": 0, "cost": 0.0 }
        });
        let s = session_from("Talk", "ok", 1, None, &detail.to_string());
        assert_eq!(s.calls.len(), 1);
        assert!(s.calls[0].is_error);
        assert_eq!(s.calls[0].args["title"], "x");
        assert_eq!((s.prompt_tokens, s.completion_tokens), (1200, 80));
    }
}
