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
pub const LLM_MODEL_SETTING: &str = "llm_model";

pub struct AdminSecrets {
    pub totp_seed: Option<Vec<u8>>,
    require_second_factor: bool,
}

impl Default for AdminSecrets {
    fn default() -> Self {
        Self { totp_seed: None, require_second_factor: true }
    }
}

impl AdminSecrets {
    pub fn with_seed(seed: Vec<u8>) -> Self {
        Self { totp_seed: Some(seed), ..Self::default() }
    }

    /// `false` is the operator's opt-out: elevation re-asks for the account
    /// password alone and no second factor is consulted.
    #[must_use]
    pub fn require_second_factor(mut self, require: bool) -> Self {
        self.require_second_factor = require;
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

    /// The legacy report, for a server whose users have enrolled nothing.
    pub fn totp_mode(&self) -> TotpMode {
        self.mode_with_factor(false)
    }

    /// `Required` as soon as any factor exists — the user's own or the shared
    /// seed. `Missing` is the release-build lockout: a second factor is asked
    /// for and there is none to ask for.
    pub fn mode_with_factor(&self, user_has_factor: bool) -> TotpMode {
        match (self.require_second_factor, self.totp_seed.is_some() || user_has_factor, INSPECT) {
            (false, _, _) | (true, false, true) => TotpMode::PasswordOnly,
            (true, true, _) => TotpMode::Required,
            (true, false, false) => TotpMode::Missing,
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
            .map_err(axum::response::IntoResponse::into_response)?;
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
        let conn = state.db();
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
        .route("/elevate/challenge", post(elevate_challenge))
        .route("/drop", post(drop_grant))
        .route("/status", get(status))
        .route("/users", get(users_list).post(users_create))
        .route("/users/{id}", patch(users_patch))
        .route("/users/{id}/revoke_sessions", post(users_revoke))
        .route("/invites", get(invites_list).post(invites_create))
        .route("/invites/{id}", axum::routing::delete(invites_revoke))
        .route("/providers/llm/model", axum::routing::put(llm_model_put))
        .route("/log", get(log_list))
        .route("/traces", get(traces_list))
        .route("/traces/{id}", get(traces_detail));
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
    let conn = state.db();
    let _ = crate::log::record(&conn, Some(actor), kind, detail);
}

/// What this admin can present as a second factor. A passkey counts only where
/// browsers will speak `WebAuthn`; the shared seed answers as this user's TOTP
/// until they enrol a secret of their own.
#[derive(Debug, Clone, Copy)]
pub struct Methods {
    pub passkey: bool,
    pub totp: bool,
}

impl Methods {
    fn any(self) -> bool {
        self.passkey || self.totp
    }

    fn preferred(self) -> &'static str {
        match (self.passkey, self.totp) {
            (true, _) => "passkey",
            (false, true) => "totp",
            (false, false) => "none",
        }
    }
}

fn methods_for(state: &AppState, user_id: i64) -> rusqlite::Result<Methods> {
    let conn = state.db();
    let (passkey, own_totp) = crate::security::user_factors(&conn, user_id)?;
    Ok(Methods {
        passkey: passkey && state.passkeys.available(),
        totp: own_totp || state.admin_secrets.totp_seed.is_some(),
    })
}

async fn gate(AdminUser(user): AdminUser, State(state): State<AppState>, headers: HeaderMap) -> Response {
    let now = jiff::Timestamp::now().as_second();
    let expires = match grant_token(&headers) {
        Some(token) => {
            let conn = state.db();
            match live_grant(&conn, &user, &token, now) {
                Ok(e) => e,
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
        None => None,
    };
    let Ok(methods) = methods_for(&state, user.id) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let mode = state.admin_secrets.mode_with_factor(methods.any());
    Json(serde_json::json!({
        "elevated": expires.is_some(),
        "expires_at": expires.map(unix_to_rfc3339),
        "second_factor": methods.preferred(),
        "methods": { "passkey": methods.passkey, "totp": methods.totp },
        "require_second_factor": mode != TotpMode::PasswordOnly,
        "totp": mode,
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
    /// A `navigator.credentials.get()` result answering `elevate/challenge`.
    #[serde(default)]
    assertion: Option<serde_json::Value>,
}

pub enum ElevateOutcome {
    Granted { token: String, expires_at: i64 },
    Denied,
}

/// What the request offered alongside the password.
pub enum SecondFactor {
    Assertion(Box<webauthn_rs::prelude::PublicKeyCredential>),
    Code(String),
    None,
}

/// Proof that survived verification and is waiting to be spent once the
/// password is known to be right.
enum Proof {
    Passkey(Box<webauthn_rs::prelude::AuthenticationResult>),
    Step(i64),
    PasswordOnly,
}

/// The password is verified on every attempt (with no lock held) whatever the
/// second factor does, so a wrong one costs the same argon2 work. An accepted
/// code's time step is recorded, and an accepted assertion's sign counter is
/// written back, only once the password is also known to be right.
pub fn elevate_blocking(
    state: &AppState,
    user: &CurrentUser,
    password: &str,
    factor: SecondFactor,
    now: jiff::Timestamp,
) -> Result<ElevateOutcome> {
    let (hash, user_seed, methods) = {
        let conn = state.db();
        let (passkey, own_totp) = crate::security::user_factors(&conn, user.id)?;
        (
            auth::stored_hash(&conn, user.id)?,
            crate::security::totp_seed(&conn, user.id)?,
            Methods {
                passkey: passkey && state.passkeys.available(),
                totp: own_totp || state.admin_secrets.totp_seed.is_some(),
            },
        )
    };
    let password_ok = auth::verify_against(password, hash.as_deref());
    let proof = match state.admin_secrets.mode_with_factor(methods.any()) {
        TotpMode::Missing => anyhow::bail!("no second factor installed"),
        TotpMode::PasswordOnly => Proof::PasswordOnly,
        TotpMode::Required => match factor {
            SecondFactor::Assertion(cred) => {
                match state
                    .passkeys
                    .finish_authentication(&user.session_token, &cred, now.as_second())
                {
                    Ok(result) => Proof::Passkey(Box::new(result)),
                    Err(_) => return Ok(ElevateOutcome::Denied),
                }
            }
            SecondFactor::Code(code) => {
                let seed = user_seed.or_else(|| state.admin_secrets.totp_seed.clone());
                match seed.as_deref().and_then(|s| crate::totp::verify(s, &code, now)) {
                    Some(step) => Proof::Step(step),
                    None => return Ok(ElevateOutcome::Denied),
                }
            }
            SecondFactor::None => return Ok(ElevateOutcome::Denied),
        },
    };
    if !password_ok {
        return Ok(ElevateOutcome::Denied);
    }
    let conn = state.db();
    match proof {
        Proof::Passkey(result) => {
            if !crate::security::record_use(&conn, user.id, &result, now)? {
                return Ok(ElevateOutcome::Denied);
            }
        }
        Proof::Step(step) => {
            if !crate::security::claim_step(&conn, user.id, step)? {
                return Ok(ElevateOutcome::Denied);
            }
        }
        Proof::PasswordOnly => {}
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

/// The passkeys this admin can assert with, for `navigator.credentials.get()`.
async fn elevate_challenge(AdminUser(user): AdminUser, State(state): State<AppState>) -> Response {
    if !state.passkeys.available() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            &crate::security::PasskeyError::Unavailable.to_string(),
        );
    }
    let credentials = {
        let conn = state.db();
        match crate::security::credentials(&conn, user.id) {
            Ok(keys) => keys,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    };
    if credentials.is_empty() {
        return error(StatusCode::CONFLICT, "no passkeys on this account");
    }
    let now = jiff::Timestamp::now().as_second();
    match state
        .passkeys
        .start_authentication(&user.session_token, &credentials, now)
    {
        Ok(challenge) => Json(challenge).into_response(),
        Err(e) => error(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string()),
    }
}

async fn elevate(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    Json(req): Json<ElevateReq>,
) -> Response {
    let Ok(methods) = methods_for(&state, user.id) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    if state.admin_secrets.mode_with_factor(methods.any()) == TotpMode::Missing {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no second factor on this account and no admin secret on this server",
        );
    }
    let now = jiff::Timestamp::now();
    if !state.admin_limiter.try_attempt(&user.username, now) {
        return error(StatusCode::TOO_MANY_REQUESTS, "too many attempts");
    }
    let factor = match req.assertion {
        Some(raw) => if let Ok(cred) = serde_json::from_value(raw) { SecondFactor::Assertion(Box::new(cred)) } else {
            record(&state, user.id, "admin_elevate_denied", &user.username);
            return error(StatusCode::UNAUTHORIZED, "that passkey could not be verified");
        },
        None => match req.code.filter(|c| !c.trim().is_empty()) {
            Some(code) => SecondFactor::Code(code),
            None => SecondFactor::None,
        },
    };
    let Ok(slot) = state.login_slots.clone().try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "2")],
            Json(serde_json::json!({ "error": "too many sign-ins in flight; try again" })),
        )
            .into_response();
    };
    let st = state.clone();
    let u = user.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        elevate_blocking(&st, &u, &req.password, factor, now)
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
        let conn = state.db();
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
    std::fs::metadata(path).map_or(0, |m| m.len())
}

async fn status(_e: Elevated, State(state): State<AppState>) -> Response {
    let now = jiff::Timestamp::now();
    let (users, sessions, push) = {
        let conn = state.db();
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
        "providers": live_providers(&state),
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
    let conn = state.db();
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
        let conn = st.db();
        let id = auth::create_user(&conn, &username, &password, admin)?;
        crate::builtin_memory::seed_new_user(&conn, &st.data_dir, &st.config_dir, &username);
        Ok::<_, anyhow::Error>((id, username, admin))
    })
    .await;
    match result {
        Ok(Ok((id, username, admin))) => {
            state.spawn_vector_backfill();
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

async fn invites_list(_e: Elevated, State(state): State<AppState>) -> Response {
    let conn = state.db();
    match crate::invites::outstanding(&conn, jiff::Timestamp::now()) {
        Ok(v) => Json(v).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateInviteReq {
    #[serde(default)]
    admin: bool,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    days: Option<u32>,
}

async fn invites_create(
    Elevated { user: actor, .. }: Elevated,
    State(state): State<AppState>,
    Json(req): Json<CreateInviteReq>,
) -> Response {
    let made = {
        let conn = state.db();
        crate::invites::create(
            &conn,
            Some(actor.id),
            req.admin,
            req.username.as_deref(),
            req.days.unwrap_or(crate::invites::DEFAULT_DAYS),
            &state.public_base_url,
            jiff::Timestamp::now(),
        )
    };
    match made {
        Ok(c) => {
            let role = if c.invite.admin { "admin" } else { "member" };
            record(&state, actor.id, "admin_invite_create", &format!("invite {} ({role})", c.invite.id));
            (StatusCode::CREATED, Json(c)).into_response()
        }
        Err(crate::invites::InviteError::Invalid(m)) => error(StatusCode::UNPROCESSABLE_ENTITY, &m),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn invites_revoke(
    Elevated { user: actor, .. }: Elevated,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let revoked = {
        let conn = state.db();
        crate::invites::revoke(&conn, id, jiff::Timestamp::now())
    };
    match revoked {
        Ok(true) => {
            record(&state, actor.id, "admin_invite_revoke", &format!("invite {id}"));
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => error(StatusCode::NOT_FOUND, "invite not found"),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
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
        let conn = state.db();
        match apply_user_patch(&conn, actor.id, id, req.role.as_deref(), req.disabled) {
            Ok(c) => c,
            Err(PatchError::NotFound) => return error(StatusCode::NOT_FOUND, "user not found"),
            Err(PatchError::Invalid(m)) => return error(StatusCode::UNPROCESSABLE_ENTITY, m),
            Err(PatchError::Conflict(m)) => return error(StatusCode::CONFLICT, m),
        }
    };
    if let Some(password) = req.password {
        let target_is_admin = {
            let conn = state.db();
            conn.query_row("SELECT role FROM users WHERE id = ?1", [id], |r| r.get::<_, String>(0))
                .is_ok_and(|r| r == "admin")
        };
        if id != actor.id && target_is_admin {
            return error(StatusCode::CONFLICT, "you can't reset another admin's password");
        }
        let st = state.clone();
        let keep = actor.session_token.clone();
        let result = tokio::task::spawn_blocking(move || {
            let conn = st.db();
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

fn live_providers(state: &AppState) -> ProvidersInfo {
    let mut info = state.providers_info.clone();
    if let (Some(llm), Some(model)) = (info.llm.as_mut(), state.llm.model()) {
        llm.model = model;
    }
    info
}

#[derive(Deserialize)]
struct ModelReq {
    model: String,
}

fn valid_model_name(model: &str) -> bool {
    !model.is_empty() && model.len() <= 200 && !model.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Switches the chat model for every later call and keeps the choice across
/// restarts, overriding `server.toml`.
async fn llm_model_put(
    Elevated { user: actor, .. }: Elevated,
    State(state): State<AppState>,
    Json(req): Json<ModelReq>,
) -> Response {
    let model = req.model.trim();
    if !valid_model_name(model) {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "model must be a non-empty name without spaces");
    }
    let before = state.llm.model();
    if !state.llm.set_model(model) {
        return error(StatusCode::CONFLICT, "the configured chat provider has no model to change");
    }
    {
        let conn = state.db();
        if crate::db::set_server_setting(&conn, LLM_MODEL_SETTING, model).is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    record(
        &state,
        actor.id,
        "admin_llm_model",
        &format!("{} -> {model}", before.as_deref().unwrap_or("?")),
    );
    Json(serde_json::json!({ "model": model })).into_response()
}

async fn users_revoke(
    Elevated { user: actor, .. }: Elevated,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let revoked = {
        let conn = state.db();
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
    let conn = state.db();
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

#[derive(Deserialize)]
struct TraceQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    user_id: Option<i64>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    outcome: Option<String>,
    #[serde(default)]
    before_id: Option<i64>,
}

async fn traces_list(
    _e: Elevated,
    State(state): State<AppState>,
    Query(q): Query<TraceQuery>,
) -> Response {
    let filter = crate::trace::Filter {
        limit: q.limit.unwrap_or(LOG_LIMIT_DEFAULT).clamp(1, LOG_LIMIT_MAX),
        user_id: q.user_id,
        kind: q.kind.filter(|k| !k.is_empty()),
        outcome: q.outcome.filter(|o| !o.is_empty()),
        before_id: q.before_id,
    };
    let conn = state.db();
    match crate::trace::list(&conn, &filter) {
        Ok(v) => Json(v).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn traces_detail(_e: Elevated, State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    let conn = state.db();
    match crate::trace::detail(&conn, id, INSPECT) {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "trace not found"),
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
        let conn = state.db();
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
            let conn = state.db();
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
        let conn = state.db();
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
            let conn = state.db();
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
        let conn = state.db();
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
        let conn = state.db();
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
    fn opting_out_of_a_second_factor_ignores_every_factor() {
        let opted_out = AdminSecrets::with_seed(vec![1; 20]).require_second_factor(false);
        assert_eq!(opted_out.totp_mode(), TotpMode::PasswordOnly);
        assert_eq!(opted_out.mode_with_factor(true), TotpMode::PasswordOnly);
        assert_eq!(
            AdminSecrets::default().require_second_factor(false).totp_mode(),
            TotpMode::PasswordOnly
        );
    }

    #[test]
    fn a_users_own_factor_requires_a_second_factor_without_a_seed() {
        let bare = AdminSecrets::default();
        assert_eq!(bare.mode_with_factor(true), TotpMode::Required);
        let expected = if INSPECT { TotpMode::PasswordOnly } else { TotpMode::Missing };
        assert_eq!(bare.mode_with_factor(false), expected);
    }

    #[test]
    fn preferred_method_puts_passkeys_first() {
        assert_eq!(Methods { passkey: true, totp: true }.preferred(), "passkey");
        assert_eq!(Methods { passkey: false, totp: true }.preferred(), "totp");
        assert_eq!(Methods { passkey: false, totp: false }.preferred(), "none");
        assert!(!Methods { passkey: false, totp: false }.any());
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
