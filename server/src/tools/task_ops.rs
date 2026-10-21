use super::loose::{self, Envelope, Obj, Ref, Target, When};
use super::{SessionKind, ToolCtx, ToolError};
use crate::tasks::{self, Actor, NewTask, TaskPatch};
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

pub const MAX_NODES: usize = 100;
/// Nodes one `task_remove` deletes before it needs `all`.
pub const MAX_REMOVE: usize = 20;

/// Fields a model hands `task_save` that another tool takes.
const ELSEWHERE: &[(&str, &str)] = &[
    ("event_id", "day_save"),
    ("date", "day_save"),
    ("start", "day_save"),
    ("at", "day_save"),
    ("scheduled_at", "day_save"),
    ("prompt", "day_save (a wake item)"),
    ("wake", "day_save"),
];

pub(super) const NODE_FIELDS: &[&str] =
    &["id", "task_id", "title", "name", "parent", "parent_id", "minutes", "duration_min", "due", "due_at", "state"];

pub(super) fn user_tz(ctx: &ToolCtx) -> jiff::tz::TimeZone {
    crate::triggers::timezone(ctx.config_dir, ctx.username)
}

fn internal(e: impl std::fmt::Display) -> ToolError {
    ToolError::internal(e.to_string())
}

/// One node of a call, its fields read and normalised.
#[derive(Default)]
struct Node {
    r: Ref,
    target: Option<Target>,
    /// Given at all; `Some(None)` makes it a root.
    parent: Option<Option<Target>>,
    before: Option<Target>,
    title: Option<String>,
    /// The whole title, when it was cut.
    full_title: Option<String>,
    description: Option<String>,
    notes: Option<String>,
    minutes: Option<i64>,
    due: Option<Option<When>>,
    progress: Option<u32>,
    state: Option<&'static str>,
    urgency: Option<&'static str>,
    notify: Option<&'static str>,
    now: Option<bool>,
    category: Option<String>,
    goal_id: Option<Option<i64>>,
    url: Option<String>,
    new: bool,
    /// Landed on an open task by its title rather than by an id.
    matched: bool,
    children: Vec<Value>,
}

#[derive(Clone, Copy)]
enum Nest {
    Top,
    Under(i64),
}

/// The state of one `task_save` (or `goal_save`'s tasks) as it lands item by item.
pub(super) struct Saver<'a> {
    conn: &'a Connection,
    ctx: &'a ToolCtx<'a>,
    tz: jiff::tz::TimeZone,
    now: jiff::Timestamp,
    pub env: Envelope,
    refs: HashMap<i64, Ref>,
    count: usize,
    /// The running work session's leaf and everything above it, for a wake-up.
    held: Vec<i64>,
    /// The goal `goal_save` hangs its roots from, and whether this call made it.
    goal: Option<i64>,
    fresh_goal: bool,
}

/// A node built for creation, with the refs of everything under it in the
/// order it is inserted.
struct Built {
    new: NewTask,
    refs: Vec<(Ref, String)>,
    places: Vec<(usize, Target)>,
    deferred: Vec<(usize, Node)>,
}

impl<'a> Saver<'a> {
    pub fn new(conn: &'a Connection, ctx: &'a ToolCtx<'a>, kind: SessionKind) -> Result<Self, ToolError> {
        let held = if kind == SessionKind::Trigger { running_line(conn, ctx.user_id).map_err(internal)? } else { Vec::new() };
        Ok(Self {
            conn,
            ctx,
            tz: user_tz(ctx),
            now: jiff::Timestamp::now(),
            env: Envelope::default(),
            refs: HashMap::new(),
            count: 0,
            held,
            goal: None,
            fresh_goal: false,
        })
    }

    pub fn set_goal(&mut self, goal: Option<i64>, fresh: bool) {
        self.goal = goal;
        self.fresh_goal = fresh;
    }

    pub fn save(&mut self, items: Vec<(Ref, Value)>) -> Result<(), ToolError> {
        self.level(items, Nest::Top)
    }

    fn ignore(&mut self, r: &Ref, field: &str, why: impl Into<String>) {
        self.env.ignored(Some(r), field, why);
    }

    fn parse(&mut self, r: &Ref, v: Value) -> Option<Node> {
        let map = match v {
            Value::Object(m) => m,
            Value::String(s) if !s.trim().is_empty() => Map::from_iter([("title".to_string(), Value::String(s))]),
            _ => {
                self.env.refused(r, "a task is an object with a title, or a plain title");
                return None;
            }
        };
        self.count += 1;
        if self.count > MAX_NODES {
            self.env.refused(r, format!("at most {MAX_NODES} tasks per call, nested ones counted; send the rest in another call"));
            return None;
        }
        let mut obj = Obj::new(map);
        let mut node = Node { r: r.clone(), ..Node::default() };
        if let Some(v) = obj.take(&["id", "task_id"]) {
            match loose::target(&v) {
                Some(t) => node.target = Some(t),
                None => self.ignore(r, "id", "an id is a number like 41, or the exact title of one open task"),
            }
        }
        if let Some(v) = obj.take(&["parent", "parent_id", "parent_task_id"]) {
            node.parent = match &v {
                Value::Number(_) if loose::integer(&v) == Some(0) => Some(None),
                Value::String(s) if matches!(loose::fold(s).as_str(), "" | "0" | "root" | "none" | "top") => Some(None),
                _ => {
                    let parent = loose::target(&v);
                    if parent.is_none() {
                        self.ignore(r, "parent", "a parent is a task id or the exact title of one open task");
                    }
                    parent.map(Some)
                }
            };
        }
        if let Some(v) = obj.take(&["before", "before_id", "before_task_id"]) {
            match loose::text(&v).map(|s| loose::fold(&s)).as_deref() {
                Some("last" | "end" | "") => {}
                _ => node.before = loose::target(&v),
            }
        }
        if let Some(v) = obj.take(&["title", "name"]) {
            match loose::text(&v).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
                Some(t) => match loose::cut_title(&t) {
                    Some(cut) => {
                        self.env.adjusted(Some(r), "title", cut.clone().into(), "a title stops at 200 characters or 500 bytes; the whole text leads the description");
                        node.full_title = Some(t);
                        node.title = Some(cut);
                    }
                    None => node.title = Some(t),
                },
                None => self.ignore(r, "title", "a title is non-empty text"),
            }
        }
        for (field, names) in [("description", &["description", "desc", "details"][..]), ("notes", &["notes", "note"][..])] {
            if let Some(v) = obj.take(names) {
                let Some(mut text) = loose::text(&v) else {
                    self.ignore(r, field, "it is text");
                    continue;
                };
                if let Some(cut) = loose::truncate(&text, loose::MAX_TEXT_BYTES) {
                    self.env.warn(format!("{} {field} was cut to {} bytes", r.0, loose::MAX_TEXT_BYTES));
                    text = cut;
                }
                if field == "description" {
                    node.description = Some(text);
                } else {
                    node.notes = Some(text);
                }
            }
        }
        if let Some(v) = obj.take(&["minutes", "duration_min", "duration", "size", "estimate", "mins", "duration_minutes", "estimate_min"]) {
            match loose::minutes(&v) {
                Some(m) => node.minutes = Some(m),
                None => self.ignore(r, "minutes", "a size is minutes: 45, \"1h30\", \"90m\" or \"1:30\""),
            }
        }
        if let Some(v) = obj.take(&["due", "due_at", "due_on", "deadline", "due_date"]) {
            match loose::text(&v) {
                Some(s) if s.trim().is_empty() => node.due = Some(None),
                Some(s) => match loose::when(&s, &self.tz, self.now) {
                    Some(w) => {
                        if let Some(d) = loose::rolled(&s, self.now.to_zoned(self.tz.clone()).date()) {
                            self.env.adjusted(Some(r), "due", d.to_string().into(), loose::rolled_why(&s, d));
                        }
                        node.due = Some(Some(w));
                    }
                    None => self.ignore(r, "due", format!("a due is {} or {}", loose::DAY_FORMS, loose::MOMENT_FORMS)),
                },
                None => self.ignore(r, "due", "a due is text such as 2026-11-06, 11/6 or 2026-11-06T17:00"),
            }
        }
        if let Some(v) = obj.take(&["progress", "percent"]) {
            match loose::integer(&v) {
                Some(p) => {
                    let clamped = p.clamp(0, 100);
                    if clamped != p {
                        self.env.adjusted(Some(r), "progress", clamped.into(), "progress is 0 to 100");
                    }
                    node.progress = Some(clamped as u32);
                }
                None => self.ignore(r, "progress", "progress is a number 0 to 100"),
            }
        }
        if let Some(v) = obj.take(&["state", "status"]) {
            match loose::text(&v).and_then(|s| loose::choice(&s, tasks::STATES, loose::STATE_SYNONYMS)) {
                Some(s) => node.state = Some(s),
                None => self.ignore(r, "state", "a state is open, in_progress, done or dropped"),
            }
        }
        let mut finished = Vec::new();
        if let Some(v) = obj.take(&["done", "completed", "finished"]) {
            match loose::boolean(&v) {
                Some(d) if node.state.is_none() => node.state = Some(if d { "done" } else { "open" }),
                Some(_) => {}
                None => finished = loose::list(v),
            }
        }
        if let Some(v) = obj.take(&["urgency", "priority"]) {
            match loose::text(&v).and_then(|s| loose::choice(&s, tasks::URGENCY, loose::URGENCY_SYNONYMS)) {
                Some(u) => node.urgency = Some(u),
                None => self.ignore(r, "urgency", "urgency is low, normal or high"),
            }
        }
        if let Some(v) = obj.take(&["notify"]) {
            match loose::text(&v).and_then(|s| loose::choice(&s, tasks::NOTIFY, loose::NOTIFY_SYNONYMS)) {
                Some(x) => node.notify = Some(x),
                None => self.ignore(r, "notify", "notify is none, chat or notify"),
            }
        }
        if let Some(v) = obj.take(&["now", "is_now", "in_now"]) {
            match loose::boolean(&v) {
                Some(b) => node.now = Some(b),
                None => self.ignore(r, "now", "now is true or false"),
            }
        }
        if let Some(v) = obj.take(&["category"]) {
            match loose::text(&v) {
                Some(c) => {
                    let c = c.trim().to_string();
                    match loose::truncate(&c, tasks::MAX_CATEGORY_BYTES) {
                        Some(cut) => {
                            self.env.adjusted(Some(r), "category", cut.clone().into(), "a category is at most 100 bytes");
                            node.category = Some(cut);
                        }
                        None => node.category = Some(c),
                    }
                }
                None => self.ignore(r, "category", "a category is text"),
            }
        }
        if let Some(v) = obj.take(&["goal_id", "goal"]) {
            node.goal_id = self.goal_of(r, &v);
        }
        if let Some(v) = obj.take(&["url", "link"]) {
            match loose::text(&v).filter(|u| u.len() <= 2048) {
                Some(u) => node.url = Some(u),
                None => self.ignore(r, "url", "a url is text of at most 2048 bytes"),
            }
        }
        if let Some(v) = obj.take(&["new", "also"]) {
            node.new = loose::boolean(&v).unwrap_or(false);
        }
        if let Some(v) = obj.take(&["children", "steps", "subtasks", "items", "tasks", "parts"]) {
            node.children = loose::list(v);
        }
        for part in finished {
            match loose::text(&part).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
                Some(title) => node.children.push(json!({ "title": title, "state": "done" })),
                None => self.ignore(r, "done", "done lists the titles of parts already finished"),
            }
        }
        for field in obj.rest() {
            let why = loose::unknown(&field, ELSEWHERE);
            self.ignore(r, &field, why);
        }
        Some(node)
    }

    fn goal_of(&mut self, r: &Ref, v: &Value) -> Option<Option<i64>> {
        if loose::integer(v) == Some(0) || v.as_str().is_some_and(|s| s.trim().is_empty()) {
            return Some(None);
        }
        let mine = |id: i64| crate::goals::get(self.conn, self.ctx.user_id, id).ok().flatten().is_some();
        match loose::target(v) {
            Some(Target::Id(id)) if mine(id) => Some(Some(id)),
            Some(Target::Id(id)) => {
                self.ignore(r, "goal_id", format!("no goal {id}"));
                None
            }
            Some(Target::Title(t)) => {
                let open: Vec<(i64, String)> = crate::goals::list(self.conn, self.ctx.user_id, None)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|g| (g.id, g.title))
                    .collect();
                match loose::by_title(&t, &open, "goal") {
                    Ok(id) => Some(Some(id)),
                    Err(why) => {
                        self.ignore(r, "goal_id", why);
                        None
                    }
                }
            }
            None => {
                self.ignore(r, "goal_id", "a goal is its id");
                None
            }
        }
    }

    /// Open tasks under `under` (`None`: the roots), or anywhere when `anywhere`.
    fn open_under(&self, under: Option<i64>, anywhere: bool) -> Result<Vec<(i64, String)>, ToolError> {
        if anywhere {
            return open_tasks(self.conn, self.ctx.user_id);
        }
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id, title FROM tasks WHERE user_id = ?1 AND state IN ('open','in_progress') AND parent_id IS ?2 ORDER BY id")
            .map_err(internal)?;
        let rows = stmt
            .query_map((self.ctx.user_id, under), |r| Ok((r.get(0)?, r.get(1)?)))
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(internal)?;
        Ok(rows)
    }

    /// The open tasks a create titled `title` lands on instead: siblings under
    /// `under`, or roots of the same goal and category.
    fn same_title(&self, title: &str, under: Option<i64>, category: Option<&str>) -> Result<Vec<i64>, ToolError> {
        let want = loose::fold(title);
        if under.is_some() {
            return Ok(self.open_under(under, false)?.into_iter().filter(|(_, t)| loose::fold(t) == want).map(|(id, _)| id).collect());
        }
        if self.fresh_goal {
            return Ok(Vec::new());
        }
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT id, title, goal_id, category FROM tasks
                 WHERE user_id = ?1 AND state IN ('open','in_progress') AND parent_id IS NULL ORDER BY id",
            )
            .map_err(internal)?;
        let roots: Vec<(i64, String, Option<i64>, String)> = stmt
            .query_map([self.ctx.user_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .and_then(|rows| rows.collect::<rusqlite::Result<_>>())
            .map_err(internal)?;
        let category = category.map(loose::fold).filter(|c| !c.is_empty());
        Ok(roots
            .into_iter()
            .filter(|(_, t, goal, cat)| {
                loose::fold(t) == want
                    && self.goal.is_none_or(|g| *goal == Some(g))
                    && (cat.is_empty() || category.as_ref().is_none_or(|c| loose::fold(cat) == *c))
            })
            .map(|(id, ..)| id)
            .collect())
    }

    /// `ids` as a refusal lists them, each with its path from its root.
    fn labeled(&self, ids: &[i64]) -> Result<String, ToolError> {
        labeled(self.conn, self.ctx.user_id, ids)
    }

    fn resolve(&mut self, r: &Ref, t: &Target, under: Option<i64>, anywhere: bool) -> Result<Option<i64>, ToolError> {
        match t {
            Target::Id(id) => {
                if tasks::get(self.conn, self.ctx.user_id, *id).map_err(internal)?.is_some() {
                    return Ok(Some(*id));
                }
                self.env.refused(r, format!("no task {id}"));
                Ok(None)
            }
            Target::Title(title) => {
                let open = self.open_under(under, anywhere)?;
                match loose::by_title_listed(title, &open, "task", |ids| listed(self.conn, self.ctx.user_id, ids)) {
                    Ok(id) => {
                        self.env.adjusted(Some(r), "id", id.into(), format!("{title:?} is task {id}"));
                        Ok(Some(id))
                    }
                    Err(why) => {
                        self.env.refused(r, why);
                        Ok(None)
                    }
                }
            }
        }
    }

    fn in_scope(&self, id: i64) -> Result<bool, ToolError> {
        let Some(scope) = self.ctx.task_scope else { return Ok(true) };
        if id == scope {
            return Ok(true);
        }
        Ok(ancestors(self.conn, id).map_err(internal)?.contains(&scope))
    }

    fn level(&mut self, items: Vec<(Ref, Value)>, nest: Nest) -> Result<(), ToolError> {
        let mut creates: Vec<(Option<i64>, Node)> = Vec::new();
        for (r, v) in items {
            let Some(mut node) = self.parse(&r, v) else { continue };
            let explicit = match (nest, node.parent.take()) {
                (Nest::Under(_), Some(_)) => {
                    self.ignore(&r, "parent", "it is nested under its parent in this call");
                    None
                }
                (_, given) => given,
            };
            let parent_given: Option<Option<i64>> = match explicit {
                None => None,
                Some(None) => Some(None),
                Some(Some(t)) => match self.resolve(&r, &t, None, true)? {
                    Some(id) => Some(Some(id)),
                    None => continue,
                },
            };
            let under: Option<Option<i64>> = match nest {
                Nest::Under(p) => Some(Some(p)),
                Nest::Top => parent_given,
            };
            let target = match node.target.take() {
                Some(t) => {
                    let (scope, anywhere) = match under {
                        Some(u) => (u, false),
                        None => (None, true),
                    };
                    match self.resolve(&r, &t, scope, anywhere)? {
                        Some(id) => Some(id),
                        None => continue,
                    }
                }
                None => match (&node.title, node.new) {
                    (Some(title), false) => match self.same_title(title, under.flatten(), node.category.as_deref())?.as_slice() {
                        [] => None,
                        [one] => {
                            node.matched = true;
                            Some(*one)
                        }
                        many => {
                            let listed = self.labeled(many)?;
                            self.env.refused(&r, format!("{} open tasks are titled {title:?}: {listed}; give the id, or new: true", many.len()));
                            continue;
                        }
                    },
                    _ => None,
                },
            };
            match target {
                Some(id) => {
                    if !self.in_scope(id)? {
                        self.env.refused(&r, format!("this session may only change task {} and its parts", self.ctx.task_scope.unwrap_or_default()));
                        continue;
                    }
                    self.update(node, id, under)?;
                }
                None if node.title.is_none() => self.env.refused(&r, "a new task needs a title; to change one, give its id"),
                None => {
                    let parent = under.flatten();
                    let allowed = match parent {
                        Some(p) => self.in_scope(p)?,
                        None => self.ctx.task_scope.is_none(),
                    };
                    if !allowed {
                        self.env.refused(&r, "this session may only add parts to its own task");
                        continue;
                    }
                    creates.push((parent, node));
                }
            }
        }
        self.create(creates)
    }

    fn report(&mut self, adjusted: Vec<tasks::Adjusted>) {
        for a in adjusted {
            let field = match a.field {
                "duration_min" => "minutes",
                "is_now" => "now",
                f => f,
            };
            let mut v = json!({ "id": a.id, "title": a.title, "field": field, "to": a.to, "why": a.why });
            if let Some(r) = self.refs.get(&a.id) {
                v["ref"] = r.0.clone().into();
            }
            if let Some(by) = a.by_title {
                v["by"] = by.into();
            }
            self.env.adjusted.push(v);
        }
    }

    fn demoted(&mut self, ids: Vec<i64>) -> Result<(), ToolError> {
        for id in ids {
            let title = tasks::get(self.conn, self.ctx.user_id, id).map_err(internal)?.map(|t| t.title).unwrap_or_default();
            let mut v = json!({ "id": id, "title": title, "field": "now", "to": false, "why": "Now holds at most 3; this one came out of Now" });
            if let Some(r) = self.refs.get(&id) {
                v["ref"] = r.0.clone().into();
            }
            self.env.adjusted.push(v);
        }
        Ok(())
    }

    fn update(&mut self, n: Node, id: i64, under: Option<Option<i64>>) -> Result<(), ToolError> {
        let r = n.r.clone();
        let Some(before) = tasks::get(self.conn, self.ctx.user_id, id).map_err(internal)? else {
            self.env.refused(&r, format!("no task {id}"));
            return Ok(());
        };
        let moving = under.filter(|p| *p != before.parent_id);
        if self.held.contains(&id) && (matches!(n.state, Some("done" | "dropped")) || moving.is_some()) {
            self.env.refused(&r, format!("task {id} is in the work session running now; leave it until the session ends"));
            return Ok(());
        }
        let parent_after = moving.unwrap_or(before.parent_id);
        let kids: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE parent_id = ?1 AND state != 'dropped'", [id], |x| x.get(0))
            .map_err(internal)?;
        let is_parent = kids > 0 || !n.children.is_empty();
        let mut p = TaskPatch { actor: Actor::Agent, zone: Some(self.tz.clone()), ..TaskPatch::default() };
        let mut changed = false;
        if let Some(t) = n.title {
            changed |= t != before.title;
            p.title = Some(t);
        }
        if let Some(full) = n.full_title {
            let rest = n.description.clone().unwrap_or_else(|| before.description.clone());
            p.description = Some(if rest.is_empty() { full } else { format!("{full}\n\n{rest}") });
            changed = true;
        } else if let Some(d) = n.description {
            changed |= d != before.description;
            p.description = Some(d);
        }
        if let Some(x) = n.notes {
            changed |= x != before.notes;
            p.notes = Some(x);
        }
        if let Some(m) = n.minutes {
            if m <= 0 {
                if is_parent {
                    self.ignore(&r, "minutes", "a parent's size is the sum of its children's");
                } else {
                    p.duration_min = Some(None);
                    changed = true;
                }
            } else {
                let rounded = loose::round_minutes(m);
                if rounded != m {
                    self.env.adjusted(Some(&r), "minutes", rounded.into(), "a size is whole 5 minutes, 5 to 1440");
                }
                p.duration_min = Some(Some(rounded as u32));
                changed = true;
            }
        }
        if let Some(due) = n.due {
            match due {
                None => p.due_at = Some(None),
                Some(When::Day(d)) => p.due_on = Some(Some(d.to_string())),
                Some(When::At(t)) => p.due_at = Some(Some(t.to_string())),
            }
            changed = true;
        }
        if let Some(x) = n.progress {
            p.progress = Some(x);
            changed = true;
        }
        if let Some(s) = n.state {
            match self.ctx.task_scope {
                Some(_) if s != "dropped" => self.ignore(&r, "state", "this session may only drop a task"),
                Some(_) if before.state != "open" => self.ignore(&r, "state", format!("task {id} is already {}; only an open task can be dropped", before.state)),
                _ => {
                    changed |= s != before.state;
                    p.state = Some(s.to_string());
                }
            }
        }
        if let Some(u) = n.urgency {
            changed |= u != before.urgency;
            p.urgency = Some(u.to_string());
        }
        if let Some(x) = n.notify {
            p.notify = Some(x.to_string());
            changed = true;
        }
        if let Some(now) = n.now {
            if self.ctx.task_scope.is_some() {
                self.ignore(&r, "now", "this session cannot move a task in or out of Now");
            } else {
                changed |= now != before.is_now;
                p.is_now = Some(now);
            }
        }
        if let Some(c) = n.category {
            if parent_after.is_some() {
                self.ignore(&r, "category", "a child reads its root's category");
            } else {
                changed |= c != before.category;
                p.category = Some(c);
            }
        }
        let goal = match (self.goal, parent_after) {
            (Some(g), None) => Some(Some(g)),
            _ => n.goal_id,
        };
        if let Some(g) = goal {
            if parent_after.is_some() {
                self.ignore(&r, "goal_id", "a child reads its root's goal");
            } else {
                changed |= g != before.goal_id;
                p.goal_id = Some(g);
            }
        }
        if let Some(u) = n.url {
            p.url = Some(u);
            changed = true;
        }
        if let Some(m) = moving {
            p.parent_id = Some(m);
            changed = true;
        }
        let op = if changed { "updated" } else { "unchanged" };
        let closing = p.state.as_deref().filter(|s| matches!(*s, "done" | "dropped") && *s != before.state).map(str::to_string);
        let open_parts = match &closing {
            Some(_) => open_below(self.conn, id).map_err(internal)?,
            None => 0,
        };
        let above = states(self.conn, &ancestors(self.conn, id).map_err(internal)?).map_err(internal)?;
        match tasks::update(self.conn, self.ctx.user_id, id, &p) {
            Ok(Some(u)) => {
                self.refs.insert(id, r.clone());
                let row = self.env.saved(&r, id, &u.task.title, op);
                if n.matched {
                    row["matched"] = "title".into();
                }
                self.report(u.adjusted);
                self.demoted(u.demoted_from_now)?;
                if let (Some(s), true) = (&closing, open_parts > 0) {
                    let why = format!("{open_parts} open part{} went {s} with it", if open_parts == 1 { "" } else { "s" });
                    self.env.adjusted.push(json!({ "ref": r.0, "id": id, "title": u.task.title, "field": "children", "to": s, "why": why }));
                }
                self.settled(&above)?;
            }
            Ok(None) => {
                self.env.refused(&r, format!("no task {id}"));
                return Ok(());
            }
            Err(tasks::UpdateError::Db(e)) => return Err(internal(e)),
            Err(e) => {
                self.env.refused(&r, e.to_string());
                return Ok(());
            }
        }
        if let Some(b) = n.before {
            self.place(&r, id, &b)?;
        }
        let children = n.children.into_iter().enumerate().map(|(i, v)| (r.child(i), v)).collect();
        self.level(children, Nest::Under(id))
    }

    /// Each ancestor whose state its parts moved, against what it was.
    fn settled(&mut self, above: &[(i64, String, String)]) -> Result<(), ToolError> {
        let ids: Vec<i64> = above.iter().map(|(id, ..)| *id).collect();
        for ((id, title, was), (_, _, now)) in above.iter().zip(states(self.conn, &ids).map_err(internal)?) {
            if *was == now {
                continue;
            }
            let why = match now.as_str() {
                "done" => "its last open part finished",
                "in_progress" if was == "open" => "a part of it is under way",
                _ => "a part of it is open again",
            };
            self.env.adjusted.push(json!({ "id": id, "title": title, "field": "state", "to": now, "why": why }));
        }
        Ok(())
    }

    /// Puts `id` in front of its sibling `before`.
    fn place(&mut self, r: &Ref, id: i64, before: &Target) -> Result<(), ToolError> {
        let parent: Option<i64> = self
            .conn
            .query_row("SELECT parent_id FROM tasks WHERE id = ?1", [id], |x| x.get(0))
            .map_err(internal)?;
        let Some(parent) = parent else {
            self.ignore(r, "before", "top-level tasks keep no order of their own; the day's order is day_save's");
            return Ok(());
        };
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id, title FROM tasks WHERE parent_id = ?1 AND id != ?2 AND state != 'dropped' ORDER BY position, id")
            .map_err(internal)?;
        let sibs: Vec<(i64, String)> = stmt
            .query_map((parent, id), |x| Ok((x.get(0)?, x.get(1)?)))
            .and_then(|rows| rows.collect::<rusqlite::Result<_>>())
            .map_err(internal)?;
        drop(stmt);
        let found = match before {
            Target::Id(b) => sibs.iter().position(|(s, _)| s == b),
            Target::Title(t) => loose::by_title(t, &sibs, "sibling").ok().and_then(|b| sibs.iter().position(|(s, _)| *s == b)),
        };
        let Some(index) = found else {
            self.ignore(r, "before", "it is not a sibling of this task");
            return Ok(());
        };
        let patch = TaskPatch { position: Some(index), actor: Actor::Agent, ..TaskPatch::default() };
        tasks::update(self.conn, self.ctx.user_id, id, &patch).map_err(|e| internal(e.to_string()))?;
        Ok(())
    }

    fn build(&mut self, n: Node, root: bool, b: &mut Built) -> NewTask {
        let r = n.r.clone();
        let title = n.title.clone().unwrap_or_default();
        b.refs.push((r.clone(), title.clone()));
        let me = b.refs.len() - 1;
        if let Some(t) = n.before.clone() {
            b.places.push((me, t));
        }
        let mut new = NewTask { title, zone: Some(self.tz.clone()), ..NewTask::default() };
        new.description = match (n.full_title, n.description) {
            (Some(full), Some(d)) => Some(format!("{full}\n\n{d}")),
            (Some(full), None) => Some(full),
            (None, d) => d,
        };
        new.notes = n.notes;
        if let Some(m) = n.minutes.filter(|m| *m > 0) {
            let rounded = loose::round_minutes(m);
            if rounded != m {
                self.env.adjusted(Some(&r), "minutes", rounded.into(), "a size is whole 5 minutes, 5 to 1440");
            }
            new.duration_min = Some(rounded as u32);
        }
        match n.due {
            Some(Some(When::Day(d))) => new.due_on = Some(Some(d.to_string())),
            Some(Some(When::At(t))) => new.due_at = Some(Some(t.to_string())),
            _ => {}
        }
        new.progress = n.progress;
        new.state = n.state.map(str::to_string);
        new.urgency = n.urgency.map(str::to_string);
        new.notify = n.notify.map(str::to_string);
        new.url = n.url;
        if let Some(now) = n.now {
            if self.ctx.task_scope.is_some() {
                self.ignore(&r, "now", "this session cannot move a task in or out of Now");
            } else {
                new.is_now = now;
            }
        }
        if let Some(c) = n.category {
            if root {
                new.category = Some(c);
            } else {
                self.ignore(&r, "category", "a child reads its root's category");
            }
        }
        let goal = if root { self.goal.map(Some).or(n.goal_id) } else { n.goal_id };
        if let Some(g) = goal {
            if root {
                new.goal_id = g;
            } else {
                self.ignore(&r, "goal_id", "a child reads its root's goal");
            }
        }
        for (i, v) in n.children.into_iter().enumerate() {
            let cr = r.child(i);
            let Some(mut child) = self.parse(&cr, v) else { continue };
            if child.parent.take().is_some() {
                self.ignore(&cr, "parent", "it is nested under its parent in this call");
            }
            if child.target.is_some() {
                b.deferred.push((me, child));
            } else if child.title.is_none() {
                self.env.refused(&cr, "a new task needs a title; to change one, give its id");
            } else {
                let built = self.build(child, false, b);
                new.children.push(built);
            }
        }
        new
    }

    fn create(&mut self, creates: Vec<(Option<i64>, Node)>) -> Result<(), ToolError> {
        let mut groups: Vec<(Option<i64>, Vec<Built>)> = Vec::new();
        for (parent, n) in creates {
            let mut parent = parent;
            if let Some(p) = parent {
                let at = tasks::get(self.conn, self.ctx.user_id, p).map_err(internal)?;
                if let Some(at) = at.filter(|t| t.depth >= tasks::MAX_DEPTH) {
                    self.env.adjusted(Some(&n.r), "depth", tasks::MAX_DEPTH.into(), format!("nothing sits more than 4 levels below its root, so it went beside {:?}", at.title));
                    parent = at.parent_id;
                }
            }
            let mut b = Built { new: NewTask::default(), refs: Vec::new(), places: Vec::new(), deferred: Vec::new() };
            b.new = self.build(n, parent.is_none(), &mut b);
            match groups.iter_mut().find(|(g, _)| *g == parent) {
                Some((_, list)) => list.push(b),
                None => groups.push((parent, vec![b])),
            }
        }
        for (parent, list) in groups {
            self.create_group(parent, list)?;
        }
        Ok(())
    }

    fn create_group(&mut self, parent: Option<i64>, list: Vec<Built>) -> Result<(), ToolError> {
        let news: Vec<NewTask> = list.iter().map(|b| clone_new(&b.new)).collect();
        match tasks::create_group(self.conn, self.ctx.user_id, parent, news, "agent", Actor::Agent) {
            Ok((ids, adjusted)) => {
                let mut ids = ids.into_iter();
                let mut landed = Vec::new();
                for b in list {
                    let made: Vec<i64> = ids.by_ref().take(b.refs.len()).collect();
                    for ((r, title), id) in b.refs.iter().zip(&made) {
                        self.refs.insert(*id, r.clone());
                        self.env.saved(r, *id, title, "created");
                    }
                    landed.push((b, made));
                }
                self.report(adjusted);
                for (b, made) in landed {
                    for (at, t) in &b.places {
                        if let (Some(id), Some((r, _))) = (made.get(*at), b.refs.get(*at)) {
                            self.place(&r.clone(), *id, t)?;
                        }
                    }
                    for (at, child) in b.deferred {
                        let Some(&under) = made.get(at) else { continue };
                        self.adopt(child, under)?;
                    }
                }
                Ok(())
            }
            Err(tasks::UpdateError::Db(e)) => Err(internal(e)),
            Err(_) if list.len() == 1 && !list[0].new.children.is_empty() => {
                let (root, kids) = split(list.into_iter().next().expect("one tree"));
                let r = root.refs[0].0.clone();
                let before = self.env.saved.len();
                self.create_group(parent, vec![root])?;
                let made = self.env.saved[before..].iter().find(|x| x["ref"] == r.0).and_then(|x| x["id"].as_i64());
                for kid in kids {
                    match made {
                        Some(id) => self.create_group(Some(id), vec![kid])?,
                        None => self.env.refused(&kid.refs[0].0, "its parent was refused"),
                    }
                }
                Ok(())
            }
            Err(e) if list.len() == 1 => {
                let r = list[0].refs.first().map(|(r, _)| r.clone()).unwrap_or_default();
                self.env.refused(&r, e.to_string());
                Ok(())
            }
            Err(_) => {
                for b in list {
                    self.create_group(parent, vec![b])?;
                }
                Ok(())
            }
        }
    }

    /// An existing node nested under one this call made: moved there.
    fn adopt(&mut self, mut child: Node, under: i64) -> Result<(), ToolError> {
        let r = child.r.clone();
        let Some(t) = child.target.take() else { return Ok(()) };
        let Some(id) = self.resolve(&r, &t, None, true)? else { return Ok(()) };
        self.update(child, id, Some(Some(under)))
    }
}

fn nodes_in(n: &NewTask) -> usize {
    1 + n.children.iter().map(nodes_in).sum::<usize>()
}

/// A tree cut into its root alone and one tree per child, each keeping the
/// refs, placings and adoptions that fall inside it.
fn split(b: Built) -> (Built, Vec<Built>) {
    let Built { mut new, refs, places, deferred } = b;
    let children = std::mem::take(&mut new.children);
    let mut spans = Vec::with_capacity(children.len());
    let mut at = 1;
    for c in &children {
        spans.push(at..at + nodes_in(c));
        at += nodes_in(c);
    }
    let bucket = |i: usize| spans.iter().position(|s| s.contains(&i));
    let mut kids: Vec<Built> = children
        .into_iter()
        .zip(&spans)
        .map(|(c, s)| Built { new: c, refs: refs[s.clone()].to_vec(), places: Vec::new(), deferred: Vec::new() })
        .collect();
    let mut root = Built { new, refs: refs[..1].to_vec(), places: Vec::new(), deferred: Vec::new() };
    for (i, t) in places {
        match bucket(i) {
            Some(k) => kids[k].places.push((i - spans[k].start, t)),
            None => root.places.push((i, t)),
        }
    }
    for (i, n) in deferred {
        match bucket(i) {
            Some(k) => kids[k].deferred.push((i - spans[k].start, n)),
            None => root.deferred.push((i, n)),
        }
    }
    (root, kids)
}

fn clone_new(n: &NewTask) -> NewTask {
    NewTask {
        title: n.title.clone(),
        description: n.description.clone(),
        notes: n.notes.clone(),
        duration_min: n.duration_min,
        parent_id: n.parent_id,
        is_now: n.is_now,
        state: n.state.clone(),
        due_at: n.due_at.clone(),
        due_on: n.due_on.clone(),
        url: n.url.clone(),
        external_id: n.external_id.clone(),
        source: n.source.clone(),
        notify: n.notify.clone(),
        progress: n.progress,
        category: n.category.clone(),
        goal_id: n.goal_id,
        urgency: n.urgency.clone(),
        children: n.children.iter().map(clone_new).collect(),
        zone: n.zone.clone(),
    }
}

fn listed(conn: &Connection, user_id: i64, ids: &[i64]) -> String {
    labeled(conn, user_id, ids).unwrap_or_else(|_| ids.iter().map(i64::to_string).collect::<Vec<_>>().join(", "))
}

/// `16 (Unit 1 › Lesson 2)` for a part, `14 (Visual Arts)` for a root under a
/// category or goal, the bare id otherwise.
pub(super) fn labeled(conn: &Connection, user_id: i64, ids: &[i64]) -> Result<String, ToolError> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let path = super::task_query::path_of(conn, user_id, *id)?;
        let owner = || {
            tasks::get(conn, user_id, *id)
                .ok()
                .flatten()
                .and_then(|t| Some(t.category).filter(|c| !c.is_empty()).or(t.goal_title))
        };
        out.push(match (path.len() > 1, owner()) {
            (true, _) => format!("{id} ({})", path.join(" › ")),
            (false, Some(o)) => format!("{id} ({o})"),
            (false, None) => id.to_string(),
        });
    }
    Ok(out.join(", "))
}

fn open_below(conn: &Connection, id: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "WITH RECURSIVE sub(id) AS (SELECT id FROM tasks WHERE parent_id = ?1
             UNION ALL SELECT c.id FROM tasks c JOIN sub ON c.parent_id = sub.id)
         SELECT COUNT(*) FROM tasks WHERE id IN sub AND state IN ('open','in_progress')",
        [id],
        |r| r.get(0),
    )
}

fn states(conn: &Connection, ids: &[i64]) -> rusqlite::Result<Vec<(i64, String, String)>> {
    ids.iter()
        .map(|id| conn.query_row("SELECT id, title, state FROM tasks WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))))
        .collect()
}

fn ancestors(conn: &Connection, id: i64) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "WITH RECURSIVE up(id) AS (SELECT parent_id FROM tasks WHERE id = ?1
             UNION ALL SELECT t.parent_id FROM tasks t JOIN up ON t.id = up.id WHERE t.parent_id IS NOT NULL)
         SELECT id FROM up WHERE id IS NOT NULL",
    )?;
    let rows = stmt.query_map([id], |r| r.get(0))?;
    rows.collect()
}

/// The running work session's leaf and every node above it.
pub(super) fn running_line(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<i64>> {
    let Some(s) = crate::work::open(conn, user_id)? else { return Ok(Vec::new()) };
    let Some(leaf) = s.leaf_id.or(s.task_id) else { return Ok(Vec::new()) };
    let mut out = vec![leaf];
    out.extend(ancestors(conn, leaf)?);
    Ok(out)
}

/// The nodes a call carries, each with its ref.
pub(super) fn nodes(o: &mut Obj, key: &str, aliases: &[&str]) -> (Vec<(Ref, Value)>, Option<String>) {
    let (list, warning) = loose::items(o, key, aliases, NODE_FIELDS);
    (list.into_iter().enumerate().map(|(i, v)| (Ref::top(i), v)).collect(), warning)
}

/// `task_save`.
pub fn save(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, raw: &str) -> Result<Value, ToolError> {
    let mut o = Obj::new(loose::args(raw)?);
    let (items, warning) = nodes(&mut o, "tasks", &["items", "nodes", "children", "steps", "list", "data"]);
    let mut saver = Saver::new(conn, ctx, kind)?;
    if let Some(w) = warning {
        saver.env.warn(w);
    }
    let rest = o.rest();
    for field in &rest {
        saver.env.ignored(None, field, loose::unknown(field, ELSEWHERE));
    }
    if items.is_empty() {
        if rest.is_empty() {
            saver.env.warn("nothing to save: tasks was empty");
        } else {
            saver.env.refuse_call("nothing was saved: the tasks go in tasks, a list");
        }
        return Ok(saver.env.into_value());
    }
    saver.save(items)?;
    Ok(saver.env.into_value())
}

/// `task_remove`.
pub fn remove(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, raw: &str) -> Result<Value, ToolError> {
    let mut o = Obj::new(loose::args(raw)?);
    let mut env = Envelope::default();
    let all = o.take(&["all", "force", "confirm"]).and_then(|v| loose::boolean(&v)).unwrap_or(false);
    let ids = loose::ids(&mut o, &["ids", "id", "task_ids", "task_id", "tasks", "items"], &mut env);
    for field in o.rest() {
        env.ignored(None, &field, "not a task_remove field");
    }
    if ids.is_empty() {
        loose::nothing_removed(&mut env);
        return Ok(env.into_value());
    }
    let held = if kind == SessionKind::Trigger { running_line(conn, ctx.user_id).map_err(internal)? } else { Vec::new() };
    let mut picked: Vec<(Ref, i64, String)> = Vec::new();
    for (i, v) in ids.into_iter().enumerate() {
        let r = Ref::top(i);
        let id = match loose::target(&v) {
            Some(Target::Id(id)) => id,
            Some(Target::Title(t)) => {
                let open = open_tasks(conn, ctx.user_id)?;
                match loose::by_title_listed(&t, &open, "task", |ids| listed(conn, ctx.user_id, ids)) {
                    Ok(id) => {
                        env.adjusted(Some(&r), "id", id.into(), format!("{t:?} is task {id}"));
                        id
                    }
                    Err(why) => {
                        env.refused(&r, why);
                        continue;
                    }
                }
            }
            None => {
                env.refused(&r, "an id is a number like 41, or the exact title of one open task");
                continue;
            }
        };
        let Some(task) = tasks::get(conn, ctx.user_id, id).map_err(internal)? else {
            env.refused(&r, format!("no task {id}"));
            continue;
        };
        if let Some(scope) = ctx.task_scope {
            if id != scope && !ancestors(conn, id).map_err(internal)?.contains(&scope) {
                env.refused(&r, format!("this session may only change task {scope} and its parts"));
                continue;
            }
        }
        if held.contains(&id) {
            env.refused(&r, format!("task {id} is in the work session running now; leave it until the session ends"));
            continue;
        }
        if !picked.iter().any(|(_, p, _)| *p == id) {
            picked.push((r, id, task.title));
        }
    }
    let subtree = |id: i64| -> Result<Vec<i64>, ToolError> {
        let mut stmt = conn
            .prepare_cached("WITH RECURSIVE sub(id) AS (SELECT ?1 UNION ALL SELECT c.id FROM tasks c JOIN sub ON c.parent_id = sub.id) SELECT id FROM sub")
            .map_err(internal)?;
        let rows = stmt.query_map([id], |r| r.get(0)).and_then(|rows| rows.collect::<rusqlite::Result<Vec<i64>>>()).map_err(internal)?;
        Ok(rows)
    };
    let mut every = std::collections::HashSet::new();
    let mut sizes = Vec::new();
    for (_, id, _) in &picked {
        let nodes = subtree(*id)?;
        sizes.push(nodes.len());
        every.extend(nodes);
    }
    if every.len() > MAX_REMOVE && !all {
        for (r, id, _) in &picked {
            env.refused(r, format!(
                "this call would delete {} tasks, parts included; pass all: true when that is meant, or set state dropped with task_save to keep them (task {id})",
                every.len()
            ));
        }
        return Ok(env.into_value());
    }
    let mut gone = std::collections::HashSet::new();
    for ((_, id, title), size) in picked.into_iter().zip(sizes) {
        if gone.contains(&id) {
            continue;
        }
        gone.extend(subtree(id)?);
        if tasks::delete_within(conn, ctx.user_id, id).map_err(internal)? {
            env.removed(id, &title);
            if size > 1 {
                if let Some(last) = env.removed.last_mut() {
                    last["parts"] = (size - 1).into();
                }
            }
        }
    }
    Ok(env.into_value())
}

fn open_tasks(conn: &Connection, user_id: i64) -> Result<Vec<(i64, String)>, ToolError> {
    let mut stmt = conn
        .prepare_cached("SELECT id, title FROM tasks WHERE user_id = ?1 AND state IN ('open','in_progress') ORDER BY id")
        .map_err(internal)?;
    let rows = stmt
        .query_map([user_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .map_err(internal)?;
    Ok(rows)
}

const MAX_BRIEF_DESCRIPTION: usize = 2 * 1024;
const MAX_REASON_CHARS: usize = 100;
const MAX_CHILD_TITLE_CHARS: usize = 100;

/// The whole import session in one call: either the task is dropped as not
/// homework, or it gets its description, size and, while it has none yet, its
/// children.
pub fn brief(conn: &Connection, ctx: &ToolCtx, raw: &str) -> Result<Value, ToolError> {
    let mut o = Obj::new(loose::args(raw)?);
    let task_id = o
        .take(&["task_id", "id"])
        .and_then(|v| loose::id(&v))
        .ok_or_else(|| ToolError::invalid_args("task_id is required"))?;
    let homework = o
        .take(&["homework", "is_homework", "work"])
        .and_then(|v| loose::boolean(&v))
        .ok_or_else(|| ToolError::invalid_args("homework is required: true or false"))?;
    if let Some(scope) = ctx.task_scope {
        if task_id != scope {
            return Err(ToolError::rejected(format!("this session may only change task {scope}")));
        }
    }
    let Some(node) = tasks::node(conn, ctx.user_id, task_id).map_err(internal)? else {
        return Err(ToolError::not_found(format!("no task {task_id}")));
    };
    let reason = o.take(&["reason", "why"]).and_then(|v| loose::text(&v)).unwrap_or_default();
    if !homework {
        return drop_as_not_homework(conn, ctx, &node.task, &reason);
    }
    let mut env = Envelope::default();
    let mut patch = TaskPatch { actor: Actor::Agent, ..TaskPatch::default() };
    if let Some(d) = o.take(&["description", "brief"]).and_then(|v| loose::text(&v)) {
        let d = match loose::truncate(&d, MAX_BRIEF_DESCRIPTION) {
            Some(cut) => {
                env.warn(format!("description was cut to {MAX_BRIEF_DESCRIPTION} bytes"));
                cut
            }
            None => d,
        };
        if d.trim().is_empty() {
            env.ignored(None, "description", "a description says something");
        } else {
            patch.description = Some(d);
        }
    }
    if let Some(v) = o.take(&["minutes", "duration_min", "duration", "estimate"]) {
        match loose::minutes(&v).filter(|m| *m > 0) {
            Some(m) => {
                let rounded = loose::round_minutes(m);
                if rounded != m {
                    env.adjusted(None, "minutes", rounded.into(), "a size is whole 5 minutes, 5 to 1440");
                }
                patch.duration_min = Some(Some(rounded as u32));
            }
            None => env.ignored(None, "minutes", "a size is minutes, such as 45 or \"1h30\""),
        }
    }
    if let Some(c) = o.take(&["category", "course"]).and_then(|v| loose::text(&v)) {
        patch.category = Some(loose::truncate(c.trim(), tasks::MAX_CATEGORY_BYTES).unwrap_or_else(|| c.trim().to_string()));
    }
    let children = o.take(&["children", "steps", "subtasks", "items"]).map(loose::list);
    for field in o.rest() {
        env.ignored(None, &field, "not a task_brief field");
    }
    if patch.description.is_some() || patch.duration_min.is_some() || patch.category.is_some() {
        match tasks::update(conn, ctx.user_id, task_id, &patch) {
            Ok(Some(_)) => {}
            Ok(None) => return Err(ToolError::not_found(format!("no task {task_id}"))),
            Err(tasks::UpdateError::Db(e)) => return Err(internal(e)),
            Err(e) => return Err(ToolError::rejected(e.to_string())),
        }
    }
    let applied = match children {
        _ if !node.children.is_empty() => json!("kept"),
        None => json!(0),
        Some(list) => {
            if list.len() > tasks::MAX_CHILDREN {
                return Err(ToolError::rejected(format!("a task holds at most {} children", tasks::MAX_CHILDREN)));
            }
            let mut news = Vec::new();
            for (i, v) in list.into_iter().enumerate() {
                let r = Ref::top(i);
                let mut child = match v {
                    Value::Object(m) => Obj::new(m),
                    Value::String(s) => Obj::new(Map::from_iter([("title".to_string(), Value::String(s))])),
                    _ => {
                        env.refused(&r, "a child is an object with a title, or a plain title");
                        continue;
                    }
                };
                let Some(title) = child.take(&["title", "name"]).and_then(|v| loose::text(&v)).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) else {
                    env.refused(&r, "a child needs a title");
                    continue;
                };
                let title = if title.chars().count() > MAX_CHILD_TITLE_CHARS {
                    let cut: String = title.chars().take(MAX_CHILD_TITLE_CHARS).collect();
                    env.adjusted(Some(&r), "title", cut.clone().into(), "a child's title stops at 100 characters");
                    cut
                } else {
                    title
                };
                let minutes = child.take(&["minutes", "duration_min", "duration", "size", "estimate"]).and_then(|v| loose::minutes(&v)).filter(|m| *m > 0);
                let duration_min = minutes.map(|m| {
                    let rounded = loose::round_minutes(m);
                    if rounded != m {
                        env.adjusted(Some(&r), "minutes", rounded.into(), "a size is whole 5 minutes, 5 to 1440");
                    }
                    rounded as u32
                });
                for field in child.rest() {
                    env.ignored(Some(&r), &field, "a brief's child takes a title and minutes");
                }
                news.push(NewTask { title, duration_min, source: Some(node.task.source.clone()), ..NewTask::default() });
            }
            let n = news.len();
            if n > 0 {
                match tasks::create_group(conn, ctx.user_id, Some(task_id), news, &node.task.source, Actor::Agent) {
                    Ok(_) => {}
                    Err(tasks::UpdateError::Db(e)) => return Err(internal(e)),
                    Err(e) => return Err(ToolError::rejected(e.to_string())),
                }
            }
            json!(n)
        }
    };
    let mut out = env.into_value();
    out["task_id"] = task_id.into();
    out["outcome"] = "briefed".into();
    out["children_applied"] = applied;
    Ok(out)
}

fn drop_as_not_homework(conn: &Connection, ctx: &ToolCtx, task: &tasks::Task, reason: &str) -> Result<Value, ToolError> {
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > MAX_REASON_CHARS {
        return Err(ToolError::rejected(format!("homework=false needs a reason of 1 to {MAX_REASON_CHARS} characters")));
    }
    if task.state != "open" {
        return Err(ToolError::rejected(format!(
            "task {} is already {}; only an open task can be dropped",
            task.id, task.state
        )));
    }
    let patch = TaskPatch {
        description: Some(format!("Not homework: {reason}")),
        state: Some("dropped".into()),
        actor: Actor::Agent,
        ..TaskPatch::default()
    };
    match tasks::update(conn, ctx.user_id, task.id, &patch) {
        Ok(Some(_)) => Ok(json!({ "ok": true, "task_id": task.id, "outcome": "dropped", "children_applied": 0 })),
        Ok(None) => Err(ToolError::not_found(format!("no task {}", task.id))),
        Err(tasks::UpdateError::Db(e)) => Err(internal(e)),
        Err(e) => Err(ToolError::rejected(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::testkit::{call, ctx, env, saved, talk, task};
    use crate::tools::{dispatch, SessionKind, ToolCtx};
    use serde_json::{json, Value};

    fn ids_under(conn: &rusqlite::Connection, parent: i64) -> Vec<i64> {
        crate::tasks::node(conn, 1, parent).unwrap().unwrap().children.iter().map(|c| c.task.id).collect()
    }

    fn field<'a>(out: &'a Value, list: &str, r: &str, field: &str) -> Option<&'a Value> {
        out[list].as_array()?.iter().find(|x| x["ref"] == r && x["field"] == field)
    }

    #[test]
    fn nodes_nest_any_depth_with_sizes_in_every_form() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{
            "title": "Essay",
            "children": [
                {"title": "Research", "steps": [{"title": "Sources", "minutes": "1h"}, {"title": "Notes", "duration": "45m"}]},
                {"title": "Draft", "duration_min": 23},
                "Proofread"
            ]
        }]}));
        assert_eq!(out["ok"], true, "{out}");
        let root = saved(&out, "0");
        assert_eq!(saved(&out, "0.0.1"), saved(&out, "0.0") + 2);
        assert_eq!(task(&conn, saved(&out, "0.0.0")).duration_min, Some(60));
        assert_eq!(task(&conn, saved(&out, "0.0.1")).duration_min, Some(45));
        assert_eq!(task(&conn, saved(&out, "0.1")).duration_min, Some(25));
        assert_eq!(field(&out, "adjusted", "0.1", "minutes").unwrap()["to"], 25);
        assert_eq!(task(&conn, saved(&out, "0.2")).title, "Proofread", "a plain string is a title");
        assert_eq!(task(&conn, root).duration_min, Some(130), "a parent's size is the sum of its children's");
        assert_eq!(task(&conn, saved(&out, "0.0.0")).depth, 2);
    }

    #[test]
    fn a_sized_parent_shares_what_is_left_among_its_unsized_parts() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "History essay", "minutes": 90,
            "children": [{"title": "Outline", "minutes": 30}, "Draft", "Cite"]}]}));
        assert_eq!(task(&conn, saved(&out, "0.1")).duration_min, Some(30));
        assert_eq!(task(&conn, saved(&out, "0")).duration_min, Some(90));
        let id = saved(&out, "0");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": id, "children": [{"title": "Proofread", "minutes": 15}]}]}));
        assert_eq!(out["saved"][0]["op"], "unchanged");
        assert_eq!(out["saved"][1]["op"], "created");
        assert_eq!(task(&conn, id).duration_min, Some(105));
    }

    #[test]
    fn ids_come_in_every_form_and_titles_resolve_only_when_unique() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Essay"}, {"title": "Lab report"}, {"title": "Essay", "new": true}]}));
        let (a, lab, b) = (saved(&out, "0"), saved(&out, "1"), saved(&out, "2"));
        for form in [json!(lab), json!(lab.to_string()), json!(format!("#{lab}")), json!(format!("t{lab}"))] {
            let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": form, "notes": "x"}]}));
            assert_eq!(saved(&out, "0"), lab, "{form}");
        }
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": "lab  REPORT", "state": "done"}]}));
        assert_eq!(saved(&out, "0"), lab);
        assert_eq!(field(&out, "adjusted", "0", "id").unwrap()["to"], lab);
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": "Essay", "state": "done"}, {"id": 9999, "notes": "y"}, {"title": "Poem"}]}));
        assert_eq!(out["ok"], false);
        let why = out["refused"][0]["why"].as_str().unwrap();
        assert!(why.contains(&format!("{a}, {b}")), "{why}");
        assert_eq!(out["refused"][1]["why"], "no task 9999");
        assert_eq!(out["refused"][1]["ref"], "1");
        assert_eq!(out["saved"][0]["ref"], "2", "the rest of the call lands");
        assert_eq!(task(&conn, a).state, "open");
    }

    #[test]
    fn a_create_with_an_open_tasks_title_updates_it_unless_new() {
        let (conn, tmp) = env("UTC");
        let first = saved(&talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Call the dentist"}]})), "0");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "call the  dentist", "minutes": 10}]}));
        assert_eq!(saved(&out, "0"), first);
        assert_eq!(out["saved"][0]["op"], "updated");
        assert_eq!(out["saved"][0]["matched"], "title");
        assert!(out.get("adjusted").is_none(), "{out}");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Call the dentist", "new": true}]}));
        assert_ne!(saved(&out, "0"), first);
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Call the dentist"}]}));
        assert_eq!(out["ok"], false, "two open tasks carry it now");
        conn.execute("UPDATE tasks SET state = 'done'", []).unwrap();
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Call the dentist"}]}));
        assert_eq!(out["saved"][0]["op"], "created", "a finished one is not matched");
    }

    #[test]
    fn a_title_in_another_category_is_a_new_task() {
        let (conn, tmp) = env("UTC");
        let math = saved(&talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Homework 3.2", "category": "Math"}]})), "0");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "homework 3.2", "category": "Physics"}]}));
        assert_eq!(out["saved"][0]["op"], "created", "{out}");
        assert_eq!(task(&conn, math).category, "Math");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Homework 3.2", "category": "math"}]}));
        assert_eq!(saved(&out, "0"), math);
    }

    #[test]
    fn a_refused_node_refuses_its_children_and_spares_its_siblings() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [
            {"id": 404, "children": [{"title": "orphan"}]},
            {"title": "Kept", "children": [{"title": "kept child"}, {"notes": "no title"}]}
        ]}));
        assert_eq!(out["ok"], false);
        let refused: Vec<&str> = out["refused"].as_array().unwrap().iter().map(|r| r["ref"].as_str().unwrap()).collect();
        assert_eq!(refused, ["0", "1.1"]);
        assert_eq!(out["saved"].as_array().unwrap().len(), 2);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM tasks WHERE title = 'orphan'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn done_lists_the_parts_already_finished_as_done_children() {
        let (conn, tmp) = env("UTC");
        let essay = saved(&talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "History essay", "minutes": 90}]})), "0");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": essay, "done": ["Outline"],
            "children": [{"title": "Drafting", "minutes": 30}, {"title": "Citations", "minutes": 20}, {"title": "Proofreading", "minutes": 10}]}]}));
        assert_eq!(out["saved"][4]["ref"], "0.3", "{out}");
        let kids = crate::tasks::node(&conn, 1, essay).unwrap().unwrap().children;
        let outline = kids.iter().find(|k| k.task.title == "Outline").unwrap();
        assert_eq!((kids.len(), outline.task.state.as_str()), (4, "done"));
        assert!(task(&conn, essay).progress > 0);
        talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": essay, "done": ["drafting"]}]}));
        let kids = crate::tasks::node(&conn, 1, essay).unwrap().unwrap().children;
        assert_eq!(kids.len(), 4, "a finished part named again is the same child");
        assert_eq!(kids.iter().find(|k| k.task.title.eq_ignore_ascii_case("drafting")).unwrap().task.state, "done");
        let leaf = saved(&talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Leaf", "done": true}]})), "0");
        assert_eq!(task(&conn, leaf).state, "done");
    }

    #[test]
    fn a_state_that_moves_a_parent_or_its_parts_says_so() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Essay", "children": [{"title": "Outline", "state": "done"}, "Draft", "Edit"]},
            {"title": "Unit 4", "children": ["Read", "Problems"]}]}));
        let (essay, draft, edit, unit) = (saved(&out, "0"), saved(&out, "0.1"), saved(&out, "0.2"), saved(&out, "1"));
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": draft, "state": "done"}, {"id": edit, "state": "done"}]}));
        let up: Vec<&Value> = out["adjusted"].as_array().unwrap().iter().filter(|a| a["id"] == essay).collect();
        assert_eq!(up.len(), 1, "{out}");
        assert_eq!((up[0]["to"].as_str(), up[0]["why"].as_str()), (Some("done"), Some("its last open part finished")));
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": unit, "state": "done"}]}));
        assert_eq!(out["adjusted"][0]["why"], "2 open parts went done with it", "{out}");
    }

    #[test]
    fn a_zero_size_is_ignored_on_a_parent_and_clears_a_leaf() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Unit 5", "minutes": 0, "steps": [{"title": "a", "minutes": 10}, {"title": "b", "minutes": 20}]}, {"title": "leaf", "minutes": 15}]}));
        assert_eq!(task(&conn, saved(&out, "0")).duration_min, Some(30));
        let (parent, leaf) = (saved(&out, "0"), saved(&out, "1"));
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": parent, "minutes": 0}, {"id": leaf, "minutes": 0}]}));
        assert!(field(&out, "ignored", "0", "minutes").is_some(), "{out}");
        assert_eq!(task(&conn, parent).duration_min, Some(30));
        assert_eq!(task(&conn, leaf).duration_min, None);
    }

    #[test]
    fn dues_read_as_days_or_moments_and_a_child_is_clamped_to_its_parent() {
        let (conn, tmp) = env("Asia/Tokyo");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Unit 1", "due": "2099-11-06", "children": [
            {"title": "Lesson 1", "due": "2099-11-20"},
            {"title": "Lesson 2", "deadline": "2099-11-01T17:30"},
            {"title": "Lesson 3", "due": "sometime soon"}
        ]}]}));
        let unit = task(&conn, saved(&out, "0"));
        assert_eq!(unit.due_on.as_deref(), Some("2099-11-06"));
        let l1 = task(&conn, saved(&out, "0.0"));
        assert_eq!(l1.due_on.as_deref(), Some("2099-11-06"), "clamped to the unit");
        assert!(field(&out, "adjusted", "0.0", "due").is_some(), "{out}");
        assert_eq!(task(&conn, saved(&out, "0.1")).due_at.as_deref(), Some("2099-11-01T08:30:00Z"));
        let why = field(&out, "ignored", "0.2", "due").unwrap()["why"].as_str().unwrap();
        assert!(why.contains("M/D"), "{why}");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": unit.id, "due": ""}]}));
        assert_eq!(out["ok"], true);
        assert_eq!(task(&conn, unit.id).due_at, None);
    }

    #[test]
    fn root_fields_on_a_child_are_ignored_with_the_reason() {
        let (conn, tmp) = env("UTC");
        let goal = saved(&talk(&conn, &tmp, "goal_save", json!({"goals": [{"title": "Pass"}]})), "0");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Course", "category": "Art", "goal_id": goal,
            "children": [{"title": "Unit", "category": "Other", "goal_id": goal}]}]}));
        assert_eq!(out["ok"], true);
        assert_eq!(field(&out, "ignored", "0.0", "category").unwrap()["why"], "a child reads its root's category");
        assert!(field(&out, "ignored", "0.0", "goal_id").is_some());
        let child = task(&conn, saved(&out, "0.0"));
        assert_eq!((child.category.as_str(), child.goal_id), ("Art", Some(goal)));
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": child.id, "category": "Other"}]}));
        assert!(field(&out, "ignored", "0", "category").is_some());
    }

    #[test]
    fn over_long_text_is_cut_and_kept() {
        let (conn, tmp) = env("UTC");
        let long = "a long title word ".repeat(20);
        let notes = "n".repeat(16 * 1024 + 10);
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": long, "description": "the rest", "notes": notes}]}));
        let t = task(&conn, saved(&out, "0"));
        assert!(t.title.chars().count() <= 200);
        assert!(t.description.starts_with(long.trim()), "{}", t.description);
        assert!(t.description.ends_with("the rest"));
        assert!(field(&out, "adjusted", "0", "title").is_some());
        assert_eq!(t.notes.len(), 16 * 1024);
        assert!(out["warnings"][0].as_str().unwrap().contains("notes"));
    }

    #[test]
    fn a_long_title_is_cut_to_fit_and_a_bad_part_spares_its_siblings() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "美術", "children": [{"title": "あ".repeat(180)}, {"title": "短い"}]}]}));
        assert_eq!(out["ok"], true, "{out}");
        let long = task(&conn, saved(&out, "0.0"));
        assert!(long.title.len() <= crate::tasks::MAX_TITLE_BYTES);
        assert_eq!(long.description, "あ".repeat(180));
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Course", "children": [
            {"title": "Unit 1", "children": ["Lesson 1", {"title": "Lesson 2 ".repeat(2500)}]}, {"title": "Unit 2", "children": ["Lesson 3"]}]}]}));
        assert_eq!(task(&conn, saved(&out, "0.1.0")).title, "Lesson 3", "{out}");
        assert_eq!(task(&conn, saved(&out, "0.0.0")).title, "Lesson 1");
        assert_eq!(out["refused"][0]["ref"], "0.0.1", "{out}");
        assert_eq!(out["refused"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_wrong_key_is_read_as_the_list_and_a_call_that_saves_nothing_fails() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "task_save", json!({"task": {"title": "Buy brushes"}}));
        assert_eq!(out["saved"][0]["title"], "Buy brushes", "{out}");
        assert_eq!(out["warnings"][0], "read task as tasks");
        let out = talk(&conn, &tmp, "task_save", json!({"why": "nothing", "note": "x"}));
        assert_eq!(out["ok"], false, "{out}");
        for (tool, args) in [
            ("goal_save", json!({"goal": "x", "why": "y"})),
            ("calendar_save", json!({"when": "tuesday"})),
            ("task_remove", json!({"task": "12"})),
            ("goal_remove", json!({"which": 3})),
            ("calendar_remove", json!({"which": 3})),
        ] {
            assert_eq!(talk(&conn, &tmp, tool, args)["ok"], false, "{tool}");
        }
        let brushes = saved(&talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Buy brushes"}]})), "0");
        let out = talk(&conn, &tmp, "task_remove", json!({"delete": [brushes]}));
        assert_eq!(out["removed"][0]["id"], brushes, "{out}");
    }

    #[test]
    fn a_day_weeks_gone_stays_this_year_and_one_rolled_to_next_year_says_so() {
        let (conn, tmp) = env("UTC");
        let today = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
        let md = |days: i64| {
            let d = today.checked_sub(jiff::Span::new().days(days)).unwrap();
            (format!("{}/{}", d.month(), d.day()), d)
        };
        let (recent, recent_day) = md(5);
        let (old, old_day) = md(120);
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Late lab report", "due": recent}, {"title": "Next year", "due": old}]}));
        assert_eq!(task(&conn, saved(&out, "0")).due_on, Some(recent_day.to_string()));
        assert!(field(&out, "adjusted", "0", "due").is_none(), "{out}");
        let next = old_day.checked_add(jiff::Span::new().years(1)).unwrap();
        assert_eq!(task(&conn, saved(&out, "1")).due_on, Some(next.to_string()));
        assert_eq!(field(&out, "adjusted", "1", "due").unwrap()["to"], next.to_string());
    }

    #[test]
    fn an_ambiguous_title_lists_each_candidate_with_its_path() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Course", "children": [{"title": "Unit 1", "children": ["Lesson 2"]}, {"title": "Unit 2", "children": ["Lesson 2"]}]}]}));
        let (a, b) = (saved(&out, "0.0.0"), saved(&out, "0.1.0"));
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": "Lesson 2", "state": "done"}]}));
        let why = out["refused"][0]["why"].as_str().unwrap();
        assert!(why.contains(&format!("{a} (Course › Unit 1 › Lesson 2), {b} (Course › Unit 2 › Lesson 2)")), "{why}");
        let arts = saved(&talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Unit 1", "category": "Art"}]})), "0");
        let music = saved(&talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Unit 1", "category": "Music"}]})), "0");
        let out = talk(&conn, &tmp, "task_remove", json!({"ids": ["Unit 1"]}));
        assert!(out["refused"][0]["why"].as_str().unwrap().contains(&format!("{arts} (Art), {music} (Music)")), "{out}");
    }

    #[test]
    fn the_list_is_found_however_it_is_sent_and_an_empty_one_is_no_error() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "task_save", json!({"title": "bare"}));
        assert_eq!(out["saved"][0]["op"], "created");
        let out = talk(&conn, &tmp, "task_save", json!({"items": [{"title": "one"}, {"title": "two"}]}));
        assert_eq!(out["saved"].as_array().unwrap().len(), 2);
        assert!(out["warnings"][0].as_str().unwrap().contains("items"));
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": {"title": "lone object"}}));
        assert_eq!(out["saved"][0]["title"], "lone object");
        for empty in [json!({"tasks": []}), json!({})] {
            let out = talk(&conn, &tmp, "task_save", empty);
            assert_eq!(out["ok"], true);
            assert!(out["warnings"][0].as_str().unwrap().starts_with("nothing to save"));
        }
        let out = call(&conn, &tmp, SessionKind::Talk, "task_save", r#""{\"tasks\":[{\"title\":\"quoted\"}]}""#).unwrap();
        assert_eq!(out["saved"][0]["title"], "quoted");
    }

    #[test]
    fn nesting_past_the_cap_is_flattened_onto_the_deepest_level() {
        let (conn, tmp) = env("UTC");
        let deep = json!({"title": "L0", "children": [{"title": "L1", "children": [{"title": "L2", "children": [{"title": "L3",
            "children": [{"title": "L4", "children": [{"title": "L5", "children": ["L6"]}]}]}]}]}]});
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [deep]}));
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(task(&conn, saved(&out, "0.0.0.0.0.0")).depth, 4);
        assert_eq!(task(&conn, saved(&out, "0.0.0.0.0.0.0")).depth, 4);
        assert!(out["adjusted"].as_array().unwrap().iter().any(|a| a["field"] == "depth" && a["ref"] == "0.0.0.0.0.0"), "{out}");
        let l4 = saved(&out, "0.0.0.0.0");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"parent": l4, "title": "under the cap"}]}));
        let made = task(&conn, saved(&out, "0"));
        assert_eq!(made.depth, 4);
        assert!(field(&out, "adjusted", "0", "depth").is_some());
    }

    #[test]
    fn fields_that_belong_elsewhere_or_nowhere_are_ignored_and_named() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "x", "event_id": 4, "colour": "red", "state": "Completed", "urgency": "urgent", "notify": "silent"}]}));
        assert_eq!(field(&out, "ignored", "0", "event_id").unwrap()["why"], "event_id belongs to day_save");
        assert_eq!(field(&out, "ignored", "0", "colour").unwrap()["why"], "not a field here");
        let t = task(&conn, saved(&out, "0"));
        assert_eq!((t.state.as_str(), t.urgency.as_str(), t.notify.as_str()), ("done", "high", "none"));
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "y", "state": "exploded"}]}));
        assert!(field(&out, "ignored", "0", "state").is_some());
        assert_eq!(task(&conn, saved(&out, "0")).state, "open");
    }

    #[test]
    fn a_call_holds_at_most_a_hundred_nodes() {
        let (conn, tmp) = env("UTC");
        let unit = |u: usize| json!({"title": format!("unit {u}"), "children": (0..40).map(|i| format!("lesson {u}.{i}")).collect::<Vec<_>>()});
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [unit(1), unit(2), unit(3)]}));
        assert_eq!(out["ok"], false);
        assert_eq!(out["saved"].as_array().unwrap().len(), 100);
        assert_eq!(out["refused"].as_array().unwrap().len(), 23);
    }

    #[test]
    fn a_node_holds_at_most_fifty_live_children() {
        let (conn, tmp) = env("UTC");
        let children: Vec<Value> = (0..55).map(|i| json!(format!("lesson {i}"))).collect();
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "big", "children": children}]}));
        assert_eq!(out["saved"].as_array().unwrap().len(), 1 + crate::tasks::MAX_CHILDREN);
        assert_eq!(out["refused"].as_array().unwrap().len(), 5);
        assert!(out["refused"][0]["why"].as_str().unwrap().contains("at most 50 live children"), "{out}");
    }

    #[test]
    fn now_takes_its_place_and_reports_who_left() {
        let (conn, tmp) = env("UTC");
        let four: Vec<Value> = ["a", "b", "c", "d"].iter().map(|t| json!({"title": t, "now": true})).collect();
        let out = talk(&conn, &tmp, "task_save", json!({ "tasks": four }));
        assert!(out["adjusted"].as_array().unwrap().iter().any(|a| a["field"] == "now" && a["to"] == false), "{out}");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM tasks WHERE is_now = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "landlord", "now": true, "children": [{"title": "find the thread", "now": true}]}]}));
        assert!(!task(&conn, saved(&out, "0")).is_now, "a node and its part never sit in Now together");
        assert!(task(&conn, saved(&out, "0.0")).is_now);
    }

    #[test]
    fn a_node_moves_by_parent_and_takes_its_place_by_before() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Course", "children": ["A", "B", "C"]}, {"title": "Loose"}]}));
        let (course, a, c, loose) = (saved(&out, "0"), saved(&out, "0.0"), saved(&out, "0.2"), saved(&out, "1"));
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": loose, "parent": "Course", "before": a}, {"id": c, "before": "A"}]}));
        assert_eq!(out["ok"], true, "{out}");
        let under = ids_under(&conn, course);
        assert_eq!(&under[..2], &[loose, c], "{under:?}");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": loose, "parent": 0}]}));
        assert_eq!(out["ok"], true);
        assert_eq!(task(&conn, loose).parent_id, None);
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": course, "parent": a}]}));
        assert_eq!(out["ok"], false, "a cycle is refused");
    }

    #[test]
    fn a_remove_takes_subtrees_and_asks_for_all_past_twenty() {
        let (conn, tmp) = env("UTC");
        let lessons: Vec<Value> = (0..25).map(|i| json!(format!("lesson {i}"))).collect();
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "Course", "children": lessons}, {"title": "Draft"}, {"title": "Old idea"}]}));
        let (course, draft) = (saved(&out, "0"), saved(&out, "1"));
        let out = talk(&conn, &tmp, "task_remove", json!({"ids": [course, draft]}));
        assert_eq!(out["ok"], false);
        assert_eq!(out["refused"].as_array().unwrap().len(), 2);
        assert!(crate::tasks::get(&conn, 1, course).unwrap().is_some());
        let out = talk(&conn, &tmp, "task_remove", json!({"ids": [course], "all": true}));
        assert_eq!(out["removed"][0]["parts"], 25);
        let out = talk(&conn, &tmp, "task_remove", json!({"ids": ["old IDEA", 9999]}));
        assert_eq!(out["removed"][0]["title"], "Old idea");
        assert_eq!(out["refused"][0]["why"], "no task 9999");
        let out = talk(&conn, &tmp, "task_remove", json!({"ids": []}));
        assert_eq!(out["ok"], true);
        assert!(out["warnings"][0].as_str().unwrap().starts_with("nothing to remove"));
        let out = talk(&conn, &tmp, "task_remove", json!({"task_id": draft}));
        assert_eq!(out["removed"][0]["id"], draft, "a bare id is a list of one");
    }

    #[test]
    fn another_users_task_is_never_reached() {
        let (conn, tmp) = env("UTC");
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('rin', 'x', 'member')", []).unwrap();
        let theirs = crate::tasks::create(&conn, 2, crate::tasks::NewTask { title: "theirs".into(), ..Default::default() }, "manual", crate::tasks::Actor::User).unwrap().id;
        for (name, args) in [("task_save", json!({"tasks": [{"id": theirs, "state": "done"}]})), ("task_remove", json!({"ids": [theirs]}))] {
            let out = talk(&conn, &tmp, name, args);
            assert_eq!(out["refused"][0]["why"], format!("no task {theirs}"), "{name}");
        }
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"id": "theirs", "state": "done"}]}));
        assert_eq!(out["ok"], false);
        assert_eq!(crate::tasks::get(&conn, 2, theirs).unwrap().unwrap().state, "open");
    }

    #[test]
    fn a_scoped_session_reaches_its_own_tree_and_only_drops() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "biology ch.4", "children": ["read", "answer"]}, {"title": "other"}]}));
        let (mine, step, other) = (saved(&out, "0"), saved(&out, "0.0"), saved(&out, "1"));
        let scoped = ToolCtx { task_scope: Some(mine), ..ctx(&tmp) };
        let run = |args: Value| dispatch(&conn, &scoped, SessionKind::Talk, "task_save", &args.to_string()).unwrap();
        let out = run(json!({"tasks": [{"id": step, "title": "read carefully", "now": true, "state": "done"}, {"id": other, "notes": "x"}]}));
        assert_eq!(task(&conn, step).title, "read carefully");
        assert!(!task(&conn, step).is_now);
        assert_eq!(task(&conn, step).state, "open");
        assert!(field(&out, "ignored", "0", "now").is_some() && field(&out, "ignored", "0", "state").is_some());
        assert_eq!(out["refused"][0]["ref"], "1");
        run(json!({"tasks": [{"id": mine, "state": "dropped"}]}));
        assert_eq!(task(&conn, mine).state, "dropped");
    }

    #[test]
    fn a_wake_up_may_drop_tasks_but_never_delete_them() {
        let (conn, tmp) = env("UTC");
        let a = saved(&talk(&conn, &tmp, "task_save", json!({"tasks": [{"title": "a"}]})), "0");
        let e = call(&conn, &tmp, SessionKind::Trigger, "task_remove", &json!({"ids": [a]}).to_string()).unwrap_err();
        assert_eq!(e.kind, "forbidden");
        call(&conn, &tmp, SessionKind::Trigger, "task_save", &json!({"tasks": [{"id": a, "state": "dropped"}]}).to_string()).unwrap();
        assert_eq!(task(&conn, a).state, "dropped");
    }

    fn brief(conn: &rusqlite::Connection, tmp: &tempfile::TempDir, args: &Value) -> Result<Value, crate::tools::ToolError> {
        let scoped = ToolCtx { task_scope: Some(1), ..ctx(tmp) };
        dispatch(conn, &scoped, SessionKind::Import, "task_brief", &args.to_string())
    }

    fn import_env() -> (rusqlite::Connection, tempfile::TempDir) {
        let (conn, tmp) = env("UTC");
        for title in ["scoped", "someone elses"] {
            crate::tasks::create(&conn, 1, crate::tasks::NewTask { title: title.into(), ..Default::default() }, "manual", crate::tasks::Actor::User).unwrap();
        }
        (conn, tmp)
    }

    #[test]
    fn one_brief_writes_the_description_the_size_and_the_children() {
        let (conn, tmp) = import_env();
        let out = brief(&conn, &tmp, &json!({"task_id": 1, "homework": true, "description": "Read chapter 4.", "duration_min": 43,
            "category": "Biology", "steps": [{"title": "read chapter 4", "duration_min": 25}, {"title": "answer", "minutes": "20m"}]})).unwrap();
        assert_eq!((out["outcome"].as_str(), out["children_applied"].as_i64()), (Some("briefed"), Some(2)));
        let node = crate::tasks::node(&conn, 1, 1).unwrap().unwrap();
        assert_eq!(node.task.description, "Read chapter 4.");
        assert_eq!(node.task.category, "Biology");
        assert_eq!(node.task.duration_min, Some(45));
        assert_eq!(node.children.len(), 2);
        let again = brief(&conn, &tmp, &json!({"task_id": 1, "homework": true, "children": ["more"]})).unwrap();
        assert_eq!(again["children_applied"], "kept");
        assert_eq!(crate::tasks::node(&conn, 1, 1).unwrap().unwrap().children.len(), 2);
    }

    #[test]
    fn not_homework_drops_an_open_task_with_its_reason() {
        let (conn, tmp) = import_env();
        assert_eq!(brief(&conn, &tmp, &json!({"task_id": 1, "homework": false})).unwrap_err().kind, "rejected");
        let out = brief(&conn, &tmp, &json!({"task_id": 1, "homework": false, "reason": "class calendar"})).unwrap();
        assert_eq!(out["outcome"], "dropped");
        let t = task(&conn, 1);
        assert_eq!((t.state.as_str(), t.description.as_str()), ("dropped", "Not homework: class calendar"));
        assert_eq!(brief(&conn, &tmp, &json!({"task_id": 1, "homework": false, "reason": "again"})).unwrap_err().kind, "rejected");
    }

    #[test]
    fn a_brief_reaches_only_the_scoped_task_and_reads_loosely() {
        let (conn, tmp) = import_env();
        for target in [2, 9999] {
            assert_eq!(brief(&conn, &tmp, &json!({"task_id": target, "homework": true, "description": "nope"})).unwrap_err().kind, "rejected");
        }
        let out = brief(&conn, &tmp, &json!({"task_id": "1", "homework": "true", "description": "  ", "surprise": 1, "children": [{"title": "x".repeat(120)}]})).unwrap();
        assert!(out["ignored"].as_array().unwrap().iter().any(|i| i["field"] == "surprise"));
        assert!(out["ignored"].as_array().unwrap().iter().any(|i| i["field"] == "description"));
        assert_eq!(crate::tasks::node(&conn, 1, 1).unwrap().unwrap().children[0].task.title.chars().count(), 100);
        assert_eq!(task(&conn, 2).description, "");
    }
}
