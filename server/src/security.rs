use crate::auth::{self, CurrentUser};
use crate::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use webauthn_rs::prelude::*;

pub const MAX_PASSKEYS: usize = 10;
pub const MAX_NAME_LEN: usize = 64;
pub const CHALLENGE_TTL_SECS: i64 = 120;
pub const ISSUER: &str = "Note";
const RP_NAME: &str = "Note";

/// A challenge in flight, keyed by the session that asked for it. Both maps are
/// single-use: finishing takes the entry out, and an entry older than
/// `CHALLENGE_TTL_SECS` is gone whether or not it was used.
type Pending<T> = Mutex<HashMap<String, (T, i64)>>;

pub struct PasskeyService {
    webauthn: Option<Webauthn>,
    /// Browsers only speak WebAuthn to an https origin (or http://localhost).
    secure_origin: bool,
    registrations: Pending<PasskeyRegistration>,
    authentications: Pending<PasskeyAuthentication>,
}

#[derive(Debug, thiserror::Error)]
pub enum PasskeyError {
    #[error("passkeys need an https address")]
    Unavailable,
    #[error("that challenge has expired")]
    NoChallenge,
    #[error("that passkey could not be verified")]
    Webauthn(#[from] WebauthnError),
}

impl Default for PasskeyService {
    fn default() -> Self {
        Self {
            webauthn: None,
            secure_origin: false,
            registrations: Mutex::new(HashMap::new()),
            authentications: Mutex::new(HashMap::new()),
        }
    }
}

fn take<T>(map: &Pending<T>, session: &str, now: i64) -> Option<T> {
    let mut map = map.lock().unwrap();
    map.retain(|_, (_, at)| now - *at < CHALLENGE_TTL_SECS);
    map.remove(session).map(|(state, _)| state)
}

fn put<T>(map: &Pending<T>, session: &str, state: T, now: i64) {
    let mut map = map.lock().unwrap();
    map.retain(|_, (_, at)| now - *at < CHALLENGE_TTL_SECS);
    map.insert(session.to_string(), (state, now));
}

impl PasskeyService {
    /// The relying party is the host of `public_base_url` unless `rp_id` and
    /// `rp_origin` override it. A URL WebAuthn cannot describe (a bare IP, say)
    /// leaves the service off with a warning rather than failing startup.
    pub fn build(
        public_base_url: &str,
        rp_id: Option<&str>,
        rp_origin: Option<&str>,
    ) -> (Self, Option<String>) {
        let origin_text = rp_origin.unwrap_or(public_base_url).trim_end_matches('/');
        let origin = match Url::parse(origin_text) {
            Ok(u) => u,
            Err(e) => return (Self::default(), Some(format!("{origin_text}: {e}"))),
        };
        let secure_origin = origin.scheme() == "https" || origin.host_str() == Some("localhost");
        let Some(id) = rp_id
            .map(str::to_string)
            .or_else(|| origin.domain().map(str::to_string))
        else {
            return (
                Self::default(),
                Some(format!(
                    "{origin_text} has no domain name, so passkeys are off"
                )),
            );
        };
        match WebauthnBuilder::new(&id, &origin).map(|b| b.rp_name(RP_NAME).build()) {
            Ok(Ok(webauthn)) => (
                Self {
                    webauthn: Some(webauthn),
                    secure_origin,
                    ..Self::default()
                },
                None,
            ),
            Ok(Err(e)) | Err(e) => (
                Self::default(),
                Some(format!("passkeys are off: {id} at {origin_text}: {e}")),
            ),
        }
    }

    pub fn available(&self) -> bool {
        self.webauthn.is_some() && self.secure_origin
    }

    fn webauthn(&self) -> Result<&Webauthn, PasskeyError> {
        self.webauthn.as_ref().ok_or(PasskeyError::Unavailable)
    }

    pub fn start_registration(
        &self,
        session: &str,
        user_id: i64,
        username: &str,
        existing: &[Passkey],
        now: i64,
    ) -> Result<CreationChallengeResponse, PasskeyError> {
        let exclude = existing.iter().map(|k| k.cred_id().clone()).collect();
        let (challenge, state) = self.webauthn()?.start_passkey_registration(
            user_handle(user_id),
            username,
            username,
            Some(exclude),
        )?;
        put(&self.registrations, session, state, now);
        Ok(challenge)
    }

    pub fn finish_registration(
        &self,
        session: &str,
        credential: &RegisterPublicKeyCredential,
        now: i64,
    ) -> Result<Passkey, PasskeyError> {
        let state = take(&self.registrations, session, now).ok_or(PasskeyError::NoChallenge)?;
        Ok(self
            .webauthn()?
            .finish_passkey_registration(credential, &state)?)
    }

    pub fn start_authentication(
        &self,
        session: &str,
        credentials: &[Passkey],
        now: i64,
    ) -> Result<RequestChallengeResponse, PasskeyError> {
        let (challenge, state) = self.webauthn()?.start_passkey_authentication(credentials)?;
        put(&self.authentications, session, state, now);
        Ok(challenge)
    }

    pub fn finish_authentication(
        &self,
        session: &str,
        credential: &PublicKeyCredential,
        now: i64,
    ) -> Result<AuthenticationResult, PasskeyError> {
        let state = take(&self.authentications, session, now).ok_or(PasskeyError::NoChallenge)?;
        Ok(self
            .webauthn()?
            .finish_passkey_authentication(credential, &state)?)
    }
}

/// The WebAuthn user handle. Derived from the account id so it is stable for
/// the life of the account without a column of its own.
fn user_handle(user_id: i64) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("note-user-{user_id}").as_bytes(),
    )
}

#[derive(Debug, Serialize)]
pub struct PasskeyInfo {
    pub id: i64,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("name must be 1 to {MAX_NAME_LEN} characters")]
    InvalidName,
    #[error("at most {MAX_PASSKEYS} passkeys per user")]
    TooMany,
    #[error("that passkey is already registered")]
    Duplicate,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Encode(#[from] serde_json::Error),
}

fn clean_name(name: &str) -> Result<&str, SaveError> {
    let name = name.trim();
    let len = name.chars().count();
    if len == 0 || len > MAX_NAME_LEN {
        return Err(SaveError::InvalidName);
    }
    Ok(name)
}

const COLS: &str = "id, name, created_at, last_used_at";

fn row_to_info(r: &rusqlite::Row) -> rusqlite::Result<PasskeyInfo> {
    Ok(PasskeyInfo {
        id: r.get(0)?,
        name: r.get(1)?,
        created_at: r.get(2)?,
        last_used_at: r.get(3)?,
    })
}

pub fn list(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<PasskeyInfo>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM passkeys WHERE user_id = ?1 ORDER BY id"
    ))?;
    let rows = stmt.query_map([user_id], row_to_info)?;
    rows.collect()
}

/// The user's credentials, ready for an assertion. A row whose stored
/// credential no longer parses is skipped rather than failing the whole login.
pub fn credentials(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<Passkey>> {
    let mut stmt =
        conn.prepare("SELECT credential FROM passkeys WHERE user_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map([user_id], |r| r.get::<_, String>(0))?;
    let mut keys = Vec::new();
    for row in rows {
        if let Ok(key) = serde_json::from_str::<Passkey>(&row?) {
            keys.push(key);
        }
    }
    Ok(keys)
}

pub fn count(conn: &Connection, user_id: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM passkeys WHERE user_id = ?1",
        [user_id],
        |r| r.get(0),
    )
}

pub fn add(
    conn: &Connection,
    user_id: i64,
    name: &str,
    key: &Passkey,
) -> Result<PasskeyInfo, SaveError> {
    let name = clean_name(name)?;
    if count(conn, user_id)? as usize >= MAX_PASSKEYS {
        return Err(SaveError::TooMany);
    }
    let created_at = jiff::Timestamp::now().to_string();
    let cred_id = key.cred_id().as_ref().to_vec();
    let credential = serde_json::to_string(key)?;
    let written = conn.execute(
        "INSERT INTO passkeys (user_id, name, credential, cred_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        (user_id, name, &credential, &cred_id, &created_at),
    );
    match written {
        Ok(_) => Ok(PasskeyInfo {
            id: conn.last_insert_rowid(),
            name: name.to_string(),
            created_at,
            last_used_at: None,
        }),
        Err(rusqlite::Error::SqliteFailure(f, _))
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Err(SaveError::Duplicate)
        }
        Err(e) => Err(SaveError::Db(e)),
    }
}

pub fn rename(
    conn: &Connection,
    user_id: i64,
    id: i64,
    name: &str,
) -> Result<Option<PasskeyInfo>, SaveError> {
    let name = clean_name(name)?;
    let changed = conn.execute(
        "UPDATE passkeys SET name = ?1 WHERE id = ?2 AND user_id = ?3",
        (name, id, user_id),
    )?;
    if changed == 0 {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM passkeys WHERE id = ?1"),
            [id],
            row_to_info,
        )
        .optional()?)
}

/// Returns the removed row, or `None` when the id is not this user's.
pub fn remove(conn: &Connection, user_id: i64, id: i64) -> rusqlite::Result<Option<PasskeyInfo>> {
    let info = conn
        .query_row(
            &format!("SELECT {COLS} FROM passkeys WHERE id = ?1 AND user_id = ?2"),
            (id, user_id),
            row_to_info,
        )
        .optional()?;
    if info.is_some() {
        conn.execute("DELETE FROM passkeys WHERE id = ?1", [id])?;
    }
    Ok(info)
}

/// Stamps the credential the assertion used and writes back the properties it
/// moved (sign counter, backup state). `false` means the credential is not this
/// user's, which the caller must treat as a failed assertion.
pub fn record_use(
    conn: &Connection,
    user_id: i64,
    result: &AuthenticationResult,
    now: jiff::Timestamp,
) -> rusqlite::Result<bool> {
    let cred_id = result.cred_id().as_ref().to_vec();
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, credential FROM passkeys WHERE user_id = ?1 AND cred_id = ?2",
            (user_id, &cred_id),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((id, stored)) = row else {
        return Ok(false);
    };
    let updated = serde_json::from_str::<Passkey>(&stored)
        .ok()
        .and_then(|mut key| {
            key.update_credential(result)?;
            serde_json::to_string(&key).ok()
        })
        .unwrap_or(stored);
    conn.execute(
        "UPDATE passkeys SET credential = ?1, last_used_at = ?2 WHERE id = ?3",
        (updated, now.to_string(), id),
    )?;
    Ok(true)
}

/// The second factors this account holds of its own: a passkey, a TOTP secret.
pub fn user_factors(conn: &Connection, user_id: i64) -> rusqlite::Result<(bool, bool)> {
    let (enabled, _) = totp_state(conn, user_id)?;
    Ok((count(conn, user_id)? > 0, enabled))
}

/// Whether the account has a confirmed secret, and whether an unconfirmed
/// enrolment is waiting.
pub fn totp_state(conn: &Connection, user_id: i64) -> rusqlite::Result<(bool, bool)> {
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT totp_secret, totp_pending FROM users WHERE id = ?1",
            [user_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (secret, pending) = row.unwrap_or((None, None));
    Ok((secret.is_some(), pending.is_some()))
}

/// The account's own TOTP seed, decoded; `None` when it has none.
pub fn totp_seed(conn: &Connection, user_id: i64) -> rusqlite::Result<Option<Vec<u8>>> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT totp_secret FROM users WHERE id = ?1",
            [user_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(stored.and_then(|s| crate::totp::parse_seed(&s).ok()))
}

/// Puts a fresh secret in `totp_pending`, replacing any enrolment the user
/// started and walked away from, and returns it base32-encoded.
pub fn totp_start(conn: &Connection, user_id: i64) -> rusqlite::Result<String> {
    let secret = crate::totp::generate_seed();
    conn.execute(
        "UPDATE users SET totp_pending = ?1 WHERE id = ?2",
        (&secret, user_id),
    )?;
    Ok(secret)
}

pub fn otpauth_uri(secret_base32: &str, account: &str) -> String {
    let seed = crate::totp::parse_seed(secret_base32).unwrap_or_default();
    crate::totp::otpauth_uri(&seed, ISSUER, account)
}

/// Promotes the pending secret when `code` matches it. The accepted step is
/// recorded, so the code that finished enrolment cannot also elevate.
pub fn totp_confirm(
    conn: &Connection,
    user_id: i64,
    code: &str,
    now: jiff::Timestamp,
) -> rusqlite::Result<bool> {
    let pending: Option<String> = conn
        .query_row(
            "SELECT totp_pending FROM users WHERE id = ?1",
            [user_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let Some(seed) = pending
        .as_deref()
        .and_then(|s| crate::totp::parse_seed(s).ok())
    else {
        return Ok(false);
    };
    let Some(step) = crate::totp::verify(&seed, code, now) else {
        return Ok(false);
    };
    if !claim_step(conn, user_id, step)? {
        return Ok(false);
    }
    conn.execute(
        "UPDATE users SET totp_secret = totp_pending, totp_pending = NULL WHERE id = ?1",
        [user_id],
    )?;
    Ok(true)
}

pub fn totp_remove(conn: &Connection, user_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE users SET totp_secret = NULL, totp_pending = NULL WHERE id = ?1",
        [user_id],
    )?;
    Ok(())
}

/// Records an accepted time step for this user, rejecting any step at or below
/// the highest one already used.
pub fn claim_step(conn: &Connection, user_id: i64, step: i64) -> rusqlite::Result<bool> {
    let last: Option<i64> = conn
        .query_row(
            "SELECT last_step FROM totp_replay WHERE user_id = ?1",
            [user_id],
            |r| r.get(0),
        )
        .optional()?;
    if last.is_some_and(|l| step <= l) {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO totp_replay (user_id, last_step) VALUES (?1, ?2)
         ON CONFLICT(user_id) DO UPDATE SET last_step = excluded.last_step",
        (user_id, step),
    )?;
    Ok(true)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(overview))
        .route("/passkeys", post(passkey_finish))
        .route("/passkeys/challenge", post(passkey_challenge))
        .route(
            "/passkeys/{id}",
            patch(passkey_rename).delete(passkey_delete),
        )
        .route("/totp/start", post(totp_start_route))
        .route("/totp/confirm", post(totp_confirm_route))
        .route("/totp", axum::routing::delete(totp_delete))
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

#[derive(Deserialize)]
struct PasswordReq {
    password: String,
}

/// The sudo check every mutating call pays: a stolen session cannot add or drop
/// a factor without the account password. Attempts are limited like a login.
/// A `Some` is the response the route must answer with.
async fn password_refused(
    state: &AppState,
    user: &CurrentUser,
    password: String,
) -> Option<Response> {
    let now = jiff::Timestamp::now();
    if !state.security_limiter.try_attempt(&user.username, now) {
        return Some(error(StatusCode::TOO_MANY_REQUESTS, "too many attempts"));
    }
    let Ok(slot) = state.login_slots.clone().try_acquire_owned() else {
        return Some(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [(axum::http::header::RETRY_AFTER, "2")],
                Json(serde_json::json!({ "error": "too many sign-ins in flight; try again" })),
            )
                .into_response(),
        );
    };
    let st = state.clone();
    let id = user.id;
    let verified = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        let hash = {
            let conn = st.db();
            auth::stored_hash(&conn, id)
        };
        hash.map(|h| auth::verify_against(&password, h.as_deref()))
    })
    .await;
    match verified {
        Ok(Ok(true)) => {
            state.security_limiter.clear(&user.username);
            None
        }
        Ok(Ok(false)) => Some(error(StatusCode::UNAUTHORIZED, "wrong password")),
        _ => Some(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    }
}

async fn overview(user: CurrentUser, State(state): State<AppState>) -> Response {
    let conn = state.db();
    let result =
        (|| -> rusqlite::Result<_> { Ok((list(&conn, user.id)?, totp_state(&conn, user.id)?)) })();
    let Ok((passkeys, (enabled, pending))) = result else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    Json(serde_json::json!({
        "passkeys": passkeys,
        "totp": { "enabled": enabled, "pending": pending },
        "webauthn_available": state.passkeys.available(),
    }))
    .into_response()
}

async fn passkey_challenge(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<PasswordReq>,
) -> Response {
    if !state.passkeys.available() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            &PasskeyError::Unavailable.to_string(),
        );
    }
    if let Some(refused) = password_refused(&state, &user, req.password).await {
        return refused;
    }
    let existing = {
        let conn = state.db();
        match (count(&conn, user.id), credentials(&conn, user.id)) {
            (Ok(held), _) if held as usize >= MAX_PASSKEYS => {
                return error(StatusCode::CONFLICT, &SaveError::TooMany.to_string())
            }
            (Ok(_), Ok(keys)) => keys,
            _ => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    };
    let now = jiff::Timestamp::now().as_second();
    match state.passkeys.start_registration(
        &user.session_token,
        user.id,
        &user.username,
        &existing,
        now,
    ) {
        Ok(challenge) => Json(challenge).into_response(),
        Err(e) => error(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string()),
    }
}

#[derive(Deserialize)]
struct NewPasskeyReq {
    name: String,
    credential: RegisterPublicKeyCredential,
}

async fn passkey_finish(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<NewPasskeyReq>,
) -> Response {
    let now = jiff::Timestamp::now();
    let key = match state.passkeys.finish_registration(
        &user.session_token,
        &req.credential,
        now.as_second(),
    ) {
        Ok(key) => key,
        Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string()),
    };
    let conn = state.db();
    match add(&conn, user.id, &req.name, &key) {
        Ok(info) => {
            let _ = crate::log::record(
                &conn,
                Some(user.id),
                "passkey_added",
                &format!("passkey {} {:?}", info.id, info.name),
            );
            Json(info).into_response()
        }
        Err(e @ SaveError::InvalidName) => error(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string()),
        Err(e @ (SaveError::TooMany | SaveError::Duplicate)) => {
            error(StatusCode::CONFLICT, &e.to_string())
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
struct RenameReq {
    name: String,
}

async fn passkey_rename(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<RenameReq>,
) -> Response {
    let conn = state.db();
    match rename(&conn, user.id, id, &req.name) {
        Ok(Some(info)) => Json(info).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "no such passkey"),
        Err(e @ SaveError::InvalidName) => error(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string()),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn passkey_delete(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<PasswordReq>,
) -> Response {
    if let Some(refused) = password_refused(&state, &user, req.password).await {
        return refused;
    }
    let conn = state.db();
    match remove(&conn, user.id, id) {
        Ok(Some(info)) => {
            let _ = crate::log::record(
                &conn,
                Some(user.id),
                "passkey_removed",
                &format!("passkey {} {:?}", info.id, info.name),
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(None) => error(StatusCode::NOT_FOUND, "no such passkey"),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn totp_start_route(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<PasswordReq>,
) -> Response {
    if let Some(refused) = password_refused(&state, &user, req.password).await {
        return refused;
    }
    let conn = state.db();
    match totp_start(&conn, user.id) {
        Ok(secret) => Json(serde_json::json!({
            "secret_base32": secret,
            "otpauth_uri": otpauth_uri(&secret, &user.username),
            "issuer": ISSUER,
            "account": user.username,
        }))
        .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
struct CodeReq {
    code: String,
}

async fn totp_confirm_route(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<CodeReq>,
) -> Response {
    let conn = state.db();
    match totp_confirm(&conn, user.id, &req.code, jiff::Timestamp::now()) {
        Ok(true) => {
            let _ = crate::log::record(&conn, Some(user.id), "totp_enrolled", &user.username);
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => error(StatusCode::UNAUTHORIZED, "that code doesn't match"),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn totp_delete(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<PasswordReq>,
) -> Response {
    if let Some(refused) = password_refused(&state, &user, req.password).await {
        return refused;
    }
    let conn = state.db();
    match totp_remove(&conn, user.id) {
        Ok(()) => {
            let _ = crate::log::record(&conn, Some(user.id), "totp_removed", &user.username);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_user() -> (Connection, i64) {
        let conn = crate::db::open_memory().unwrap();
        let id = auth::create_user(&conn, "aki", "pw", true).unwrap();
        (conn, id)
    }

    fn t0() -> jiff::Timestamp {
        "2026-09-16T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn a_domain_origin_builds_and_a_bare_ip_stays_off() {
        let (svc, warning) = PasskeyService::build("https://note.example.net", None, None);
        assert!(svc.available() && warning.is_none());

        let (svc, warning) = PasskeyService::build("http://100.100.20.30:3271", None, None);
        assert!(!svc.available(), "an IP origin has no relying party id");
        assert!(warning.is_some());

        let (svc, warning) = PasskeyService::build("http://localhost:5173", None, None);
        assert!(svc.available(), "localhost is a secure context");
        assert!(warning.is_none());

        // an overridden relying party covers a split deployment
        let (svc, warning) = PasskeyService::build(
            "http://10.0.0.2:3271",
            Some("note.example.net"),
            Some("https://note.example.net"),
        );
        assert!(svc.available() && warning.is_none());
    }

    #[test]
    fn totp_enrolment_promotes_only_on_a_matching_code() {
        let (conn, uid) = db_with_user();
        assert_eq!(totp_state(&conn, uid).unwrap(), (false, false));
        assert!(totp_seed(&conn, uid).unwrap().is_none());

        let secret = totp_start(&conn, uid).unwrap();
        assert_eq!(totp_state(&conn, uid).unwrap(), (false, true));
        let seed = crate::totp::parse_seed(&secret).unwrap();

        assert!(
            !totp_confirm(&conn, uid, "000000", t0()).unwrap()
                || crate::totp::code(&seed, crate::totp::step_at(t0())) == "000000"
        );
        assert_eq!(
            totp_state(&conn, uid).unwrap(),
            (false, true),
            "a wrong code leaves the enrolment pending"
        );

        let code = crate::totp::code(&seed, crate::totp::step_at(t0()));
        assert!(totp_confirm(&conn, uid, &code, t0()).unwrap());
        assert_eq!(totp_state(&conn, uid).unwrap(), (true, false));
        assert_eq!(totp_seed(&conn, uid).unwrap().unwrap(), seed);
        assert!(
            !totp_confirm(&conn, uid, &code, t0()).unwrap(),
            "the code that enrolled cannot be replayed"
        );

        totp_remove(&conn, uid).unwrap();
        assert_eq!(totp_state(&conn, uid).unwrap(), (false, false));
        assert!(totp_seed(&conn, uid).unwrap().is_none());
    }

    #[test]
    fn starting_again_replaces_an_abandoned_enrolment() {
        let (conn, uid) = db_with_user();
        let first = totp_start(&conn, uid).unwrap();
        let second = totp_start(&conn, uid).unwrap();
        assert_ne!(first, second);
        let stale = crate::totp::code(
            &crate::totp::parse_seed(&first).unwrap(),
            crate::totp::step_at(t0()),
        );
        assert!(!totp_confirm(&conn, uid, &stale, t0()).unwrap());
        let fresh = crate::totp::code(
            &crate::totp::parse_seed(&second).unwrap(),
            crate::totp::step_at(t0()),
        );
        assert!(totp_confirm(&conn, uid, &fresh, t0()).unwrap());
    }

    #[test]
    fn otpauth_uri_carries_the_issuer_account_and_secret() {
        let uri = otpauth_uri("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", "aki");
        assert!(uri.starts_with("otpauth://totp/Note:aki?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"));
        assert!(uri.contains("issuer=Note") && uri.contains("algorithm=SHA1"));
        assert!(uri.contains("digits=6") && uri.contains("period=30"));
    }

    #[test]
    fn a_used_step_is_claimed_once() {
        let (conn, uid) = db_with_user();
        assert!(claim_step(&conn, uid, 100).unwrap());
        assert!(!claim_step(&conn, uid, 100).unwrap());
        assert!(!claim_step(&conn, uid, 99).unwrap());
        assert!(claim_step(&conn, uid, 101).unwrap());
    }

    #[test]
    fn names_are_trimmed_bounded_and_the_count_is_capped() {
        let (conn, uid) = db_with_user();
        assert!(matches!(clean_name("   "), Err(SaveError::InvalidName)));
        assert!(matches!(
            clean_name(&"a".repeat(MAX_NAME_LEN + 1)),
            Err(SaveError::InvalidName)
        ));
        assert_eq!(clean_name("  phone  ").unwrap(), "phone");

        for i in 0..MAX_PASSKEYS {
            conn.execute(
                "INSERT INTO passkeys (user_id, name, credential, cred_id, created_at)
                 VALUES (?1, ?2, '{}', ?3, 'now')",
                (uid, format!("k{i}"), vec![i as u8]),
            )
            .unwrap();
        }
        assert_eq!(list(&conn, uid).unwrap().len(), MAX_PASSKEYS);
        assert_eq!(user_factors(&conn, uid).unwrap(), (true, false));
    }

    #[test]
    fn rename_and_remove_are_scoped_to_the_owner() {
        let (conn, uid) = db_with_user();
        let other = auth::create_user(&conn, "bo", "pw", false).unwrap();
        conn.execute(
            "INSERT INTO passkeys (user_id, name, credential, cred_id, created_at)
             VALUES (?1, 'phone', '{}', X'01', 'now')",
            [uid],
        )
        .unwrap();
        let id = conn.last_insert_rowid();

        assert!(rename(&conn, other, id, "theirs").unwrap().is_none());
        assert!(matches!(
            rename(&conn, uid, id, " "),
            Err(SaveError::InvalidName)
        ));
        assert_eq!(
            rename(&conn, uid, id, " laptop ").unwrap().unwrap().name,
            "laptop"
        );
        assert!(remove(&conn, other, id).unwrap().is_none());
        assert_eq!(remove(&conn, uid, id).unwrap().unwrap().name, "laptop");
        assert!(list(&conn, uid).unwrap().is_empty());
        assert_eq!(user_factors(&conn, uid).unwrap(), (false, false));
    }
}
