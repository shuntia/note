use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PREFIX: &str = "share_";
pub const MAX_NAME_LEN: usize = 64;
pub const MAX_BRIEF_BYTES: usize = 4096;
pub const MAX_CATEGORIES: usize = 20;
pub const MAX_HORIZON_DAYS: u8 = 14;
/// Unknown-token lookups or message posts one client address may make in a
/// limiter window before it is refused.
pub const ADDRESS_ATTEMPTS: u32 = 60;
const TOUCH_INTERVAL_SECS: i64 = 60;

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
            max_days: state.share_max_days,
            messages_per_day: state.share_messages_per_day,
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
    #[error("at most this many share links per user")]
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
    let ceiling = now + jiff::Span::new().hours(i64::from(max_days) * 24);
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
    if raw.len() > MAX_BRIEF_BYTES {
        return Err(ShareError::Invalid(format!(
            "brief must be at most {MAX_BRIEF_BYTES} bytes"
        )));
    }
    Ok(raw.trim().to_string())
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
        scope: serde_json::from_str(&scope).unwrap_or_default(),
        expires_at: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
        last_used_at: r.get(9)?,
    })
}

pub fn create(
    conn: &Connection,
    user_id: i64,
    new: NewShare,
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
    patch: SharePatch,
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

pub fn thread_for(
    conn: &Connection,
    share_id: i64,
    visitor_key: &str,
    now: jiff::Timestamp,
) -> rusqlite::Result<i64> {
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM share_threads WHERE share_id = ?1 AND visitor_key = ?2",
            (share_id, visitor_key),
            |r| r.get(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO share_threads (share_id, visitor_key, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
        (share_id, visitor_key, now.to_string()),
    )?;
    Ok(conn.last_insert_rowid())
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

pub fn append(
    conn: &Connection,
    thread_id: i64,
    role: &str,
    content: &str,
    now: jiff::Timestamp,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO share_messages (thread_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
        (thread_id, role, content, now.to_string()),
    )?;
    conn.execute(
        "UPDATE share_threads SET updated_at = ?1 WHERE id = ?2",
        (now.to_string(), thread_id),
    )?;
    Ok(())
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
            categories: vec![" school ".into(), "".into()],
            ..ShareScope::default()
        };
        assert_eq!(
            ok.checked(&Limits::default()).unwrap().categories,
            vec!["school".to_string()]
        );
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
        let a = create(&conn, 1, new("Mom"), now(), &limits).unwrap();
        assert_eq!(a.name, "Mom");
        assert!(a.token.starts_with(PREFIX));
        let _b = create(&conn, 1, new("Tutor"), now(), &limits).unwrap();
        assert!(matches!(
            create(&conn, 1, new("Third"), now(), &limits),
            Err(ShareError::TooMany)
        ));
        assert!(matches!(
            create(&conn, 1, new(""), now(), &limits),
            Err(ShareError::Invalid(_))
        ));
        assert_eq!(list(&conn, 1).unwrap().len(), 2);
        let patched = update(
            &conn,
            1,
            a.id,
            SharePatch {
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
        let s = create(&conn, 1, new("Mom"), now(), &Limits::default()).unwrap();
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
    fn threads_are_per_visitor_and_history_reads_back_in_order() {
        let conn = conn();
        let s = create(&conn, 1, new("Mom"), now(), &Limits::default()).unwrap();
        let t1 = thread_for(&conn, s.id, "v1", now()).unwrap();
        let t2 = thread_for(&conn, s.id, "v2", now()).unwrap();
        assert_ne!(t1, t2);
        assert_eq!(thread_for(&conn, s.id, "v1", now()).unwrap(), t1);
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
}
