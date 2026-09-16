# Per-user API tokens Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user mint bearer tokens that give scripts and other agents read, create, update, and delete access to that user's tasks, and nothing else.

**Architecture:** A new `tokens` module owns token generation, hashing, storage, and lookup. A new `TaskPrincipal` extractor in `auth.rs` is the only code that reads `Authorization`, and only the task handlers use it, so tokens cannot reach other routes. Token management routes stay behind the session cookie. A hard `DELETE /api/tasks/{id}` completes the CRUD set.

**Tech Stack:** Rust, axum 0.8, rusqlite, sha2, base64; React 19 + TypeScript + Vite for the web client.

**Spec:** `docs/superpowers/specs/2026-09-15-api-tokens-design.md`

## Global Constraints

- Token plaintext is `note_` + base64url (no padding) of 32 random bytes; only the SHA-256 hex digest is stored.
- Token name: trimmed, 1 to 64 characters, else 422. At most 20 tokens per user, else 409.
- `last_used_at` is written only when absent or older than one minute.
- A bearer header present on a task route must resolve or the request is 401; the cookie is not consulted on that path.
- `CurrentUser` must not change. Non-task routes must reject bearer tokens with 401.
- Writes over a token use `Actor::User`.
- Follow `CLAUDE.md`: no process-history comments, comment only what code cannot say itself.
- Every commit message ends with:
  ```
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_016vuHqrhcoBdoGnqUxjd3rQ
  ```
- Run Rust checks from the repo root: `cargo test -p note-server`. Run web checks from `web/`: `pnpm build` (type check plus bundle). The web tsconfig has `strict` and `noUnusedLocals` on.

---

### Task 1: Schema migration v11 and the sha2 dependency

**Files:**
- Modify: `server/Cargo.toml` (dependencies block)
- Modify: `server/src/db.rs:157-171` (append a v11 entry after v10)
- Test: `server/src/db.rs` tests module

**Interfaces:**
- Produces: table `api_tokens(id, user_id, name, token_hash UNIQUE, created_at, last_used_at NULL)` and index `idx_api_tokens_user`.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module at the bottom of `server/src/db.rs`:

```rust
    #[test]
    fn v11_creates_api_tokens_with_a_unique_hash() {
        let conn = open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO api_tokens (user_id, name, token_hash, created_at)
             VALUES (1, 'cli', 'abc', 'now')",
            [],
        )
        .unwrap();
        assert!(conn
            .execute(
                "INSERT INTO api_tokens (user_id, name, token_hash, created_at)
                 VALUES (1, 'other', 'abc', 'now')",
                [],
            )
            .is_err());
        let last: Option<String> = conn
            .query_row("SELECT last_used_at FROM api_tokens WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert!(last.is_none());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p note-server --lib db::tests::v11_creates_api_tokens_with_a_unique_hash`
Expected: FAIL with `no such table: api_tokens`

- [ ] **Step 3: Add the migration**

In `server/src/db.rs`, after the v10 entry's closing `",` and before the `];`, add:

```rust
    // v11
    "
    CREATE TABLE api_tokens (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        name TEXT NOT NULL,
        token_hash TEXT NOT NULL UNIQUE,
        created_at TEXT NOT NULL,
        last_used_at TEXT
    );
    CREATE INDEX idx_api_tokens_user ON api_tokens(user_id, id);
    ",
```

- [ ] **Step 4: Add the sha2 dependency**

In `server/Cargo.toml` under `[dependencies]`, after `sha1 = "0.10"`, add:

```toml
sha2 = "0.10"
```

`sha2 0.10.9` is already in `Cargo.lock` as a transitive dependency, so no new download is needed.

- [ ] **Step 5: Run the db tests**

Run: `cargo test -p note-server --lib db::`
Expected: all PASS, including `migrations_apply_and_are_idempotent` (it asserts `user_version == MIGRATIONS.len()`).

- [ ] **Step 6: Commit**

```bash
git add server/Cargo.toml Cargo.lock server/src/db.rs
git commit -m "feat(db): api_tokens table (v11)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016vuHqrhcoBdoGnqUxjd3rQ"
```

---

### Task 2: `tokens` module — generate, store, list, revoke, resolve

**Files:**
- Create: `server/src/tokens.rs`
- Modify: `server/src/lib.rs:1-21` (add `pub mod tokens;` after `pub mod tasks;`)
- Test: unit tests inside `server/src/tokens.rs`

**Interfaces:**
- Produces:
  - `pub const PREFIX: &str = "note_"`, `pub const MAX_PER_USER: usize = 20`, `pub const MAX_NAME_LEN: usize = 64`
  - `pub struct TokenInfo { id: i64, name: String, created_at: String, last_used_at: Option<String> }` (Serialize)
  - `pub struct Created { #[serde(flatten)] info: TokenInfo, token: String }` (Serialize)
  - `pub enum CreateError { InvalidName, TooMany, Db(rusqlite::Error) }` (Display via thiserror)
  - `pub struct Resolved { token_id: i64, user_id: i64, username: String }`
  - `pub fn generate_secret() -> String`, `pub fn hash_secret(secret: &str) -> String`
  - `pub fn create(conn: &Connection, user_id: i64, name: &str) -> Result<Created, CreateError>`
  - `pub fn list(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<TokenInfo>>`
  - `pub fn revoke(conn: &Connection, user_id: i64, id: i64) -> rusqlite::Result<Option<TokenInfo>>`
  - `pub fn resolve(conn: &Connection, secret: &str, now: jiff::Timestamp) -> rusqlite::Result<Option<Resolved>>`

- [ ] **Step 1: Register the module**

In `server/src/lib.rs`, after `pub mod tasks;`, add:

```rust
pub mod tokens;
```

- [ ] **Step 2: Write the failing tests**

Create `server/src/tokens.rs` containing only the tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_user() -> (Connection, i64) {
        let conn = crate::db::open_memory().unwrap();
        let id = crate::auth::create_user(&conn, "aki", "pw", false).unwrap();
        (conn, id)
    }

    fn t0() -> jiff::Timestamp {
        "2026-09-15T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn secrets_carry_the_prefix_and_differ() {
        let a = generate_secret();
        let b = generate_secret();
        assert!(a.starts_with(PREFIX));
        assert_eq!(a.len(), PREFIX.len() + 43);
        assert_ne!(a, b);
    }

    #[test]
    fn create_stores_only_the_hash_and_resolves_the_plaintext() {
        let (conn, uid) = db_with_user();
        let made = create(&conn, uid, "  cli  ").unwrap();
        assert_eq!(made.info.name, "cli");
        assert!(made.token.starts_with(PREFIX));
        let stored: String = conn
            .query_row("SELECT token_hash FROM api_tokens WHERE id = ?1", [made.info.id], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, hash_secret(&made.token));
        assert_ne!(stored, made.token);

        let r = resolve(&conn, &made.token, t0()).unwrap().unwrap();
        assert_eq!(r.user_id, uid);
        assert_eq!(r.token_id, made.info.id);
        assert_eq!(r.username, "aki");
        assert!(resolve(&conn, "note_nope", t0()).unwrap().is_none());
    }

    #[test]
    fn name_is_validated_and_count_is_capped() {
        let (conn, uid) = db_with_user();
        assert!(matches!(create(&conn, uid, "   "), Err(CreateError::InvalidName)));
        assert!(matches!(
            create(&conn, uid, &"a".repeat(MAX_NAME_LEN + 1)),
            Err(CreateError::InvalidName)
        ));
        assert!(create(&conn, uid, &"あ".repeat(MAX_NAME_LEN)).is_ok());
        for i in 1..MAX_PER_USER {
            create(&conn, uid, &format!("t{i}")).unwrap();
        }
        assert!(matches!(create(&conn, uid, "one more"), Err(CreateError::TooMany)));
    }

    #[test]
    fn list_and_revoke_are_scoped_to_the_owner() {
        let (conn, uid) = db_with_user();
        let other = crate::auth::create_user(&conn, "bo", "pw", false).unwrap();
        let mine = create(&conn, uid, "mine").unwrap();
        let theirs = create(&conn, other, "theirs").unwrap();

        let names: Vec<String> = list(&conn, uid).unwrap().into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["mine"]);

        assert!(revoke(&conn, uid, theirs.info.id).unwrap().is_none());
        assert!(resolve(&conn, &theirs.token, t0()).unwrap().is_some());

        let gone = revoke(&conn, uid, mine.info.id).unwrap().unwrap();
        assert_eq!(gone.name, "mine");
        assert!(resolve(&conn, &mine.token, t0()).unwrap().is_none());
        assert!(list(&conn, uid).unwrap().is_empty());
    }

    #[test]
    fn disabled_users_tokens_do_not_resolve() {
        let (conn, uid) = db_with_user();
        let made = create(&conn, uid, "cli").unwrap();
        conn.execute("UPDATE users SET disabled = 1 WHERE id = ?1", [uid]).unwrap();
        assert!(resolve(&conn, &made.token, t0()).unwrap().is_none());
    }

    #[test]
    fn last_used_is_written_at_most_once_a_minute() {
        let (conn, uid) = db_with_user();
        let made = create(&conn, uid, "cli").unwrap();
        let last = |conn: &Connection| -> Option<String> {
            conn.query_row("SELECT last_used_at FROM api_tokens WHERE id = ?1", [made.info.id], |r| r.get(0))
                .unwrap()
        };
        assert!(last(&conn).is_none());
        resolve(&conn, &made.token, t0()).unwrap();
        assert_eq!(last(&conn).unwrap(), t0().to_string());
        let soon = t0() + jiff::Span::new().seconds(30);
        resolve(&conn, &made.token, soon).unwrap();
        assert_eq!(last(&conn).unwrap(), t0().to_string());
        let later = t0() + jiff::Span::new().seconds(TOUCH_INTERVAL_SECS + 1);
        resolve(&conn, &made.token, later).unwrap();
        assert_eq!(last(&conn).unwrap(), later.to_string());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p note-server --lib tokens::`
Expected: compile error, `generate_secret`, `create`, etc. not found.

- [ ] **Step 4: Implement the module**

Put this above the `#[cfg(test)]` block in `server/src/tokens.rs`:

```rust
use argon2::password_hash::rand_core::{OsRng, RngCore};
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const PREFIX: &str = "note_";
pub const MAX_PER_USER: usize = 20;
pub const MAX_NAME_LEN: usize = 64;
const TOUCH_INTERVAL_SECS: i64 = 60;

#[derive(Debug, Serialize)]
pub struct TokenInfo {
    pub id: i64,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

/// The only response that ever carries the plaintext secret.
#[derive(Debug, Serialize)]
pub struct Created {
    #[serde(flatten)]
    pub info: TokenInfo,
    pub token: String,
}

#[derive(Debug, Error)]
pub enum CreateError {
    #[error("name must be 1 to {MAX_NAME_LEN} characters")]
    InvalidName,
    #[error("at most {MAX_PER_USER} tokens per user")]
    TooMany,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub token_id: i64,
    pub user_id: i64,
    pub username: String,
}

pub fn generate_secret() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!(
        "{PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

/// Secrets are 256 random bits, so an unsalted digest is a safe lookup key.
pub fn hash_secret(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret.as_bytes()))
}

const COLS: &str = "id, name, created_at, last_used_at";

fn row_to_info(r: &rusqlite::Row) -> rusqlite::Result<TokenInfo> {
    Ok(TokenInfo {
        id: r.get(0)?,
        name: r.get(1)?,
        created_at: r.get(2)?,
        last_used_at: r.get(3)?,
    })
}

pub fn create(conn: &Connection, user_id: i64, name: &str) -> Result<Created, CreateError> {
    let name = name.trim();
    let len = name.chars().count();
    if len == 0 || len > MAX_NAME_LEN {
        return Err(CreateError::InvalidName);
    }
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM api_tokens WHERE user_id = ?1",
        [user_id],
        |r| r.get(0),
    )?;
    if count as usize >= MAX_PER_USER {
        return Err(CreateError::TooMany);
    }
    let token = generate_secret();
    let created_at = jiff::Timestamp::now().to_string();
    conn.execute(
        "INSERT INTO api_tokens (user_id, name, token_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
        (user_id, name, hash_secret(&token), &created_at),
    )?;
    let info = TokenInfo {
        id: conn.last_insert_rowid(),
        name: name.to_string(),
        created_at,
        last_used_at: None,
    };
    Ok(Created { info, token })
}

pub fn list(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<TokenInfo>> {
    let mut stmt =
        conn.prepare(&format!("SELECT {COLS} FROM api_tokens WHERE user_id = ?1 ORDER BY id"))?;
    let rows = stmt.query_map([user_id], row_to_info)?;
    rows.collect()
}

/// Returns the removed token, or `None` when the id is not this user's.
pub fn revoke(conn: &Connection, user_id: i64, id: i64) -> rusqlite::Result<Option<TokenInfo>> {
    let info = conn
        .query_row(
            &format!("SELECT {COLS} FROM api_tokens WHERE id = ?1 AND user_id = ?2"),
            (id, user_id),
            row_to_info,
        )
        .optional()?;
    if info.is_some() {
        conn.execute("DELETE FROM api_tokens WHERE id = ?1", [id])?;
    }
    Ok(info)
}

/// Looks the presented secret up by digest. A disabled owner resolves to
/// `None` exactly like an unknown token. Stamps `last_used_at` when it is
/// unset or older than `TOUCH_INTERVAL_SECS`, so a busy client does not write
/// on every request.
pub fn resolve(
    conn: &Connection,
    secret: &str,
    now: jiff::Timestamp,
) -> rusqlite::Result<Option<Resolved>> {
    let row: Option<(i64, i64, String, bool, Option<String>)> = conn
        .query_row(
            "SELECT t.id, t.user_id, u.username, u.disabled, t.last_used_at
             FROM api_tokens t JOIN users u ON u.id = t.user_id
             WHERE t.token_hash = ?1",
            [hash_secret(secret)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((token_id, user_id, username, disabled, last_used_at)) = row else {
        return Ok(None);
    };
    if disabled {
        return Ok(None);
    }
    let stale = match last_used_at.and_then(|s| s.parse::<jiff::Timestamp>().ok()) {
        Some(last) => (now.as_second() - last.as_second()) > TOUCH_INTERVAL_SECS,
        None => true,
    };
    if stale {
        conn.execute(
            "UPDATE api_tokens SET last_used_at = ?1 WHERE id = ?2",
            (now.to_string(), token_id),
        )?;
    }
    Ok(Some(Resolved { token_id, user_id, username }))
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p note-server --lib tokens::`
Expected: 6 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add server/src/lib.rs server/src/tokens.rs
git commit -m "feat: api token generation, storage and lookup

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016vuHqrhcoBdoGnqUxjd3rQ"
```

---

### Task 3: Token management routes

**Files:**
- Modify: `server/src/api.rs` (routes near line 17; handlers after `task_error`, around line 210)
- Create: `server/tests/tokens_api.rs`

**Interfaces:**
- Consumes: `tokens::{create, list, revoke, CreateError}` from Task 2, `log::record` (existing, `server/src/log.rs:6`).
- Produces: `GET /api/tokens`, `POST /api/tokens {name}`, `DELETE /api/tokens/{id}`; test helpers `read`, `with_cookie`, `with_bearer`, `mint`, `minted` in `tokens_api.rs` that Tasks 4 and 5 reuse.

- [ ] **Step 1: Write the failing tests**

Create `server/tests/tokens_api.rs`:

```rust
mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::auth;
use tower::ServiceExt;

async fn read(res: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

async fn with_cookie(
    app: &axum::Router,
    cookie: &str,
    method: Method,
    path: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(path).header(header::COOKIE, cookie);
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.unwrap_or("").to_string())).unwrap())
        .await
        .unwrap();
    read(res).await
}

async fn with_bearer(
    app: &axum::Router,
    token: &str,
    method: Method,
    path: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.unwrap_or("").to_string())).unwrap())
        .await
        .unwrap();
    read(res).await
}

async fn mint(app: &axum::Router, cookie: &str, name: &str) -> (StatusCode, serde_json::Value) {
    with_cookie(app, cookie, Method::POST, "/api/tokens", Some(&format!(r#"{{"name":"{name}"}}"#)))
        .await
}

async fn minted(app: &axum::Router, cookie: &str, name: &str) -> (i64, String) {
    let (status, v) = mint(app, cookie, name).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    (v["id"].as_i64().unwrap(), v["token"].as_str().unwrap().to_string())
}

#[tokio::test]
async fn mint_list_revoke_round_trip() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let (status, made) = mint(&app, &cookie, "laptop").await;
    assert_eq!(status, StatusCode::OK, "{made}");
    assert_eq!(made["name"], "laptop");
    assert!(made["token"].as_str().unwrap().starts_with("note_"));
    assert!(made["last_used_at"].is_null());
    let id = made["id"].as_i64().unwrap();

    let (status, list) = with_cookie(&app, &cookie, Method::GET, "/api/tokens", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], id);
    assert!(list[0].get("token").is_none());

    let (status, _) =
        with_cookie(&app, &cookie, Method::DELETE, &format!("/api/tokens/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) =
        with_cookie(&app, &cookie, Method::DELETE, &format!("/api/tokens/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, list) = with_cookie(&app, &cookie, Method::GET, "/api/tokens", None).await;
    assert!(list.as_array().unwrap().is_empty());

    let kinds: Vec<String> = {
        let conn = state.db.lock().unwrap();
        let mut stmt = conn.prepare("SELECT kind FROM event_log ORDER BY id").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
    };
    assert!(kinds.contains(&"token_created".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"token_revoked".to_string()), "{kinds:?}");
}

#[tokio::test]
async fn name_and_cap_are_enforced() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (status, v) = mint(&app, &cookie, "   ").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(v["error"].is_string());
    let (status, _) = mint(&app, &cookie, &"x".repeat(65)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    for i in 0..20 {
        let (status, _) = mint(&app, &cookie, &format!("t{i}")).await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, v) = mint(&app, &cookie, "one more").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(v["error"].is_string());
}

#[tokio::test]
async fn another_users_token_id_is_404() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    {
        let conn = state.db.lock().unwrap();
        auth::create_user(&conn, "bo", "pw", false).unwrap();
    }
    let bo = common::login(&app, "bo", "pw").await;
    let (id, _) = minted(&app, &bo, "bos").await;
    let (status, _) =
        with_cookie(&app, &cookie, Method::DELETE, &format!("/api/tokens/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, list) = with_cookie(&app, &bo, Method::GET, "/api/tokens", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn token_routes_need_the_cookie_not_a_bearer() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (_, token) = minted(&app, &cookie, "cli").await;
    let (status, _) = with_bearer(&app, &token, Method::GET, "/api/tokens", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p note-server --test tokens_api`
Expected: FAIL with status 404 on `/api/tokens`.

- [ ] **Step 3: Add routes and handlers**

In `server/src/api.rs` routes, after the `/api/tasks/{id}/flatten` line add:

```rust
        .route("/api/tokens", get(tokens_list).post(tokens_create))
        .route("/api/tokens/{id}", axum::routing::delete(tokens_revoke))
```

After `fn task_error`, add:

```rust
#[derive(Deserialize)]
struct NewTokenReq {
    name: String,
}

async fn tokens_list(user: CurrentUser, State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tokens::list(&conn, user.id) {
        Ok(ts) => Json(ts).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn tokens_create(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<NewTokenReq>,
) -> impl IntoResponse {
    use crate::tokens::CreateError as E;
    let conn = state.db.lock().unwrap();
    match crate::tokens::create(&conn, user.id, &req.name) {
        Ok(made) => {
            let _ = crate::log::record(
                &conn,
                Some(user.id),
                "token_created",
                &format!("token {} {:?}", made.info.id, made.info.name),
            );
            Json(made).into_response()
        }
        Err(e @ E::InvalidName) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
        Err(e @ E::TooMany) => {
            (StatusCode::CONFLICT, Json(serde_json::json!({ "error": e.to_string() })))
                .into_response()
        }
        Err(E::Db(_)) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn tokens_revoke(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tokens::revoke(&conn, user.id, id) {
        Ok(Some(t)) => {
            let _ = crate::log::record(
                &conn,
                Some(user.id),
                "token_revoked",
                &format!("token {} {:?}", t.id, t.name),
            );
            StatusCode::NO_CONTENT
        }
        Ok(None) => StatusCode::NOT_FOUND,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
```

- [ ] **Step 4: Run the suite**

Run: `cargo test -p note-server --test tokens_api`
Expected: 4 tests PASS. `token_routes_need_the_cookie_not_a_bearer` passes because `CurrentUser` ignores the header and finds no cookie.

- [ ] **Step 5: Commit**

```bash
git add server/src/api.rs server/tests/tokens_api.rs
git commit -m "feat: mint, list and revoke API tokens over the session

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016vuHqrhcoBdoGnqUxjd3rQ"
```

---

### Task 4: `TaskPrincipal` extractor and bearer access to the task routes

**Files:**
- Modify: `server/src/auth.rs` (add `Credential` and `TaskPrincipal` after the `impl FromRequestParts<AppState> for CurrentUser` block, before `#[cfg(test)]`)
- Modify: `server/src/api.rs:1` (import), `server/src/api.rs:128-195` (five task handlers)
- Modify: `server/tests/tokens_api.rs` (append tests)

**Interfaces:**
- Consumes: `crate::tokens::resolve(conn, secret, now)` from Task 2; helpers from Task 3's test file.
- Produces:
  - `pub enum Credential { Session, Token(i64) }`
  - `pub struct TaskPrincipal { pub id: i64, pub username: String, pub via: Credential }` implementing `FromRequestParts<AppState>` with `Rejection = StatusCode`.

- [ ] **Step 1: Append the failing tests**

Append to `server/tests/tokens_api.rs`:

```rust
#[tokio::test]
async fn bearer_token_reaches_every_task_route() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (_, token) = minted(&app, &cookie, "cli").await;

    let (status, t) =
        with_bearer(&app, &token, Method::POST, "/api/tasks", Some(r#"{"title":"via token"}"#)).await;
    assert_eq!(status, StatusCode::OK, "{t}");
    let id = t["id"].as_i64().unwrap();

    let (status, list) = with_bearer(&app, &token, Method::GET, "/api/tasks", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["title"], "via token");

    let (status, t) = with_bearer(
        &app,
        &token,
        Method::PATCH,
        &format!("/api/tasks/{id}"),
        Some(r#"{"duration_min":20}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{t}");
    assert_eq!(t["duration_source"], "user");

    let (status, _) = with_bearer(
        &app,
        &token,
        Method::POST,
        &format!("/api/tasks/{id}/split"),
        Some(r#"{"steps":[{"title":"a"},{"title":"b"}]}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) =
        with_bearer(&app, &token, Method::POST, &format!("/api/tasks/{id}/flatten"), None).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = with_cookie(&app, &cookie, Method::GET, "/api/tasks", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn bearer_token_is_refused_outside_tasks() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (_, token) = minted(&app, &cookie, "cli").await;
    for path in ["/api/me", "/api/settings", "/api/plan/today", "/api/conversations"] {
        let (status, _) = with_bearer(&app, &token, Method::GET, path, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
    }
}

#[tokio::test]
async fn bad_bearer_is_401_even_with_a_valid_cookie() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    for auth_value in ["Bearer note_not_a_real_token", "Basic abc", "Bearer "] {
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/tasks")
                    .header(header::COOKIE, cookie.as_str())
                    .header(header::AUTHORIZATION, auth_value)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{auth_value:?}");
    }
}

#[tokio::test]
async fn disabled_user_token_stops_resolving() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let (_, token) = minted(&app, &cookie, "cli").await;
    state
        .db
        .lock()
        .unwrap()
        .execute("UPDATE users SET disabled = 1 WHERE username = 'aki'", [])
        .unwrap();
    let (status, _) = with_bearer(&app, &token, Method::GET, "/api/tasks", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoked_token_is_401_on_next_use() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (id, token) = minted(&app, &cookie, "cli").await;
    let (status, _) = with_bearer(&app, &token, Method::GET, "/api/tasks", None).await;
    assert_eq!(status, StatusCode::OK);
    with_cookie(&app, &cookie, Method::DELETE, &format!("/api/tokens/{id}"), None).await;
    let (status, _) = with_bearer(&app, &token, Method::GET, "/api/tasks", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn token_writes_are_user_actor() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (_, token) = minted(&app, &cookie, "cli").await;
    let (_, t) = with_bearer(
        &app,
        &token,
        Method::POST,
        "/api/tasks",
        Some(r#"{"title":"timed","duration_min":15}"#),
    )
    .await;
    assert_eq!(t["duration_source"], "user");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p note-server --test tokens_api`
Expected: the six new tests FAIL with 401 on the task routes (no bearer support yet). `bad_bearer_is_401_even_with_a_valid_cookie` and `bearer_token_is_refused_outside_tasks` pass already; that is expected.

- [ ] **Step 3: Add the extractor**

In `server/src/auth.rs`, after the `impl FromRequestParts<AppState> for CurrentUser { ... }` block and before `#[cfg(test)]`, add:

```rust
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
            let conn = state.db.lock().unwrap();
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
```

- [ ] **Step 4: Switch the task handlers**

In `server/src/api.rs` change line 1 to:

```rust
use crate::auth::{self, CurrentUser, TaskPrincipal};
```

Then in each of `tasks_list`, `tasks_create`, `tasks_update`, `task_split`, and `task_flatten`, replace the parameter `user: CurrentUser` with `user: TaskPrincipal`. The bodies do not change; they only use `user.id`.

- [ ] **Step 5: Run the token suite, then the neighbours**

Run: `cargo test -p note-server --test tokens_api`
Expected: 10 tests PASS.

Run: `cargo test -p note-server --lib && cargo test -p note-server --test tasks_api --test auth`
Expected: all PASS. The task routes still accept the cookie because `TaskPrincipal` falls back to `CurrentUser`.

- [ ] **Step 6: Commit**

```bash
git add server/src/auth.rs server/src/api.rs server/tests/tokens_api.rs
git commit -m "feat: TaskPrincipal extractor; task routes accept bearer tokens

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016vuHqrhcoBdoGnqUxjd3rQ"
```

---

### Task 5: Hard delete for tasks

**Files:**
- Modify: `server/src/tasks.rs` (add `delete` after `flatten`, before the `Updated` struct; add a `tests` module at the end of the file)
- Modify: `server/src/api.rs:18` (route) and add a `tasks_delete` handler after `task_flatten`
- Modify: `server/tests/tasks_api.rs` (append a test)
- Modify: `server/tests/tokens_api.rs` (append a test)

**Interfaces:**
- Consumes: `TaskPrincipal` from Task 4; `with_bearer`/`minted` helpers from Task 3.
- Produces: `pub fn delete(conn: &Connection, user_id: i64, task_id: i64) -> rusqlite::Result<bool>`; route `DELETE /api/tasks/{id}` → 204 or 404.

- [ ] **Step 1: Write the failing unit tests**

`server/src/tasks.rs` has no tests module yet. Append at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_user() -> (Connection, i64) {
        let conn = crate::db::open_memory().unwrap();
        let id = crate::auth::create_user(&conn, "aki", "pw", false).unwrap();
        (conn, id)
    }

    fn task(conn: &Connection, uid: i64, title: &str, parent: Option<i64>) -> i64 {
        create(
            conn,
            uid,
            NewTask { title: title.into(), parent_id: parent, ..NewTask::default() },
            "manual",
            Actor::User,
        )
        .unwrap()
        .id
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn delete_removes_a_leaf() {
        let (conn, uid) = db_with_user();
        let id = task(&conn, uid, "solo", None);
        assert!(delete(&conn, uid, id).unwrap());
        assert!(get(&conn, uid, id).unwrap().is_none());
        assert!(!delete(&conn, uid, id).unwrap());
    }

    #[test]
    fn delete_removes_a_parent_with_its_steps_and_event_links() {
        let (conn, uid) = db_with_user();
        let parent = task(&conn, uid, "parent", None);
        let step = task(&conn, uid, "step", Some(parent));
        let other = task(&conn, uid, "other", None);
        conn.execute(
            "INSERT INTO plans (user_id, date, created_at) VALUES (?1, '2026-09-15', 'x')",
            [uid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time) VALUES (1, 'checkin', '09:00')",
            [],
        )
        .unwrap();
        for t in [parent, step, other] {
            conn.execute("INSERT INTO event_tasks (event_id, task_id) VALUES (1, ?1)", [t])
                .unwrap();
        }
        assert!(delete(&conn, uid, parent).unwrap());
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM tasks"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM event_tasks"), 1);
        assert!(get(&conn, uid, other).unwrap().is_some());
    }

    #[test]
    fn delete_ignores_another_users_task() {
        let (conn, uid) = db_with_user();
        let bo = crate::auth::create_user(&conn, "bo", "pw", false).unwrap();
        let theirs = task(&conn, bo, "theirs", None);
        assert!(!delete(&conn, uid, theirs).unwrap());
        assert!(get(&conn, bo, theirs).unwrap().is_some());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p note-server --lib tasks::tests`
Expected: compile error, `delete` not found.

- [ ] **Step 3: Implement `delete`**

In `server/src/tasks.rs`, after `flatten` and before the `Updated` struct, add:

```rust
/// Removes the task, its steps, and every event link to them. `false` when the
/// task is not this user's.
pub fn delete(conn: &Connection, user_id: i64, task_id: i64) -> rusqlite::Result<bool> {
    let tx = conn.unchecked_transaction()?;
    if get(&tx, user_id, task_id)?.is_none() {
        return Ok(false);
    }
    tx.execute(
        "DELETE FROM event_tasks
         WHERE task_id = ?1 OR task_id IN (SELECT id FROM tasks WHERE parent_id = ?1)",
        [task_id],
    )?;
    tx.execute("DELETE FROM tasks WHERE parent_id = ?1", [task_id])?;
    tx.execute("DELETE FROM tasks WHERE id = ?1", [task_id])?;
    tx.commit()?;
    Ok(true)
}
```

- [ ] **Step 4: Run unit tests to verify they pass**

Run: `cargo test -p note-server --lib tasks::tests`
Expected: 3 PASS.

- [ ] **Step 5: Write the failing route tests**

Append to `server/tests/tasks_api.rs`:

```rust
#[tokio::test]
async fn delete_removes_task_and_steps_and_404s_after() {
    let (app, cookie, _tmp) = app_with_user().await;
    let (_, parent) = post(&app, &cookie, "/api/tasks", r#"{"title":"parent"}"#).await;
    let pid = parent["id"].as_i64().unwrap();
    post(&app, &cookie, "/api/tasks", &format!(r#"{{"title":"step","parent_id":{pid}}}"#)).await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"other"}"#).await;

    let res = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/tasks/{pid}"))
                .header(header::COOKIE, cookie.as_str())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let all = list(&app, &cookie).await;
    assert_eq!(all.as_array().unwrap().len(), 1);
    assert_eq!(all[0]["title"], "other");

    let res = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/tasks/{pid}"))
                .header(header::COOKIE, cookie.as_str())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
```

Append to `server/tests/tokens_api.rs`:

```rust
#[tokio::test]
async fn bearer_token_can_delete_a_task() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let (_, token) = minted(&app, &cookie, "cli").await;
    let (_, t) =
        with_bearer(&app, &token, Method::POST, "/api/tasks", Some(r#"{"title":"gone"}"#)).await;
    let id = t["id"].as_i64().unwrap();
    let (status, _) =
        with_bearer(&app, &token, Method::DELETE, &format!("/api/tasks/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) =
        with_bearer(&app, &token, Method::DELETE, &format!("/api/tasks/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
```

- [ ] **Step 6: Run to verify they fail**

Run: `cargo test -p note-server --test tasks_api delete_removes && cargo test -p note-server --test tokens_api bearer_token_can_delete`
Expected: FAIL with status 405 (no DELETE method on the route).

- [ ] **Step 7: Add the route and handler**

In `server/src/api.rs`, change the route line:

```rust
        .route("/api/tasks/{id}", patch(tasks_update).delete(tasks_delete))
```

After `task_flatten`, add:

```rust
async fn tasks_delete(
    user: TaskPrincipal,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tasks::delete(&conn, user.id, id) {
        Ok(true) => StatusCode::NO_CONTENT,
        Ok(false) => StatusCode::NOT_FOUND,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
```

- [ ] **Step 8: Run the suites**

Run: `cargo test -p note-server --lib tasks:: && cargo test -p note-server --test tasks_api --test tokens_api`
Expected: all PASS.

- [ ] **Step 9: Commit**

```bash
git add server/src/tasks.rs server/src/api.rs server/tests/tasks_api.rs server/tests/tokens_api.rs
git commit -m "feat: DELETE /api/tasks/{id} hard-deletes a task and its steps

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016vuHqrhcoBdoGnqUxjd3rQ"
```

---

### Task 6: Web client — API tokens group in Settings

**Files:**
- Modify: `web/src/types.ts` (after the `TaskNode` type)
- Modify: `web/src/api.ts` (type imports at top; `api` object, after the `flatten` entry near line 129)
- Modify: `web/src/views/Settings.tsx` (type imports at lines 9-15; new group between the NOTE group and the ADMIN group, around line 455; new `TokensSection` component after `PersonaSection`)
- Modify: `web/src/styles.css` (after line 899, the `.btn-haze.small` rule)

**Interfaces:**
- Consumes: `GET/POST /api/tokens`, `DELETE /api/tokens/{id}` from Task 3.
- Produces: `Token`, `TokenCreated` types; `api.tokens()`, `api.createToken(name)`, `api.revokeToken(id)`.

The web project has no unit test runner; the check is `pnpm build` (runs `tsc` then Vite).

- [ ] **Step 1: Add the types**

In `web/src/types.ts`, after the `TaskNode` type, add:

```ts
export type Token = {
  id: number
  name: string
  created_at: string
  last_used_at: string | null
}

// The secret is present only in the create response.
export type TokenCreated = Token & { token: string }
```

- [ ] **Step 2: Add the API calls**

In `web/src/api.ts`, add `Token` and `TokenCreated` to the `import type { ... } from './types'` list (alphabetical, after `TaskUpdate`). In the `api` object, after the `flatten` entry, add:

```ts
  tokens: () => request<Token[]>('/api/tokens'),
  createToken: (name: string) =>
    request<TokenCreated>('/api/tokens', { method: 'POST', body: JSON.stringify({ name }) }),
  revokeToken: (id: number) => request<void>(`/api/tokens/${id}`, { method: 'DELETE' }),
```

- [ ] **Step 3: Type-check the additions**

Run: `cd web && pnpm build`
Expected: succeeds.

- [ ] **Step 4: Add the Settings section**

In `web/src/views/Settings.tsx`, extend the `import type { ... } from '../types'` block with `Token` and `TokenCreated` (after `Settings as UserSettings`).

In the `Settings` component's JSX, between the closing `</Group>` of `<Group head="NOTE">` and `{me.admin && (`, insert:

```tsx
      <Group head="API TOKENS">
        <FoldRow label="Tokens" open={open === 'tokens'} onToggle={fold('tokens')}>
          {open === 'tokens' && <TokensSection notify={notify} />}
        </FoldRow>
      </Group>
```

After the `PersonaSection` function, add:

```tsx
function dayOf(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
}

function TokensSection({ notify }: { notify: Notify }) {
  const [tokens, setTokens] = useState<Token[] | 'error' | undefined>(undefined)
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)
  const [fresh, setFresh] = useState<TokenCreated | null>(null)
  // Revoke is two taps: the first arms the row, the second removes it.
  const [arming, setArming] = useState<number | null>(null)

  useEffect(() => {
    api
      .tokens()
      .then(setTokens)
      .catch(() => setTokens('error'))
  }, [])

  const create = async (e: FormEvent) => {
    e.preventDefault()
    const trimmed = name.trim()
    if (!trimmed || busy) return
    setBusy(true)
    try {
      const made = await api.createToken(trimmed)
      const info: Token = {
        id: made.id,
        name: made.name,
        created_at: made.created_at,
        last_used_at: made.last_used_at,
      }
      setFresh(made)
      setName('')
      setTokens((all) => (Array.isArray(all) ? [...all, info] : all))
    } catch (err) {
      notify(
        err instanceof ApiError && (err.status === 409 || err.status === 422)
          ? err.message
          : "That token wasn't created. Try again.",
      )
    } finally {
      setBusy(false)
    }
  }

  const revoke = async (t: Token) => {
    if (arming !== t.id) {
      setArming(t.id)
      return
    }
    setArming(null)
    try {
      await api.revokeToken(t.id)
      setTokens((all) => (Array.isArray(all) ? all.filter((x) => x.id !== t.id) : all))
      if (fresh?.id === t.id) setFresh(null)
    } catch {
      notify("That token wasn't revoked. Try again.")
    }
  }

  return (
    <div className="set-fold-body">
      <form className="set-token-form" onSubmit={(e) => void create(e)}>
        <input
          aria-label="Token name"
          placeholder="Name this token"
          maxLength={64}
          spellCheck={false}
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <button type="submit" className="btn-haze small" disabled={busy || !name.trim()}>
          Create
        </button>
      </form>
      {fresh && (
        <div className="set-token-fresh">
          <code className="set-token-secret">{fresh.token}</code>
          <span className="set-sub">Copy it now. It won't be shown again.</span>
        </div>
      )}
      {tokens === 'error' && <span className="set-sub">Tokens didn't load.</span>}
      {Array.isArray(tokens) && tokens.length === 0 && (
        <span className="set-sub">No tokens yet.</span>
      )}
      {Array.isArray(tokens) &&
        tokens.map((t) => (
          <div className="set-row set-token-row" key={t.id}>
            <span className="set-row-body">
              <span className="set-label">{t.name}</span>
              <span className="set-sub">
                Created {dayOf(t.created_at)} ·{' '}
                {t.last_used_at ? `used ${dayOf(t.last_used_at)}` : 'never used'}
              </span>
            </span>
            <button
              type="button"
              className="btn-haze small"
              onClick={() => void revoke(t)}
              onBlur={() => setArming((a) => (a === t.id ? null : a))}
            >
              {arming === t.id ? 'Really revoke?' : 'Revoke'}
            </button>
          </div>
        ))}
    </div>
  )
}
```

- [ ] **Step 5: Add styles**

In `web/src/styles.css`, after the `.btn-haze.small` rule, add:

```css
.set-token-form { display: flex; align-items: center; gap: 10px; width: 100%; }
.set-token-form input { flex: 1; min-width: 0; }
.set-token-fresh { display: flex; flex-direction: column; gap: 4px; width: 100%; }
.set-token-secret { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 0.85rem; word-break: break-all; user-select: all; color: var(--ink); }
.set-token-row { width: 100%; min-height: 48px; }
```

- [ ] **Step 6: Build**

Run: `cd web && pnpm build`
Expected: succeeds with no type errors.

- [ ] **Step 7: Commit**

```bash
git add web/src/types.ts web/src/api.ts web/src/views/Settings.tsx web/src/styles.css
git commit -m "feat(web): API tokens section in Settings

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016vuHqrhcoBdoGnqUxjd3rQ"
```

---

### Task 7: README documentation

**Files:**
- Modify: `README.md` (after the "Sessions & limits" subsection, before `## Web client`)

- [ ] **Step 1: Add the subsection**

Insert after the "Sessions & limits" list and before `## Web client`:

```markdown
### API tokens

A user can mint long-lived bearer tokens for scripts and other agents. Tokens
reach the task routes only; every other route still needs the session cookie.

- Mint one in Settings → API tokens, or over the session:
  `POST /api/tokens {name}` → `{id, name, created_at, last_used_at, token}`.
  The `token` (`note_…`) is shown once; only its SHA-256 digest is stored.
  `GET /api/tokens` lists `{id, name, created_at, last_used_at}`;
  `DELETE /api/tokens/{id}` revokes. Names are 1 to 64 characters (`422`),
  and a user holds at most 20 tokens (`409`).
- Send it as `Authorization: Bearer note_…` on `GET/POST /api/tasks`,
  `PATCH/DELETE /api/tasks/{id}`, `POST /api/tasks/{id}/split`, and
  `POST /api/tasks/{id}/flatten`. A bearer header that does not resolve is
  `401` even when a session cookie is also present. Disabling the user stops
  their tokens.
- `DELETE /api/tasks/{id}` removes the task, its steps, and their event links
  (`204`, or `404` when the task is not the caller's).
- Minting and revoking log `token_created` / `token_revoked` to `event_log`.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: API tokens and task delete

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016vuHqrhcoBdoGnqUxjd3rQ"
```

---

### Task 8: Final verification

- [ ] **Step 1: Full Rust suite and warnings**

Run: `cargo build -p note-server 2>&1 | grep -c warning; cargo test -p note-server`
Expected: no new warnings; all tests PASS.

- [ ] **Step 2: Web build**

Run: `cd web && pnpm build`
Expected: succeeds.

- [ ] **Step 3: Format**

Run: `cargo fmt -p note-server -- --check`
Expected: clean. If not, run `cargo fmt -p note-server` and commit as `style: fmt` with the standard trailers.
