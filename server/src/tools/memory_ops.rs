//! `memory_find`, `memory_save` and `memory_remove`: the user's long-term
//! memory and Note's working notes, which are memory files of kind `note`.

use super::loose::{self, Envelope, Obj, Ref};
use super::{ToolCtx, ToolError};
use crate::memory::{self, MemoryFile, NOTE};
use crate::notes::{self, Change, NoteError, Outcome};
use rusqlite::Connection;
use serde_json::{json, Value};

pub(crate) const MAX_SUMMARY: usize = 200;
const DEFAULT_LIMIT: i64 = 8;
const KINDS: &[&str] = &["semantic", "episodic", "procedural", NOTE];
const KIND_SYNONYMS: &[(&str, &str)] = &[
    ("fact", "semantic"),
    ("memory", "semantic"),
    ("long_term", "semantic"),
    ("event", "episodic"),
    ("episode", "episodic"),
    ("procedure", "procedural"),
    ("how_to", "procedural"),
    ("habit", "procedural"),
    ("notes", NOTE),
    ("working", NOTE),
    ("working_note", NOTE),
];
const ITEMS: &str = "items";
const ITEM_ALIASES: &[&str] = &["memories", "notes", "facts", "list", "data"];
const ITEM_FIELDS: &[&str] = &["title", "summary", "body", "id", "kind", "category", "supersedes"];
const TITLE: &[&str] = &["title", "summary", "name", "text"];
const BODY: &[&str] = &["body", "content", "details", "description"];
const KIND: &[&str] = &["kind", "category", "type"];
const WHEN_FORMS: &str = "a note's from and until are YYYY-MM-DDTHH:MM, HH:MM or a day";

fn internal(e: impl std::fmt::Display) -> ToolError {
    ToolError::internal(e.to_string())
}

fn kind_of(v: &Value) -> Option<&'static str> {
    loose::text(v).and_then(|s| loose::choice(&s, KINDS, KIND_SYNONYMS))
}

/// What an id or a title names among the user's memories and notes.
enum Found {
    Note(notes::Note),
    Fact(MemoryFile),
}

fn note_row(n: &notes::Note) -> Value {
    let mut row = json!({ "id": notes::short(&n.id), "kind": NOTE, "title": n.title });
    if let Some(f) = &n.from {
        row["from"] = f.clone().into();
    }
    if let Some(u) = &n.until {
        row["until"] = u.clone().into();
    }
    row
}

fn fact_row(f: &MemoryFile) -> Value {
    let mut row = json!({ "id": f.id, "kind": f.category, "title": f.summary, "body": f.body });
    if let Some(u) = &f.until {
        row["until"] = u.clone().into();
    }
    if f.archived {
        row["archived"] = true.into();
    }
    if let Some(s) = &f.source {
        row["source"] = s.clone().into();
    }
    row
}

fn found_row(f: &Found) -> Value {
    match f {
        Found::Note(n) => note_row(n),
        Found::Fact(f) => fact_row(f),
    }
}

/// The memory or note `raw` names: a whole id, a note's short id, or an exact
/// title; or why there is none.
fn resolve(conn: &Connection, ctx: &ToolCtx, raw: &str) -> Result<Result<Found, String>, ToolError> {
    let s = raw.trim().trim_start_matches('#');
    if memory::valid_id(s) {
        return Ok(match memory::read(ctx.data_dir, ctx.username, s).map_err(internal)? {
            Some(f) if f.category == NOTE && !f.archived => match notes::get(ctx.data_dir, ctx.username, s).map_err(internal)? {
                Some(n) => Ok(Found::Note(n)),
                None => Err(format!("no memory {s}")),
            },
            Some(f) => Ok(Found::Fact(f)),
            None => Err(format!("no memory {s}")),
        });
    }
    let hex = s.len() >= notes::SHORT_ID && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f' | b'-'));
    if hex {
        return Ok(match notes::resolve(conn, ctx.data_dir, ctx.username, s) {
            Ok(id) => notes::get(ctx.data_dir, ctx.username, &id).map_err(internal)?.map(Found::Note).ok_or_else(|| format!("no note {s}")),
            Err(NoteError::Other(e)) => return Err(internal(e)),
            Err(e) => Err(e.to_string()),
        });
    }
    if s.is_empty() {
        return Ok(Err("an id is a memory's id, a note's short id or an exact title".into()));
    }
    let mut hits: Vec<Found> = notes::all(conn, ctx.data_dir, ctx.username)
        .map_err(internal)?
        .into_iter()
        .filter(|n| super::same_title(&n.title, s))
        .map(Found::Note)
        .collect();
    let mut stmt = conn
        .prepare_cached("SELECT id, summary FROM memory_index WHERE user = ?1 AND archived = 0 AND category != 'note'")
        .map_err(internal)?;
    let facts: Vec<(String, String)> = stmt
        .query_map([ctx.username], |r| Ok((r.get(0)?, r.get(1)?)))
        .and_then(|rows| rows.collect::<rusqlite::Result<_>>())
        .map_err(internal)?;
    for (id, _) in facts.into_iter().filter(|(_, t)| super::same_title(t, s)) {
        if let Some(f) = memory::read(ctx.data_dir, ctx.username, &id).map_err(internal)? {
            hits.push(Found::Fact(f));
        }
    }
    Ok(match hits.len() {
        1 => Ok(hits.remove(0)),
        0 => Err(format!("no memory or note is titled {s:?}; give its id")),
        n => {
            let listed: Vec<String> = hits
                .iter()
                .map(|h| match h {
                    Found::Note(x) => format!("note {} {:?}", notes::short(&x.id), x.title),
                    Found::Fact(x) => format!("{} {} {:?}", x.category, x.id, x.summary),
                })
                .collect();
            Err(format!("{n} are titled {s:?}: {}; give the id", listed.join(", ")))
        }
    })
}

fn words_match(title: &str, query: &str) -> bool {
    let have = loose::fold(title);
    query.split_whitespace().map(str::to_lowercase).any(|w| have.contains(&w))
}

/// `memory_find`: memories and notes by id, ranked by a query, or the newest.
pub fn find(conn: &Connection, ctx: &ToolCtx, raw: &str) -> Result<Value, ToolError> {
    let mut o = Obj::new(loose::args(raw)?);
    let mut warnings: Vec<String> = Vec::new();
    let ids = o.take(&["id", "ids", "memory_id", "note_id"]).map(loose::list).unwrap_or_default();
    let query = o.take(&["query", "q", "search", "text"]).and_then(|v| loose::text(&v)).map(|q| q.trim().to_string()).filter(|q| !q.is_empty());
    let kind = o.take(KIND).and_then(|v| {
        let k = kind_of(&v);
        if k.is_none() {
            warnings.push("kind was ignored: it is semantic, episodic, procedural or note".into());
        }
        k
    });
    let limit = match o.take(&["limit", "n", "count"]).map(|v| loose::integer(&v)) {
        Some(Some(n)) if !(1..=50).contains(&n) => {
            warnings.push(format!("limit {n} was read as {}: it is 1 to 50", n.clamp(1, 50)));
            n.clamp(1, 50)
        }
        Some(Some(n)) => n,
        _ => DEFAULT_LIMIT,
    };
    for field in o.rest() {
        warnings.push(format!("{field} was ignored: not a memory_find field"));
    }
    if let Some(err) = &ctx.vectors.error {
        let _ = crate::log::record(conn, None, "memory_embed_error", &format!("query: {err}"));
    }
    let mut out = if ids.is_empty() {
        let items: Vec<Value> = match (kind, &query) {
            (Some(NOTE), q) => notes::all(conn, ctx.data_dir, ctx.username)
                .map_err(internal)?
                .iter()
                .filter(|n| q.as_deref().is_none_or(|q| words_match(&n.title, q)))
                .take(limit as usize)
                .map(note_row)
                .collect(),
            (_, Some(q)) => {
                let take = if kind.is_some() { 50 } else { limit };
                memory::query(conn, ctx.username, q, take, ctx.vectors.query.as_deref())
                    .map_err(internal)?
                    .into_iter()
                    .filter(|h| kind.is_none_or(|k| h.category == k))
                    .take(limit as usize)
                    .map(|h| json!({ "id": h.id, "kind": h.category, "title": h.summary }))
                    .collect()
            }
            (_, None) => memory::list(conn, ctx.username, kind, limit as usize)
                .map_err(internal)?
                .into_iter()
                .map(|h| json!({ "id": h.id, "kind": h.category, "title": h.summary }))
                .collect(),
        };
        if items.is_empty() {
            warnings.push(match kind {
                Some(NOTE) => "no note matched".to_string(),
                _ => "nothing matched; working notes are found with kind note".to_string(),
            });
        }
        json!({ "total": items.len(), "items": items })
    } else {
        let mut items = Vec::new();
        for v in &ids {
            let Some(s) = loose::text(v) else {
                warnings.push(format!("{v} is not an id"));
                continue;
            };
            match resolve(conn, ctx, &s)? {
                Ok(found) => items.push(found_row(&found)),
                Err(why) => warnings.push(why),
            }
        }
        if ids.len() == 1 && items.len() == 1 {
            json!({ "item": items.remove(0) })
        } else {
            json!({ "total": items.len(), "items": items })
        }
    };
    if !warnings.is_empty() {
        out["warnings"] = warnings.into();
    }
    Ok(out)
}

/// The text each item of a `memory_save` call is embedded by, in order; a note
/// is never embedded.
pub(crate) fn embed_texts(raw: &str) -> Vec<Option<String>> {
    let Ok(map) = loose::args(raw) else { return Vec::new() };
    let mut o = Obj::new(map);
    loose::items(&mut o, ITEMS, ITEM_ALIASES, ITEM_FIELDS)
        .0
        .into_iter()
        .map(|v| {
            let Value::Object(item) = v else { return None };
            let field = |names: &[&str]| names.iter().find_map(|n| item.get(*n).and_then(loose::text)).unwrap_or_default();
            if KIND.iter().any(|k| item.get(*k).and_then(kind_of) == Some(NOTE)) {
                return None;
            }
            let (title, body) = (field(TITLE), field(BODY));
            (!title.trim().is_empty() || !body.trim().is_empty()).then(|| memory::embed_text(&title, &body))
        })
        .collect()
}

/// A note's bound as an instant on the user's clock: `None` leaves it alone,
/// `Some(None)` clears it.
fn note_when(raw: &str, there: &jiff::tz::TimeZone, user: &jiff::tz::TimeZone) -> Option<Option<String>> {
    let s = raw.trim();
    if s.is_empty() {
        return Some(None);
    }
    let read = notes::parse_when(s, there).ok().or_else(|| match loose::when(s, there, jiff::Timestamp::now())? {
        loose::When::At(t) => Some(t.to_string()),
        loose::When::Day(d) => notes::parse_when(&d.to_string(), there).ok(),
    })?;
    notes::parse_when(&read, user).ok().map(Some)
}

struct Item {
    id: Option<String>,
    supersedes: Option<String>,
    kind: Option<&'static str>,
    title: Option<String>,
    body: Option<String>,
    from: Option<Option<String>>,
    until: Option<String>,
    there: jiff::tz::TimeZone,
    new: bool,
}

struct Writer<'a> {
    conn: &'a Connection,
    ctx: &'a ToolCtx<'a>,
    env: Envelope,
    tz: jiff::tz::TimeZone,
}

impl Writer<'_> {
    fn saved(&mut self, r: &Ref, id: &str, title: &str, kind: &str, op: &str) -> &mut Value {
        self.env.saved.push(json!({ "ref": r.0, "id": id, "title": title, "kind": kind, "op": op }));
        self.env.saved.last_mut().expect("just pushed")
    }

    fn record_source(&self, memory_id: &str) -> Result<(), ToolError> {
        let Some(source) = &self.ctx.memory_source else { return Ok(()) };
        self.conn
            .execute(
                "INSERT OR IGNORE INTO memory_sources (user_id, source_id, memory_id) VALUES (?1, ?2, ?3)",
                (self.ctx.user_id, source, memory_id),
            )
            .map_err(internal)?;
        Ok(())
    }

    fn read(&mut self, r: &Ref, v: Value) -> Option<Item> {
        let map = match v {
            Value::Object(m) => m,
            Value::String(s) => serde_json::Map::from_iter([("title".to_string(), Value::String(s))]),
            _ => {
                self.env.refused(r, "a memory is an object with a title");
                return None;
            }
        };
        let mut o = Obj::new(map);
        let text = |o: &mut Obj, names: &[&str]| o.take(names).and_then(|v| loose::text(&v));
        let id = text(&mut o, &["id", "memory_id", "note_id"]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let supersedes = text(&mut o, &["supersedes", "replaces", "supersede"]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let kind = o.take(KIND).and_then(|v| {
            let k = kind_of(&v);
            if k.is_none() {
                self.env.ignored(Some(r), "kind", "kind is semantic, episodic, procedural or note");
            }
            k
        });
        let title = text(&mut o, TITLE).map(|t| t.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|t| !t.is_empty());
        let mut body = text(&mut o, BODY);
        if let Some(cut) = body.as_deref().and_then(|b| loose::truncate(b, loose::MAX_TEXT_BYTES)) {
            self.env.warn(format!("{} body was cut to {} bytes", r.0, loose::MAX_TEXT_BYTES));
            body = Some(cut);
        }
        let there = match text(&mut o, &["tz", "timezone"]).as_deref().map(|z| jiff::tz::TimeZone::get(z.trim())) {
            Some(Ok(z)) => z,
            Some(Err(_)) => {
                self.env.ignored(Some(r), "tz", "tz is an IANA zone such as America/New_York");
                self.tz.clone()
            }
            None => self.tz.clone(),
        };
        let mut from = None;
        if let Some(raw) = text(&mut o, &["from", "start", "starts"]) {
            match note_when(&raw, &there, &self.tz) {
                Some(w) => from = Some(w),
                None => self.env.ignored(Some(r), "from", WHEN_FORMS),
            }
        }
        let until = text(&mut o, &["until", "end", "expires", "expires_on"]);
        let new = o.take(&["new", "also"]).and_then(|v| loose::boolean(&v)).unwrap_or(false);
        for field in o.rest() {
            self.env.ignored(Some(r), &field, "not a memory field");
        }
        Some(Item { id, supersedes, kind, title, body, from, until, there, new })
    }

    /// A long-term memory's until as `YYYY-MM-DD`: `Some(None)` clears it.
    fn fact_until(&mut self, r: &Ref, raw: Option<&str>) -> Option<Option<String>> {
        let s = raw?.trim();
        if s.is_empty() {
            return Some(None);
        }
        let today = jiff::Timestamp::now().to_zoned(self.tz.clone()).date();
        let day = loose::day(s, today).or_else(|| s.get(..10).and_then(|d| d.parse().ok()));
        if day.is_none() {
            self.env.ignored(Some(r), "until", format!("a memory's until is a day: {}", loose::DAY_FORMS));
        }
        day.map(|d| Some(d.to_string()))
    }

    /// A note's until, an instant on the user's clock.
    fn note_until(&mut self, r: &Ref, raw: Option<&str>, there: &jiff::tz::TimeZone) -> Option<Option<String>> {
        let s = raw?;
        let read = note_when(s, there, &self.tz);
        if read.is_none() {
            self.env.ignored(Some(r), "until", WHEN_FORMS);
        }
        read
    }

    fn set_until(&self, id: &str, until: Option<String>) -> Result<(), ToolError> {
        let Some(mut f) = memory::read(self.ctx.data_dir, self.ctx.username, id).map_err(internal)? else { return Ok(()) };
        f.until = until;
        memory::put(self.conn, self.ctx.data_dir, self.ctx.username, &f).map_err(internal)
    }

    /// A long-term memory's title within its cap, the whole of an over-long one
    /// leading the body.
    fn fit_title(&mut self, r: &Ref, title: String, body: Option<String>) -> (String, Option<String>) {
        match loose::cut_bytes(&title, MAX_SUMMARY) {
            Some(cut) => {
                self.env.adjusted(Some(r), "title", cut.clone().into(), "a memory's title stops at 200 bytes; the whole text leads the body");
                let body = Some(match body {
                    Some(b) if !b.trim().is_empty() => format!("{title}\n\n{b}"),
                    _ => title,
                });
                (cut, body)
            }
            None => (title, body),
        }
    }

    fn write(&mut self, i: usize, r: &Ref, item: Item) -> Result<(), ToolError> {
        let vector = self.ctx.vectors.facts.get(i).cloned().flatten();
        if let Some(old) = item.supersedes.clone() {
            if item.id.as_ref().is_some_and(|id| *id != old) {
                self.env.ignored(Some(r), "id", "supersedes names the memory replaced; a new one is made");
            }
            return self.supersede(r, &old, item, vector);
        }
        if let Some(id) = item.id.clone() {
            return match resolve(self.conn, self.ctx, &id)? {
                Err(why) => {
                    self.env.refused(r, why);
                    Ok(())
                }
                Ok(Found::Note(n)) => match item.kind {
                    Some(k) if k != NOTE => self.settle(r, &n, k, item),
                    _ => self.change_note(r, &n, item),
                },
                Ok(Found::Fact(f)) => self.change_fact(r, &f, item, vector),
            };
        }
        let kind = match item.kind {
            None if item.from.is_some() => {
                self.env.adjusted(Some(r), "kind", NOTE.into(), "from belongs to working notes");
                NOTE
            }
            None => "semantic",
            Some(k) => k,
        };
        if kind == NOTE {
            self.add_note(r, item, vector.as_deref())
        } else {
            self.add_fact(r, kind, item, vector.as_deref())
        }
    }

    fn add_fact(&mut self, r: &Ref, kind: &str, item: Item, vector: Option<&[f32]>) -> Result<(), ToolError> {
        if item.from.is_some() {
            self.env.ignored(Some(r), "from", "from belongs to working notes");
        }
        let title = match (item.title, &item.body) {
            (Some(t), _) => t,
            (None, Some(b)) if !b.trim().is_empty() => {
                let line = b.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim().to_string();
                self.env.adjusted(Some(r), "title", line.clone().into(), "a memory needs a title; its body's first line is it");
                line
            }
            _ => {
                self.env.refused(r, "a memory needs a title");
                return Ok(());
            }
        };
        let (title, body) = self.fit_title(r, title, item.body);
        let until = self.fact_until(r, item.until.as_deref()).flatten();
        let fact = memory::Fact { category: kind, summary: &title, body: body.as_deref().unwrap_or(""), until: until.as_deref() };
        let id = memory::add_until(self.conn, self.ctx.data_dir, self.ctx.username, &fact, vector).map_err(internal)?;
        self.record_source(&id)?;
        self.saved(r, &id, &memory::one_line(&title), kind, "created");
        Ok(())
    }

    fn add_note(&mut self, r: &Ref, item: Item, vector: Option<&[f32]>) -> Result<(), ToolError> {
        let Some(title) = item.title.clone() else {
            self.env.refused(r, "a note needs a title: one short line");
            return Ok(());
        };
        let long = title.chars().count() > notes::MAX_TITLE_CHARS;
        if long || item.body.as_ref().is_some_and(|b| !b.trim().is_empty()) {
            let why = if long { "a note is at most 160 characters, so it was kept as a memory" } else { "a note is one line, so one with a body was kept as a memory" };
            self.env.adjusted(Some(r), "kind", "semantic".into(), why);
            return self.add_fact(r, "semantic", Item { from: None, ..item }, vector);
        }
        if self.ctx.task_scope.is_some() {
            self.env.refused(r, "this session reaches its own task alone");
            return Ok(());
        }
        let until = self.note_until(r, item.until.as_deref(), &item.there);
        if !item.new {
            let existing = notes::all(self.conn, self.ctx.data_dir, self.ctx.username).map_err(internal)?;
            if let Some(n) = existing.into_iter().find(|n| super::same_title(&n.title, &title)) {
                let changed = item.from.is_some() || until.is_some();
                let now = jiff::Timestamp::now();
                let saved = notes::update(self.conn, self.ctx.data_dir, self.ctx.username, &n.id, Change { title: None, from: item.from, until }, now);
                return match saved {
                    Ok(n) => {
                        let row = self.saved(r, notes::short(&n.id), &n.title, NOTE, if changed { "updated" } else { "kept" });
                        row["matched"] = "title".into();
                        Ok(())
                    }
                    Err(e) => self.note_failed(r, e),
                };
            }
        }
        let now = jiff::Timestamp::now();
        match notes::add(self.conn, self.ctx.data_dir, self.ctx.username, &title, item.from.flatten(), until.flatten(), now) {
            Ok(n) => {
                self.saved(r, notes::short(&n.id), &n.title, NOTE, "created");
                Ok(())
            }
            Err(e) => self.note_failed(r, e),
        }
    }

    fn note_failed(&mut self, r: &Ref, e: NoteError) -> Result<(), ToolError> {
        match e {
            NoteError::Other(e) => Err(internal(e)),
            e => {
                self.env.refused(r, e.to_string());
                Ok(())
            }
        }
    }

    fn change_note(&mut self, r: &Ref, n: &notes::Note, item: Item) -> Result<(), ToolError> {
        if item.body.as_ref().is_some_and(|b| !b.trim().is_empty()) {
            self.env.ignored(Some(r), "body", "a note is one line; give kind semantic to keep it as a memory");
        }
        if item.title.as_ref().is_some_and(|t| t.chars().count() > notes::MAX_TITLE_CHARS) {
            self.env.refused(r, "a note is at most 160 characters; give kind semantic to keep the whole text as a memory");
            return Ok(());
        }
        let until = self.note_until(r, item.until.as_deref(), &item.there);
        let changed = item.title.is_some() || item.from.is_some() || until.is_some();
        let change = Change { title: item.title, from: item.from, until };
        match notes::update(self.conn, self.ctx.data_dir, self.ctx.username, &n.id, change, jiff::Timestamp::now()) {
            Ok(n) => {
                self.saved(r, notes::short(&n.id), &n.title, NOTE, if changed { "updated" } else { "kept" });
                Ok(())
            }
            Err(e) => self.note_failed(r, e),
        }
    }

    fn settle(&mut self, r: &Ref, n: &notes::Note, kind: &str, item: Item) -> Result<(), ToolError> {
        if item.from.is_some() {
            self.env.ignored(Some(r), "from", "from belongs to working notes");
        }
        let (title, body) = match item.title {
            Some(t) => {
                let (t, b) = self.fit_title(r, t, item.body);
                (Some(t), b)
            }
            None => (None, item.body),
        };
        let outcome = Outcome::Memory { category: Some(kind), summary: title.as_deref(), body: body.as_deref() };
        let id = match notes::settle(self.conn, self.ctx.data_dir, self.ctx.username, &n.id, &outcome) {
            Ok(Some(id)) => id,
            Ok(None) => return Ok(()),
            Err(e) => return self.note_failed(r, e),
        };
        if let Some(until) = self.fact_until(r, item.until.as_deref()) {
            self.set_until(&id, until)?;
        }
        self.record_source(&id)?;
        let shown = memory::one_line(title.as_deref().unwrap_or(&n.title));
        let row = self.saved(r, &id, &shown, kind, "created");
        row["settled"] = notes::short(&n.id).into();
        Ok(())
    }

    fn change_fact(&mut self, r: &Ref, fact: &MemoryFile, item: Item, vector: Option<Vec<f32>>) -> Result<(), ToolError> {
        if fact.archived {
            self.env.refused(r, format!("memory {} is archived; change the live one that replaced it", fact.id));
            return Ok(());
        }
        if item.kind.is_some_and(|k| k != fact.category) {
            self.env.ignored(Some(r), "kind", "a memory keeps its kind; to move it, save a new one with supersedes");
        }
        if item.from.is_some() {
            self.env.ignored(Some(r), "from", "from belongs to working notes");
        }
        let until = self.fact_until(r, item.until.as_deref());
        let (title, body) = match item.title {
            Some(t) => {
                let (t, b) = self.fit_title(r, t, item.body);
                (Some(t), b)
            }
            None => (None, item.body),
        };
        let text_changed = title.as_ref().is_some_and(|t| memory::one_line(t) != fact.summary) || body.as_ref().is_some_and(|b| b.trim() != fact.body);
        if text_changed {
            let both = title.is_some() && body.is_some();
            let (t, b) = (title.unwrap_or_else(|| fact.summary.clone()), body.unwrap_or_else(|| fact.body.clone()));
            let vector = if both { vector } else { None };
            match memory::update(self.conn, self.ctx.data_dir, self.ctx.username, &fact.id, &t, &b, vector.as_deref()) {
                Ok(_) => {}
                Err(memory::WriteError::Archived(id)) => {
                    self.env.refused(r, format!("memory {id} is archived"));
                    return Ok(());
                }
                Err(e) => return Err(internal(e)),
            }
        }
        let until_changed = until.as_ref().is_some_and(|u| *u != fact.until);
        if let Some(u) = until.filter(|_| until_changed) {
            self.set_until(&fact.id, u)?;
        }
        let changed = text_changed || until_changed;
        if changed {
            self.record_source(&fact.id)?;
        }
        let shown = memory::read(self.ctx.data_dir, self.ctx.username, &fact.id).map_err(internal)?.map_or_else(|| fact.summary.clone(), |x| x.summary);
        self.saved(r, &fact.id, &shown, &fact.category, if changed { "updated" } else { "unchanged" });
        Ok(())
    }

    fn supersede(&mut self, r: &Ref, old: &str, item: Item, vector: Option<Vec<f32>>) -> Result<(), ToolError> {
        let f = match resolve(self.conn, self.ctx, old)? {
            Ok(Found::Fact(f)) if !f.archived => f,
            Ok(Found::Fact(f)) => {
                self.env.refused(r, format!("memory {} is archived; supersede the live one that replaced it", f.id));
                return Ok(());
            }
            Ok(Found::Note(n)) => {
                self.env.refused(r, format!("{} is a working note; give it as id with a kind to keep it as a memory", notes::short(&n.id)));
                return Ok(());
            }
            Err(why) => {
                self.env.refused(r, why);
                return Ok(());
            }
        };
        if item.kind.is_some_and(|k| k != f.category) {
            self.env.ignored(Some(r), "kind", "a memory that supersedes another keeps its kind");
        }
        let (title, body) = match item.title {
            Some(t) => self.fit_title(r, t, item.body),
            None => (f.summary.clone(), item.body),
        };
        let vector = if body.is_some() { vector } else { None };
        let body = body.unwrap_or_else(|| f.body.clone());
        let id = match memory::supersede(self.conn, self.ctx.data_dir, self.ctx.username, &f.id, &title, &body, vector.as_deref()) {
            Ok(Some(id)) => id,
            Ok(None) => {
                self.env.refused(r, format!("no memory {old}"));
                return Ok(());
            }
            Err(memory::WriteError::Archived(id)) => {
                self.env.refused(r, format!("memory {id} is archived"));
                return Ok(());
            }
            Err(e) => return Err(internal(e)),
        };
        if let Some(u) = self.fact_until(r, item.until.as_deref()) {
            self.set_until(&id, u)?;
        }
        self.record_source(&id)?;
        let row = self.saved(r, &id, &memory::one_line(&title), &f.category, "created");
        row["supersedes"] = f.id.into();
        Ok(())
    }
}

/// `memory_save`: memories and notes made, changed, kept, settled or replaced,
/// item by item.
pub fn save(conn: &Connection, ctx: &ToolCtx, raw: &str) -> Result<Value, ToolError> {
    let mut o = Obj::new(loose::args(raw)?);
    let (items, warning) = loose::items(&mut o, ITEMS, ITEM_ALIASES, ITEM_FIELDS);
    let mut w = Writer { conn, ctx, env: Envelope::default(), tz: super::task_ops::user_tz(ctx) };
    if let Some(warning) = warning {
        w.env.warn(warning);
    }
    let rest = o.rest();
    for field in &rest {
        w.env.ignored(None, field, "not a memory_save field");
    }
    if let Some(err) = &ctx.vectors.error {
        let _ = crate::log::record(conn, None, "memory_embed_error", &format!("write: {err}"));
    }
    if items.is_empty() {
        if rest.is_empty() {
            w.env.warn("nothing to save: items was empty");
        } else {
            w.env.refuse_call("nothing was saved: the memories go in items, a list");
        }
        return Ok(w.env.into_value());
    }
    for (i, v) in items.into_iter().enumerate() {
        let r = Ref::top(i);
        if let Some(item) = w.read(&r, v) {
            w.write(i, &r, item)?;
        }
    }
    Ok(w.env.into_value())
}

/// `memory_remove`: a long-term memory is archived, a note deleted.
pub fn remove(conn: &Connection, ctx: &ToolCtx, raw: &str) -> Result<Value, ToolError> {
    let mut obj = Obj::new(loose::args(raw)?);
    let mut env = Envelope::default();
    let ids = loose::ids(&mut obj, &["ids", "id", "memory_ids", "note_ids", "items"], &mut env);
    for field in obj.rest() {
        env.ignored(None, &field, "not a memory_remove field");
    }
    if ids.is_empty() {
        loose::nothing_removed(&mut env);
        return Ok(env.into_value());
    }
    for (i, value) in ids.into_iter().enumerate() {
        let r = Ref::top(i);
        let Some(named) = loose::text(&value).or_else(|| value.get("id").and_then(loose::text)) else {
            env.refused(&r, "an id is a memory's id, a note's short id or an exact title");
            continue;
        };
        match resolve(conn, ctx, &named)? {
            Err(why) => env.refused(&r, why),
            Ok(Found::Note(n)) => match notes::remove(conn, ctx.data_dir, ctx.username, &n.id) {
                Ok(()) => env.removed.push(json!({ "id": notes::short(&n.id), "title": n.title, "kind": NOTE })),
                Err(NoteError::Other(e)) => return Err(internal(e)),
                Err(e) => env.refused(&r, e.to_string()),
            },
            Ok(Found::Fact(f)) if f.archived => env.warn(format!("memory {} was already archived", f.id)),
            Ok(Found::Fact(f)) => {
                memory::archive(conn, ctx.data_dir, ctx.username, &f.id).map_err(internal)?;
                env.removed.push(json!({ "id": f.id, "title": f.summary, "kind": f.category }));
            }
        }
    }
    Ok(env.into_value())
}

fn rewrite(raw: &str, f: impl FnOnce(&mut serde_json::Map<String, Value>) -> Option<Value>) -> Result<String, ToolError> {
    let mut map = loose::args(raw)?;
    Ok(f(&mut map).unwrap_or(Value::Object(map)).to_string())
}

fn op_of(map: &mut serde_json::Map<String, Value>) -> String {
    map.remove("op").and_then(|v| loose::text(&v)).unwrap_or_default().trim().to_lowercase()
}

/// `memory_write`, read as the `memory_save` it means.
pub fn write_adapter(conn: &Connection, ctx: &ToolCtx, raw: &str) -> Result<Value, ToolError> {
    let raw = rewrite(raw, |m| {
        match op_of(m).as_str() {
            "supersede" => {
                if let Some(id) = m.remove("id") {
                    m.insert("supersedes".into(), id);
                }
            }
            "update" => {}
            _ => {
                m.remove("id");
            }
        }
        Some(json!({ ITEMS: [Value::Object(std::mem::take(m))] }))
    })?;
    save(conn, ctx, &raw)
}

/// `note_write`, read as the `memory_save` or `memory_remove` it means.
pub fn note_adapter(conn: &Connection, ctx: &ToolCtx, raw: &str) -> Result<Value, ToolError> {
    let mut removing = false;
    let raw = rewrite(raw, |m| {
        match op_of(m).as_str() {
            "remove" | "delete" => {
                removing = true;
                return Some(json!({ "ids": [m.remove("id").unwrap_or(Value::Null)] }));
            }
            "keep" => m.retain(|k, _| k == "id"),
            "add" => {
                m.remove("id");
                m.insert("kind".into(), NOTE.into());
            }
            _ => {
                m.insert("kind".into(), NOTE.into());
            }
        }
        Some(json!({ ITEMS: [Value::Object(std::mem::take(m))] }))
    })?;
    if removing {
        remove(conn, ctx, &raw)
    } else {
        save(conn, ctx, &raw)
    }
}

/// `note_settle`, read as the `memory_save` or `memory_remove` it means.
pub fn settle_adapter(conn: &Connection, ctx: &ToolCtx, raw: &str) -> Result<Value, ToolError> {
    let mut map = loose::args(raw)?;
    let id = map.remove("id").unwrap_or(Value::Null);
    let outcome = map.remove("outcome").and_then(|v| loose::text(&v)).unwrap_or_default();
    if outcome.trim().eq_ignore_ascii_case("drop") {
        return remove(conn, ctx, &json!({ "ids": [id] }).to_string());
    }
    let kind = map.remove("category").and_then(|v| kind_of(&v)).unwrap_or_else(|| {
        let note = loose::text(&id)
            .and_then(|s| notes::resolve(conn, ctx.data_dir, ctx.username, s.trim()).ok())
            .and_then(|full| notes::get(ctx.data_dir, ctx.username, &full).ok().flatten());
        if note.is_some_and(|n| n.windowed()) { "episodic" } else { "semantic" }
    });
    map.insert("id".into(), id);
    map.insert("kind".into(), kind.into());
    save(conn, ctx, &json!({ ITEMS: [Value::Object(map)] }).to_string())
}

#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, PreparedVectors, SessionKind, ToolCtx};
    use rusqlite::Connection;
    use serde_json::{json, Value};

    const USER: &str = "aitest";

    fn env() -> (Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES (?1, 'x', 'member')", [USER]).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("defaults")).unwrap();
        std::fs::write(tmp.path().join("defaults/user.toml"), "display_name = \"X\"\ntimezone = \"Asia/Tokyo\"\ntemplate = \"default\"\n").unwrap();
        (conn, tmp)
    }

    fn ctx(tmp: &tempfile::TempDir) -> ToolCtx<'_> {
        ToolCtx { config_dir: tmp.path(), data_dir: tmp.path(), user_id: 1, username: USER, vectors: PreparedVectors::default(), task_scope: None, inbox_source: None, memory_source: None, share: None, share_thread: None }
    }

    fn run(conn: &Connection, tmp: &tempfile::TempDir, kind: SessionKind, name: &str, args: impl std::fmt::Display) -> Value {
        dispatch(conn, &ctx(tmp), kind, name, &args.to_string()).unwrap()
    }

    fn talk(conn: &Connection, tmp: &tempfile::TempDir, name: &str, args: impl std::fmt::Display) -> Value {
        run(conn, tmp, SessionKind::Talk, name, args)
    }

    fn id(out: &Value, i: usize) -> String {
        out["saved"][i]["id"].as_str().unwrap_or_else(|| panic!("nothing saved at {i}: {out}")).to_string()
    }

    fn notes(conn: &Connection, tmp: &tempfile::TempDir) -> Vec<crate::notes::Note> {
        crate::notes::all(conn, tmp.path(), USER).unwrap()
    }

    #[test]
    fn a_memory_is_saved_found_read_updated_and_superseded() {
        let (conn, tmp) = env();
        let out = talk(&conn, &tmp, "memory_save", json!({"items": [{"title": "tea preference", "body": "green, no sugar"}]}));
        assert_eq!((out["saved"][0]["op"].as_str(), out["saved"][0]["kind"].as_str()), (Some("created"), Some("semantic")));
        let tea = id(&out, 0);
        assert_eq!(talk(&conn, &tmp, "memory_find", json!({"query": "tea"}))["items"][0]["id"], tea.as_str());
        assert_eq!(talk(&conn, &tmp, "memory_find", json!({"id": tea}))["item"]["body"], "green, no sugar");
        let out = talk(&conn, &tmp, "memory_save", json!({"items": [{"id": tea, "body": "green, a little honey"}]}));
        assert_eq!(out["saved"][0]["op"], "updated");
        assert_eq!(crate::memory::read(tmp.path(), USER, &tea).unwrap().unwrap().summary, "tea preference");
        let out = talk(&conn, &tmp, "memory_save", json!({"items": [{"supersedes": tea, "title": "coffee now", "body": "switched in October"}]}));
        let coffee = id(&out, 0);
        assert_eq!(out["saved"][0]["supersedes"], tea.as_str());
        assert!(crate::memory::read(tmp.path(), USER, &tea).unwrap().unwrap().archived);
        let out = talk(&conn, &tmp, "memory_save", json!({"items": [{"id": tea, "title": "x"}]}));
        assert!(out["refused"][0]["why"].as_str().unwrap().contains("archived"), "{out}");
        let out = talk(&conn, &tmp, "memory_find", json!({"query": "nothing like it"}));
        assert_eq!(out["total"], 0);
        assert!(out["warnings"].is_array());
        assert_eq!(talk(&conn, &tmp, "memory_find", json!({}))["items"][0]["id"], coffee.as_str());
    }

    #[test]
    fn a_memory_may_carry_the_day_it_stops_mattering_and_change_it() {
        let (conn, tmp) = env();
        let out = run(&conn, &tmp, SessionKind::Nightly, "memory_save",
            json!({"items": [{"kind": "episodic", "title": "Day 2026-09-17: the essay went in", "body": "drafting", "until": "2026-12-16"}]}));
        let day = id(&out, 0);
        let f = crate::memory::read(tmp.path(), USER, &day).unwrap().unwrap();
        assert_eq!((f.category.as_str(), f.until.as_deref()), ("episodic", Some("2026-12-16")));
        talk(&conn, &tmp, "memory_save", json!({"items": [{"id": day, "until": ""}]}));
        assert_eq!(crate::memory::read(tmp.path(), USER, &day).unwrap().unwrap().until, None);
        let out = talk(&conn, &tmp, "memory_save", json!({"items": [{"title": "s", "until": "someday"}]}));
        assert_eq!((out["ok"].as_bool(), out["ignored"][0]["field"].as_str()), (Some(true), Some("until")));
    }

    #[test]
    fn a_note_is_added_kept_reworded_and_removed() {
        let (conn, tmp) = env();
        let out = talk(&conn, &tmp, "memory_save", json!({"items": [{"kind": "note", "title": "  call the bank\nbefore five ", "until": "2026-10-07T17:00"}]}));
        let note = id(&out, 0);
        assert_eq!(note.len(), crate::notes::SHORT_ID);
        assert_eq!(notes(&conn, &tmp)[0].title, "call the bank before five");
        assert_eq!(notes(&conn, &tmp)[0].until.as_deref(), Some("2026-10-07T17:00:00+09:00"));
        let out = talk(&conn, &tmp, "memory_save", json!({"items": [{"id": note}]}));
        assert_eq!(out["saved"][0]["op"], "kept");
        talk(&conn, &tmp, "memory_save", json!({"items": [{"id": note, "title": "call the bank", "until": ""}]}));
        assert_eq!((notes(&conn, &tmp)[0].title.as_str(), notes(&conn, &tmp)[0].until.clone()), ("call the bank", None));
        let found = talk(&conn, &tmp, "memory_find", json!({"kind": "note"}));
        assert_eq!(found["items"][0]["id"], note.as_str());
        assert_eq!(talk(&conn, &tmp, "memory_find", json!({"query": "bank"}))["total"], 0, "notes stay out of a plain search");
        let out = talk(&conn, &tmp, "memory_remove", json!({"ids": [note]}));
        assert_eq!(out["removed"][0]["kind"], "note");
        assert!(notes(&conn, &tmp).is_empty());
    }

    #[test]
    fn a_note_added_again_by_title_is_the_same_note() {
        let (conn, tmp) = env();
        let first = id(&talk(&conn, &tmp, "memory_save", json!({"items": [{"kind": "note", "title": "Call the bank"}]})), 0);
        let out = talk(&conn, &tmp, "memory_save", json!({"items": [{"kind": "note", "title": "call the bank."}]}));
        assert_eq!((id(&out, 0), out["saved"][0]["matched"].as_str(), out["saved"][0]["op"].as_str()), (first, Some("title"), Some("kept")));
        talk(&conn, &tmp, "memory_save", json!({"items": [{"kind": "note", "title": "call the bank", "new": true}]}));
        assert_eq!(notes(&conn, &tmp).len(), 2);
    }

    #[test]
    fn a_note_too_long_or_with_a_body_is_kept_as_a_memory() {
        let (conn, tmp) = env();
        let out = talk(&conn, &tmp, "memory_save", json!({"items": [{"kind": "note", "title": "x ".repeat(100)}, {"kind": "note", "title": "pin", "body": "4321"}]}));
        assert_eq!(out["adjusted"].as_array().unwrap().iter().filter(|a| a["field"] == "kind").count(), 2, "{out}");
        assert!(notes(&conn, &tmp).is_empty());
        assert_eq!(out["saved"][1]["kind"], "semantic");
    }

    #[test]
    fn a_note_given_a_long_term_kind_settles_into_that_memory() {
        let (conn, tmp) = env();
        let note = id(&talk(&conn, &tmp, "memory_save", json!({"items": [{"kind": "note", "title": "prefers short check-ins"}]})), 0);
        let out = run(&conn, &tmp, SessionKind::Nightly, "memory_save", json!({"items": [{"id": note, "kind": "procedural"}]}));
        assert_eq!(out["saved"][0]["settled"], note.as_str());
        let f = crate::memory::read(tmp.path(), USER, &id(&out, 0)).unwrap().unwrap();
        assert_eq!((f.category.as_str(), f.summary.as_str()), ("procedural", "prefers short check-ins"));
        assert!(notes(&conn, &tmp).is_empty());
    }

    #[test]
    fn remove_archives_a_memory_and_a_title_names_one() {
        let (conn, tmp) = env();
        let fact = id(&talk(&conn, &tmp, "memory_save", json!({"items": [{"title": "Mira lives next door", "body": "since september"}]})), 0);
        let out = talk(&conn, &tmp, "memory_remove", json!({"ids": ["mira lives next door"]}));
        assert_eq!(out["removed"][0]["id"], fact.as_str());
        assert!(crate::memory::read(tmp.path(), USER, &fact).unwrap().unwrap().archived);
        talk(&conn, &tmp, "memory_save", json!({"items": [{"title": "Essay"}, {"kind": "note", "title": "essay"}]}));
        let out = talk(&conn, &tmp, "memory_remove", json!({"ids": ["Essay"]}));
        let why = out["refused"][0]["why"].as_str().unwrap();
        assert!(why.contains("note ") && why.contains("semantic "), "candidates name their kind: {why}");
        let out = talk(&conn, &tmp, "memory_remove", json!({"ids": ["00000000-0000-4000-8000-000000000404", "../../etc"]}));
        assert_eq!(out["refused"].as_array().unwrap().len(), 2);
        assert_eq!(talk(&conn, &tmp, "memory_remove", json!({"ids": []}))["ok"], true);
    }

    #[test]
    fn the_old_names_run_through_memory_save_and_say_so() {
        let (conn, tmp) = env();
        let out = talk(&conn, &tmp, "memory_write", json!({"op": "add", "category": "semantic", "summary": "likes tea", "body": "green"}));
        let tea = id(&out, 0);
        assert!(out["warnings"][0].as_str().unwrap().contains("memory_write is now memory_save"));
        let out = talk(&conn, &tmp, "memory_write", json!({"op": "supersede", "id": tea, "summary": "likes coffee", "body": "black"}));
        assert_eq!(out["saved"][0]["supersedes"], tea.as_str());
        assert_eq!(talk(&conn, &tmp, "memory_query", json!({"query": "coffee"}))["items"][0]["title"], "likes coffee");
        let note = id(&talk(&conn, &tmp, "note_write", json!({"op": "add", "title": "flight home", "until": "2026-10-07T17:00", "tz": "America/New_York"})), 0);
        assert_eq!(notes(&conn, &tmp)[0].until.as_deref(), Some("2026-10-08T06:00:00+09:00"));
        assert_eq!(talk(&conn, &tmp, "note_write", json!({"op": "keep", "id": note}))["saved"][0]["op"], "kept");
        let out = run(&conn, &tmp, SessionKind::Nightly, "note_settle", json!({"id": note, "outcome": "memory"}));
        assert_eq!(out["saved"][0]["kind"], "episodic", "a windowed note settles as episodic");
        let other = id(&talk(&conn, &tmp, "note_write", json!({"op": "add", "title": "milk"})), 0);
        assert_eq!(talk(&conn, &tmp, "note_write", json!({"op": "remove", "id": other}))["removed"][0]["title"], "milk");
    }

    #[test]
    fn a_sourced_session_records_where_each_fact_came_from() {
        let (conn, tmp) = env();
        let sourced = ToolCtx { memory_source: Some("inbox:canvas-17".into()), ..ctx(&tmp) };
        let out = dispatch(&conn, &sourced, SessionKind::Nightly, "memory_save", r#"{"items":[{"title":"runs on tuesdays","body":"with mira"}]}"#).unwrap();
        let first = id(&out, 0);
        let out = dispatch(&conn, &sourced, SessionKind::Nightly, "memory_save", &format!(r#"{{"items":[{{"supersedes":"{first}","title":"runs on thursdays","body":"alone"}}]}}"#)).unwrap();
        let second = id(&out, 0);
        talk(&conn, &tmp, "memory_save", json!({"items": [{"title": "likes tea"}]}));
        let mut stmt = conn.prepare("SELECT memory_id FROM memory_sources WHERE source_id = 'inbox:canvas-17'").unwrap();
        let mut ids: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        ids.sort();
        let mut want = vec![first, second];
        want.sort();
        assert_eq!(ids, want);
    }

    #[test]
    fn each_item_lands_on_its_own_and_unknown_fields_are_reported() {
        let (conn, tmp) = env();
        let out = talk(&conn, &tmp, "memory_save", json!({"items": [{"id": "00000000-0000-4000-8000-000000000404", "title": "x"}, {"title": "kept", "colour": "red"}]}));
        assert_eq!(out["ok"], false);
        assert_eq!(out["refused"][0]["ref"], "0");
        assert_eq!((out["saved"][0]["ref"].as_str(), out["ignored"][0]["field"].as_str()), (Some("1"), Some("colour")));
        let out = talk(&conn, &tmp, "memory_save", json!({"title": "a bare item", "body": "is the list"}));
        assert_eq!(out["saved"][0]["op"], "created");
    }

    #[test]
    fn embed_texts_follow_the_items_and_skip_notes() {
        let texts = super::embed_texts(r#"{"items":[{"title":"a","body":"b"},{"kind":"note","title":"n"},{"summary":"s"}]}"#);
        assert_eq!(texts, vec![Some(crate::memory::embed_text("a", "b")), None, Some(crate::memory::embed_text("s", ""))]);
        assert_eq!(super::embed_texts(r#"{"op":"add","summary":"s","body":"b"}"#), vec![Some(crate::memory::embed_text("s", "b"))]);
    }
}
