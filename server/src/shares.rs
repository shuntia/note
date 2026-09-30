use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use std::fmt::Write as _;

pub const PREFIX: &str = "share_";
pub const MAX_NAME_LEN: usize = 64;
pub const MAX_BRIEF_BYTES: usize = 4096;
pub const MAX_CATEGORIES: usize = 20;
pub const MAX_HORIZON_DAYS: u8 = 14;
/// Unknown-token lookups or message posts one client address may make in a
/// limiter window before it is refused.
pub const ADDRESS_ATTEMPTS: u32 = 60;
const TOUCH_INTERVAL_SECS: i64 = 60;
const MAX_EXPIRY_DAYS: u32 = 36500;

/// What a link lets its visitor learn. Stored as JSON in `shares.scope`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShareScope {
    pub today: bool,
    pub tasks: bool,
    /// Empty means every category.
    pub categories: Vec<String>,
    pub goals: bool,
    pub progress: bool,
    /// Task descriptions and notes travel; off means titles only.
    pub details: bool,
    pub horizon_days: u8,
    /// The visitor may leave a note for the owner.
    pub notes: bool,
    pub messages_per_day: u32,
}

impl Default for ShareScope {
    fn default() -> Self {
        Self {
            today: true,
            tasks: true,
            categories: Vec::new(),
            goals: true,
            progress: true,
            details: false,
            horizon_days: 3,
            notes: false,
            messages_per_day: 40,
        }
    }
}

impl ShareScope {
    /// Trims and drops blank categories; refuses a horizon or cap outside its range.
    pub fn checked(&self, limits: &Limits) -> Result<ShareScope, ShareError> {
        let mut s = self.clone();
        s.categories = s
            .categories
            .iter()
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect();
        s.categories.sort();
        s.categories.dedup();
        if s.categories.len() > MAX_CATEGORIES {
            return Err(ShareError::Invalid(format!(
                "at most {MAX_CATEGORIES} categories"
            )));
        }
        if !(1..=MAX_HORIZON_DAYS).contains(&s.horizon_days) {
            return Err(ShareError::Invalid(format!(
                "horizon_days must be 1 to {MAX_HORIZON_DAYS}"
            )));
        }
        if s.messages_per_day == 0 || s.messages_per_day > limits.messages_per_day {
            return Err(ShareError::Invalid(format!(
                "messages_per_day must be 1 to {}",
                limits.messages_per_day
            )));
        }
        Ok(s)
    }

    pub fn allows_category(&self, category: &str) -> bool {
        self.categories.is_empty() || self.categories.iter().any(|c| c == category)
    }

    /// ` AND <column> IN (?, ...)` with its parameters; `None` when every
    /// category is shared.
    pub fn category_clause(&self, column: &str) -> Option<(String, Vec<rusqlite::types::Value>)> {
        if self.categories.is_empty() {
            return None;
        }
        let marks = std::iter::repeat_n("?", self.categories.len()).collect::<Vec<_>>().join(", ");
        let params = self.categories.iter().map(|c| c.clone().into()).collect();
        Some((format!(" AND {column} IN ({marks})"), params))
    }

    /// The days this link shares, as `[start, end)` from `today`.
    pub fn horizon(&self, today: jiff::civil::Date) -> (jiff::civil::Date, jiff::civil::Date) {
        let end = today
            .checked_add(jiff::Span::new().days(i64::from(self.horizon_days)))
            .unwrap_or(jiff::civil::Date::MAX);
        (today, end)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_days: u32,
    pub messages_per_day: u32,
    pub per_user: u32,
}

impl Default for Limits {
    fn default() -> Self {
        let l = crate::config::LimitsConfig::default();
        Self {
            max_days: l.share_max_days,
            messages_per_day: l.share_messages_per_day,
            per_user: l.shares_per_user,
        }
    }
}

impl Limits {
    pub fn of(state: &crate::AppState) -> Self {
        Self {
            max_days: state.share_max_days.clamp(1, MAX_EXPIRY_DAYS),
            messages_per_day: state.share_messages_per_day.max(1),
            per_user: state.shares_per_user,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Share {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub brief: String,
    pub token: String,
    pub scope: ShareScope,
    pub expires_at: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NewShare {
    pub name: String,
    #[serde(default)]
    pub brief: String,
    #[serde(default)]
    pub scope: ShareScope,
    pub expires_at: jiff::Timestamp,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharePatch {
    pub name: Option<String>,
    pub brief: Option<String>,
    pub scope: Option<ShareScope>,
    pub expires_at: Option<jiff::Timestamp>,
}

#[derive(Debug, Error)]
pub enum ShareError {
    #[error("{0}")]
    Invalid(String),
    #[error("share links are off, or the cap is reached")]
    TooMany,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("os rng");
    format!(
        "{PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

pub fn url_for(public_base_url: &str, token: &str) -> String {
    format!("{}/s/{token}", public_base_url.trim_end_matches('/'))
}

/// A past expiry is refused; one beyond the ceiling is pulled back to it.
pub fn clamp_expiry(
    requested: jiff::Timestamp,
    now: jiff::Timestamp,
    max_days: u32,
) -> Result<jiff::Timestamp, ShareError> {
    if requested <= now {
        return Err(ShareError::Invalid(
            "expires_at must be in the future".into(),
        ));
    }
    let ceiling = now
        .checked_add(jiff::SignedDuration::from_hours(i64::from(max_days) * 24))
        .map_err(|_| ShareError::Invalid("share_max_days reaches past the calendar".into()))?;
    Ok(if requested > ceiling {
        ceiling
    } else {
        requested
    })
}

fn checked_name(raw: &str) -> Result<String, ShareError> {
    let name = raw.trim();
    let len = name.chars().count();
    if len == 0 || len > MAX_NAME_LEN {
        return Err(ShareError::Invalid(format!(
            "name must be 1 to {MAX_NAME_LEN} characters"
        )));
    }
    Ok(name.to_string())
}

fn checked_brief(raw: &str) -> Result<String, ShareError> {
    let brief = raw.trim();
    if brief.len() > MAX_BRIEF_BYTES {
        return Err(ShareError::Invalid(format!(
            "brief must be at most {MAX_BRIEF_BYTES} bytes"
        )));
    }
    Ok(brief.to_string())
}

const COLS: &str =
    "id, user_id, name, brief, token, scope, expires_at, created_at, updated_at, last_used_at";

fn row_to_share(r: &rusqlite::Row) -> rusqlite::Result<Share> {
    let scope: String = r.get(5)?;
    Ok(Share {
        id: r.get(0)?,
        user_id: r.get(1)?,
        name: r.get(2)?,
        brief: r.get(3)?,
        token: r.get(4)?,
        scope: serde_json::from_str(&scope).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
        })?,
        expires_at: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
        last_used_at: r.get(9)?,
    })
}

pub fn create(
    conn: &Connection,
    user_id: i64,
    new: &NewShare,
    now: jiff::Timestamp,
    limits: &Limits,
) -> Result<Share, ShareError> {
    let name = checked_name(&new.name)?;
    let brief = checked_brief(&new.brief)?;
    let scope = new.scope.checked(limits)?;
    let expires_at = clamp_expiry(new.expires_at, now, limits.max_days)?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM shares WHERE user_id = ?1",
        [user_id],
        |r| r.get(0),
    )?;
    if count >= i64::from(limits.per_user) {
        return Err(ShareError::TooMany);
    }
    let token = generate_token();
    conn.execute(
        "INSERT INTO shares (user_id, name, brief, token, scope, expires_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        (user_id, &name, &brief, &token, serde_json::to_string(&scope)?, expires_at.to_string(), now.to_string()),
    )?;
    Ok(get(conn, user_id, conn.last_insert_rowid())?.expect("row was just created"))
}

pub fn list(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<Share>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM shares WHERE user_id = ?1 ORDER BY id"
    ))?;
    let rows = stmt.query_map([user_id], row_to_share)?;
    rows.collect()
}

pub fn get(conn: &Connection, user_id: i64, id: i64) -> rusqlite::Result<Option<Share>> {
    conn.query_row(
        &format!("SELECT {COLS} FROM shares WHERE id = ?1 AND user_id = ?2"),
        (id, user_id),
        row_to_share,
    )
    .optional()
}

/// `Ok(None)` when the id is not this user's. The token is never changed.
pub fn update(
    conn: &Connection,
    user_id: i64,
    id: i64,
    patch: &SharePatch,
    now: jiff::Timestamp,
    limits: &Limits,
) -> Result<Option<Share>, ShareError> {
    let Some(before) = get(conn, user_id, id)? else {
        return Ok(None);
    };
    let name = match &patch.name {
        Some(n) => checked_name(n)?,
        None => before.name,
    };
    let brief = match &patch.brief {
        Some(b) => checked_brief(b)?,
        None => before.brief,
    };
    let scope = match &patch.scope {
        Some(s) => s.checked(limits)?,
        None => before.scope,
    };
    let expires_at = match patch.expires_at {
        Some(e) => clamp_expiry(e, now, limits.max_days)?.to_string(),
        None => before.expires_at,
    };
    conn.execute(
        "UPDATE shares SET name = ?1, brief = ?2, scope = ?3, expires_at = ?4, updated_at = ?5 WHERE id = ?6",
        (&name, &brief, serde_json::to_string(&scope)?, &expires_at, now.to_string(), id),
    )?;
    Ok(get(conn, user_id, id)?)
}

/// Returns the removed link, or `None` when the id is not this user's.
pub fn revoke(conn: &Connection, user_id: i64, id: i64) -> rusqlite::Result<Option<Share>> {
    let found = get(conn, user_id, id)?;
    if found.is_some() {
        conn.execute("DELETE FROM shares WHERE id = ?1", [id])?;
    }
    Ok(found)
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub share: Share,
    pub owner_username: String,
}

/// An expired link or a disabled owner resolves to `None` exactly like an
/// unknown token. Touches `last_used_at` at most once a minute.
pub fn resolve(
    conn: &Connection,
    token: &str,
    now: jiff::Timestamp,
) -> rusqlite::Result<Option<Resolved>> {
    let row: Option<(Share, String, bool)> = conn
        .query_row(
            &format!(
                "SELECT {}, u.username, u.disabled FROM shares s JOIN users u ON u.id = s.user_id WHERE s.token = ?1",
                COLS.split(", ").map(|c| format!("s.{c}")).collect::<Vec<_>>().join(", ")
            ),
            [token],
            |r| Ok((row_to_share(r)?, r.get(10)?, r.get(11)?)),
        )
        .optional()?;
    let Some((share, owner_username, disabled)) = row else {
        return Ok(None);
    };
    if disabled {
        return Ok(None);
    }
    let expires: jiff::Timestamp = match share.expires_at.parse() {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };
    if expires <= now {
        return Ok(None);
    }
    let stale = match share
        .last_used_at
        .as_deref()
        .and_then(|s| s.parse::<jiff::Timestamp>().ok())
    {
        Some(last) => (now.as_second() - last.as_second()) > TOUCH_INTERVAL_SECS,
        None => true,
    };
    if stale {
        conn.execute(
            "UPDATE shares SET last_used_at = ?1 WHERE id = ?2",
            (now.to_string(), share.id),
        )?;
    }
    Ok(Some(Resolved {
        share,
        owner_username,
    }))
}

/// Visitor turns across every thread of the link since `since`.
pub fn messages_today(
    conn: &Connection,
    share_id: i64,
    since: jiff::Timestamp,
) -> rusqlite::Result<u32> {
    conn.query_row(
        "SELECT COUNT(*) FROM share_messages m JOIN share_threads t ON t.id = m.thread_id
         WHERE t.share_id = ?1 AND m.role = 'user' AND m.created_at > ?2",
        (share_id, since.to_string()),
        |r| r.get(0),
    )
}

pub fn new_thread(
    conn: &Connection,
    share_id: i64,
    visitor_key: &str,
    now: jiff::Timestamp,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO share_threads (share_id, visitor_key, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
        (share_id, visitor_key, now.to_string()),
    )?;
    Ok(conn.last_insert_rowid())
}

/// `Some(id)` only when the thread was started on this link by this visitor.
pub fn thread_of(
    conn: &Connection,
    share_id: i64,
    visitor_key: &str,
    id: i64,
) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT id FROM share_threads WHERE id = ?1 AND share_id = ?2 AND visitor_key = ?3",
        (id, share_id, visitor_key),
        |r| r.get(0),
    )
    .optional()
}

/// Records one opening of the link; `km` is measured from where the owner was
/// last seen, when both places are known.
pub fn record_visit(
    conn: &Connection,
    share_id: i64,
    owner_id: i64,
    visitor_key: &str,
    place: &crate::net::Place,
    now: jiff::Timestamp,
) -> rusqlite::Result<()> {
    let owner: Option<(f64, f64)> = conn
        .query_row(
            "SELECT seen_lat, seen_lon FROM users WHERE id = ?1 AND seen_lat IS NOT NULL AND seen_lon IS NOT NULL",
            [owner_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let km = owner.zip(place.coords).map(|(a, b)| crate::net::km(a, b));
    conn.execute(
        "INSERT INTO share_visits (share_id, visitor_key, city, country, km, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        (share_id, visitor_key, &place.city, &place.country, km, now.to_string()),
    )?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct VisitOut {
    pub city: Option<String>,
    pub country: Option<String>,
    pub km: Option<f64>,
    pub distant: bool,
    pub at: String,
}

/// Every opening of a link, newest first.
pub fn visits(conn: &Connection, share_id: i64, distant_km: u32) -> rusqlite::Result<Vec<VisitOut>> {
    let mut stmt = conn.prepare("SELECT city, country, km, at FROM share_visits WHERE share_id = ?1 ORDER BY id DESC")?;
    let rows = stmt.query_map([share_id], |r| {
        let km: Option<f64> = r.get(2)?;
        Ok(VisitOut {
            city: r.get(0)?,
            country: r.get(1)?,
            km,
            distant: km.is_some_and(|k| k > f64::from(distant_km)),
            at: r.get(3)?,
        })
    })?;
    rows.collect()
}

/// Distinct visitors a link has had, and how many of its openings were distant.
pub fn visit_counts(conn: &Connection, share_id: i64, distant_km: u32) -> rusqlite::Result<(i64, i64)> {
    conn.query_row(
        "SELECT COUNT(DISTINCT visitor_key), COALESCE(SUM(km > ?2), 0) FROM share_visits WHERE share_id = ?1",
        (share_id, f64::from(distant_km)),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

/// The last `limit` user and assistant turns, oldest first; notes stay out.
pub fn history(
    conn: &Connection,
    thread_id: i64,
    limit: usize,
) -> rusqlite::Result<Vec<crate::providers::Message>> {
    let mut stmt = conn.prepare(
        "SELECT role, content FROM share_messages WHERE thread_id = ?1 AND role IN ('user','assistant') ORDER BY id DESC LIMIT ?2",
    )?;
    let mut out: Vec<crate::providers::Message> = stmt
        .query_map((thread_id, limit as i64), |r| {
            let role: String = r.get(0)?;
            let content: String = r.get(1)?;
            Ok(match role.as_str() {
                "user" => crate::providers::Message::User(content),
                _ => crate::providers::Message::Assistant {
                    text: content,
                    tool_calls: vec![],
                },
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    out.reverse();
    Ok(out)
}

/// Returns the new message's id.
pub fn append(
    conn: &Connection,
    thread_id: i64,
    role: &str,
    content: &str,
    now: jiff::Timestamp,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO share_messages (thread_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
        (thread_id, role, content, now.to_string()),
    )?;
    let id = conn.last_insert_rowid();
    conn.execute(
        "UPDATE share_threads SET updated_at = ?1 WHERE id = ?2",
        (now.to_string(), thread_id),
    )?;
    Ok(id)
}

#[derive(Debug, Serialize)]
pub struct MessageOut {
    pub role: String,
    pub content: String,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct ThreadOut {
    pub id: i64,
    pub created_at: String,
    pub updated_at: String,
    pub messages: Vec<MessageOut>,
}

pub fn messages(conn: &Connection, thread_id: i64) -> rusqlite::Result<Vec<MessageOut>> {
    let mut stmt = conn.prepare(
        "SELECT role, content, created_at FROM share_messages WHERE thread_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([thread_id], |r| {
        Ok(MessageOut {
            role: r.get(0)?,
            content: r.get(1)?,
            created_at: r.get(2)?,
        })
    })?;
    rows.collect()
}

/// Every visitor thread of a link, newest first, each with all of its messages.
pub fn threads(conn: &Connection, share_id: i64) -> rusqlite::Result<Vec<ThreadOut>> {
    let mut stmt = conn.prepare("SELECT id, created_at, updated_at FROM share_threads WHERE share_id = ?1 ORDER BY updated_at DESC, id DESC")?;
    let heads: Vec<(i64, String, String)> = stmt
        .query_map([share_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    heads
        .into_iter()
        .map(|(id, created_at, updated_at)| {
            Ok(ThreadOut {
                id,
                created_at,
                updated_at,
                messages: messages(conn, id)?,
            })
        })
        .collect()
}

pub const OPENER_MAX_BYTES: usize = 8192;
/// How far back a link with progress on reaches into finished tasks.
pub const RECENT_DAYS: i64 = 7;

/// The earliest completion a link with progress on still shows.
pub fn done_since(now: jiff::Timestamp) -> jiff::Timestamp {
    now - jiff::Span::new().hours(24 * RECENT_DAYS)
}
const DONE_RECENT_MAX: usize = 40;
const GOAL_ROWS_MAX: usize = 30;
/// Task and done-recently row caps tried in order until the opener fits: the
/// task list gives way entirely before the done list is touched.
const CAPS: &[(usize, usize)] = &[(80, 40), (40, 40), (20, 40), (0, 40), (0, 10), (0, 0)];

pub struct Rendered {
    /// The `# What is shared` block for the system prompt.
    pub text: String,
    /// The same facts as data for the visitor page: `days`, `tasks`, `goals`,
    /// `done_recent`, each present only when its switch is on.
    pub view: serde_json::Value,
}

struct DayRow {
    start: String,
    end: Option<String>,
    title: String,
    status: String,
    busy: bool,
}

struct TaskRow {
    id: i64,
    title: String,
    state: String,
    due_at: Option<String>,
    urgency: String,
    pressing: bool,
    overdue: bool,
    steps: i64,
    done_steps: i64,
    category: String,
    goal_title: Option<String>,
    description: Option<String>,
    rank: u8,
}

/// One pass over the owner's data, filtered by the scope, rendered twice.
pub fn render(
    conn: &Connection,
    config_dir: &std::path::Path,
    owner_id: i64,
    owner_username: &str,
    scope: &ShareScope,
    now: jiff::Timestamp,
) -> anyhow::Result<Rendered> {
    let tz = crate::triggers::timezone(config_dir, owner_username);
    let today = now.to_zoned(tz).date();
    let mut view = serde_json::Map::new();
    let mut sections: Vec<String> = Vec::new();

    if scope.today {
        let mut days_json = Vec::new();
        let mut text = String::from("# Today and ahead\n\n");
        let mut date = today;
        for _ in 0..scope.horizon_days {
            let calendar = crate::calendar::occurrences(conn, owner_id, date)?;
            let events = crate::plan::events_for(conn, owner_id, date)?;
            let mut rows: Vec<DayRow> = calendar
                .iter()
                .map(|o| DayRow {
                    start: o.start.clone(),
                    end: Some(o.end.clone()),
                    title: o.title.clone(),
                    status: "calendar".into(),
                    busy: false,
                })
                .collect();
            for e in events.iter().filter(|e| e.kind != crate::triggers::KIND) {
                let (title, busy) = match &e.task {
                    Some(t) if !scope.allows_category(&t.category) => ("Busy".to_string(), true),
                    Some(t) => (t.title.clone(), false),
                    None => (kind_label(&e.kind), false),
                };
                rows.push(DayRow {
                    start: e.wall_time.clone(),
                    end: e.end_wall_time.clone(),
                    title,
                    status: e.status.clone(),
                    busy,
                });
            }
            rows.sort_by(|a, b| a.start.cmp(&b.start));
            let _ = writeln!(text, "{date}{}:", if date == today { " (today)" } else { "" });
            if rows.is_empty() {
                text.push_str("- nothing planned\n");
            }
            for r in &rows {
                let end = r.end.as_deref().map(|e| format!("-{e}")).unwrap_or_default();
                let _ = writeln!(text, "- {}{end} {} [{}]", r.start, r.title, r.status);
            }
            text.push('\n');
            days_json.push(serde_json::json!({
                "date": date.to_string(),
                "rows": rows.iter().map(|r| serde_json::json!({
                    "start": r.start, "end": r.end, "title": r.title, "status": r.status, "busy": r.busy,
                })).collect::<Vec<_>>(),
            }));
            date = date.tomorrow()?;
        }
        view.insert("days".into(), serde_json::Value::Array(days_json));
        sections.push(text);
    }

    let mut tasks: Vec<TaskRow> = Vec::new();
    if scope.tasks {
        for node in crate::tasks::list(conn, owner_id)? {
            let t = &node.task;
            if !(t.state == "open" || t.state == "in_progress") || !scope.allows_category(&t.category) {
                continue;
            }
            let pressing = crate::tasks::pressing_at(&t.state, t.due_at.as_deref(), now);
            tasks.push(TaskRow {
                id: t.id,
                title: t.title.clone(),
                state: t.state.clone(),
                due_at: t.due_at.clone(),
                urgency: t.urgency.clone(),
                pressing,
                overdue: t
                    .due_at
                    .as_deref()
                    .and_then(|d| d.parse::<jiff::Timestamp>().ok())
                    .is_some_and(|d| d < now),
                steps: node.children.iter().filter(|c| c.state != "dropped").count() as i64,
                done_steps: node.children.iter().filter(|c| c.state == "done").count() as i64,
                category: t.category.clone(),
                goal_title: t.goal_title.clone().filter(|_| scope.goals),
                description: scope.details.then(|| t.description.clone()),
                rank: crate::tasks::urgency_rank(&t.urgency, pressing),
            });
        }
        tasks.sort_by(|a, b| {
            a.rank
                .cmp(&b.rank)
                .then(a.due_at.is_none().cmp(&b.due_at.is_none()))
                .then(a.due_at.cmp(&b.due_at))
                .then(b.id.cmp(&a.id))
        });
        view.insert(
            "tasks".into(),
            serde_json::Value::Array(
                tasks
                    .iter()
                    .map(|t| {
                        let mut v = serde_json::json!({
                            "id": t.id, "title": t.title, "state": t.state, "due_at": t.due_at,
                            "urgency": t.urgency, "pressing": t.pressing, "steps": t.steps,
                            "done_steps": t.done_steps, "category": t.category,
                        });
                        if scope.goals {
                            v["goal_title"] = serde_json::json!(t.goal_title);
                        }
                        if let Some(d) = &t.description {
                            v["description"] = serde_json::json!(d);
                        }
                        v
                    })
                    .collect(),
            ),
        );
    }

    let mut goals_text = String::new();
    if scope.goals {
        let mut rows = Vec::new();
        goals_text.push_str("# Goals\n\n");
        for g in crate::goals::list(conn, owner_id, None)? {
            let (total, done) = goal_counts(conn, g.id, scope)?;
            if total == 0 && !scope.categories.is_empty() {
                continue;
            }
            if rows.len() < GOAL_ROWS_MAX {
                let due = g.due_at.as_deref().map(|d| format!(", due {}", day_of(d))).unwrap_or_default();
                let _ = writeln!(goals_text, "- {} ({done} of {total} tasks done{due})", g.title);
            }
            rows.push(serde_json::json!({
                "id": g.id, "title": g.title, "due_at": g.due_at, "tasks": total, "done_tasks": done,
            }));
        }
        if rows.is_empty() {
            goals_text.push_str("- none\n");
        }
        if rows.len() > GOAL_ROWS_MAX {
            let _ = writeln!(goals_text, "- and {} more", rows.len() - GOAL_ROWS_MAX);
        }
        goals_text.push('\n');
        view.insert("goals".into(), serde_json::Value::Array(rows));
    }

    let mut done_recent: Vec<(String, String)> = Vec::new();
    if scope.progress {
        let since = done_since(now).to_string();
        let (filter, category_params) = scope.category_clause("category").unwrap_or_default();
        let mut stmt = conn.prepare(&format!(
            "SELECT title, completed_at FROM tasks
             WHERE user_id = ? AND parent_id IS NULL AND state = 'done' AND completed_at >= ?{filter}
             ORDER BY completed_at DESC LIMIT {DONE_RECENT_MAX}"
        ))?;
        let params: Vec<rusqlite::types::Value> =
            [owner_id.into(), since.into()].into_iter().chain(category_params).collect();
        done_recent = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        view.insert(
            "done_recent".into(),
            serde_json::Value::Array(
                done_recent
                    .iter()
                    .map(|(t, at)| serde_json::json!({ "title": t, "completed_at": at }))
                    .collect(),
            ),
        );
    }

    let render_text = |task_cap: usize, done_cap: usize| -> String {
        let mut s = String::from("# What is shared\n\n");
        for sec in &sections {
            s.push_str(sec);
        }
        if scope.tasks {
            s.push_str("# Open tasks\n\n");
            let urgent: Vec<&TaskRow> = tasks.iter().filter(|t| t.rank <= 1).collect();
            if !urgent.is_empty() && task_cap > 0 {
                s.push_str("Urgent:\n");
                for t in urgent.iter().take(task_cap) {
                    s.push_str(&task_line(t, scope));
                }
            }
            let rest: Vec<&TaskRow> = tasks.iter().filter(|t| t.rank > 1).collect();
            let room = task_cap.saturating_sub(urgent.len().min(task_cap));
            if !rest.is_empty() && room > 0 {
                s.push_str("Others:\n");
                for t in rest.iter().take(room) {
                    s.push_str(&task_line(t, scope));
                }
            }
            let shown = urgent.len().min(task_cap) + rest.len().min(room);
            if tasks.len() > shown {
                let _ = writeln!(s, "- and {} more; ask", tasks.len() - shown);
            }
            if tasks.is_empty() {
                s.push_str("- none open\n");
            }
            s.push('\n');
        }
        s.push_str(&goals_text);
        if scope.progress {
            let _ = write!(s, "# Done in the last {RECENT_DAYS} days\n\n");
            if done_recent.is_empty() {
                s.push_str("- nothing yet\n");
            }
            for (t, at) in done_recent.iter().take(done_cap) {
                let _ = writeln!(s, "- {t} ({})", day_of(at));
            }
            if done_recent.len() > done_cap {
                let _ = writeln!(s, "- and {} more", done_recent.len() - done_cap);
            }
            s.push('\n');
        }
        s
    };
    let mut text = render_text(CAPS[0].0, CAPS[0].1);
    for (t, d) in &CAPS[1..] {
        if text.len() <= OPENER_MAX_BYTES {
            break;
        }
        text = render_text(*t, *d);
    }
    if text.len() > OPENER_MAX_BYTES {
        let mut cut = OPENER_MAX_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    Ok(Rendered { text, view: serde_json::Value::Object(view) })
}

/// `checkin_call` reads "Checkin call".
fn kind_label(kind: &str) -> String {
    let spaced = kind.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => spaced,
    }
}

fn day_of(timestamp: &str) -> &str {
    timestamp.get(..10).unwrap_or(timestamp)
}

fn task_line(t: &TaskRow, scope: &ShareScope) -> String {
    let mut s = format!("- {}", t.title);
    if t.urgency == "high" {
        s.push_str(" [urgent]");
    } else if t.pressing {
        s.push_str(if t.overdue { " [overdue]" } else { " [due soon]" });
    }
    if let Some(d) = &t.due_at {
        let _ = write!(s, ", due {}", day_of(d));
    }
    if t.steps > 0 {
        let _ = write!(s, ", {} of {} steps done", t.done_steps, t.steps);
    }
    if scope.categories.len() != 1 && !t.category.is_empty() {
        let _ = write!(s, " ({})", t.category);
    }
    if let Some(g) = &t.goal_title {
        let _ = write!(s, ", goal: {g}");
    }
    if let Some(d) = t.description.as_deref().filter(|d| !d.trim().is_empty()) {
        let _ = write!(s, " — {}", d.trim().chars().take(200).collect::<String>());
    }
    let _ = writeln!(s, " (task_id {})", t.id);
    s
}

/// A goal's non-dropped top-level tasks and how many are done, counted from the
/// shared categories alone.
pub fn goal_counts(conn: &Connection, goal_id: i64, scope: &ShareScope) -> rusqlite::Result<(i64, i64)> {
    let (filter, category_params) = scope.category_clause("category").unwrap_or_default();
    let params: Vec<rusqlite::types::Value> =
        std::iter::once(goal_id.into()).chain(category_params).collect();
    conn.query_row(
        &format!(
            "SELECT COUNT(*), COALESCE(SUM(state = 'done'), 0) FROM tasks
             WHERE goal_id = ? AND parent_id IS NULL AND state != 'dropped'{filter}"
        ),
        rusqlite::params_from_iter(params.iter()),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

/// The goal's open or in-progress top-level task due soonest, from the shared
/// categories alone: its id, title and due time.
pub fn goal_next_task(
    conn: &Connection,
    goal_id: i64,
    scope: &ShareScope,
) -> rusqlite::Result<Option<(i64, String, Option<String>)>> {
    let (filter, category_params) = scope.category_clause("category").unwrap_or_default();
    let params: Vec<rusqlite::types::Value> =
        std::iter::once(goal_id.into()).chain(category_params).collect();
    conn.query_row(
        &format!(
            "SELECT id, title, due_at FROM tasks
             WHERE goal_id = ? AND parent_id IS NULL AND state IN ('open','in_progress'){filter}
             ORDER BY due_at IS NULL, due_at, id LIMIT 1"
        ),
        rusqlite::params_from_iter(params.iter()),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()
}

const HISTORY_LIMIT: usize = 40;
pub const MAX_MESSAGE: usize = crate::talk::MAX_MESSAGE;

/// The visitor's thread key, placed on the request by the share middleware.
#[derive(Debug, Clone)]
pub struct VisitorKey(pub String);

pub struct VisitorTurn {
    pub thread: i64,
    pub reply: String,
    /// Whether the session filed a note for the owner.
    pub note: bool,
}

#[derive(Debug)]
pub enum TurnError {
    Blank,
    /// The thread named is not this visitor's on this link.
    NoThread,
    Cap,
    Busy,
    /// The session did not finish. The turn is not stored, but a note the
    /// session already filed stays on the thread undelivered.
    Unavailable,
    Internal,
}

/// Continues `thread` when given, and otherwise starts a new one.
pub async fn run_turn(state: &crate::AppState, principal: &crate::auth::SharePrincipal, visitor_key: &str, thread: Option<i64>, message: &str) -> Result<VisitorTurn, TurnError> {
    let message = message.trim().to_string();
    if message.is_empty() || message.len() > MAX_MESSAGE {
        return Err(TurnError::Blank);
    }
    let share = principal.share.clone();
    let now = jiff::Timestamp::now();
    {
        let conn = state.db();
        let used = messages_today(&conn, share.id, now - jiff::Span::new().hours(24)).map_err(|_| TurnError::Internal)?;
        if used >= share.scope.messages_per_day {
            return Err(TurnError::Cap);
        }
    }
    if let Some(id) = thread {
        if thread_of(&state.db(), share.id, visitor_key, id).map_err(|_| TurnError::Internal)?.is_none() {
            return Err(TurnError::NoThread);
        }
    }
    let permit = state.talk_gate.try_enter_global().map_err(|_| TurnError::Busy)?;
    let thread_id = match thread {
        Some(id) => id,
        None => new_thread(&state.db(), share.id, visitor_key, now).map_err(|_| TurnError::Internal)?,
    };
    let started = thread.is_none();
    let st = state.clone();
    let owner_id = principal.owner_id;
    let owner = principal.owner_username.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let deps = crate::agent::SessionDeps {
            db: &st.db,
            config_dir: &st.config_dir,
            data_dir: &st.data_dir,
            llm: st.llm.as_ref(),
            embeddings: None,
            search: None,
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            token_id: None,
            thread_note: None,
            share: Some(crate::agent::ShareSession { id: share.id, thread_id, brief: share.brief.clone(), scope: share.scope.clone() }),
        };
        let past = history(&st.db(), thread_id, HISTORY_LIMIT)?;
        let question = append(&st.db(), thread_id, "user", &message, now)?;
        let answered = (|| {
            let out = crate::agent::run_session(&deps, owner_id, &owner, crate::tools::SessionKind::Share, now, &past, &message)?;
            let noted: Vec<String> = out
                .steps
                .iter()
                .filter(|s| s.name == "share_note" && !s.is_error)
                .map(|s| {
                    serde_json::from_str::<serde_json::Value>(&s.args)
                        .ok()
                        .and_then(|v| v["text"].as_str().map(|t| t.trim().to_string()))
                        .unwrap_or_default()
                })
                .collect();
            let reply = if !noted.is_empty() {
                let display = crate::config::UserConfig::load(&st.config_dir, &owner).map_or_else(|_| owner.clone(), |c| c.display_name);
                format!("Passed on to {display}.")
            } else if out.reply.trim().is_empty() {
                crate::EMPTY_REPLY_FALLBACK.to_string()
            } else {
                out.reply.clone()
            };
            append(&st.db(), thread_id, "assistant", &reply, jiff::Timestamp::now())?;
            Ok::<_, anyhow::Error>((reply, noted))
        })();
        let (reply, noted) = match answered {
            Ok(done) => done,
            Err(e) => {
                let _ = st.db().execute("DELETE FROM share_messages WHERE id = ?1", [question]);
                if started {
                    let _ = st.db().execute(
                        "DELETE FROM share_threads WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM share_messages WHERE thread_id = ?1)",
                        [thread_id],
                    );
                }
                return Err(e);
            }
        };
        for text in &noted {
            let msg = crate::channels::OutboundMessage {
                title: format!("Note from {}", share.name),
                body: text.clone(),
                urgency: crate::channels::Urgency::Normal,
                event_id: None,
                conversation_id: None,
                actions: Vec::new(),
            };
            crate::channels::deliver_via(&st.db, &st.channels, owner_id, &owner, &msg);
        }
        Ok::<_, anyhow::Error>(VisitorTurn { thread: thread_id, reply, note: !noted.is_empty() })
    })
    .await;
    match result {
        Ok(Ok(turn)) => Ok(turn),
        Ok(Err(_)) => Err(TurnError::Unavailable),
        Err(_) => Err(TurnError::Internal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        conn
    }

    fn now() -> jiff::Timestamp {
        "2026-09-24T12:00:00Z".parse().unwrap()
    }

    fn new(name: &str) -> NewShare {
        NewShare {
            name: name.into(),
            brief: String::new(),
            scope: ShareScope::default(),
            expires_at: now() + jiff::Span::new().hours(30 * 24),
        }
    }

    #[test]
    fn scope_defaults_and_validates() {
        let s: ShareScope = serde_json::from_str("{}").unwrap();
        assert_eq!(s, ShareScope::default());
        assert!(s.today && s.tasks && s.goals && s.progress && !s.details && !s.notes);
        assert_eq!(s.horizon_days, 3);
        assert_eq!(s.messages_per_day, 40);
        let bad = ShareScope {
            horizon_days: 15,
            ..ShareScope::default()
        };
        assert!(matches!(
            bad.checked(&Limits::default()),
            Err(ShareError::Invalid(_))
        ));
        let bad = ShareScope {
            messages_per_day: 101,
            ..ShareScope::default()
        };
        assert!(matches!(
            bad.checked(&Limits::default()),
            Err(ShareError::Invalid(_))
        ));
        let ok = ShareScope {
            categories: vec![" school ".into(), String::new()],
            ..ShareScope::default()
        };
        assert_eq!(
            ok.checked(&Limits::default()).unwrap().categories,
            vec!["school".to_string()]
        );
    }

    #[test]
    fn an_unreadable_stored_scope_is_an_error_not_the_default() {
        let conn = conn();
        let s = create(&conn, 1, &new("Mom"), now(), &Limits::default()).unwrap();
        conn.execute("UPDATE shares SET scope = '{\"nope\":1}' WHERE id = ?1", [s.id]).unwrap();
        assert!(get(&conn, 1, s.id).is_err());
        assert!(list(&conn, 1).is_err());
        assert!(resolve(&conn, &s.token, now()).is_err());
    }

    #[test]
    fn zero_shares_per_user_turns_links_off() {
        let conn = conn();
        let limits = Limits { per_user: 0, ..Limits::default() };
        assert!(matches!(create(&conn, 1, &new("Mom"), now(), &limits), Err(ShareError::TooMany)));
    }

    #[test]
    fn zero_expiry_and_message_ceilings_floor_at_one() {
        let mut state = crate::AppState::new(
            crate::db::open_memory().unwrap(),
            std::path::PathBuf::new(),
            std::path::PathBuf::new(),
        );
        state.share_max_days = 0;
        state.share_messages_per_day = 0;
        let limits = Limits::of(&state);
        assert_eq!(limits.max_days, 1);
        assert_eq!(limits.messages_per_day, 1);
        state.share_max_days = u32::MAX;
        assert_eq!(Limits::of(&state).max_days, MAX_EXPIRY_DAYS);
    }

    #[test]
    fn an_expiry_ceiling_past_the_calendar_is_refused_not_a_panic() {
        let far = now() + jiff::Span::new().hours(400 * 24);
        assert!(matches!(clamp_expiry(far, now(), u32::MAX), Err(ShareError::Invalid(_))));
    }

    #[test]
    fn a_brief_is_measured_after_trimming() {
        let padded = format!("  {}  ", "a".repeat(MAX_BRIEF_BYTES));
        assert_eq!(checked_brief(&padded).unwrap().len(), MAX_BRIEF_BYTES);
        assert!(checked_brief(&"a".repeat(MAX_BRIEF_BYTES + 1)).is_err());
    }

    #[test]
    fn a_token_carries_the_prefix_and_is_unique() {
        let a = generate_token();
        let b = generate_token();
        assert!(a.starts_with(PREFIX) && b.starts_with(PREFIX));
        assert_ne!(a, b);
        assert!(a.len() > 40);
    }

    #[test]
    fn expiry_clamps_to_the_ceiling_and_refuses_the_past() {
        let far = now() + jiff::Span::new().hours(400 * 24);
        assert_eq!(
            clamp_expiry(far, now(), 120).unwrap(),
            now() + jiff::Span::new().hours(120 * 24)
        );
        let near = now() + jiff::Span::new().hours(7 * 24);
        assert_eq!(clamp_expiry(near, now(), 120).unwrap(), near);
        assert!(matches!(
            clamp_expiry(now() - jiff::Span::new().hours(1), now(), 120),
            Err(ShareError::Invalid(_))
        ));
    }

    #[test]
    fn create_list_update_revoke_and_the_cap() {
        let conn = conn();
        let limits = Limits {
            per_user: 2,
            ..Limits::default()
        };
        let a = create(&conn, 1, &new("Mom"), now(), &limits).unwrap();
        assert_eq!(a.name, "Mom");
        assert!(a.token.starts_with(PREFIX));
        let _b = create(&conn, 1, &new("Tutor"), now(), &limits).unwrap();
        assert!(matches!(
            create(&conn, 1, &new("Third"), now(), &limits),
            Err(ShareError::TooMany)
        ));
        assert!(matches!(
            create(&conn, 1, &new(""), now(), &limits),
            Err(ShareError::Invalid(_))
        ));
        assert_eq!(list(&conn, 1).unwrap().len(), 2);
        let patched = update(
            &conn,
            1,
            a.id,
            &SharePatch {
                name: Some("Mother".into()),
                brief: Some("be kind".into()),
                scope: None,
                expires_at: None,
            },
            now(),
            &limits,
        )
        .unwrap()
        .unwrap();
        assert_eq!(patched.name, "Mother");
        assert_eq!(patched.brief, "be kind");
        assert_eq!(patched.token, a.token, "the token never changes");
        assert!(revoke(&conn, 1, a.id).unwrap().is_some());
        assert!(revoke(&conn, 1, a.id).unwrap().is_none());
        assert!(
            revoke(&conn, 2, patched.id).unwrap().is_none(),
            "another user's id is not found"
        );
    }

    #[test]
    fn resolve_refuses_expired_missing_and_disabled_and_throttles_touch() {
        let conn = conn();
        let s = create(&conn, 1, &new("Mom"), now(), &Limits::default()).unwrap();
        assert!(resolve(&conn, "share_nope", now()).unwrap().is_none());
        let r = resolve(&conn, &s.token, now()).unwrap().unwrap();
        assert_eq!(r.share.id, s.id);
        assert_eq!(r.owner_username, "aki");
        let first = get(&conn, 1, s.id).unwrap().unwrap().last_used_at.unwrap();
        resolve(&conn, &s.token, now() + jiff::Span::new().seconds(10)).unwrap();
        assert_eq!(
            get(&conn, 1, s.id).unwrap().unwrap().last_used_at.unwrap(),
            first
        );
        resolve(&conn, &s.token, now() + jiff::Span::new().seconds(120)).unwrap();
        assert_ne!(
            get(&conn, 1, s.id).unwrap().unwrap().last_used_at.unwrap(),
            first
        );
        assert!(
            resolve(&conn, &s.token, now() + jiff::Span::new().hours(31 * 24))
                .unwrap()
                .is_none(),
            "expired"
        );
        conn.execute("UPDATE users SET disabled = 1 WHERE id = 1", [])
            .unwrap();
        assert!(
            resolve(&conn, &s.token, now()).unwrap().is_none(),
            "disabled owner"
        );
    }

    #[test]
    fn threads_belong_to_their_visitor_and_history_reads_back_in_order() {
        let conn = conn();
        let s = create(&conn, 1, &new("Mom"), now(), &Limits::default()).unwrap();
        let t1 = new_thread(&conn, s.id, "v1", now()).unwrap();
        let again = new_thread(&conn, s.id, "v1", now()).unwrap();
        assert_ne!(t1, again, "a visitor may hold many threads");
        assert_eq!(thread_of(&conn, s.id, "v1", t1).unwrap(), Some(t1));
        assert_eq!(thread_of(&conn, s.id, "v2", t1).unwrap(), None, "another visitor's thread");
        assert_eq!(thread_of(&conn, s.id + 1, "v1", t1).unwrap(), None, "another link's thread");
        append(&conn, t1, "user", "hi", now()).unwrap();
        append(&conn, t1, "assistant", "hello", now()).unwrap();
        append(&conn, t1, "note", "tell aki", now()).unwrap();
        let h = history(&conn, t1, 40).unwrap();
        assert_eq!(h.len(), 2, "notes stay out of the model's history");
        assert!(matches!(&h[0], crate::providers::Message::User(t) if t == "hi"));
        assert_eq!(
            messages_today(&conn, s.id, now() - jiff::Span::new().hours(24)).unwrap(),
            1
        );
        let all = threads(&conn, s.id).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all.iter().find(|t| t.id == t1).unwrap().messages.len(), 3);
        revoke(&conn, 1, s.id).unwrap();
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM share_messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "revocation cascades");
    }

    #[test]
    fn visits_measure_from_the_owner_and_count_distinct_visitors() {
        let conn = conn();
        let s = create(&conn, 1, &new("Mom"), now(), &Limits::default()).unwrap();
        let far = crate::net::Place { city: Some("Tokyo".into()), country: Some("JP".into()), coords: Some((35.68, 139.65)) };
        record_visit(&conn, s.id, 1, "v1", &far, now()).unwrap();
        assert_eq!(visits(&conn, s.id, 300).unwrap()[0].km, None, "no distance before the owner is seen");

        conn.execute("UPDATE users SET seen_lat = 47.61, seen_lon = -122.33 WHERE id = 1", []).unwrap();
        let near = crate::net::Place { city: Some("Portland".into()), country: Some("US".into()), coords: Some((45.52, -122.68)) };
        record_visit(&conn, s.id, 1, "v1", &near, now()).unwrap();
        record_visit(&conn, s.id, 1, "v2", &far, now()).unwrap();
        record_visit(&conn, s.id, 1, "v3", &crate::net::Place::default(), now()).unwrap();

        let all = visits(&conn, s.id, 300).unwrap();
        assert_eq!(all.len(), 4);
        assert!(all[0].km.is_none() && !all[0].distant, "an unplaced visit is not marked");
        assert!(all[1].distant && all[1].country.as_deref() == Some("JP"));
        assert!(!all[2].distant && all[2].km.unwrap() < 300.0);
        assert_eq!(visit_counts(&conn, s.id, 300).unwrap(), (3, 1));
        assert_eq!(visit_counts(&conn, s.id, 10_000).unwrap(), (3, 0));
    }

    fn seed_owner(conn: &Connection) -> (tempfile::TempDir, i64) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("defaults")).unwrap();
        std::fs::write(tmp.path().join("defaults/user.toml"), "display_name = \"Aki\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n").unwrap();
        let mk = |title: &str, cat: &str, urgency: &str, due: Option<&str>| {
            crate::tasks::create(
                conn,
                1,
                crate::tasks::NewTask {
                    title: title.into(),
                    category: Some(cat.into()),
                    urgency: Some(urgency.into()),
                    due_at: due.map(|d| Some(d.to_string())),
                    description: Some("private detail".into()),
                    ..Default::default()
                },
                "manual",
                crate::tasks::Actor::User,
            )
            .unwrap()
            .id
        };
        let lab = mk("lab report", "school", "high", None);
        mk("problem set", "school", "normal", Some("2026-09-25T00:00:00Z"));
        mk("therapy forms", "health", "normal", None);
        let done = mk("reading", "school", "normal", None);
        crate::tasks::update(conn, 1, done, &crate::tasks::TaskPatch { state: Some("done".into()), ..Default::default() }).unwrap();
        (tmp, lab)
    }

    #[test]
    fn render_keeps_to_the_scope_and_leads_with_urgency() {
        let conn = conn();
        let (tmp, _) = seed_owner(&conn);
        let scope = ShareScope { categories: vec!["school".into()], ..ShareScope::default() };
        let r = render(&conn, tmp.path(), 1, "aki", &scope, now()).unwrap();
        assert!(r.text.contains("# What is shared"));
        assert!(r.text.contains("lab report"), "{}", r.text);
        assert!(!r.text.contains("therapy"), "a hidden category never renders:\n{}", r.text);
        assert!(!r.text.contains("private detail"), "details are off:\n{}", r.text);
        let urgent = r.text.find("Urgent").unwrap();
        assert!(urgent < r.text.find("lab report").unwrap());
        assert!(r.text.find("lab report").unwrap() < r.text.find("problem set").unwrap(), "high before pressing");
        assert_eq!(r.view["tasks"][0]["title"], "lab report");
        assert_eq!(r.view["tasks"][0]["urgency"], "high");
        assert_eq!(r.view["tasks"][1]["pressing"], true);
        assert!(r.view["tasks"].as_array().unwrap().iter().all(|t| t.get("description").is_none()));
        assert_eq!(r.view["done_recent"][0]["title"], "reading");
        assert_eq!(r.view["days"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn render_leaves_out_what_is_switched_off_and_carries_details_when_asked() {
        let conn = conn();
        let (tmp, _) = seed_owner(&conn);
        let scope = ShareScope { today: false, goals: false, progress: false, details: true, ..ShareScope::default() };
        let r = render(&conn, tmp.path(), 1, "aki", &scope, now()).unwrap();
        assert!(r.view.get("days").is_none() && r.view.get("goals").is_none() && r.view.get("done_recent").is_none());
        assert_eq!(r.view["tasks"][0]["description"], "private detail");
        assert!(!r.text.contains("# Today"));
    }

    #[test]
    fn render_trims_tasks_first_to_stay_under_the_cap() {
        let conn = conn();
        let (tmp, _) = seed_owner(&conn);
        for i in 0..200 {
            crate::tasks::create(&conn, 1, crate::tasks::NewTask { title: format!("filler task number {i} with a title long enough that eighty of these lines overflow the opener"), ..Default::default() }, "manual", crate::tasks::Actor::User).unwrap();
        }
        for i in 0..30 {
            let id = crate::tasks::create(&conn, 1, crate::tasks::NewTask { title: format!("finished {i}"), ..Default::default() }, "manual", crate::tasks::Actor::User).unwrap().id;
            crate::tasks::update(&conn, 1, id, &crate::tasks::TaskPatch { state: Some("done".into()), ..Default::default() }).unwrap();
        }
        conn.execute("UPDATE tasks SET completed_at = '2026-09-24T11:00:00Z' WHERE state = 'done'", []).unwrap();
        let r = render(&conn, tmp.path(), 1, "aki", &ShareScope::default(), now()).unwrap();
        assert!(r.text.len() <= OPENER_MAX_BYTES, "{}", r.text.len());
        assert!(r.text.contains("more; ask"), "{}", r.text);
        let task_lines = r.text.lines().filter(|l| l.contains("filler task")).count();
        assert!(task_lines < 80, "the task list was trimmed: {task_lines} lines");
        let done_lines = r.text.lines().filter(|l| l.starts_with("- finished") || l.starts_with("- reading")).count();
        assert!(done_lines > 10, "the done list outlasts the task list: {done_lines} rows\n{}", r.text);
        assert!(r.text.contains("# Today and ahead"), "{}", r.text);
        assert!(r.view["tasks"].as_array().unwrap().len() > 80, "the view is not trimmed with the text");
    }

    #[test]
    fn threads_come_newest_first() {
        let conn = conn();
        let s = create(&conn, 1, &new("Mom"), now(), &Limits::default()).unwrap();
        let older = new_thread(&conn, s.id, "v1", now()).unwrap();
        let newer = new_thread(&conn, s.id, "v2", now()).unwrap();
        append(&conn, newer, "user", "first", now()).unwrap();
        append(&conn, older, "user", "later", now() + jiff::Span::new().minutes(5)).unwrap();
        let ids: Vec<i64> = threads(&conn, s.id).unwrap().iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![older, newer], "the thread spoken in last leads");
    }

    #[test]
    fn an_overdue_task_reads_overdue_and_a_goal_title_needs_goals() {
        let conn = conn();
        let (tmp, lab) = seed_owner(&conn);
        crate::tasks::create(
            &conn,
            1,
            crate::tasks::NewTask {
                title: "late essay".into(),
                category: Some("school".into()),
                due_at: Some(Some("2026-09-23T00:00:00Z".into())),
                ..Default::default()
            },
            "manual",
            crate::tasks::Actor::User,
        )
        .unwrap();
        let goal = crate::goals::create(&conn, 1, crate::goals::NewGoal { title: "GOAL-TITLE-SECRET".into(), ..Default::default() }).unwrap();
        conn.execute("UPDATE tasks SET goal_id = ?1 WHERE id = ?2", [goal.id, lab]).unwrap();
        let r = render(&conn, tmp.path(), 1, "aki", &ShareScope::default(), now()).unwrap();
        assert!(r.text.contains("- late essay [overdue]"), "{}", r.text);
        assert!(r.text.contains("- problem set [due soon]"), "{}", r.text);
        assert!(r.text.contains(", goal: GOAL-TITLE-SECRET"), "{}", r.text);
        let hidden = ShareScope { goals: false, ..ShareScope::default() };
        let r = render(&conn, tmp.path(), 1, "aki", &hidden, now()).unwrap();
        assert!(!r.text.contains("GOAL-TITLE-SECRET"), "{}", r.text);
        assert!(!r.view.to_string().contains("GOAL-TITLE-SECRET"), "{}", r.view);
    }

    #[test]
    fn the_plan_leaves_triggers_out_and_labels_a_routine_block() {
        let conn = conn();
        let (tmp, _) = seed_owner(&conn);
        conn.execute("INSERT INTO plans (user_id, date, created_at) VALUES (1, '2026-09-24', 'x')", []).unwrap();
        let plan = conn.last_insert_rowid();
        conn.execute("INSERT INTO events (plan_id, kind, wall_time) VALUES (?1, ?2, '13:00')", (plan, crate::triggers::KIND)).unwrap();
        conn.execute("INSERT INTO events (plan_id, kind, wall_time) VALUES (?1, 'checkin_call', '14:00')", [plan]).unwrap();
        let r = render(&conn, tmp.path(), 1, "aki", &ShareScope::default(), now()).unwrap();
        let today = r.view["days"][0]["rows"].as_array().unwrap();
        assert!(today.iter().all(|row| row["title"] != crate::triggers::KIND && row["start"] != "13:00"), "{today:?}");
        assert!(today.iter().any(|row| row["title"] == "Checkin call"), "{today:?}");
        assert!(!r.text.contains("13:00"), "{}", r.text);
    }
}
