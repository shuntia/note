use crate::text::{self, Lang};
use crate::AppState;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PREFIX: &str = "join_";
pub const DEFAULT_DAYS: u32 = 7;
pub const MAX_DAYS: u32 = 90;
pub const MIN_PASSWORD_LEN: usize = 8;
/// Unknown-token lookups and refused joins one client address may make in a
/// limiter window.
pub const ADDRESS_ATTEMPTS: u32 = 30;

#[derive(Debug, Clone, Serialize)]
pub struct Invite {
    pub id: i64,
    pub admin: bool,
    pub username: Option<String>,
    pub created_by: Option<String>,
    pub created_at: String,
    pub expires_at: String,
}

/// The only value that ever carries the plaintext token.
#[derive(Debug, Serialize)]
pub struct Created {
    #[serde(flatten)]
    pub invite: Invite,
    pub token: String,
    pub url: String,
}

#[derive(Debug, Error)]
pub enum InviteError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

#[derive(Debug, Error)]
pub enum JoinError {
    #[error("this invite is no longer open")]
    Gone,
    #[error("username is taken")]
    Taken,
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("os rng");
    format!("{PREFIX}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

pub fn url_for(public_base_url: &str, token: &str) -> String {
    format!("{}/join/{token}", public_base_url.trim_end_matches('/'))
}

fn stamp(secs: i64) -> String {
    jiff::Timestamp::from_second(secs).map(|t| t.to_string()).unwrap_or_default()
}

const COLS: &str = "i.id, i.role, i.username, u.username, i.created_at, i.expires_at";

fn row_to_invite(r: &rusqlite::Row) -> rusqlite::Result<Invite> {
    Ok(Invite {
        id: r.get(0)?,
        admin: r.get::<_, String>(1)? == "admin",
        username: r.get(2)?,
        created_by: r.get(3)?,
        created_at: r.get(4)?,
        expires_at: stamp(r.get(5)?),
    })
}

/// `days` is clamped to `1..=MAX_DAYS`; a suggested username must be one an
/// account could take.
pub fn create(
    conn: &Connection,
    created_by: Option<i64>,
    admin: bool,
    username: Option<&str>,
    days: u32,
    public_base_url: &str,
    now: jiff::Timestamp,
) -> Result<Created, InviteError> {
    let username = username.map(str::trim).filter(|u| !u.is_empty());
    if let Some(u) = username {
        crate::auth::validate_username(u).map_err(|e| InviteError::Invalid(e.to_string()))?;
    }
    let expires = now.as_second() + i64::from(days.clamp(1, MAX_DAYS)) * 86_400;
    let token = generate_token();
    conn.execute(
        "INSERT INTO invites (token_hash, role, username, created_by, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        (
            crate::tokens::hash_secret(&token),
            if admin { "admin" } else { "member" },
            username,
            created_by,
            now.to_string(),
            expires,
        ),
    )?;
    let id = conn.last_insert_rowid();
    let invite = conn.query_row(
        &format!("SELECT {COLS} FROM invites i LEFT JOIN users u ON u.id = i.created_by WHERE i.id = ?1"),
        [id],
        row_to_invite,
    )?;
    let url = url_for(public_base_url, &token);
    Ok(Created { invite, token, url })
}

/// Unused, unrevoked and unexpired, newest first.
pub fn outstanding(conn: &Connection, now: jiff::Timestamp) -> rusqlite::Result<Vec<Invite>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM invites i LEFT JOIN users u ON u.id = i.created_by
         WHERE i.used_at IS NULL AND i.revoked_at IS NULL AND i.expires_at > ?1
         ORDER BY i.id DESC"
    ))?;
    let rows = stmt.query_map([now.as_second()], row_to_invite)?;
    rows.collect()
}

/// `false` when there is no such invite still open.
pub fn revoke(conn: &Connection, id: i64, now: jiff::Timestamp) -> rusqlite::Result<bool> {
    let n = conn.execute(
        "UPDATE invites SET revoked_at = ?1 WHERE id = ?2 AND used_at IS NULL AND revoked_at IS NULL",
        (now.to_string(), id),
    )?;
    Ok(n > 0)
}

/// The open invite `token` names. The lookup is by digest, so the comparison
/// never runs over the secret itself.
pub fn open(conn: &Connection, token: &str, now: jiff::Timestamp) -> rusqlite::Result<Option<Invite>> {
    conn.query_row(
        &format!(
            "SELECT {COLS} FROM invites i LEFT JOIN users u ON u.id = i.created_by
             WHERE i.token_hash = ?1 AND i.used_at IS NULL AND i.revoked_at IS NULL AND i.expires_at > ?2"
        ),
        (crate::tokens::hash_secret(token), now.as_second()),
        row_to_invite,
    )
    .optional()
}

/// Spends the invite and creates its account in one transaction: the claim is
/// a conditional update, so of two joins racing on one token exactly one
/// changes the row and the other finds it gone. A taken username rolls the
/// claim back, leaving the invite open.
pub fn consume(
    conn: &Connection,
    token: &str,
    username: &str,
    pass_hash: &str,
    now: jiff::Timestamp,
) -> Result<i64, JoinError> {
    crate::auth::validate_username(username).map_err(|e| JoinError::Invalid(e.to_string()))?;
    let tx = conn.unchecked_transaction()?;
    let claimed: Option<(i64, String)> = tx
        .query_row(
            "UPDATE invites SET used_at = ?1
             WHERE token_hash = ?2 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?3
             RETURNING id, role",
            (now.to_string(), crate::tokens::hash_secret(token), now.as_second()),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((invite_id, role)) = claimed else {
        return Err(JoinError::Gone);
    };
    let user_id = match crate::auth::insert_user(&tx, username, pass_hash, role == "admin") {
        Ok(id) => id,
        Err(e) => {
            return Err(match e.downcast::<rusqlite::Error>() {
                Ok(rusqlite::Error::SqliteFailure(f, _)) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                    JoinError::Taken
                }
                Ok(other) => JoinError::Db(other),
                Err(e) => JoinError::Invalid(e.to_string()),
            });
        }
    };
    tx.execute("UPDATE invites SET used_by = ?1 WHERE id = ?2", (user_id, invite_id))?;
    tx.execute("UPDATE users SET onboarding = 1 WHERE id = ?1", [user_id])?;
    tx.commit()?;
    Ok(user_id)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/join/{token}", get(info).post(join))
        .layer(axum::middleware::from_fn(envelope))
}

/// Uncacheable, no referrer (the token is in the path), never indexed.
async fn envelope(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert("x-robots-tag", HeaderValue::from_static("noindex"));
    res
}

fn lang_of(headers: &HeaderMap) -> Lang {
    Lang::resolve("", headers.get(header::ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok()))
}

fn refuse(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

fn gone(state: &AppState, headers: &HeaderMap, lang: Lang) -> Response {
    if state.join_limiter.try_attempt(&crate::net::client_key(headers), jiff::Timestamp::now()) {
        refuse(StatusCode::NOT_FOUND, &text::err_invite_gone(lang))
    } else {
        refuse(StatusCode::TOO_MANY_REQUESTS, &text::err_invite_rate(lang))
    }
}

async fn info(State(state): State<AppState>, headers: HeaderMap, Path(token): Path<String>) -> Response {
    let lang = lang_of(&headers);
    let found = {
        let conn = state.db();
        open(&conn, &token, jiff::Timestamp::now())
    };
    match found {
        Ok(Some(i)) => Json(serde_json::json!({ "username": i.username, "expires_at": i.expires_at })).into_response(),
        Ok(None) => gone(&state, &headers, lang),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinReq {
    username: String,
    password: String,
}

/// The token is checked before any hashing, so a guessed token costs a digest
/// and a lookup, never an argon2 run; only an open invite's holder can learn
/// whether a username is taken.
async fn join(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Json(req): Json<JoinReq>,
) -> Response {
    let lang = lang_of(&headers);
    if !crate::net::fetch_site_ok(&headers) {
        return refuse(StatusCode::FORBIDDEN, "cross-site request refused");
    }
    let key = crate::net::client_key(&headers);
    let open_now = {
        let conn = state.db();
        open(&conn, &token, jiff::Timestamp::now())
    };
    match open_now {
        Ok(Some(_)) => {}
        Ok(None) => return gone(&state, &headers, lang),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
    let username = req.username.trim().to_string();
    if crate::auth::validate_username(&username).is_err() {
        return refuse(StatusCode::UNPROCESSABLE_ENTITY, &text::err_join_username(lang));
    }
    if req.password.chars().count() < MIN_PASSWORD_LEN {
        return refuse(StatusCode::UNPROCESSABLE_ENTITY, &text::err_join_password(lang, MIN_PASSWORD_LEN));
    }
    if !state.join_limiter.try_attempt(&key, jiff::Timestamp::now()) {
        return refuse(StatusCode::TOO_MANY_REQUESTS, &text::err_invite_rate(lang));
    }
    let Ok(slot) = state.login_slots.clone().try_acquire_owned() else {
        return (StatusCode::SERVICE_UNAVAILABLE, [(header::RETRY_AFTER, "2")]).into_response();
    };
    let browser_lang = headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok())
        .and_then(Lang::from_accept_language);
    let st = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        let hash = crate::auth::hash_password(&req.password).map_err(|e| JoinError::Invalid(e.to_string()))?;
        let conn = st.db();
        let id = consume(&conn, &token, &username, &hash, jiff::Timestamp::now())?;
        if let Some(seen) = browser_lang {
            let _ = text::remember_seen(&st.config_dir, &username, seen);
        }
        crate::builtin_memory::seed_new_user(&conn, &st.data_dir, &st.config_dir, &username);
        let session = crate::auth::start_session(&conn, id).map_err(|e| JoinError::Invalid(e.to_string()))?;
        let _ = crate::log::record(&conn, Some(id), "invite_join", &username);
        Ok::<_, JoinError>(session)
    })
    .await;
    if matches!(result, Ok(Ok(_))) {
        state.spawn_vector_backfill();
    }
    match result {
        Ok(Ok(session)) => (
            StatusCode::CREATED,
            [(header::SET_COOKIE, crate::auth::session_cookie(&session, state.secure_cookies))],
        )
            .into_response(),
        Ok(Err(JoinError::Gone)) => refuse(StatusCode::NOT_FOUND, &text::err_invite_gone(lang)),
        Ok(Err(JoinError::Taken)) => refuse(StatusCode::CONFLICT, &text::err_join_taken(lang)),
        Ok(Err(JoinError::Invalid(_))) => refuse(StatusCode::UNPROCESSABLE_ENTITY, &text::err_join_username(lang)),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> jiff::Timestamp {
        "2026-10-06T12:00:00Z".parse().unwrap()
    }

    fn hash() -> String {
        crate::auth::hash_password("password1").unwrap()
    }

    #[test]
    fn create_stores_only_the_digest_and_opens_by_the_plaintext() {
        let conn = crate::db::open_memory().unwrap();
        let made = create(&conn, None, false, Some("mika"), 7, "https://n.example/", now()).unwrap();
        assert!(made.token.starts_with(PREFIX));
        assert_eq!(made.url, format!("https://n.example/join/{}", made.token));
        let stored: String = conn.query_row("SELECT token_hash FROM invites", [], |r| r.get(0)).unwrap();
        assert_eq!(stored, crate::tokens::hash_secret(&made.token));
        let found = open(&conn, &made.token, now()).unwrap().unwrap();
        assert_eq!(found.username.as_deref(), Some("mika"));
        assert!(open(&conn, "join_nope", now()).unwrap().is_none());
    }

    #[test]
    fn a_suggested_username_must_be_a_valid_one_and_days_are_clamped() {
        let conn = crate::db::open_memory().unwrap();
        assert!(matches!(
            create(&conn, None, false, Some("../x"), 7, "", now()),
            Err(InviteError::Invalid(_))
        ));
        let made = create(&conn, None, false, None, 10_000, "", now()).unwrap();
        let max = now() + jiff::Span::new().hours(i64::from(MAX_DAYS) * 24);
        assert_eq!(made.invite.expires_at, max.to_string());
    }

    #[test]
    fn consume_creates_the_account_once_and_marks_it_onboarding() {
        let conn = crate::db::open_memory().unwrap();
        let made = create(&conn, None, true, None, 7, "", now()).unwrap();
        let id = consume(&conn, &made.token, "mika", &hash(), now()).unwrap();
        let (role, onboarding): (String, bool) = conn
            .query_row("SELECT role, onboarding FROM users WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((role.as_str(), onboarding), ("admin", true));
        assert!(matches!(consume(&conn, &made.token, "other", &hash(), now()), Err(JoinError::Gone)));
        assert!(outstanding(&conn, now()).unwrap().is_empty());
    }

    #[test]
    fn a_taken_username_leaves_the_invite_open() {
        let conn = crate::db::open_memory().unwrap();
        crate::auth::create_user(&conn, "aki", "pw", false).unwrap();
        let made = create(&conn, None, false, None, 7, "", now()).unwrap();
        assert!(matches!(consume(&conn, &made.token, "aki", &hash(), now()), Err(JoinError::Taken)));
        assert!(open(&conn, &made.token, now()).unwrap().is_some());
        consume(&conn, &made.token, "mika", &hash(), now()).unwrap();
    }

    #[test]
    fn expired_and_revoked_invites_are_gone() {
        let conn = crate::db::open_memory().unwrap();
        let old = create(&conn, None, false, None, 1, "", now()).unwrap();
        let later = now() + jiff::Span::new().hours(25);
        assert!(open(&conn, &old.token, later).unwrap().is_none());
        assert!(matches!(consume(&conn, &old.token, "mika", &hash(), later), Err(JoinError::Gone)));

        let made = create(&conn, None, false, None, 7, "", now()).unwrap();
        assert!(revoke(&conn, made.invite.id, now()).unwrap());
        assert!(!revoke(&conn, made.invite.id, now()).unwrap());
        assert!(matches!(consume(&conn, &made.token, "mika", &hash(), now()), Err(JoinError::Gone)));
    }

    #[test]
    fn two_connections_racing_on_one_token_create_one_account() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.db");
        let token = {
            let conn = crate::db::open(&path).unwrap();
            create(&conn, None, false, None, 7, "", now()).unwrap().token
        };
        let pass = hash();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let (path, token, pass, barrier) = (path.clone(), token.clone(), pass.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let conn = crate::db::open(&path).unwrap();
                    conn.busy_timeout(std::time::Duration::from_secs(10)).unwrap();
                    barrier.wait();
                    consume(&conn, &token, &format!("user{i}"), &pass, now()).is_ok()
                })
            })
            .collect();
        let won = handles.into_iter().map(|h| h.join().unwrap()).filter(|ok| *ok).count();
        assert_eq!(won, 1);
        let conn = crate::db::open(&path).unwrap();
        let users: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)).unwrap();
        assert_eq!(users, 1);
    }
}
