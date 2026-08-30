use crate::AppState;
use anyhow::Result;
use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::{request::Parts, StatusCode};
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const SESSION_LIFETIME_HOURS: i64 = 30 * 24;

const MAX_USERNAME_LEN: usize = 64;

pub const MAX_FAILURES: u32 = 10;
pub const WINDOW_MINS: i64 = 15;

/// Per-username fixed-window failure counter; keys are usernames (attackers
/// rotating usernames still pay the argon2 cost per attempt).
#[derive(Default)]
pub struct LoginLimiter {
    attempts: Mutex<HashMap<String, (u32, jiff::Timestamp)>>,
}

fn window_elapsed(now: jiff::Timestamp, start: jiff::Timestamp) -> bool {
    (now.as_second() - start.as_second()) > WINDOW_MINS * 60
}

impl LoginLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow(&self, username: &str, now: jiff::Timestamp) -> bool {
        let mut a = self.attempts.lock().unwrap();
        match a.get(username) {
            Some((count, start)) => {
                if window_elapsed(now, *start) {
                    a.remove(username);
                    true
                } else {
                    *count < MAX_FAILURES
                }
            }
            None => true,
        }
    }

    pub fn record_failure(&self, username: &str, now: jiff::Timestamp) {
        let mut a = self.attempts.lock().unwrap();
        let entry = a.entry(username.to_string()).or_insert((0, now));
        if window_elapsed(now, entry.1) {
            *entry = (0, now);
        }
        entry.0 += 1;
    }

    pub fn clear(&self, username: &str) {
        self.attempts.lock().unwrap().remove(username);
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

pub fn create_user(conn: &Connection, username: &str, password: &str, admin: bool) -> Result<i64> {
    validate_username(username)?;
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!(e))?
        .to_string();
    conn.execute(
        "INSERT INTO users (username, pass_hash, role) VALUES (?1, ?2, ?3)",
        (username, hash, if admin { "admin" } else { "member" }),
    )?;
    Ok(conn.last_insert_rowid())
}

fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(b"timing-equalizer", &salt)
            .expect("static hash")
            .to_string()
    })
}

fn verify(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

/// Returns a fresh session token, or `None` when the credentials do not match.
/// Row lookup and session insert each take a short lock; the argon2 work runs
/// with no lock held. Unknown usernames verify against a dummy hash so the
/// response time does not reveal which usernames exist.
pub fn login(db: &Mutex<Connection>, username: &str, password: &str) -> Result<Option<String>> {
    let row: Option<(i64, String)> = {
        let conn = db.lock().unwrap();
        conn.query_row(
            "SELECT id, pass_hash FROM users WHERE username = ?1",
            [username],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    };
    let ok = match &row {
        Some((_, hash)) => verify(password, hash),
        None => {
            let _ = verify(password, dummy_hash());
            false
        }
    };
    if !ok {
        return Ok(None);
    }
    let (id, _) = row.expect("checked above");
    let token = uuid::Uuid::new_v4().to_string();
    let expires = jiff::Timestamp::now() + jiff::Span::new().hours(SESSION_LIFETIME_HOURS);
    let conn = db.lock().unwrap();
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
        let conn = state.db.lock().unwrap();
        let row: Option<(i64, String, String, String)> = conn
            .query_row(
                "SELECT u.id, u.username, u.role, s.expires_at
                 FROM sessions s JOIN users u ON u.id = s.user_id
                 WHERE s.token = ?1",
                [&token],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let Some((id, username, role, expires_at)) = row else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        let expires: jiff::Timestamp = expires_at.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
        if expires < jiff::Timestamp::now() {
            return Err(StatusCode::UNAUTHORIZED);
        }
        Ok(CurrentUser {
            id,
            username,
            admin: role == "admin",
        })
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
    fn limiter_blocks_after_max_failures_and_resets_after_window() {
        let lim = LoginLimiter::new();
        let t0: jiff::Timestamp = "2026-08-31T00:00:00Z".parse().unwrap();
        for _ in 0..MAX_FAILURES {
            assert!(lim.allow("aki", t0));
            lim.record_failure("aki", t0);
        }
        assert!(!lim.allow("aki", t0));
        assert!(lim.allow("other", t0));
        let later = t0 + jiff::Span::new().minutes(WINDOW_MINS + 1);
        assert!(lim.allow("aki", later));
        lim.record_failure("aki", later);
        lim.clear("aki");
        assert!(lim.allow("aki", later));
    }

    #[test]
    fn cookies_carry_hardened_attributes() {
        let c = session_cookie("tok", false);
        assert!(c.contains("HttpOnly") && c.contains("SameSite=Lax") && c.contains("Max-Age="));
        assert!(!c.contains("Secure"));
        let c = session_cookie("tok", true);
        assert!(c.contains("; Secure"));
        let c = clear_cookie(false);
        assert!(c.contains("session=;") && c.contains("Max-Age=0"));
    }
}
