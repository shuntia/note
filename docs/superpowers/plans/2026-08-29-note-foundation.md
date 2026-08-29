# Note Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Note server core: config loading, SQLite storage with migrations, auth, task CRUD, day-template → day-plan generation, and a runner that fires plan events — no LLM, no channels yet.

**Architecture:** Single Rust binary (axum + tokio). SQLite via rusqlite behind a small DAO layer. Day templates are per-user TOML; a generator materializes one plan per user per day; a tokio loop fires due events into an event log. Wall-clock times + IANA timezone throughout (jiff).

**Tech Stack:** Rust stable (host rustup, 1.98), axum 0.8, tokio, rusqlite (bundled), serde + toml, jiff (tz), argon2, uuid, axum-extra (cookies).

**Spec:** `docs/superpowers/specs/2026-08-29-note-design.md`

## Global Constraints

- Workspace root is the repo root; server code lives in `server/`.
- No shell/filesystem access is ever exposed to model-facing code (spec: "Tool calls bound the AI") — not relevant to this plan's surface, but do not add any "run command" utility.
- All times persisted as wall-clock strings (`HH:MM`) or ISO dates plus IANA tz name; UTC instants only for `created_at`-style audit fields.
- SQLite pragmas on every open: `journal_mode=WAL`, `foreign_keys=ON`.
- Comments follow `CLAUDE.md`: only where the code can't speak for itself, at fn declarations.
- Every commit message: imperative summary line, no body needed.
- Run tests with `cargo test` from repo root; all tests must pass before every commit.

---

### Task 1: Workspace scaffold + health endpoint

**Files:**
- Create: `Cargo.toml`, `flake.nix`, `.gitignore`, `server/Cargo.toml`, `server/src/main.rs`, `server/src/lib.rs`, `server/src/api.rs`
- Test: `server/tests/health.rs`

**Interfaces:**
- Produces: `note_server::api::router(state: AppState) -> axum::Router`, `note_server::AppState` (Clone; fields added by later tasks).

- [ ] **Step 1: Scaffold workspace**

`Cargo.toml` (root):
```toml
[workspace]
members = ["server"]
resolver = "2"
```

`.gitignore`:
```
target/
node_modules/
data/
```

`flake.nix`:
```nix
{
  description = "Note dev environment";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { self, nixpkgs }:
    let pkgs = nixpkgs.legacyPackages.x86_64-linux;
    in {
      devShells.x86_64-linux.default = pkgs.mkShell {
        packages = with pkgs; [ cargo rustc rustfmt clippy nodejs pnpm sqlite ];
      };
    };
}
```

`server/Cargo.toml`:
```toml
[package]
name = "note-server"
version = "0.1.0"
edition = "2021"

[lib]
name = "note_server"
path = "src/lib.rs"

[[bin]]
name = "note-server"
path = "src/main.rs"

[dependencies]
axum = "0.8"
axum-extra = { version = "0.10", features = ["cookie"] }
tokio = { version = "1", features = ["full"] }
rusqlite = { version = "0.32", features = ["bundled"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
jiff = { version = "0.2", features = ["tzdb-bundle-always"] }
argon2 = "0.5"
uuid = { version = "1", features = ["v4"] }
anyhow = "1"
thiserror = "1"

[dev-dependencies]
proptest = "1"
tempfile = "3"
```

`server/src/lib.rs`:
```rust
pub mod api;

#[derive(Clone, Default)]
pub struct AppState {}
```

`server/src/api.rs`:
```rust
use crate::AppState;
use axum::{routing::get, Router};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state)
}
```

`server/src/main.rs`:
```rust
use note_server::{api, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = api::router(AppState::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

- [ ] **Step 2: Write the test**

`server/tests/health.rs`:
```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use note_server::{api, AppState};
use tower::ServiceExt;

#[tokio::test]
async fn healthz_returns_ok() {
    let app = api::router(AppState::default());
    let res = app
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}
```

Add to `server/Cargo.toml` dev-dependencies: `tower = { version = "0.5", features = ["util"] }`.

- [ ] **Step 3: Run** `cargo test` — expected: PASS (build errors first are fine; fix until green).

- [ ] **Step 4: Commit** — `git add -A && git commit -m "feat: workspace scaffold with axum health endpoint"`

---

### Task 2: Config loading with defaults overlay

**Files:**
- Create: `server/src/config.rs`
- Modify: `server/src/lib.rs` (add `pub mod config;`)
- Test: inline `#[cfg(test)]` in `config.rs`

**Interfaces:**
- Produces:
  - `config::ServerConfig { bind_addr: String, public_base_url: String, data_dir: PathBuf }` via `ServerConfig::load(config_dir: &Path) -> anyhow::Result<ServerConfig>` reading `config_dir/server.toml`.
  - `config::UserConfig { display_name: String, timezone: String, template: String }` via `UserConfig::load(config_dir: &Path, user: &str) -> anyhow::Result<UserConfig>` — reads `config_dir/defaults/user.toml` then overlays `config_dir/users/<user>/user.toml` key-by-key (per-user values win; missing per-user file means pure defaults).

- [ ] **Step 1: Write failing tests**

In `server/src/config.rs`:
```rust
use anyhow::Context;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub bind_addr: String,
    pub public_base_url: String,
    pub data_dir: PathBuf,
}

impl ServerConfig {
    pub fn load(config_dir: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(config_dir.join("server.toml"))
            .context("reading server.toml")?;
        Ok(toml::from_str(&raw)?)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserConfig {
    pub display_name: String,
    pub timezone: String,
    pub template: String,
}

impl UserConfig {
    pub fn load(config_dir: &Path, user: &str) -> anyhow::Result<Self> {
        let defaults: toml::Value = std::fs::read_to_string(config_dir.join("defaults/user.toml"))
            .context("reading defaults/user.toml")?
            .parse()?;
        let merged = match std::fs::read_to_string(
            config_dir.join("users").join(user).join("user.toml"),
        ) {
            Ok(raw) => overlay(defaults, raw.parse()?),
            Err(_) => defaults,
        };
        Ok(merged.try_into()?)
    }
}

fn overlay(base: toml::Value, over: toml::Value) -> toml::Value {
    match (base, over) {
        (toml::Value::Table(mut b), toml::Value::Table(o)) => {
            for (k, v) in o {
                let merged = match b.remove(&k) {
                    Some(bv) => overlay(bv, v),
                    None => v,
                };
                b.insert(k, merged);
            }
            toml::Value::Table(b)
        }
        (_, over) => over,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn user_config_overlays_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/user.toml",
            "display_name = \"Someone\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");
        write(tmp.path(), "users/aki/user.toml", "timezone = \"Asia/Tokyo\"\n");
        let cfg = UserConfig::load(tmp.path(), "aki").unwrap();
        assert_eq!(cfg.timezone, "Asia/Tokyo");
        assert_eq!(cfg.display_name, "Someone");
    }

    #[test]
    fn missing_user_dir_uses_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/user.toml",
            "display_name = \"Someone\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");
        let cfg = UserConfig::load(tmp.path(), "nobody").unwrap();
        assert_eq!(cfg.timezone, "UTC");
    }
}
```

- [ ] **Step 2: Run** `cargo test` — expected: PASS after compile fixes.

- [ ] **Step 3: Commit** — `git commit -am "feat: server and per-user config with defaults overlay"`

---

### Task 3: Storage — open, migrations, schema v1

**Files:**
- Create: `server/src/db.rs`
- Modify: `server/src/lib.rs` (add `pub mod db;`, add `db: Arc<Mutex<rusqlite::Connection>>` to `AppState` with constructor `AppState::new(conn: Connection) -> AppState`)
- Test: inline in `db.rs`

**Interfaces:**
- Produces: `db::open(path: &Path) -> anyhow::Result<Connection>` (applies pragmas + migrations; `path` may be `:memory:` via `db::open_memory()` for tests). `AppState::new(Connection)`, `AppState.db: Arc<Mutex<Connection>>`.

- [ ] **Step 1: Write failing test + implementation**

`server/src/db.rs`:
```rust
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

const MIGRATIONS: &[&str] = &[
    // v1
    "
    CREATE TABLE users (
        id INTEGER PRIMARY KEY,
        username TEXT NOT NULL UNIQUE,
        pass_hash TEXT NOT NULL,
        role TEXT NOT NULL CHECK (role IN ('admin','member'))
    );
    CREATE TABLE sessions (
        token TEXT PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        expires_at TEXT NOT NULL
    );
    CREATE TABLE tasks (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        title TEXT NOT NULL,
        description TEXT NOT NULL DEFAULT '',
        state TEXT NOT NULL DEFAULT 'open'
            CHECK (state IN ('open','in_progress','done','dropped')),
        source TEXT NOT NULL DEFAULT 'manual',
        parent_id INTEGER REFERENCES tasks(id),
        notes TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE plans (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id),
        date TEXT NOT NULL,
        created_at TEXT NOT NULL,
        UNIQUE (user_id, date)
    );
    CREATE TABLE events (
        id INTEGER PRIMARY KEY,
        plan_id INTEGER NOT NULL REFERENCES plans(id),
        kind TEXT NOT NULL,
        wall_time TEXT NOT NULL,
        flexibility TEXT NOT NULL DEFAULT 'fixed'
            CHECK (flexibility IN ('fixed','slide','drop')),
        slide_window_min INTEGER NOT NULL DEFAULT 0,
        channel TEXT NOT NULL DEFAULT 'push',
        status TEXT NOT NULL DEFAULT 'pending'
            CHECK (status IN ('pending','fired','snoozed','dropped','done')),
        fired_at TEXT
    );
    CREATE TABLE event_tasks (
        event_id INTEGER NOT NULL REFERENCES events(id),
        task_id INTEGER NOT NULL REFERENCES tasks(id),
        PRIMARY KEY (event_id, task_id)
    );
    CREATE TABLE event_log (
        id INTEGER PRIMARY KEY,
        ts TEXT NOT NULL,
        user_id INTEGER,
        kind TEXT NOT NULL,
        detail TEXT NOT NULL DEFAULT ''
    );
    ",
];

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    init(&conn)?;
    Ok(conn)
}

pub fn open_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    init(&conn)?;
    Ok(conn)
}

fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        conn.execute_batch(sql)?;
        conn.pragma_update(None, "user_version", (i + 1) as i64)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_and_are_idempotent() {
        let conn = open_memory().unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        init(&conn).unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','admin')",
            [],
        )
        .unwrap();
    }
}
```

`server/src/lib.rs` becomes:
```rust
pub mod api;
pub mod config;
pub mod db;

use rusqlite::Connection;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
}

impl AppState {
    pub fn new(conn: Connection) -> Self {
        Self { db: Arc::new(Mutex::new(conn)) }
    }
}
```

Update `main.rs` and `tests/health.rs` to build state with `AppState::new(db::open_memory()?)` (main: `db::open(&cfg.data_dir.join("note.db"))` after `std::fs::create_dir_all(&cfg.data_dir)`; main now loads `ServerConfig` from `./config` or `$NOTE_CONFIG_DIR` and binds `cfg.bind_addr`).

- [ ] **Step 2: Run** `cargo test` — expected: PASS.

- [ ] **Step 3: Commit** — `git commit -am "feat: sqlite storage with versioned migrations"`

---

### Task 4: Auth — password hashing, login, session extractor, bootstrap admin

**Files:**
- Create: `server/src/auth.rs`
- Modify: `server/src/lib.rs` (add `pub mod auth;`), `server/src/api.rs` (mount routes), `server/src/main.rs` (CLI: `note-server create-user <name> <password> [--admin]`)
- Test: `server/tests/auth.rs`

**Interfaces:**
- Produces:
  - `auth::create_user(conn, username, password, admin: bool) -> anyhow::Result<i64>` (argon2id hash).
  - `auth::login(conn, username, password) -> anyhow::Result<Option<String>>` — verifies, inserts session (uuid token, 30-day expiry, jiff UTC now), returns token.
  - `auth::CurrentUser { id: i64, username: String, admin: bool }` — axum extractor reading `session` cookie against the sessions table; rejects with 401 if absent/expired.
  - Routes: `POST /api/login {username,password}` → `Set-Cookie: session=<token>; HttpOnly; Path=/`; `GET /api/me` → `{"username":..,"admin":..}`.

- [ ] **Step 1: Write failing integration test**

`server/tests/auth.rs`:
```rust
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use note_server::{api, auth, db, AppState};
use tower::ServiceExt;

fn state_with_user() -> AppState {
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "hunter2", true).unwrap();
    AppState::new(conn)
}

#[tokio::test]
async fn login_sets_cookie_and_me_works() {
    let app = api::router(state_with_user());
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"username":"aki","password":"hunter2"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = res.headers()[header::SET_COOKIE].to_str().unwrap().to_string();

    let res = app
        .oneshot(
            Request::get("/api/me")
                .header(header::COOKIE, cookie.split(';').next().unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn wrong_password_is_401_and_me_without_cookie_is_401() {
    let app = api::router(state_with_user());
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"username":"aki","password":"nope"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let res = app
        .oneshot(Request::get("/api/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test --test auth` — expected: compile FAIL (`auth` unresolved).

- [ ] **Step 3: Implement**

`server/src/auth.rs`:
```rust
use crate::AppState;
use anyhow::Result;
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::{request::Parts, StatusCode};
use rusqlite::{Connection, OptionalExtension};

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

pub fn login(conn: &Connection, username: &str, password: &str) -> Result<Option<String>> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, pass_hash FROM users WHERE username = ?1",
            [username],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((id, hash)) = row else { return Ok(None) };
    let parsed = PasswordHash::new(&hash).map_err(|e| anyhow::anyhow!(e))?;
    if Argon2::default().verify_password(password.as_bytes(), &parsed).is_err() {
        return Ok(None);
    }
    let token = uuid::Uuid::new_v4().to_string();
    let expires = jiff::Timestamp::now() + jiff::Span::new().days(30);
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
        let token = jar.get("session").ok_or(StatusCode::UNAUTHORIZED)?.value().to_string();
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
        Ok(CurrentUser { id, username, admin: role == "admin" })
    }
}
```

In `server/src/api.rs`, add handlers and routes:
```rust
use crate::auth::{self, CurrentUser};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Json;
use serde::Deserialize;

#[derive(Deserialize)]
struct LoginReq { username: String, password: String }

async fn login(State(state): State<AppState>, Json(req): Json<LoginReq>) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match auth::login(&conn, &req.username, &req.password) {
        Ok(Some(token)) => (
            StatusCode::OK,
            [(header::SET_COOKIE, format!("session={token}; HttpOnly; Path=/; SameSite=Lax"))],
        )
            .into_response(),
        Ok(None) => StatusCode::UNAUTHORIZED.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn me(user: CurrentUser) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "username": user.username, "admin": user.admin }))
}
```
Routes added to `router()`: `.route("/api/login", post(login)).route("/api/me", get(me))`.

In `server/src/main.rs`, before starting the server:
```rust
let args: Vec<String> = std::env::args().collect();
if args.get(1).map(String::as_str) == Some("create-user") {
    let (name, pass) = (&args[2], &args[3]);
    let admin = args.get(4).map(String::as_str) == Some("--admin");
    auth::create_user(&conn, name, pass, admin)?;
    println!("created {name}");
    return Ok(());
}
```

- [ ] **Step 4: Run** `cargo test` — expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat: argon2 auth with cookie sessions and create-user CLI"`

---

### Task 5: Task CRUD — DAO + REST

**Files:**
- Create: `server/src/tasks.rs`
- Modify: `server/src/lib.rs` (add `pub mod tasks;`), `server/src/api.rs` (routes)
- Test: `server/tests/tasks_api.rs`

**Interfaces:**
- Produces:
  - `tasks::Task { id: i64, title: String, description: String, state: String, source: String, notes: String }` (serde Serialize).
  - `tasks::create(conn, user_id, title: &str, source: &str) -> Result<Task>`; `tasks::list(conn, user_id) -> Result<Vec<Task>>` (excludes `dropped`); `tasks::update(conn, user_id, task_id, patch: TaskPatch) -> Result<Option<Task>>` where `TaskPatch { title: Option<String>, description: Option<String>, state: Option<String>, notes: Option<String> }` (Deserialize; invalid `state` value returns Err).
  - Routes (all behind `CurrentUser`): `GET /api/tasks`, `POST /api/tasks {title}`, `PATCH /api/tasks/{id} <TaskPatch>` (404 if not owner).

- [ ] **Step 1: Write failing test**

`server/tests/tasks_api.rs`:
```rust
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use note_server::{api, auth, db, AppState};
use tower::ServiceExt;

async fn login(app: &axum::Router) -> String {
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"username":"aki","password":"pw"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string()
}

#[tokio::test]
async fn create_list_update_task() {
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", false).unwrap();
    let app = api::router(AppState::new(conn));
    let cookie = login(&app).await;

    let res = app.clone().oneshot(
        Request::post("/api/tasks")
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"title":"call dentist"}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.clone().oneshot(
        Request::patch("/api/tasks/1")
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"state":"done"}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.oneshot(
        Request::get("/api/tasks").header(header::COOKIE, &cookie)
            .body(Body::empty()).unwrap(),
    ).await.unwrap();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v[0]["state"], "done");
}

#[tokio::test]
async fn invalid_state_is_rejected() {
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", false).unwrap();
    let app = api::router(AppState::new(conn));
    let cookie = login(&app).await;
    app.clone().oneshot(
        Request::post("/api/tasks")
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"title":"x"}"#)).unwrap(),
    ).await.unwrap();
    let res = app.oneshot(
        Request::patch("/api/tasks/1")
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"state":"exploded"}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
```

Add `http-body-util = "0.1"` to dev-dependencies.

- [ ] **Step 2: Run to verify failure** — `cargo test --test tasks_api` — expected: compile FAIL.

- [ ] **Step 3: Implement**

`server/src/tasks.rs`:
```rust
use anyhow::{bail, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

const STATES: &[&str] = &["open", "in_progress", "done", "dropped"];

#[derive(Debug, Serialize)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub state: String,
    pub source: String,
    pub notes: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub state: Option<String>,
    pub notes: Option<String>,
}

fn now() -> String {
    jiff::Timestamp::now().to_string()
}

fn row_to_task(r: &rusqlite::Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get(0)?, title: r.get(1)?, description: r.get(2)?,
        state: r.get(3)?, source: r.get(4)?, notes: r.get(5)?,
    })
}

const COLS: &str = "id, title, description, state, source, notes";

pub fn create(conn: &Connection, user_id: i64, title: &str, source: &str) -> Result<Task> {
    conn.execute(
        "INSERT INTO tasks (user_id, title, source, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?4)",
        (user_id, title, source, now()),
    )?;
    let id = conn.last_insert_rowid();
    Ok(conn.query_row(
        &format!("SELECT {COLS} FROM tasks WHERE id = ?1"), [id], row_to_task,
    )?)
}

pub fn list(conn: &Connection, user_id: i64) -> Result<Vec<Task>> {
    let mut stmt = conn.prepare(
        &format!("SELECT {COLS} FROM tasks WHERE user_id = ?1 AND state != 'dropped' ORDER BY id"),
    )?;
    let rows = stmt.query_map([user_id], row_to_task)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn update(conn: &Connection, user_id: i64, task_id: i64, patch: TaskPatch) -> Result<Option<Task>> {
    if let Some(s) = &patch.state {
        if !STATES.contains(&s.as_str()) {
            bail!("invalid state: {s}");
        }
    }
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM tasks WHERE id = ?1 AND user_id = ?2",
            (task_id, user_id), |r| r.get(0),
        )
        .optional()?;
    if existing.is_none() {
        return Ok(None);
    }
    conn.execute(
        "UPDATE tasks SET
            title = COALESCE(?1, title),
            description = COALESCE(?2, description),
            state = COALESCE(?3, state),
            notes = COALESCE(?4, notes),
            updated_at = ?5
         WHERE id = ?6",
        (&patch.title, &patch.description, &patch.state, &patch.notes, now(), task_id),
    )?;
    Ok(Some(conn.query_row(
        &format!("SELECT {COLS} FROM tasks WHERE id = ?1"), [task_id], row_to_task,
    )?))
}
```

API handlers in `api.rs` (routes `GET/POST /api/tasks`, `PATCH /api/tasks/{id}`):
```rust
#[derive(Deserialize)]
struct CreateTaskReq { title: String }

async fn tasks_list(user: CurrentUser, State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tasks::list(&conn, user.id) {
        Ok(ts) => Json(ts).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn tasks_create(
    user: CurrentUser, State(state): State<AppState>, Json(req): Json<CreateTaskReq>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tasks::create(&conn, user.id, &req.title, "manual") {
        Ok(t) => Json(t).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn tasks_update(
    user: CurrentUser, State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Json(patch): Json<crate::tasks::TaskPatch>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tasks::update(&conn, user.id, id, patch) {
        Ok(Some(t)) => Json(t).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}
```

- [ ] **Step 4: Run** `cargo test` — expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat: task CRUD with owner-scoped REST endpoints"`

---

### Task 6: Day templates + plan generation

**Files:**
- Create: `server/src/templates.rs`, `server/src/plan.rs`
- Modify: `server/src/lib.rs` (add `pub mod templates; pub mod plan;`)
- Test: inline in both files

**Interfaces:**
- Consumes: `db::open_memory`, `config::UserConfig`.
- Produces:
  - `templates::Template { events: Vec<TemplateEvent> }`, `templates::TemplateEvent { kind: String, time: String, days: Vec<String>, flexibility: String, slide_window_min: i64, channel: String }`; `Template::load(config_dir, user, name) -> anyhow::Result<Template>` reading `config_dir/users/<user>/templates/<name>.toml`, falling back to `config_dir/defaults/templates/<name>.toml`.
  - `plan::generate(conn, user_id, template: &Template, date: jiff::civil::Date) -> Result<i64>` — inserts plan + events matching the date's weekday (`mon`..`sun`); returns plan id; second call for same (user,date) is a no-op returning existing id.
  - `plan::PlanEvent { id: i64, kind: String, wall_time: String, status: String, flexibility: String, slide_window_min: i64, channel: String }` (Serialize); `plan::events_for(conn, user_id, date) -> Result<Vec<PlanEvent>>`.

- [ ] **Step 1: Write failing tests**

Inline in `plan.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::{Template, TemplateEvent};

    fn tmpl() -> Template {
        Template {
            events: vec![
                TemplateEvent {
                    kind: "checkin_call".into(), time: "09:00".into(),
                    days: vec!["mon".into(), "tue".into(), "wed".into(), "thu".into(), "fri".into()],
                    flexibility: "slide".into(), slide_window_min: 60, channel: "voice".into(),
                },
                TemplateEvent {
                    kind: "nudge".into(), time: "14:00".into(),
                    days: vec!["sat".into()],
                    flexibility: "drop".into(), slide_window_min: 0, channel: "push".into(),
                },
            ],
        }
    }

    #[test]
    fn generates_weekday_events_only() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        // 2026-08-31 is a Monday
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, uid, &tmpl(), date).unwrap();
        let evs = events_for(&conn, uid, date).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].kind, "checkin_call");
        assert_eq!(evs[0].wall_time, "09:00");
    }

    #[test]
    fn regenerating_same_day_is_noop() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let p1 = generate(&conn, uid, &tmpl(), date).unwrap();
        let p2 = generate(&conn, uid, &tmpl(), date).unwrap();
        assert_eq!(p1, p2);
        assert_eq!(events_for(&conn, uid, date).unwrap().len(), 1);
    }
}
```

Inline in `templates.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_user_template_over_default() {
        let tmp = tempfile::tempdir().unwrap();
        let write = |rel: &str, c: &str| {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        };
        write("defaults/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='08:00'\ndays=['mon']\n");
        write("users/aki/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='10:00'\ndays=['mon']\n");
        let t = Template::load(tmp.path(), "aki", "default").unwrap();
        assert_eq!(t.events[0].time, "10:00");
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test plan templates` — expected: compile FAIL.

- [ ] **Step 3: Implement**

`server/src/templates.rs`:
```rust
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Template {
    pub events: Vec<TemplateEvent>,
}

#[derive(Debug, Deserialize)]
pub struct TemplateEvent {
    pub kind: String,
    pub time: String,
    pub days: Vec<String>,
    #[serde(default = "default_flexibility")]
    pub flexibility: String,
    #[serde(default)]
    pub slide_window_min: i64,
    #[serde(default = "default_channel")]
    pub channel: String,
}

fn default_flexibility() -> String { "fixed".into() }
fn default_channel() -> String { "push".into() }

impl Template {
    pub fn load(config_dir: &Path, user: &str, name: &str) -> Result<Self> {
        let user_path = config_dir.join("users").join(user).join("templates").join(format!("{name}.toml"));
        let default_path = config_dir.join("defaults/templates").join(format!("{name}.toml"));
        let path = if user_path.exists() { user_path } else { default_path };
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading template {}", path.display()))?;
        Ok(toml::from_str(&raw)?)
    }
}
```

`server/src/plan.rs`:
```rust
use crate::templates::Template;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct PlanEvent {
    pub id: i64,
    pub kind: String,
    pub wall_time: String,
    pub status: String,
    pub flexibility: String,
    pub slide_window_min: i64,
    pub channel: String,
}

fn weekday_key(date: jiff::civil::Date) -> &'static str {
    match date.weekday() {
        jiff::civil::Weekday::Monday => "mon",
        jiff::civil::Weekday::Tuesday => "tue",
        jiff::civil::Weekday::Wednesday => "wed",
        jiff::civil::Weekday::Thursday => "thu",
        jiff::civil::Weekday::Friday => "fri",
        jiff::civil::Weekday::Saturday => "sat",
        jiff::civil::Weekday::Sunday => "sun",
    }
}

pub fn generate(conn: &Connection, user_id: i64, template: &Template, date: jiff::civil::Date) -> Result<i64> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM plans WHERE user_id = ?1 AND date = ?2",
            (user_id, date.to_string()), |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO plans (user_id, date, created_at) VALUES (?1, ?2, ?3)",
        (user_id, date.to_string(), jiff::Timestamp::now().to_string()),
    )?;
    let plan_id = conn.last_insert_rowid();
    let day = weekday_key(date);
    for ev in template.events.iter().filter(|e| e.days.iter().any(|d| d == day)) {
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, flexibility, slide_window_min, channel)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (plan_id, &ev.kind, &ev.time, &ev.flexibility, ev.slide_window_min, &ev.channel),
        )?;
    }
    Ok(plan_id)
}

pub fn events_for(conn: &Connection, user_id: i64, date: jiff::civil::Date) -> Result<Vec<PlanEvent>> {
    let mut stmt = conn.prepare(
        "SELECT e.id, e.kind, e.wall_time, e.status, e.flexibility, e.slide_window_min, e.channel
         FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND p.date = ?2 ORDER BY e.wall_time",
    )?;
    let rows = stmt.query_map((user_id, date.to_string()), |r| {
        Ok(PlanEvent {
            id: r.get(0)?, kind: r.get(1)?, wall_time: r.get(2)?, status: r.get(3)?,
            flexibility: r.get(4)?, slide_window_min: r.get(5)?, channel: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
```

- [ ] **Step 4: Run** `cargo test` — expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat: day templates and per-day plan generation"`

---

### Task 7: Shift operations + plan API

**Files:**
- Modify: `server/src/plan.rs`, `server/src/api.rs`
- Test: inline in `plan.rs` + extend `server/tests/tasks_api.rs`-style test in `server/tests/plan_api.rs`

**Interfaces:**
- Consumes: Task 6's `plan::events_for`, `plan::generate`.
- Produces:
  - `plan::shift(conn, user_id, event_id, minutes: i64) -> Result<Option<()>>` — adds minutes to `wall_time` (HH:MM arithmetic clamped to 00:00–23:59), sets status `snoozed` back to `pending`; `None` if event not owned by user or not shiftable (`flexibility == "fixed"`).
  - `plan::set_status(conn, user_id, event_id, status: &str) -> Result<Option<()>>` — only `dropped`/`done` accepted here; Err on other values.
  - Routes behind `CurrentUser`: `GET /api/plan/today?date=YYYY-MM-DD` (date param optional; default = today in the user's tz — resolve tz via `config::UserConfig`, loaded from `AppState.config_dir: PathBuf` added to state in this task); `POST /api/events/{id}/shift {minutes}`; `POST /api/events/{id}/done`; `POST /api/events/{id}/drop`.

- [ ] **Step 1: Write failing unit tests**

Add to `plan.rs` tests:
```rust
    #[test]
    fn shift_moves_wall_time_and_respects_fixed() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let mut t = tmpl();
        t.events[0].flexibility = "slide".into();
        generate(&conn, uid, &t, date).unwrap();
        let ev = &events_for(&conn, uid, date).unwrap()[0];
        assert!(shift(&conn, uid, ev.id, 45).unwrap().is_some());
        let ev = &events_for(&conn, uid, date).unwrap()[0];
        assert_eq!(ev.wall_time, "09:45");

        conn.execute("UPDATE events SET flexibility='fixed'", []).unwrap();
        assert!(shift(&conn, uid, ev.id, 15).unwrap().is_none());
    }

    #[test]
    fn set_status_accepts_only_done_and_dropped() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, uid, &tmpl(), date).unwrap();
        let ev_id = events_for(&conn, uid, date).unwrap()[0].id;
        assert!(set_status(&conn, uid, ev_id, "done").unwrap().is_some());
        assert!(set_status(&conn, uid, ev_id, "fired").is_err());
        let other = crate::auth::create_user(&conn, "b", "p", false).unwrap();
        assert!(set_status(&conn, other, ev_id, "done").unwrap().is_none());
    }
```

(Change `tmpl()` to return owned `Template` and mark test fn params accordingly — it already does.)

- [ ] **Step 2: Run to verify failure** — `cargo test shift` — expected: FAIL (functions missing).

- [ ] **Step 3: Implement in `plan.rs`**

```rust
fn owned_event(conn: &Connection, user_id: i64, event_id: i64) -> Result<Option<(String, String)>> {
    Ok(conn
        .query_row(
            "SELECT e.wall_time, e.flexibility FROM events e
             JOIN plans p ON p.id = e.plan_id
             WHERE e.id = ?1 AND p.user_id = ?2",
            (event_id, user_id),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

pub fn shift(conn: &Connection, user_id: i64, event_id: i64, minutes: i64) -> Result<Option<()>> {
    let Some((wall, flex)) = owned_event(conn, user_id, event_id)? else { return Ok(None) };
    if flex == "fixed" {
        return Ok(None);
    }
    let (h, m) = wall.split_once(':').ok_or_else(|| anyhow::anyhow!("bad wall_time"))?;
    let total = (h.parse::<i64>()? * 60 + m.parse::<i64>()? + minutes).clamp(0, 23 * 60 + 59);
    conn.execute(
        "UPDATE events SET wall_time = ?1, status = 'pending' WHERE id = ?2",
        (format!("{:02}:{:02}", total / 60, total % 60), event_id),
    )?;
    Ok(Some(()))
}

pub fn set_status(conn: &Connection, user_id: i64, event_id: i64, status: &str) -> Result<Option<()>> {
    if status != "done" && status != "dropped" {
        anyhow::bail!("invalid status: {status}");
    }
    if owned_event(conn, user_id, event_id)?.is_none() {
        return Ok(None);
    }
    conn.execute("UPDATE events SET status = ?1 WHERE id = ?2", (status, event_id))?;
    Ok(Some(()))
}
```

- [ ] **Step 4: Add plan API routes**

Add `config_dir: PathBuf` to `AppState` (`AppState::new(conn, config_dir)`); update all constructors/tests (tests pass `tempfile::tempdir()` path with a written `defaults/user.toml` + `defaults/templates/default.toml`, or a helper `test_state()` in a new `server/tests/common/mod.rs`).

`server/tests/plan_api.rs`:
```rust
mod common;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn today_generates_and_returns_events() {
    let (app, cookie) = common::app_with_logged_in_user().await;
    let res = app.clone().oneshot(
        Request::get("/api/plan/today?date=2026-08-31")
            .header(header::COOKIE, &cookie)
            .body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.oneshot(
        Request::post("/api/events/1/done")
            .header(header::COOKIE, &cookie)
            .body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}
```

`server/tests/common/mod.rs`:
```rust
use axum::body::Body;
use axum::http::{header, Request};
use note_server::{api, auth, db, AppState};
use tower::ServiceExt;

pub async fn app_with_logged_in_user() -> (axum::Router, String) {
    let tmp = tempfile::tempdir().unwrap();
    let write = |rel: &str, c: &str| {
        let p = tmp.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, c).unwrap();
    };
    write("defaults/user.toml",
        "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");
    write("defaults/templates/default.toml",
        "[[events]]\nkind='checkin_call'\ntime='09:00'\ndays=['mon','tue','wed','thu','fri','sat','sun']\nflexibility='slide'\nslide_window_min=60\nchannel='voice'\n");
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", true).unwrap();
    let app = api::router(AppState::new(conn, tmp.into_path()));
    let res = app.clone().oneshot(
        Request::post("/api/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"username":"aki","password":"pw"}"#)).unwrap(),
    ).await.unwrap();
    let cookie = res.headers()[header::SET_COOKIE]
        .to_str().unwrap().split(';').next().unwrap().to_string();
    (app, cookie)
}
```

Handlers in `api.rs`:
```rust
#[derive(Deserialize)]
struct PlanQuery { date: Option<String> }

async fn plan_today(
    user: CurrentUser, State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<PlanQuery>,
) -> impl IntoResponse {
    let ucfg = match crate::config::UserConfig::load(&state.config_dir, &user.username) {
        Ok(c) => c,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let date = match q.date {
        Some(d) => match d.parse::<jiff::civil::Date>() {
            Ok(d) => d,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        },
        None => {
            let tz = match jiff::tz::TimeZone::get(&ucfg.timezone) {
                Ok(tz) => tz,
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };
            jiff::Timestamp::now().to_zoned(tz).date()
        }
    };
    let tmpl = match crate::templates::Template::load(&state.config_dir, &user.username, &ucfg.template) {
        Ok(t) => t,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let conn = state.db.lock().unwrap();
    if crate::plan::generate(&conn, user.id, &tmpl, date).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    match crate::plan::events_for(&conn, user.id, date) {
        Ok(evs) => Json(evs).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
struct ShiftReq { minutes: i64 }

async fn event_shift(
    user: CurrentUser, State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Json(req): Json<ShiftReq>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::plan::shift(&conn, user.id, id, req.minutes) {
        Ok(Some(())) => StatusCode::OK.into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn event_done(
    user: CurrentUser, State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> impl IntoResponse {
    event_set(state, user, id, "done")
}

async fn event_drop(
    user: CurrentUser, State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> impl IntoResponse {
    event_set(state, user, id, "dropped")
}

fn event_set(state: AppState, user: CurrentUser, id: i64, status: &str) -> axum::response::Response {
    let conn = state.db.lock().unwrap();
    match crate::plan::set_status(&conn, user.id, id, status) {
        Ok(Some(())) => StatusCode::OK.into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}
```

- [ ] **Step 5: Run** `cargo test` — expected: PASS. Then **Commit** — `git commit -am "feat: shift/done/drop operations and plan API"`

---

### Task 8: Runner — fire due events into the event log

**Files:**
- Create: `server/src/runner.rs`, `server/src/log.rs`
- Modify: `server/src/lib.rs`, `server/src/main.rs` (spawn runner loop)
- Test: inline in `runner.rs`

**Interfaces:**
- Consumes: plan/events tables, `config::UserConfig` for tz.
- Produces:
  - `log::record(conn, user_id: Option<i64>, kind: &str, detail: &str) -> Result<()>` — inserts into `event_log` with UTC timestamp.
  - `runner::fire_due(conn, config_dir: &Path, now: jiff::Timestamp) -> Result<Vec<i64>>` — pure-ish core: for every `pending` event on a plan dated today-or-earlier *in that user's tz* whose wall-clock time (resolved in the user's tz on the plan date; DST gaps resolve to next valid instant via jiff's lenient `to_zoned`) is `<= now`, set `status='fired'`, `fired_at=now`, log `event_fired`; returns fired event ids.
  - `runner::spawn(state: AppState)` — tokio task calling `fire_due` every 30s with `jiff::Timestamp::now()`, logging errors to `event_log` as `runner_error`.

- [ ] **Step 1: Write failing tests**

Inline in `runner.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::{Template, TemplateEvent};

    fn setup(tz: &str) -> (rusqlite::Connection, tempfile::TempDir, i64) {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("users/aki");
        std::fs::create_dir_all(tmp.path().join("defaults")).unwrap();
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            tmp.path().join("defaults/user.toml"),
            format!("display_name = \"X\"\ntimezone = \"{tz}\"\ntemplate = \"default\"\n"),
        ).unwrap();
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        (conn, tmp, uid)
    }

    fn one_event_template(time: &str) -> Template {
        Template { events: vec![TemplateEvent {
            kind: "nudge".into(), time: time.into(),
            days: vec!["mon".into(),"tue".into(),"wed".into(),"thu".into(),"fri".into(),"sat".into(),"sun".into()],
            flexibility: "slide".into(), slide_window_min: 60, channel: "push".into(),
        }]}
    }

    #[test]
    fn fires_only_when_due_in_user_tz() {
        let (conn, tmp, uid) = setup("Asia/Tokyo");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, uid, &one_event_template("09:00"), date).unwrap();

        // 08:59 JST on the plan date = 2026-08-30T23:59Z
        let early: jiff::Timestamp = "2026-08-30T23:59:00Z".parse().unwrap();
        assert!(fire_due(&conn, tmp.path(), early).unwrap().is_empty());

        // 09:01 JST
        let due: jiff::Timestamp = "2026-08-31T00:01:00Z".parse().unwrap();
        let fired = fire_due(&conn, tmp.path(), due).unwrap();
        assert_eq!(fired.len(), 1);

        // second run does not double-fire
        assert!(fire_due(&conn, tmp.path(), due).unwrap().is_empty());
    }

    #[test]
    fn fired_events_are_logged() {
        let (conn, tmp, uid) = setup("UTC");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, uid, &one_event_template("00:00"), date).unwrap();
        let now: jiff::Timestamp = "2026-08-31T12:00:00Z".parse().unwrap();
        fire_due(&conn, tmp.path(), now).unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM event_log WHERE kind='event_fired'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(n, 1);
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test runner` — expected: compile FAIL.

- [ ] **Step 3: Implement**

`server/src/log.rs`:
```rust
use anyhow::Result;
use rusqlite::Connection;

pub fn record(conn: &Connection, user_id: Option<i64>, kind: &str, detail: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO event_log (ts, user_id, kind, detail) VALUES (?1, ?2, ?3, ?4)",
        (jiff::Timestamp::now().to_string(), user_id, kind, detail),
    )?;
    Ok(())
}
```

`server/src/runner.rs`:
```rust
use crate::{config::UserConfig, AppState};
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

pub fn fire_due(conn: &Connection, config_dir: &Path, now: jiff::Timestamp) -> Result<Vec<i64>> {
    struct Candidate { event_id: i64, user_id: i64, username: String, date: String, wall_time: String }
    let mut stmt = conn.prepare(
        "SELECT e.id, p.user_id, u.username, p.date, e.wall_time
         FROM events e
         JOIN plans p ON p.id = e.plan_id
         JOIN users u ON u.id = p.user_id
         WHERE e.status = 'pending'",
    )?;
    let candidates: Vec<Candidate> = stmt
        .query_map([], |r| {
            Ok(Candidate {
                event_id: r.get(0)?, user_id: r.get(1)?, username: r.get(2)?,
                date: r.get(3)?, wall_time: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut fired = Vec::new();
    for c in candidates {
        let tz_name = UserConfig::load(config_dir, &c.username)
            .map(|u| u.timezone)
            .unwrap_or_else(|_| "UTC".into());
        let tz = jiff::tz::TimeZone::get(&tz_name).unwrap_or(jiff::tz::TimeZone::UTC);
        let date: jiff::civil::Date = c.date.parse()?;
        let time: jiff::civil::Time = format!("{}:00", c.wall_time).parse()?;
        // DST gaps resolve leniently to the next valid instant.
        let due = date.to_datetime(time).to_zoned(tz)?.timestamp();
        if due <= now {
            conn.execute(
                "UPDATE events SET status='fired', fired_at=?1 WHERE id=?2",
                (now.to_string(), c.event_id),
            )?;
            crate::log::record(conn, Some(c.user_id), "event_fired",
                &format!("event {} due {}", c.event_id, due))?;
            fired.push(c.event_id);
        }
    }
    Ok(fired)
}

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tick.tick().await;
            let conn = state.db.lock().unwrap();
            if let Err(e) = fire_due(&conn, &state.config_dir, jiff::Timestamp::now()) {
                let _ = crate::log::record(&conn, None, "runner_error", &e.to_string());
            }
        }
    });
}
```

In `main.rs` after building state: `runner::spawn(state.clone());`

Note: `date.to_datetime(time).to_zoned(tz)` — if the jiff API for lenient resolution differs on the pinned version, use `tz.to_ambiguous_zoned(date.to_datetime(time)).compatible()?`; the test with a DST-less tz passes either way, and correctness of the gap case is covered by jiff's "compatible" disambiguation.

- [ ] **Step 4: Run** `cargo test` — expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat: runner fires due events per user timezone into event log"`

---

### Task 9: Admin surface + ship-shape

**Files:**
- Modify: `server/src/api.rs`, `server/src/main.rs`
- Create: `config/server.toml`, `config/defaults/user.toml`, `config/defaults/templates/default.toml`, `README.md`
- Test: `server/tests/admin_api.rs`

**Interfaces:**
- Consumes: everything prior.
- Produces:
  - `GET /api/admin/log?limit=100` — last N event_log rows, admin-only (403 for members).
  - `POST /api/admin/users {username, password, admin}` — admin-only user creation.
  - Checked-in starter config (UTC defaults; `bind_addr = "127.0.0.1:3271"`, `public_base_url = "http://localhost:3271"`, `data_dir = "data"`).
  - `README.md`: build (`cargo build --release`), first run (`note-server create-user`), config layout, systemd unit example.

- [ ] **Step 1: Write failing test**

`server/tests/admin_api.rs`:
```rust
mod common;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn member_cannot_use_admin_routes() {
    let (app, admin_cookie) = common::app_with_logged_in_user().await;
    // create a member via admin route, then log in as them
    let res = app.clone().oneshot(
        Request::post("/api/admin/users")
            .header(header::COOKIE, &admin_cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"username":"kid","password":"pw","admin":false}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app.clone().oneshot(
        Request::post("/api/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"username":"kid","password":"pw"}"#)).unwrap(),
    ).await.unwrap();
    let kid_cookie = res.headers()[header::SET_COOKIE]
        .to_str().unwrap().split(';').next().unwrap().to_string();

    let res = app.oneshot(
        Request::get("/api/admin/log")
            .header(header::COOKIE, &kid_cookie)
            .body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}
```

- [ ] **Step 2: Run to verify failure** — expected: 404/compile FAIL.

- [ ] **Step 3: Implement**

Handlers in `api.rs`:
```rust
#[derive(Deserialize)]
struct CreateUserReq { username: String, password: String, admin: bool }

async fn admin_create_user(
    user: CurrentUser, State(state): State<AppState>, Json(req): Json<CreateUserReq>,
) -> impl IntoResponse {
    if !user.admin {
        return StatusCode::FORBIDDEN.into_response();
    }
    let conn = state.db.lock().unwrap();
    match auth::create_user(&conn, &req.username, &req.password, req.admin) {
        Ok(_) => StatusCode::OK.into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

#[derive(Deserialize)]
struct LogQuery { #[serde(default = "default_limit")] limit: i64 }
fn default_limit() -> i64 { 100 }

async fn admin_log(
    user: CurrentUser, State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<LogQuery>,
) -> impl IntoResponse {
    if !user.admin {
        return StatusCode::FORBIDDEN.into_response();
    }
    let conn = state.db.lock().unwrap();
    let mut stmt = match conn.prepare(
        "SELECT ts, user_id, kind, detail FROM event_log ORDER BY id DESC LIMIT ?1",
    ) {
        Ok(s) => s,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let rows: Result<Vec<serde_json::Value>, _> = stmt
        .query_map([q.limit], |r| {
            Ok(serde_json::json!({
                "ts": r.get::<_, String>(0)?,
                "user_id": r.get::<_, Option<i64>>(1)?,
                "kind": r.get::<_, String>(2)?,
                "detail": r.get::<_, String>(3)?,
            }))
        })
        .and_then(|m| m.collect());
    match rows {
        Ok(v) => Json(v).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
```

Config starter files exactly as in Interfaces. `README.md` covers: prerequisites (host rustup or `nix develop`), build, create first admin, run, config layout tree, systemd unit:
```ini
[Unit]
Description=Note server
After=network.target

[Service]
ExecStart=/home/shuntia/Projects/note/target/release/note-server
WorkingDirectory=/home/shuntia/Projects/note
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 4: Run** `cargo test` — expected: PASS. Manual smoke: `cargo run` + `curl localhost:3271/healthz`.

- [ ] **Step 5: Commit** — `git commit -am "feat: admin routes, starter config, and deployment README"`

---

## Follow-on plans (not in this document)

2. Tool layer (serde+schemars registry, proptest fuzzing harness) + memory store + context assembly.
3. Provider layer (Anthropic + OpenAI-compatible + embeddings; mocks) + agent runtime + nightly plan/debrief job.
4. Channels: web-push, Twilio voice bridge, WebSocket delivery; escalation via agent decisions.
5. Web PWA.
