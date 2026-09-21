use crate::AppState;
use anyhow::Result;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::{request::Parts, StatusCode};
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const SESSION_LIFETIME_HOURS: i64 = 30 * 24;

const MAX_USERNAME_LEN: usize = 64;

pub const MAX_ATTEMPTS: u32 = 10;
pub const WINDOW_MINS: i64 = 15;

/// How many argon2 verifications may be in flight server-wide. The login route
/// is reachable unauthenticated and every call costs ~19 MiB and a blocking
/// thread, so admission is bounded before the hash rather than by username.
pub const MAX_CONCURRENT_LOGINS: usize = 4;

/// Per-username fixed-window counter of *failed* attempts, consulted only after
/// a verification has already failed, so a correct password is never refused.
#[derive(Default)]
pub struct LoginLimiter {
    attempts: Mutex<HashMap<String, (u32, jiff::Timestamp)>>,
}

fn window_elapsed(now: jiff::Timestamp, start: jiff::Timestamp) -> bool {
    (now.as_second() - start.as_second()) > WINDOW_MINS * 60
}

/// Request usernames are unbounded, so keys are capped at the length a real
/// username can reach; longer ones collapse onto their prefix rather than
/// being rejected, which would leak that the username is invalid via timing.
fn limiter_key(username: &str) -> &str {
    if username.len() <= MAX_USERNAME_LEN {
        return username;
    }
    let mut end = MAX_USERNAME_LEN;
    while !username.is_char_boundary(end) {
        end -= 1;
    }
    &username[..end]
}

impl LoginLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Admits an attempt and counts it in one lock acquisition; a separate
    /// check and record would let concurrent requests all pass the check.
    pub fn try_attempt(&self, username: &str, now: jiff::Timestamp) -> bool {
        let mut a = self.attempts.lock().unwrap();
        let entry = a.entry(limiter_key(username).to_string()).or_insert((0, now));
        if window_elapsed(now, entry.1) {
            *entry = (0, now);
        }
        if entry.0 >= MAX_ATTEMPTS {
            return false;
        }
        entry.0 += 1;
        true
    }

    pub fn clear(&self, username: &str) {
        self.attempts.lock().unwrap().remove(limiter_key(username));
    }

    /// Drops entries whose window has elapsed; without it the map grows for
    /// every distinct username ever tried.
    pub fn sweep(&self, now: jiff::Timestamp) {
        self.attempts
            .lock()
            .unwrap()
            .retain(|_, (_, start)| !window_elapsed(now, *start));
    }

    #[cfg(test)]
    pub fn tracked(&self) -> usize {
        self.attempts.lock().unwrap().len()
    }
}

/// Usernames become path segments under the config tree, so anything outside
/// `[A-Za-z0-9_-]` (`..` and separators above all) is rejected at creation.
fn validate_username(username: &str) -> Result<()> {
    if username.is_empty() {
        anyhow::bail!("username must not be empty");
    }
    if username.len() > MAX_USERNAME_LEN {
        anyhow::bail!("username must be at most {MAX_USERNAME_LEN} characters");
    }
    if let Some(c) = username
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-'))
    {
        anyhow::bail!("username contains disallowed character {c:?}");
    }
    Ok(())
}

fn hash_password(password: &str) -> Result<String> {
    Ok(Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|e| anyhow::anyhow!(e))?
        .to_string())
}

pub fn create_user(conn: &Connection, username: &str, password: &str, admin: bool) -> Result<i64> {
    validate_username(username)?;
    let hash = hash_password(password)?;
    conn.execute(
        "INSERT INTO users (username, pass_hash, role) VALUES (?1, ?2, ?3)",
        (username, hash, if admin { "admin" } else { "member" }),
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn category(conn: &Connection, username: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT category FROM users WHERE username = ?1", [username], |r| r.get(0))
        .optional()?)
}

/// Moves an account between categories; `false` means there is no such user.
pub fn set_category(conn: &Connection, username: &str, category: &str) -> Result<bool> {
    anyhow::ensure!(
        crate::config::CATEGORIES.contains(&category),
        "unknown category {category:?}"
    );
    let n = conn.execute(
        "UPDATE users SET category = ?1 WHERE username = ?2",
        (category, username),
    )?;
    Ok(n > 0)
}

fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        Argon2::default()
            .hash_password(b"timing-equalizer")
            .expect("static hash")
            .to_string()
    })
}

pub fn stored_hash(conn: &Connection, user_id: i64) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT pass_hash FROM users WHERE id = ?1", [user_id], |r| r.get(0))
        .optional()?)
}

/// Verifies against the stored hash, or against a dummy hash when there is
/// none, so every path pays the same argon2 cost. Runs with no lock held.
pub fn verify_against(password: &str, hash: Option<&str>) -> bool {
    match hash {
        Some(h) => verify(password, h),
        None => {
            let _ = verify(password, dummy_hash());
            false
        }
    }
}

pub fn set_password(conn: &Connection, user_id: i64, password: &str) -> Result<()> {
    let hash = hash_password(password)?;
    conn.execute("UPDATE users SET pass_hash = ?1 WHERE id = ?2", (hash, user_id))?;
    Ok(())
}

fn verify(password: &str, hash: &str) -> bool {
    Argon2::default().verify_password(password.as_bytes(), hash).is_ok()
}

/// Returns a fresh session token, or `None` when the credentials do not match
/// or the account is disabled. Row lookup and session insert each take a short
/// lock; the argon2 work runs with no lock held. Unknown usernames verify
/// against a dummy hash and disabled accounts still verify, so the response
/// time reveals neither which usernames exist nor which are disabled.
pub fn login(db: &Mutex<Connection>, username: &str, password: &str) -> Result<Option<String>> {
    let row: Option<(i64, String, bool)> = {
        let conn = crate::db_guard(db);
        conn.query_row(
            "SELECT id, pass_hash, disabled FROM users WHERE username = ?1",
            [username],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
    };
    let ok = match &row {
        Some((_, hash, disabled)) => verify(password, hash) && !disabled,
        None => {
            let _ = verify(password, dummy_hash());
            false
        }
    };
    if !ok {
        return Ok(None);
    }
    let (id, _, _) = row.expect("checked above");
    let token = uuid::Uuid::new_v4().to_string();
    let expires = jiff::Timestamp::now() + jiff::Span::new().hours(SESSION_LIFETIME_HOURS);
    let conn = crate::db_guard(db);
    conn.execute(
        "INSERT INTO sessions (token, user_id, expires_at) VALUES (?1, ?2, ?3)",
        (&token, id, expires.to_string()),
    )?;
    Ok(Some(token))
}

pub fn session_cookie(token: &str, secure: bool) -> String {
    let max_age = SESSION_LIFETIME_HOURS * 3600;
    let mut c = format!("session={token}; HttpOnly; Path=/; SameSite=Lax; Max-Age={max_age}");
    if secure {
        c.push_str("; Secure");
    }
    c
}

pub fn clear_cookie(secure: bool) -> String {
    let mut c = "session=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0".to_string();
    if secure {
        c.push_str("; Secure");
    }
    c
}

#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub id: i64,
    pub username: String,
    pub admin: bool,
    pub category: String,
    pub session_token: String,
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, StatusCode> {
        let jar = axum_extra::extract::CookieJar::from_headers(&parts.headers);
        let token = jar
            .get("session")
            .ok_or(StatusCode::UNAUTHORIZED)?
            .value()
            .to_string();
        let conn = state.db();
        let row: Option<(i64, String, String, String, bool, String)> = conn
            .query_row(
                "SELECT u.id, u.username, u.role, s.expires_at, u.disabled, u.category
                 FROM sessions s JOIN users u ON u.id = s.user_id
                 WHERE s.token = ?1",
                [&token],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .optional()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let Some((id, username, role, expires_at, disabled, category)) = row else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        let expires: jiff::Timestamp = expires_at.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
        if disabled || expires < jiff::Timestamp::now() {
            return Err(StatusCode::UNAUTHORIZED);
        }
        Ok(CurrentUser {
            id,
            username,
            admin: role == "admin",
            category,
            session_token: token,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Credential {
    Session,
    Token(i64),
}

/// The caller on a task route: a session cookie or a per-user API token.
/// This is the only extractor that reads `Authorization`, so a token cannot
/// reach a route that still takes `CurrentUser`. A bearer header that does
/// not resolve is a 401 even when a valid cookie rides along.
#[derive(Debug, Clone)]
pub struct TaskPrincipal {
    pub id: i64,
    pub username: String,
    pub via: Credential,
}

impl FromRequestParts<AppState> for TaskPrincipal {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, StatusCode> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .map(|v| v.to_str().map_err(|_| StatusCode::UNAUTHORIZED))
            .transpose()?;
        if let Some(value) = header {
            let secret = value
                .strip_prefix("Bearer ")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or(StatusCode::UNAUTHORIZED)?;
            let conn = state.db();
            let resolved = crate::tokens::resolve(&conn, secret, jiff::Timestamp::now())
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            let Some(r) = resolved else {
                return Err(StatusCode::UNAUTHORIZED);
            };
            return Ok(TaskPrincipal {
                id: r.user_id,
                username: r.username,
                via: Credential::Token(r.token_id),
            });
        }
        let user = CurrentUser::from_request_parts(parts, state).await?;
        Ok(TaskPrincipal { id: user.id, username: user.username, via: Credential::Session })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_user_rejects_unsafe_usernames() {
        let conn = crate::db::open_memory().unwrap();
        for bad in ["../evil", "", &"a".repeat(MAX_USERNAME_LEN + 1), "aki/../root", "a b"] {
            assert!(
                create_user(&conn, bad, "pw", false).is_err(),
                "expected rejection of {bad:?}"
            );
        }
        assert!(create_user(&conn, "aki_2", "pw", false).is_ok());
        assert!(create_user(&conn, &"a".repeat(MAX_USERNAME_LEN), "pw", false).is_ok());
    }

    #[test]
    fn users_are_members_until_put_in_another_category() {
        let conn = crate::db::open_memory().unwrap();
        create_user(&conn, "aki", "pw", false).unwrap();
        assert_eq!(category(&conn, "aki").unwrap().as_deref(), Some("member"));

        assert!(set_category(&conn, "aki", "test").unwrap());
        assert_eq!(category(&conn, "aki").unwrap().as_deref(), Some("test"));

        assert!(!set_category(&conn, "nobody", "test").unwrap());
        assert!(set_category(&conn, "aki", "vip").is_err());
        assert_eq!(category(&conn, "aki").unwrap().as_deref(), Some("test"));
    }

    #[test]
    fn unknown_user_and_wrong_password_both_return_none() {
        let db = std::sync::Mutex::new(crate::db::open_memory().unwrap());
        {
            let conn = db.lock().unwrap();
            create_user(&conn, "aki", "right", false).unwrap();
        }
        assert!(login(&db, "aki", "wrong").unwrap().is_none());
        assert!(login(&db, "nobody", "whatever").unwrap().is_none());
        assert!(login(&db, "aki", "right").unwrap().is_some());
    }

    #[test]
    fn login_with_a_corrupt_stored_hash_is_a_mismatch_not_an_error() {
        let db = std::sync::Mutex::new(crate::db::open_memory().unwrap());
        {
            let conn = db.lock().unwrap();
            create_user(&conn, "aki", "right", false).unwrap();
            conn.execute("UPDATE users SET pass_hash = 'not-a-phc-string'", [])
                .unwrap();
        }
        assert!(login(&db, "aki", "right").unwrap().is_none());
    }

    #[test]
    fn disabled_users_cannot_log_in() {
        let db = std::sync::Mutex::new(crate::db::open_memory().unwrap());
        {
            let conn = db.lock().unwrap();
            create_user(&conn, "aki", "right", false).unwrap();
            conn.execute("UPDATE users SET disabled = 1", []).unwrap();
        }
        assert!(login(&db, "aki", "right").unwrap().is_none());
    }

    #[test]
    fn verify_against_stored_hash_and_set_password() {
        let conn = crate::db::open_memory().unwrap();
        let id = create_user(&conn, "aki", "one", false).unwrap();
        let hash = stored_hash(&conn, id).unwrap();
        assert!(verify_against("one", hash.as_deref()));
        assert!(!verify_against("two", hash.as_deref()));
        assert!(stored_hash(&conn, id + 99).unwrap().is_none());
        assert!(!verify_against("one", None));
        set_password(&conn, id, "two").unwrap();
        let hash = stored_hash(&conn, id).unwrap();
        assert!(verify_against("two", hash.as_deref()));
        assert!(!verify_against("one", hash.as_deref()));
    }

    fn t0() -> jiff::Timestamp {
        "2026-08-31T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn limiter_blocks_after_max_attempts_and_resets_after_window() {
        let lim = LoginLimiter::new();
        for _ in 0..MAX_ATTEMPTS {
            assert!(lim.try_attempt("aki", t0()));
        }
        assert!(!lim.try_attempt("aki", t0()));
        assert!(lim.try_attempt("other", t0()));
        let later = t0() + jiff::Span::new().minutes(WINDOW_MINS + 1);
        assert!(lim.try_attempt("aki", later));
    }

    #[test]
    fn clear_releases_a_blocked_username() {
        let lim = LoginLimiter::new();
        for _ in 0..MAX_ATTEMPTS {
            assert!(lim.try_attempt("aki", t0()));
        }
        assert!(!lim.try_attempt("aki", t0()));
        lim.clear("aki");
        assert!(lim.try_attempt("aki", t0()));
    }

    #[test]
    fn oversized_usernames_share_one_truncated_key() {
        let lim = LoginLimiter::new();
        let long = "a".repeat(MAX_USERNAME_LEN * 100);
        let longer = format!("{long}{long}");
        for _ in 0..MAX_ATTEMPTS {
            assert!(lim.try_attempt(&long, t0()));
        }
        assert!(!lim.try_attempt(&longer, t0()));
        assert_eq!(lim.tracked(), 1);
        // multi-byte characters must not be split mid-boundary
        assert!(lim.try_attempt(&"あ".repeat(MAX_USERNAME_LEN), t0()));
    }

    #[test]
    fn sweep_drops_only_elapsed_windows() {
        let lim = LoginLimiter::new();
        let later = t0() + jiff::Span::new().minutes(WINDOW_MINS + 1);
        lim.try_attempt("old", t0());
        lim.try_attempt("fresh", later);
        lim.sweep(later);
        assert_eq!(lim.tracked(), 1);
        lim.sweep(later + jiff::Span::new().minutes(WINDOW_MINS + 1));
        assert_eq!(lim.tracked(), 0);
    }

    #[test]
    fn cookies_carry_hardened_attributes() {
        let c = session_cookie("tok", false);
        assert_eq!(
            c,
            "session=tok; HttpOnly; Path=/; SameSite=Lax; Max-Age=2592000"
        );
        assert_eq!(
            session_cookie("tok", true),
            "session=tok; HttpOnly; Path=/; SameSite=Lax; Max-Age=2592000; Secure"
        );
        assert_eq!(
            clear_cookie(false),
            "session=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0"
        );
        assert_eq!(
            clear_cookie(true),
            "session=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0; Secure"
        );
    }
}
