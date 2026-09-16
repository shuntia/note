use crate::auth::{self, CurrentUser};
use crate::AppState;
use anyhow::Result;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::{header, request::Parts, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path as FsPath;

pub const GRANT_LIFETIME_MINS: i64 = 15;
pub const INSPECT: bool = cfg!(feature = "dev-inspect");
const TOTP_FILE: &str = "admin_totp";
const COOKIE: &str = "admin";
const LOG_LIMIT_DEFAULT: i64 = 100;
const LOG_LIMIT_MAX: i64 = 500;

pub struct AdminSecrets {
    pub totp_seed: Option<Vec<u8>>,
    require_totp: bool,
}

impl Default for AdminSecrets {
    fn default() -> Self {
        Self { totp_seed: None, require_totp: true }
    }
}

impl AdminSecrets {
    pub fn with_seed(seed: Vec<u8>) -> Self {
        Self { totp_seed: Some(seed), ..Self::default() }
    }

    /// `false` is the operator's opt-out: elevation re-asks for the account
    /// password alone and the seed is never consulted.
    pub fn require_totp(mut self, require: bool) -> Self {
        self.require_totp = require;
        self
    }

    /// A missing file is the normal "not installed yet" state; anything else
    /// wrong with it comes back as a warning so startup can log it.
    pub fn load(secrets_dir: &FsPath) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let path = secrets_dir.join(TOTP_FILE);
        let totp_seed = match std::fs::read_to_string(&path) {
            Ok(raw) => match crate::totp::parse_seed(&raw) {
                Ok(seed) => Some(seed),
                Err(e) => {
                    warnings.push(format!("{}: {e}", path.display()));
                    None
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                warnings.push(format!("{}: {e}", path.display()));
                None
            }
        };
        (Self { totp_seed, ..Self::default() }, warnings)
    }

    pub fn totp_mode(&self) -> TotpMode {
        match (self.require_totp, &self.totp_seed, INSPECT) {
            (false, _, _) => TotpMode::PasswordOnly,
            (true, Some(_), _) => TotpMode::Required,
            (true, None, true) => TotpMode::PasswordOnly,
            (true, None, false) => TotpMode::Missing,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TotpMode {
    Required,
    PasswordOnly,
    Missing,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderInfo {
    pub kind: String,
    pub model: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ProvidersInfo {
    pub llm: Option<ProviderInfo>,
    pub embeddings: Option<ProviderInfo>,
}

impl From<&crate::config::ProvidersConfig> for ProvidersInfo {
    fn from(cfg: &crate::config::ProvidersConfig) -> Self {
        let info = |p: &crate::config::ProviderConfig| ProviderInfo {
            kind: p.kind.clone(),
            model: p.model.clone(),
        };
        Self {
            llm: cfg.llm.as_ref().map(info),
            embeddings: cfg.embeddings.as_ref().map(info),
        }
    }
}

fn grant_cookie(token: &str, secure: bool) -> String {
    let max_age = GRANT_LIFETIME_MINS * 60;
    let mut c =
        format!("{COOKIE}={token}; HttpOnly; Path=/api/admin; SameSite=Strict; Max-Age={max_age}");
    if secure {
        c.push_str("; Secure");
    }
    c
}

fn clear_grant_cookie(secure: bool) -> String {
    let mut c = format!("{COOKIE}=; HttpOnly; Path=/api/admin; SameSite=Strict; Max-Age=0");
    if secure {
        c.push_str("; Secure");
    }
    c
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

fn elevation_required() -> Response {
    error(StatusCode::UNAUTHORIZED, "elevation required")
}

/// Browsers stamp every request with its provenance; anything a foreign site
/// initiated is refused before the session is even looked at. Requests without
/// the header (non-browser clients) fall through to the cookie rules.
fn fetch_site_ok(headers: &HeaderMap) -> bool {
    match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        None => true,
        Some(v) => matches!(v, "same-origin" | "none"),
    }
}

/// An admin-role session with acceptable provenance; the pre-elevation
/// routes (gate, elevate, drop) run on this alone.
pub struct AdminUser(pub CurrentUser);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        if !fetch_site_ok(&parts.headers) {
            return Err(error(StatusCode::FORBIDDEN, "cross-site request refused"));
        }
        let user = CurrentUser::from_request_parts(parts, state)
            .await
            .map_err(|s| s.into_response())?;
        if !user.admin {
            return Err(error(StatusCode::FORBIDDEN, "admin only"));
        }
        Ok(AdminUser(user))
    }
}

/// An admin session holding a live grant issued to this same session.
pub struct Elevated {
    pub user: CurrentUser,
    pub grant: String,
}

fn grant_token(headers: &HeaderMap) -> Option<String> {
    axum_extra::extract::CookieJar::from_headers(headers)
        .get(COOKIE)
        .map(|c| c.value().to_string())
}

/// The expiry (unix seconds) of the grant in `token` if it belongs to this
/// user's current session and has not expired.
fn live_grant(conn: &Connection, user: &CurrentUser, token: &str, now: i64) -> rusqlite::Result<Option<i64>> {
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT session_token, expires_at FROM admin_grants WHERE token = ?1 AND user_id = ?2",
            (token, user.id),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row
        .filter(|(session, expires)| *session == user.session_token && *expires > now)
        .map(|(_, expires)| expires))
}

impl FromRequestParts<AppState> for Elevated {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let AdminUser(user) = AdminUser::from_request_parts(parts, state).await?;
        let token = grant_token(&parts.headers).ok_or_else(elevation_required)?;
        let now = jiff::Timestamp::now().as_second();
        let conn = state.db.lock().unwrap();
        match live_grant(&conn, &user, &token, now) {
            Ok(Some(_)) => Ok(Elevated { user, grant: token }),
            Ok(None) => Err(elevation_required()),
            Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        }
    }
}

pub fn routes() -> Router<AppState> {
    let r = Router::new()
        .route("/gate", get(gate))
        .route("/elevate", post(elevate))
        .route("/drop", post(drop_grant))
        .route("/status", get(status))
        .route("/users", get(users_list).post(users_create))
        .route("/users/{id}", patch(users_patch))
        .route("/users/{id}/revoke_sessions", post(users_revoke))
        .route("/log", get(log_list));
    #[cfg(feature = "dev-inspect")]
    let r = r.merge(inspect::routes());
    r.layer(axum::middleware::from_fn(no_store))
}

async fn no_store(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let mut res = next.run(req).await;
    res.headers_mut()
        .insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    res
}

fn record(state: &AppState, actor: i64, kind: &str, detail: &str) {
    let conn = state.db.lock().unwrap();
    let _ = crate::log::record(&conn, Some(actor), kind, detail);
}

async fn gate(AdminUser(user): AdminUser, State(state): State<AppState>, headers: HeaderMap) -> Response {
    let now = jiff::Timestamp::now().as_second();
    let expires = match grant_token(&headers) {
        Some(token) => {
            let conn = state.db.lock().unwrap();
            match live_grant(&conn, &user, &token, now) {
                Ok(e) => e,
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
        None => None,
    };
    Json(serde_json::json!({
        "elevated": expires.is_some(),
        "expires_at": expires.map(unix_to_rfc3339),
        "totp": state.admin_secrets.totp_mode(),
        "inspect": INSPECT,
    }))
    .into_response()
}

fn unix_to_rfc3339(secs: i64) -> String {
    jiff::Timestamp::from_second(secs)
        .map(|t| t.to_string())
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct ElevateReq {
    password: String,
    #[serde(default)]
    code: Option<String>,
}

pub enum ElevateOutcome {
    Granted { token: String, expires_at: i64 },
    Denied,
}

/// Password and (when a seed is installed) code are both checked on every
/// attempt, and an accepted code's time step is recorded so it cannot be
/// replayed. The argon2 work runs with no lock held.
pub fn elevate_blocking(
    state: &AppState,
    user: &CurrentUser,
    password: &str,
    code: Option<&str>,
    now: jiff::Timestamp,
) -> Result<ElevateOutcome> {
    let hash = {
        let conn = state.db.lock().unwrap();
        auth::stored_hash(&conn, user.id)?
    };
    let password_ok = auth::verify_against(password, hash.as_deref());
    let step = match state.admin_secrets.totp_mode() {
        TotpMode::Missing => anyhow::bail!("no admin TOTP seed installed"),
        TotpMode::PasswordOnly => None,
        TotpMode::Required => {
            let seed = state.admin_secrets.totp_seed.as_deref().expect("mode implies seed");
            match crate::totp::verify(seed, code.unwrap_or(""), now) {
                Some(step) => Some(step),
                None => return Ok(ElevateOutcome::Denied),
            }
        }
    };
    if !password_ok {
        return Ok(ElevateOutcome::Denied);
    }
    let conn = state.db.lock().unwrap();
    if let Some(step) = step {
        let last: Option<i64> = conn
            .query_row("SELECT last_step FROM totp_replay WHERE user_id = ?1", [user.id], |r| r.get(0))
            .optional()?;
        if last.is_some_and(|l| step <= l) {
            return Ok(ElevateOutcome::Denied);
        }
        conn.execute(
            "INSERT INTO totp_replay (user_id, last_step) VALUES (?1, ?2)
             ON CONFLICT(user_id) DO UPDATE SET last_step = excluded.last_step",
            (user.id, step),
        )?;
    }
    let token = uuid::Uuid::new_v4().to_string();
    let expires_at = now.as_second() + GRANT_LIFETIME_MINS * 60;
    conn.execute("DELETE FROM admin_grants WHERE expires_at <= ?1", [now.as_second()])?;
    conn.execute(
        "INSERT INTO admin_grants (token, session_token, user_id, expires_at) VALUES (?1, ?2, ?3, ?4)",
        (&token, &user.session_token, user.id, expires_at),
    )?;
    Ok(ElevateOutcome::Granted { token, expires_at })
}

async fn elevate(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    Json(req): Json<ElevateReq>,
) -> Response {
    if state.admin_secrets.totp_mode() == TotpMode::Missing {
        return error(StatusCode::SERVICE_UNAVAILABLE, "no admin secret installed on this server");
    }
    let now = jiff::Timestamp::now();
    if !state.admin_limiter.try_attempt(&user.username, now) {
        return error(StatusCode::TOO_MANY_REQUESTS, "too many attempts");
    }
    let st = state.clone();
    let u = user.clone();
    let result = tokio::task::spawn_blocking(move || {
        elevate_blocking(&st, &u, &req.password, req.code.as_deref(), now)
    })
    .await;
    match result {
        Ok(Ok(ElevateOutcome::Granted { token, expires_at })) => {
            state.admin_limiter.clear(&user.username);
            record(&state, user.id, "admin_elevate", &user.username);
            (
                StatusCode::OK,
                [(header::SET_COOKIE, grant_cookie(&token, state.secure_cookies))],
                Json(serde_json::json!({ "expires_at": unix_to_rfc3339(expires_at) })),
            )
                .into_response()
        }
        Ok(Ok(ElevateOutcome::Denied)) => {
            record(&state, user.id, "admin_elevate_denied", &user.username);
            error(StatusCode::UNAUTHORIZED, "wrong password or code")
        }
        Ok(Err(e)) => {
            record(&state, user.id, "admin_error", &e.to_string());
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
        Err(e) => {
            record(&state, user.id, "admin_error", &format!("elevate task failed: {e}"));
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn drop_grant(AdminUser(user): AdminUser, State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = grant_token(&headers) {
        let conn = state.db.lock().unwrap();
        let _ = conn.execute(
            "DELETE FROM admin_grants WHERE token = ?1 AND user_id = ?2",
            (token, user.id),
        );
    }
    (
        StatusCode::OK,
        [(header::SET_COOKIE, clear_grant_cookie(state.secure_cookies))],
    )
        .into_response()
}

fn count(conn: &Connection, sql: &str) -> rusqlite::Result<i64> {
    conn.query_row(sql, [], |r| r.get(0))
}

fn file_len(path: &FsPath) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

async fn status(_e: Elevated, State(state): State<AppState>) -> Response {
    let now = jiff::Timestamp::now();
    let (users, sessions, push) = {
        let conn = state.db.lock().unwrap();
        let r = (|| -> rusqlite::Result<_> {
            Ok((
                count(&conn, "SELECT COUNT(*) FROM users")?,
                live_sessions(&conn, now)?.values().sum::<i64>(),
                count(&conn, "SELECT COUNT(*) FROM push_subscriptions")?,
            ))
        })();
        match r {
            Ok(v) => v,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    };
    let db = state.data_dir.join("note.db");
    let db_bytes = file_len(&db) + file_len(&db.with_extension("db-wal"));
    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "build": if INSPECT { "dev-inspect" } else { "release" },
        "started_at": state.started_at.to_string(),
        "uptime_s": now.as_second() - state.started_at.as_second(),
        "db_bytes": db_bytes,
        "users": users,
        "sessions": sessions,
        "push_subscriptions": push,
        "providers": state.providers_info,
        "webpush": state.vapid_public_key.is_some(),
        "secrets": { "admin_totp": state.admin_secrets.totp_seed.is_some() },
    }))
    .into_response()
}

/// Unexpired session counts per user. Expiries are stored as RFC 3339 text
/// whose fractional seconds vary in width, so they are parsed rather than
/// compared as strings.
fn live_sessions(conn: &Connection, now: jiff::Timestamp) -> rusqlite::Result<std::collections::HashMap<i64, i64>> {
    let mut stmt = conn.prepare("SELECT user_id, expires_at FROM sessions")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let mut counts = std::collections::HashMap::new();
    for row in rows {
        let (uid, expires) = row?;
        if expires.parse::<jiff::Timestamp>().is_ok_and(|e| e > now) {
            *counts.entry(uid).or_insert(0) += 1;
        }
    }
    Ok(counts)
}

#[derive(Serialize)]
pub struct UserRow {
    pub id: i64,
    pub username: String,
    pub role: String,
    pub disabled: bool,
    pub sessions: i64,
}

pub fn list_users(conn: &Connection, now: jiff::Timestamp) -> rusqlite::Result<Vec<UserRow>> {
    let sessions = live_sessions(conn, now)?;
    let mut stmt = conn.prepare("SELECT id, username, role, disabled FROM users ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        let id: i64 = r.get(0)?;
        Ok(UserRow {
            id,
            username: r.get(1)?,
            role: r.get(2)?,
            disabled: r.get(3)?,
            sessions: sessions.get(&id).copied().unwrap_or(0),
        })
    })?;
    rows.collect()
}

async fn users_list(_e: Elevated, State(state): State<AppState>) -> Response {
    let conn = state.db.lock().unwrap();
    match list_users(&conn, jiff::Timestamp::now()) {
        Ok(v) => Json(v).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateUserReq {
    username: String,
    password: String,
    #[serde(default)]
    admin: bool,
}

fn is_unique_violation(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(f, _))
            if f.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

async fn users_create(
    Elevated { user: actor, .. }: Elevated,
    State(state): State<AppState>,
    Json(req): Json<CreateUserReq>,
) -> Response {
    if req.password.is_empty() {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "password must not be empty");
    }
    let st = state.clone();
    let (username, password, admin) = (req.username, req.password, req.admin);
    let result = tokio::task::spawn_blocking(move || {
        let conn = st.db.lock().unwrap();
        auth::create_user(&conn, &username, &password, admin).map(|id| (id, username, admin))
    })
    .await;
    match result {
        Ok(Ok((id, username, admin))) => {
            let role = if admin { "admin" } else { "member" };
            record(&state, actor.id, "admin_user_create", &format!("{username} ({role})"));
            (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response()
        }
        Ok(Err(e)) if is_unique_violation(&e) => error(StatusCode::CONFLICT, "username is taken"),
        Ok(Err(e)) if e.downcast_ref::<rusqlite::Error>().is_none() => {
            error(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string())
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchUserReq {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    disabled: Option<bool>,
    #[serde(default)]
    password: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PatchError {
    NotFound,
    Invalid(&'static str),
    Conflict(&'static str),
}

/// Applies role/disabled changes with the guards that keep the panel
/// reachable: an admin cannot touch their own role or disabled flag, and no
/// change may leave the server without an enabled admin. Returns the fields
/// that changed, for the audit row.
pub fn apply_user_patch(
    conn: &Connection,
    actor_id: i64,
    target_id: i64,
    role: Option<&str>,
    disabled: Option<bool>,
) -> Result<Vec<String>, PatchError> {
    let db = |_: rusqlite::Error| PatchError::Conflict("database error");
    let current: Option<(String, bool)> = conn
        .query_row(
            "SELECT role, disabled FROM users WHERE id = ?1",
            [target_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db)?;
    let Some((cur_role, cur_disabled)) = current else {
        return Err(PatchError::NotFound);
    };
    if let Some(r) = role {
        if r != "admin" && r != "member" {
            return Err(PatchError::Invalid("role must be admin or member"));
        }
    }
    if actor_id == target_id && (role.is_some() || disabled.is_some()) {
        return Err(PatchError::Conflict("you can't change your own role or disable yourself"));
    }
    let new_role = role.unwrap_or(&cur_role);
    let new_disabled = disabled.unwrap_or(cur_disabled);
    let others: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM users WHERE role = 'admin' AND disabled = 0 AND id != ?1",
            [target_id],
            |r| r.get(0),
        )
        .map_err(db)?;
    if others == 0 && (new_role != "admin" || new_disabled) {
        return Err(PatchError::Conflict("that would leave no enabled admin"));
    }
    let mut changed = Vec::new();
    if new_role != cur_role {
        conn.execute("UPDATE users SET role = ?1 WHERE id = ?2", (new_role, target_id))
            .map_err(db)?;
        changed.push(format!("role={new_role}"));
    }
    if new_disabled != cur_disabled {
        conn.execute("UPDATE users SET disabled = ?1 WHERE id = ?2", (new_disabled, target_id))
            .map_err(db)?;
        changed.push(format!("disabled={new_disabled}"));
        if new_disabled {
            conn.execute("DELETE FROM sessions WHERE user_id = ?1", [target_id])
                .map_err(db)?;
        }
    }
    Ok(changed)
}

async fn users_patch(
    Elevated { user: actor, .. }: Elevated,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<PatchUserReq>,
) -> Response {
    if req.password.as_deref().is_some_and(str::is_empty) {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "password must not be empty");
    }
    let mut changed = {
        let conn = state.db.lock().unwrap();
        match apply_user_patch(&conn, actor.id, id, req.role.as_deref(), req.disabled) {
            Ok(c) => c,
            Err(PatchError::NotFound) => return error(StatusCode::NOT_FOUND, "user not found"),
            Err(PatchError::Invalid(m)) => return error(StatusCode::UNPROCESSABLE_ENTITY, m),
            Err(PatchError::Conflict(m)) => return error(StatusCode::CONFLICT, m),
        }
    };
    if let Some(password) = req.password {
        let st = state.clone();
        let keep = actor.session_token.clone();
        let result = tokio::task::spawn_blocking(move || {
            let conn = st.db.lock().unwrap();
            auth::set_password(&conn, id, &password)?;
            // every other session of the account ends with the old password
            conn.execute(
                "DELETE FROM sessions WHERE user_id = ?1 AND token != ?2",
                (id, keep),
            )?;
            Ok::<_, anyhow::Error>(())
        })
        .await;
        if !matches!(result, Ok(Ok(()))) {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        changed.push("password".into());
    }
    if !changed.is_empty() {
        record(&state, actor.id, "admin_user_update", &format!("user {id}: {}", changed.join(", ")));
    }
    StatusCode::OK.into_response()
}

async fn users_revoke(
    Elevated { user: actor, .. }: Elevated,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let revoked = {
        let conn = state.db.lock().unwrap();
        conn.execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND token != ?2",
            (id, &actor.session_token),
        )
    };
    match revoked {
        Ok(n) => {
            record(&state, actor.id, "admin_sessions_revoke", &format!("user {id}: {n}"));
            Json(serde_json::json!({ "revoked": n })).into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    before_id: Option<i64>,
}

async fn log_list(_e: Elevated, State(state): State<AppState>, Query(q): Query<LogQuery>) -> Response {
    let limit = q.limit.unwrap_or(LOG_LIMIT_DEFAULT).clamp(1, LOG_LIMIT_MAX);
    let kind = q.kind.filter(|k| !k.is_empty());
    let conn = state.db.lock().unwrap();
    let result = (|| -> rusqlite::Result<serde_json::Value> {
        let mut stmt = conn.prepare(
            "SELECT id, ts, user_id, kind, detail FROM event_log
             WHERE (?1 IS NULL OR kind = ?1) AND (?2 IS NULL OR id < ?2)
             ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map((&kind, q.before_id, limit), |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, i64>(0)?,
                    "ts": r.get::<_, String>(1)?,
                    "user_id": r.get::<_, Option<i64>>(2)?,
                    "kind": r.get::<_, String>(3)?,
                    "detail": r.get::<_, String>(4)?,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut kinds = conn.prepare("SELECT DISTINCT kind FROM event_log ORDER BY kind")?;
        let kinds = kinds
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(serde_json::json!({ "rows": rows, "kinds": kinds }))
    })();
    match result {
        Ok(v) => Json(v).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(feature = "dev-inspect")]
mod inspect {
    use super::*;

    const SQL_ROW_CAP: usize = 500;

    pub fn routes() -> Router<AppState> {
        Router::new()
            .route("/inspect/users/{id}", get(user_overview))
            .route("/inspect/users/{id}/config", axum::routing::put(config_put))
            .route("/inspect/users/{id}/conversations/{cid}", get(conversation))
            .route("/inspect/users/{id}/memory/{mid}", get(memory_get).put(memory_put))
            .route("/inspect/sql", post(sql))
    }

    fn username_of(conn: &Connection, id: i64) -> rusqlite::Result<Option<String>> {
        conn.query_row("SELECT username FROM users WHERE id = ?1", [id], |r| r.get(0))
            .optional()
    }

    fn user_or_404(conn: &Connection, id: i64) -> Result<String, Box<Response>> {
        match username_of(conn, id) {
            Ok(Some(u)) => Ok(u),
            Ok(None) => Err(Box::new(error(StatusCode::NOT_FOUND, "user not found"))),
            Err(_) => Err(Box::new(StatusCode::INTERNAL_SERVER_ERROR.into_response())),
        }
    }

    fn config_path(state: &AppState, username: &str) -> std::path::PathBuf {
        state.config_dir.join("users").join(username).join("user.toml")
    }

    async fn user_overview(_e: Elevated, State(state): State<AppState>, Path(id): Path<i64>) -> Response {
        let conn = state.db.lock().unwrap();
        let username = match user_or_404(&conn, id) {
            Ok(u) => u,
            Err(r) => return *r,
        };
        let path = config_path(&state, &username);
        let config_toml = std::fs::read_to_string(&path).unwrap_or_default();
        let tz = crate::config::UserConfig::load(&state.config_dir, &username)
            .ok()
            .and_then(|c| jiff::tz::TimeZone::get(&c.timezone).ok())
            .unwrap_or(jiff::tz::TimeZone::UTC);
        let today = jiff::Timestamp::now().to_zoned(tz).date();
        let result = (|| -> anyhow::Result<serde_json::Value> {
            let tasks = crate::tasks::list(&conn, id)?;
            let mut stmt = conn.prepare(
                "SELECT id, title, updated_at FROM conversations
                 WHERE user_id = ?1 ORDER BY updated_at DESC, id DESC",
            )?;
            let conversations = stmt
                .query_map([id], |r| {
                    Ok(serde_json::json!({
                        "id": r.get::<_, i64>(0)?,
                        "title": r.get::<_, String>(1)?,
                        "updated_at": r.get::<_, String>(2)?,
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut stmt = conn.prepare(
                "SELECT id, category, summary, archived FROM memory_index
                 WHERE user = ?1 ORDER BY rowid DESC",
            )?;
            let memory = stmt
                .query_map([&username], |r| {
                    Ok(serde_json::json!({
                        "id": r.get::<_, String>(0)?,
                        "category": r.get::<_, String>(1)?,
                        "summary": r.get::<_, String>(2)?,
                        "archived": r.get::<_, bool>(3)?,
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let events_today = crate::plan::events_for(&conn, id, today)?;
            Ok(serde_json::json!({
                "username": username,
                "config_path": path.display().to_string(),
                "config_toml": config_toml,
                "tasks": tasks,
                "conversations": conversations,
                "memory": memory,
                "events_today": events_today,
            }))
        })();
        match result {
            Ok(v) => Json(v).into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }

    #[derive(Deserialize)]
    struct ConfigReq {
        toml: String,
    }

    async fn config_put(
        Elevated { user: actor, .. }: Elevated,
        State(state): State<AppState>,
        Path(id): Path<i64>,
        Json(req): Json<ConfigReq>,
    ) -> Response {
        let username = {
            let conn = state.db.lock().unwrap();
            match user_or_404(&conn, id) {
                Ok(u) => u,
                Err(r) => return *r,
            }
        };
        if let Err(e) = crate::config::UserConfig::from_overlay(&state.config_dir, Some(&req.toml)) {
            return error(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string());
        }
        let path = config_path(&state, &username);
        let written = std::fs::create_dir_all(path.parent().expect("user.toml has a parent"))
            .and_then(|_| crate::context::write_atomic(&path, &req.toml));
        if written.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        record(&state, actor.id, "admin_inspect_config_write", &username);
        StatusCode::OK.into_response()
    }

    async fn conversation(
        _e: Elevated,
        State(state): State<AppState>,
        Path((id, cid)): Path<(i64, i64)>,
    ) -> Response {
        let conn = state.db.lock().unwrap();
        match crate::talk::owned(&conn, id, cid) {
            Ok(true) => {}
            Ok(false) => return error(StatusCode::NOT_FOUND, "conversation not found"),
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
        match crate::talk::messages_json(&conn, cid) {
            Ok(v) => Json(v).into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }

    async fn memory_get(
        _e: Elevated,
        State(state): State<AppState>,
        Path((id, mid)): Path<(i64, String)>,
    ) -> Response {
        let username = {
            let conn = state.db.lock().unwrap();
            match user_or_404(&conn, id) {
                Ok(u) => u,
                Err(r) => return *r,
            }
        };
        match crate::memory::read_raw(&state.data_dir, &username, &mid) {
            Ok(Some(content)) => Json(serde_json::json!({ "content": content })).into_response(),
            Ok(None) => error(StatusCode::NOT_FOUND, "memory not found"),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }

    #[derive(Deserialize)]
    struct MemoryReq {
        content: String,
    }

    async fn memory_put(
        Elevated { user: actor, .. }: Elevated,
        State(state): State<AppState>,
        Path((id, mid)): Path<(i64, String)>,
        Json(req): Json<MemoryReq>,
    ) -> Response {
        let conn = state.db.lock().unwrap();
        let username = match user_or_404(&conn, id) {
            Ok(u) => u,
            Err(r) => return *r,
        };
        match crate::memory::write_raw(&conn, &state.data_dir, &username, &mid, &req.content) {
            Ok(Some(())) => {
                let _ = crate::log::record(
                    &conn,
                    Some(actor.id),
                    "admin_inspect_memory_write",
                    &format!("{username}/{mid}"),
                );
                StatusCode::OK.into_response()
            }
            Ok(None) => error(StatusCode::NOT_FOUND, "memory not found"),
            Err(e) => error(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string()),
        }
    }

    #[derive(Deserialize)]
    struct SqlReq {
        sql: String,
    }

    fn cell(v: rusqlite::types::ValueRef<'_>) -> serde_json::Value {
        use rusqlite::types::ValueRef as V;
        match v {
            V::Null => serde_json::Value::Null,
            V::Integer(i) => i.into(),
            V::Real(f) => f.into(),
            V::Text(t) => String::from_utf8_lossy(t).into_owned().into(),
            V::Blob(b) => format!("<blob {} bytes>", b.len()).into(),
        }
    }

    /// One statement; a statement that yields columns is read (capped), any
    /// other is executed and reports its change count.
    pub fn run_sql(conn: &Connection, sql: &str) -> rusqlite::Result<serde_json::Value> {
        let mut stmt = conn.prepare(sql)?;
        if stmt.column_count() == 0 {
            let changes = stmt.execute([])?;
            return Ok(serde_json::json!({ "changes": changes }));
        }
        let columns: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
        let width = columns.len();
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        let mut truncated = false;
        while let Some(row) = rows.next()? {
            if out.len() >= SQL_ROW_CAP {
                truncated = true;
                break;
            }
            let cells: Vec<serde_json::Value> = (0..width).map(|i| cell(row.get_ref_unwrap(i))).collect();
            out.push(serde_json::Value::Array(cells));
        }
        Ok(serde_json::json!({ "columns": columns, "rows": out, "truncated": truncated }))
    }

    async fn sql(
        Elevated { user: actor, .. }: Elevated,
        State(state): State<AppState>,
        Json(req): Json<SqlReq>,
    ) -> Response {
        let conn = state.db.lock().unwrap();
        let _ = crate::log::record(&conn, Some(actor.id), "admin_inspect_sql", req.sql.trim());
        match run_sql(&conn, &req.sql) {
            Ok(v) => Json(v).into_response(),
            Err(e) => error(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn_with(users: &[(&str, bool)]) -> Connection {
        let conn = crate::db::open_memory().unwrap();
        for (name, admin) in users {
            auth::create_user(&conn, name, "pw", *admin).unwrap();
        }
        conn
    }

    #[test]
    fn last_enabled_admin_cannot_be_demoted_or_disabled() {
        let conn = conn_with(&[("root", true), ("kid", false)]);
        assert_eq!(
            apply_user_patch(&conn, 2, 1, Some("member"), None),
            Err(PatchError::Conflict("that would leave no enabled admin"))
        );
        assert_eq!(
            apply_user_patch(&conn, 2, 1, None, Some(true)),
            Err(PatchError::Conflict("that would leave no enabled admin"))
        );
        assert_eq!(apply_user_patch(&conn, 1, 2, Some("admin"), None).unwrap(), vec!["role=admin"]);
        assert_eq!(apply_user_patch(&conn, 2, 1, Some("member"), None).unwrap(), vec!["role=member"]);
    }

    #[test]
    fn actors_cannot_touch_their_own_role_or_flag_and_unknown_targets_are_absent() {
        let conn = conn_with(&[("root", true), ("other", true)]);
        assert!(matches!(apply_user_patch(&conn, 1, 1, Some("member"), None), Err(PatchError::Conflict(_))));
        assert!(matches!(apply_user_patch(&conn, 1, 1, None, Some(true)), Err(PatchError::Conflict(_))));
        assert_eq!(apply_user_patch(&conn, 1, 99, None, Some(true)), Err(PatchError::NotFound));
        assert!(matches!(apply_user_patch(&conn, 1, 2, Some("root"), None), Err(PatchError::Invalid(_))));
        assert!(apply_user_patch(&conn, 1, 2, None, None).unwrap().is_empty());
    }

    #[test]
    fn disabling_drops_the_users_sessions() {
        let conn = conn_with(&[("root", true), ("kid", false)]);
        conn.execute(
            "INSERT INTO sessions (token, user_id, expires_at) VALUES ('t', 2, '2999-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        apply_user_patch(&conn, 1, 2, None, Some(true)).unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        let rows = list_users(&conn, jiff::Timestamp::now()).unwrap();
        assert!(rows[1].disabled);
    }

    #[test]
    fn secrets_load_reports_missing_and_malformed_seeds() {
        let tmp = tempfile::tempdir().unwrap();
        let (s, warnings) = AdminSecrets::load(tmp.path());
        assert!(s.totp_seed.is_none() && warnings.is_empty());
        std::fs::write(tmp.path().join(TOTP_FILE), "!!!").unwrap();
        let (s, warnings) = AdminSecrets::load(tmp.path());
        assert!(s.totp_seed.is_none() && warnings.len() == 1);
        std::fs::write(tmp.path().join(TOTP_FILE), "gezdgnbvgy3tqojqgezdgnbvgy3tqojq\n").unwrap();
        let (s, warnings) = AdminSecrets::load(tmp.path());
        assert_eq!(s.totp_seed.unwrap(), b"12345678901234567890");
        assert!(warnings.is_empty());
    }

    #[test]
    fn totp_mode_follows_seed_and_build() {
        assert_eq!(AdminSecrets::with_seed(vec![1; 20]).totp_mode(), TotpMode::Required);
        let expected = if INSPECT { TotpMode::PasswordOnly } else { TotpMode::Missing };
        assert_eq!(AdminSecrets::default().totp_mode(), expected);
    }

    #[test]
    fn opting_out_of_totp_ignores_the_seed() {
        assert_eq!(
            AdminSecrets::with_seed(vec![1; 20]).require_totp(false).totp_mode(),
            TotpMode::PasswordOnly
        );
        assert_eq!(
            AdminSecrets::default().require_totp(false).totp_mode(),
            TotpMode::PasswordOnly
        );
    }

    #[test]
    fn grant_cookies_are_scoped_and_strict() {
        assert_eq!(
            grant_cookie("t", false),
            "admin=t; HttpOnly; Path=/api/admin; SameSite=Strict; Max-Age=900"
        );
        assert!(grant_cookie("t", true).ends_with("; Secure"));
        assert!(clear_grant_cookie(false).contains("Max-Age=0"));
    }

    #[test]
    fn fetch_site_rejects_only_foreign_origins() {
        let mut h = HeaderMap::new();
        assert!(fetch_site_ok(&h));
        h.insert("sec-fetch-site", "same-origin".parse().unwrap());
        assert!(fetch_site_ok(&h));
        h.insert("sec-fetch-site", "none".parse().unwrap());
        assert!(fetch_site_ok(&h));
        h.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert!(!fetch_site_ok(&h));
        h.insert("sec-fetch-site", "same-site".parse().unwrap());
        assert!(!fetch_site_ok(&h));
    }

    #[cfg(feature = "dev-inspect")]
    #[test]
    fn sql_console_reads_and_writes() {
        let conn = conn_with(&[("root", true)]);
        let v = inspect::run_sql(&conn, "SELECT id, username FROM users").unwrap();
        assert_eq!(v["columns"], serde_json::json!(["id", "username"]));
        assert_eq!(v["rows"], serde_json::json!([[1, "root"]]));
        let v = inspect::run_sql(&conn, "UPDATE users SET username = 'r2'").unwrap();
        assert_eq!(v["changes"], 1);
        assert!(inspect::run_sql(&conn, "SELEKT").is_err());
    }
}
