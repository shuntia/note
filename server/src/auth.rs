use crate::AppState;
use anyhow::Result;
use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::{request::Parts, StatusCode};
use rusqlite::{Connection, OptionalExtension};

const SESSION_LIFETIME_HOURS: i64 = 30 * 24;

pub fn create_user(conn: &Connection, username: &str, password: &str, admin: bool) -> Result<i64> {
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

/// Returns a fresh session token, or `None` when the credentials do not match.
pub fn login(conn: &Connection, username: &str, password: &str) -> Result<Option<String>> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, pass_hash FROM users WHERE username = ?1",
            [username],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((id, hash)) = row else {
        return Ok(None);
    };
    let parsed = PasswordHash::new(&hash).map_err(|e| anyhow::anyhow!(e))?;
    if Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_err()
    {
        return Ok(None);
    }
    let token = uuid::Uuid::new_v4().to_string();
    let expires = jiff::Timestamp::now() + jiff::Span::new().hours(SESSION_LIFETIME_HOURS);
    conn.execute(
        "INSERT INTO sessions (token, user_id, expires_at) VALUES (?1, ?2, ?3)",
        (&token, id, expires.to_string()),
    )?;
    Ok(Some(token))
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
