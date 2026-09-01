use crate::auth::{self, CurrentUser};
use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use rusqlite::OptionalExtension;
use serde::Deserialize;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/tasks", get(tasks_list).post(tasks_create))
        .route("/api/tasks/{id}", patch(tasks_update))
        .route("/api/tasks/{id}/split", post(task_split))
        .route("/api/tasks/{id}/flatten", post(task_flatten))
        .route("/api/talk", post(talk))
        .route("/api/conversations", get(conversations_list))
        .route(
            "/api/conversations/{id}",
            patch(conversation_rename).delete(conversation_delete),
        )
        .route("/api/conversations/{id}/messages", get(conversation_messages))
        .route("/api/settings", get(settings_get).put(settings_put))
        .route(
            "/api/prompts/{name}",
            get(prompt_get).put(prompt_put).delete(prompt_delete),
        )
        .route("/api/memory", get(memory_list))
        .route("/api/memory/{id}", get(memory_read))
        .route("/api/plan/today", get(plan_today))
        .route("/api/debrief", get(debrief))
        .route("/api/events/{id}/shift", post(event_shift))
        .route("/api/events/{id}/snooze", post(event_snooze))
        .route("/api/events/{id}/done", post(event_done))
        .route("/api/events/{id}/drop", post(event_drop))
        .route("/api/ws", get(ws_connect))
        .route("/api/push/subscribe", post(push_subscribe))
        .route("/api/push/unsubscribe", post(push_unsubscribe))
        .route("/api/push/vapid_public_key", get(vapid_public_key))
        .route("/api/admin/log", get(admin_log))
        .with_state(state)
}

/// Serves the built web client around the API: unknown non-API paths fall back
/// to `index.html` (SPA routing), unknown API paths stay 404, and a missing
/// build directory leaves the API-only router untouched so the server runs
/// without the client ever being built.
pub fn router_with_web(state: AppState, web_dir: &std::path::Path) -> Router {
    let api = router(state);
    if !web_dir.join("index.html").exists() {
        return api;
    }
    let files = tower_http::services::ServeDir::new(web_dir)
        .fallback(tower_http::services::ServeFile::new(web_dir.join("index.html")));
    // `{*rest}` needs at least one character, so the bare prefix forms are
    // registered separately or they would fall through to the SPA shell.
    api.route(
        "/api/{*rest}",
        axum::routing::any(|| async { StatusCode::NOT_FOUND }),
    )
    .route("/api", axum::routing::any(|| async { StatusCode::NOT_FOUND }))
    .route("/api/", axum::routing::any(|| async { StatusCode::NOT_FOUND }))
    .fallback_service(files)
}

#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}

async fn login(State(state): State<AppState>, Json(req): Json<LoginReq>) -> impl IntoResponse {
    let now = jiff::Timestamp::now();
    if !state.login_limiter.try_attempt(&req.username, now) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let db = state.db.clone();
    let (username, password) = (req.username.clone(), req.password);
    let result = tokio::task::spawn_blocking(move || auth::login(&db, &username, &password)).await;
    match result {
        Ok(Ok(Some(token))) => {
            state.login_limiter.clear(&req.username);
            (
                StatusCode::OK,
                [(
                    header::SET_COOKIE,
                    auth::session_cookie(&token, state.secure_cookies),
                )],
            )
                .into_response()
        }
        Ok(Ok(None)) => StatusCode::UNAUTHORIZED.into_response(),
        Ok(Err(e)) => log_login_error(&state, &e.to_string()),
        Err(e) => log_login_error(&state, &format!("login task failed: {e}")),
    }
}

fn log_login_error(state: &AppState, detail: &str) -> axum::response::Response {
    let conn = state.db.lock().unwrap();
    let _ = crate::log::record(&conn, None, "login_error", detail);
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

async fn logout(State(state): State<AppState>, headers: axum::http::HeaderMap) -> impl IntoResponse {
    let jar = axum_extra::extract::CookieJar::from_headers(&headers);
    if let Some(c) = jar.get("session") {
        let conn = state.db.lock().unwrap();
        let _ = conn.execute("DELETE FROM sessions WHERE token = ?1", [c.value()]);
    }
    (
        StatusCode::OK,
        [(header::SET_COOKIE, auth::clear_cookie(state.secure_cookies))],
    )
        .into_response()
}

async fn me(user: CurrentUser) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "username": user.username, "admin": user.admin }))
}

async fn tasks_list(user: CurrentUser, State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tasks::list(&conn, user.id) {
        Ok(ts) => Json(ts).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn tasks_create(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<crate::tasks::NewTask>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tasks::create(&conn, user.id, req, "manual", crate::tasks::Actor::User) {
        Ok(t) => Json(t).into_response(),
        Err(e) => task_error(e),
    }
}

async fn tasks_update(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(patch): Json<crate::tasks::TaskPatch>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tasks::update(&conn, user.id, id, patch) {
        Ok(Some(t)) => Json(t).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => task_error(e),
    }
}

#[derive(Deserialize)]
struct SplitReq {
    steps: Vec<crate::tasks::Step>,
}

async fn task_split(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<SplitReq>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tasks::split(&conn, user.id, id, req.steps, crate::tasks::Actor::User) {
        Ok(Some(n)) => Json(n).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => task_error(e),
    }
}

async fn task_flatten(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tasks::flatten(&conn, user.id, id) {
        Ok(Some((task, removed))) => {
            Json(serde_json::json!({ "task": task, "removed": removed })).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => task_error(e),
    }
}

fn task_error(e: crate::tasks::UpdateError) -> axum::response::Response {
    use crate::tasks::UpdateError as E;
    match e {
        E::InvalidState(_) => StatusCode::BAD_REQUEST.into_response(),
        E::InvalidDuration(m) | E::InvalidHierarchy(m) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": m })),
        )
            .into_response(),
        E::NowFull(m) => {
            (StatusCode::CONFLICT, Json(serde_json::json!({ "error": m }))).into_response()
        }
        E::Db(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
struct TalkReq {
    message: String,
    conversation_id: Option<i64>,
}

const MAX_TALK_MESSAGE: usize = 16 * 1024;
// History windows stay user-first/assistant-last: each success appends exactly
// one user and one assistant row, and errors persist nothing.
const TALK_HISTORY_LIMIT: usize = 32;

/// A session makes synchronous provider calls and blocking DB writes, so it
/// runs off the async executor.
async fn talk(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<TalkReq>,
) -> impl IntoResponse {
    let message = req.message.trim().to_string();
    if message.is_empty() || message.len() > MAX_TALK_MESSAGE {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "message must be non-blank and at most 16384 bytes" })),
        )
            .into_response();
    }
    if let Some(id) = req.conversation_id {
        let conn = state.db.lock().unwrap();
        match crate::talk::owned(&conn, user.id, id) {
            Ok(true) => {}
            Ok(false) => return conversation_not_found(),
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
    let permit = match state.talk_gate.try_enter(user.id) {
        Ok(p) => p,
        Err(crate::TalkBusy::UserBusy) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({ "error": "a reply is already in progress" })),
            )
                .into_response()
        }
        Err(crate::TalkBusy::Full) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::RETRY_AFTER, "5")],
                Json(serde_json::json!({ "error": "the server is at capacity" })),
            )
                .into_response()
        }
    };
    let err_db = state.db.clone();
    let uid = user.id;
    let req_conversation = req.conversation_id;
    let result = tokio::task::spawn_blocking(move || {
        // held here, not in the handler future, so a cancelled request still
        // holds the slot until the session it orphaned actually finishes
        let _permit = permit;
        let deps = crate::agent::SessionDeps {
            db: &state.db,
            config_dir: &state.config_dir,
            data_dir: &state.data_dir,
            llm: state.llm.as_ref(),
            embeddings: state.embeddings.as_deref(),
        };
        let now = jiff::Timestamp::now();
        let history = match req_conversation {
            Some(id) => {
                let conn = state.db.lock().unwrap();
                crate::talk::history(&conn, id, TALK_HISTORY_LIMIT)?
            }
            None => Vec::new(),
        };
        let out = crate::agent::run_session(
            &deps,
            user.id,
            &user.username,
            crate::tools::SessionKind::Talk,
            now,
            &history,
            &message,
        )?;
        let reply = if out.reply.trim().is_empty() {
            crate::EMPTY_REPLY_FALLBACK.to_string()
        } else {
            out.reply.clone()
        };
        let conn = state.db.lock().unwrap();
        let conv_id = match req_conversation {
            Some(id) => id,
            None => crate::talk::create(&conn, user.id, &crate::talk::title_from(&message), now)?,
        };
        crate::talk::append_text(&conn, conv_id, "user", &message, now)?;
        for s in &out.steps {
            crate::talk::append_tool(&conn, conv_id, &s.name, &s.args, &s.result, s.is_error, now)?;
        }
        crate::talk::append_text(&conn, conv_id, "assistant", &reply, now)?;
        crate::talk::touch(&conn, conv_id, now)?;
        Ok::<_, anyhow::Error>((conv_id, reply, out.steps))
    })
    .await;
    match result {
        Ok(Ok((conv_id, reply, steps))) => {
            let steps: Vec<_> = steps
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "name": s.name,
                        "args": s.args,
                        "result": s.result,
                        "is_error": s.is_error,
                    })
                })
                .collect();
            Json(serde_json::json!({
                "conversation_id": conv_id,
                "reply": reply,
                "steps": steps,
            }))
            .into_response()
        }
        Ok(Err(e)) => {
            {
                let conn = err_db.lock().unwrap();
                let _ = crate::log::record(&conn, Some(uid), "talk_error", &e.to_string());
            }
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": "the assistant is unavailable; try again" })),
            )
                .into_response()
        }
        Err(e) => {
            {
                let conn = err_db.lock().unwrap();
                let _ = crate::log::record(
                    &conn,
                    Some(uid),
                    "talk_error",
                    &format!("talk task failed: {e}"),
                );
            }
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

const MAX_CONVERSATION_TITLE: usize = 120;

/// A conversation belonging to someone else is reported as absent, so the API
/// never confirms that an id exists to a user who cannot see it.
fn conversation_not_found() -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": "conversation not found" })),
    )
        .into_response()
}

async fn conversations_list(user: CurrentUser, State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    let mut stmt = match conn.prepare(
        "SELECT id, title, updated_at FROM conversations
         WHERE user_id = ?1 ORDER BY updated_at DESC, id DESC",
    ) {
        Ok(s) => s,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let rows: Result<Vec<serde_json::Value>, _> = stmt
        .query_map([user.id], |r| {
            Ok(serde_json::json!({
                "id": r.get::<_, i64>(0)?,
                "title": r.get::<_, String>(1)?,
                "updated_at": r.get::<_, String>(2)?,
            }))
        })
        .and_then(|m| m.collect());
    match rows {
        Ok(v) => Json(v).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
struct RenameConversationReq {
    title: String,
}

async fn conversation_rename(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<RenameConversationReq>,
) -> impl IntoResponse {
    let title = req.title.trim();
    if title.is_empty() || title.chars().count() > MAX_CONVERSATION_TITLE {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "title must be non-blank and at most 120 characters"
            })),
        )
            .into_response();
    }
    let conn = state.db.lock().unwrap();
    match conn.execute(
        "UPDATE conversations SET title = ?1 WHERE id = ?2 AND user_id = ?3",
        (title, id, user.id),
    ) {
        Ok(0) => conversation_not_found(),
        Ok(_) => StatusCode::OK.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn conversation_delete(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match conn.execute(
        "DELETE FROM conversations WHERE id = ?1 AND user_id = ?2",
        (id, user.id),
    ) {
        Ok(0) => conversation_not_found(),
        Ok(_) => StatusCode::OK.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn conversation_messages(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::talk::owned(&conn, user.id, id) {
        Ok(true) => {}
        Ok(false) => return conversation_not_found(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
    let mut stmt = match conn.prepare(
        "SELECT id, role, content, tool_name, tool_args, is_error, created_at
         FROM talk_messages WHERE conversation_id = ?1 ORDER BY id",
    ) {
        Ok(s) => s,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let rows: Result<Vec<serde_json::Value>, _> = stmt
        .query_map([id], |r| {
            Ok(serde_json::json!({
                "id": r.get::<_, i64>(0)?,
                "role": r.get::<_, String>(1)?,
                "content": r.get::<_, String>(2)?,
                "tool_name": r.get::<_, Option<String>>(3)?,
                "tool_args": r.get::<_, Option<String>>(4)?,
                "is_error": r.get::<_, bool>(5)?,
                "created_at": r.get::<_, String>(6)?,
            }))
        })
        .and_then(|m| m.collect());
    match rows {
        Ok(v) => Json(v).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

const MAX_DISPLAY_NAME: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsPatch {
    display_name: Option<String>,
    timezone: Option<String>,
    nightly_time: Option<String>,
    template: Option<String>,
}

fn settings_body(cfg: &crate::config::UserConfig) -> serde_json::Value {
    serde_json::json!({
        "display_name": cfg.display_name,
        "timezone": cfg.timezone,
        "nightly_time": cfg.nightly_time,
        "template": cfg.template,
    })
}

fn invalid_field(field: &str, requirement: &str) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": format!("{field} {requirement}") })),
    )
        .into_response()
}

/// The effective settings plus the two closed choice lists the client needs to
/// render them.
async fn settings_get(user: CurrentUser, State(state): State<AppState>) -> impl IntoResponse {
    let cfg = match crate::config::UserConfig::load(&state.config_dir, &user.username) {
        Ok(c) => c,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let mut zones: Vec<String> = jiff::tz::db()
        .available()
        .map(|n| n.as_str().to_string())
        .collect();
    zones.sort_unstable();
    let templates = crate::templates::available(&state.config_dir, &user.username);
    let mut body = settings_body(&cfg);
    body["templates"] = serde_json::json!(templates);
    body["timezones"] = serde_json::json!(zones);
    Json(body).into_response()
}

/// Merges the supplied subset into the effective values and rewrites the user's
/// file, rejecting the first invalid field without touching disk. The read,
/// merge and write run under the DB lock: settings writes are rare, and the
/// guard is the cheapest serializer that stops two concurrent PUTs from each
/// writing a file built from the values they read before the other landed.
async fn settings_put(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<SettingsPatch>,
) -> impl IntoResponse {
    let templates = crate::templates::available(&state.config_dir, &user.username);
    let _serializer = state.db.lock().unwrap();
    let mut cfg = match crate::config::UserConfig::load(&state.config_dir, &user.username) {
        Ok(c) => c,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if let Some(name) = req.display_name {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > MAX_DISPLAY_NAME {
            return invalid_field(
                "display_name",
                &format!("must be non-blank and at most {MAX_DISPLAY_NAME} characters"),
            );
        }
        cfg.display_name = name.to_string();
    }
    if let Some(tz) = req.timezone {
        if jiff::tz::TimeZone::get(&tz).is_err() {
            return invalid_field("timezone", "is not a known IANA timezone");
        }
        cfg.timezone = tz;
    }
    if let Some(time) = req.nightly_time {
        if !crate::templates::valid_time(&time) {
            return invalid_field("nightly_time", "must be a zero-padded 24-hour HH:MM");
        }
        cfg.nightly_time = time;
    }
    if let Some(template) = req.template {
        if !templates.contains(&template) {
            return invalid_field("template", "is not one of the available templates");
        }
        cfg.template = template;
    }
    match cfg.save(&state.config_dir, &user.username) {
        Ok(()) => Json(settings_body(&cfg)).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

const MAX_PROMPT_BYTES: usize = 32 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptPut {
    content: String,
}

/// The effective prompt plus whether it comes from the user's own override.
fn prompt_body(state: &AppState, user: &str, name: &str) -> axum::response::Response {
    match crate::prompts::load(&state.config_dir, user, name) {
        Ok(content) => Json(serde_json::json!({
            "name": name,
            "content": content,
            "custom": crate::prompts::custom(&state.config_dir, user, name),
        }))
        .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// A name outside `EDITABLE` is rejected before it can reach the filesystem, so
/// no traversal or probe of an arbitrary path is possible through these routes.
fn editable(name: &str) -> bool {
    crate::prompts::EDITABLE.contains(&name)
}

async fn prompt_get(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if !editable(&name) {
        return StatusCode::NOT_FOUND.into_response();
    }
    prompt_body(&state, &user.username, &name)
}

async fn prompt_put(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<PromptPut>,
) -> impl IntoResponse {
    if !editable(&name) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if req.content.trim().is_empty() || req.content.len() > MAX_PROMPT_BYTES {
        return invalid_field(
            "content",
            &format!("must be non-blank and at most {MAX_PROMPT_BYTES} bytes"),
        );
    }
    match crate::prompts::save(&state.config_dir, &user.username, &name, &req.content) {
        Ok(()) => prompt_body(&state, &user.username, &name),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn prompt_delete(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if !editable(&name) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match crate::prompts::reset(&state.config_dir, &user.username, &name) {
        Ok(()) => prompt_body(&state, &user.username, &name),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Every field arrives as a string so that a blank value means "absent" for all
/// three alike; `limit` is parsed in the handler to keep that symmetry.
#[derive(Deserialize)]
struct MemoryQuery {
    category: Option<String>,
    q: Option<String>,
    limit: Option<String>,
}

const MEMORY_LIMIT_DEFAULT: usize = 100;
const MEMORY_LIMIT_MAX: usize = 200;

fn blank_as_none(s: Option<&String>) -> Option<&str> {
    s.map(|s| s.trim()).filter(|s| !s.is_empty())
}

fn memory_error(message: &str) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

/// Browses the user's live facts, newest first; a non-blank `q` switches to
/// lexical search instead (no embedding is computed for a browse request, so
/// the vector arm stays out of it) and the category filter no longer applies.
async fn memory_list(
    user: CurrentUser,
    State(state): State<AppState>,
    Query(q): Query<MemoryQuery>,
) -> impl IntoResponse {
    let limit = match blank_as_none(q.limit.as_ref()) {
        Some(raw) => match raw.parse::<usize>() {
            Ok(n) => n,
            Err(_) => return memory_error("limit must be a number"),
        },
        None => MEMORY_LIMIT_DEFAULT,
    }
    .clamp(1, MEMORY_LIMIT_MAX);
    let category = blank_as_none(q.category.as_ref());
    if category.is_some_and(|c| !crate::memory::CATEGORIES.contains(&c)) {
        return memory_error("unknown category");
    }
    let conn = state.db.lock().unwrap();
    let hits = match blank_as_none(q.q.as_ref()) {
        Some(search) => crate::memory::query(&conn, &user.username, search, limit as i64, None),
        None => crate::memory::list(&conn, &user.username, category, limit),
    };
    match hits {
        Ok(items) => Json(serde_json::json!({ "items": items })).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// A fact of another user is indistinguishable from one that does not exist:
/// the id is only ever looked up under the caller's own memory root.
async fn memory_read(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if !crate::memory::valid_id(&id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match crate::memory::read(&state.data_dir, &user.username, &id) {
        Ok(Some(f)) => Json(serde_json::json!({
            "id": f.id,
            "category": f.category,
            "summary": f.summary,
            "body": f.body,
            "supersedes": f.supersedes,
            "created": f.created,
            "archived": f.archived,
        }))
        .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
struct PlanQuery {
    date: Option<String>,
}

/// Generates the plan for `date` (default: today in the user's configured
/// timezone) if it does not exist yet, then returns its events.
async fn plan_today(
    user: CurrentUser,
    State(state): State<AppState>,
    Query(q): Query<PlanQuery>,
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
        None => match jiff::tz::TimeZone::get(&ucfg.timezone) {
            Ok(tz) => jiff::Timestamp::now().to_zoned(tz).date(),
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
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
struct DebriefQuery {
    date: Option<String>,
}

/// Reads the stored debrief for `date` (default: today in the user's
/// configured timezone); the nightly job is the only writer.
async fn debrief(
    user: CurrentUser,
    State(state): State<AppState>,
    Query(q): Query<DebriefQuery>,
) -> impl IntoResponse {
    let date = match q.date {
        Some(d) => match d.parse::<jiff::civil::Date>() {
            Ok(d) => d.to_string(),
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        },
        None => {
            let ucfg = match crate::config::UserConfig::load(&state.config_dir, &user.username) {
                Ok(c) => c,
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };
            let tz = match jiff::tz::TimeZone::get(&ucfg.timezone) {
                Ok(tz) => tz,
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };
            jiff::Timestamp::now().to_zoned(tz).date().to_string()
        }
    };
    let conn = state.db.lock().unwrap();
    let row = conn
        .query_row(
            "SELECT date, content FROM debriefs WHERE user_id = ?1 AND date = ?2",
            (user.id, &date),
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional();
    match row {
        Ok(Some((date, content))) => {
            Json(serde_json::json!({ "date": date, "content": content })).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
struct ShiftReq {
    minutes: i64,
}

async fn event_shift(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<ShiftReq>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::plan::shift(&conn, user.id, id, req.minutes) {
        Ok(Some(())) => StatusCode::OK.into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(crate::plan::ShiftError::OutOfWindow { .. }) => StatusCode::BAD_REQUEST.into_response(),
        Err(crate::plan::ShiftError::Decided { .. }) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
struct SnoozeReq {
    minutes: i64,
}

/// The range is checked here so a bad `minutes` is a 400 while a failure inside
/// `plan::snooze` — which rejects the same range as defense in depth — stays a
/// 500; a settled `done`/`dropped` event is a 409, as on the shift route.
async fn event_snooze(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<SnoozeReq>,
) -> impl IntoResponse {
    if !(1..=24 * 60).contains(&req.minutes) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let conn = state.db.lock().unwrap();
    match crate::plan::snooze(&conn, user.id, id, req.minutes) {
        Ok(Some(())) => StatusCode::OK.into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(crate::plan::ShiftError::Decided { .. }) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn event_done(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    event_set(&state, &user, id, "done")
}

async fn event_drop(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    event_set(&state, &user, id, "dropped")
}

fn event_set(state: &AppState, user: &CurrentUser, id: i64, status: &str) -> axum::response::Response {
    let conn = state.db.lock().unwrap();
    match crate::plan::set_status(&conn, user.id, id, status) {
        Ok(Some(())) => StatusCode::OK.into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
struct SubKeys {
    p256dh: String,
    auth: String,
}

#[derive(Deserialize)]
struct SubscribeReq {
    endpoint: String,
    keys: SubKeys,
}

const MAX_ENDPOINT_LEN: usize = 2048;
const MAX_P256DH_LEN: usize = 256;
const MAX_AUTH_LEN: usize = 64;

async fn push_subscribe(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<SubscribeReq>,
) -> impl IntoResponse {
    let scheme_ok = req.endpoint.starts_with("https://") || req.endpoint.starts_with("http://");
    if !scheme_ok
        || req.endpoint.len() > MAX_ENDPOINT_LEN
        || req.keys.p256dh.len() > MAX_P256DH_LEN
        || req.keys.auth.len() > MAX_AUTH_LEN
        || req.keys.p256dh.is_empty()
        || req.keys.auth.is_empty()
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let conn = state.db.lock().unwrap();
    match crate::push_subs::add(&conn, user.id, &req.endpoint, &req.keys.p256dh, &req.keys.auth) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
struct UnsubscribeReq {
    endpoint: String,
}

async fn push_unsubscribe(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<UnsubscribeReq>,
) -> impl IntoResponse {
    if req.endpoint.len() > MAX_ENDPOINT_LEN {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let conn = state.db.lock().unwrap();
    match crate::push_subs::remove(&conn, user.id, &req.endpoint) {
        Ok(true) => StatusCode::OK.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn vapid_public_key(_user: CurrentUser, State(state): State<AppState>) -> impl IntoResponse {
    match &state.vapid_public_key {
        Some(k) => Json(serde_json::json!({ "key": k })).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Bridges hub messages to the socket; inbound frames are drained and ignored
/// (delivery is one-way in v1), and either side closing tears the bridge down.
async fn ws_connect(
    user: CurrentUser,
    State(state): State<AppState>,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_pump(socket, state.hub.clone(), user.id))
}

const WS_PING_EVERY: std::time::Duration = std::time::Duration::from_secs(30);
const WS_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Half-open connections (NAT drop, lid close) never produce a FIN, and an
/// unbounded sender only fails once its receiver is dropped, so a silent socket
/// would stay registered and keep absorbing deliveries. Pings force the peer to
/// speak; going `WS_IDLE_TIMEOUT` without any inbound frame tears the bridge
/// down so the dispatcher's ladder can fall through to another channel.
async fn ws_pump(
    mut socket: axum::extract::ws::WebSocket,
    hub: std::sync::Arc<crate::channels::ws::ClientHub>,
    user_id: i64,
) {
    use axum::extract::ws::Message;

    let (conn_id, mut rx) = hub.register(user_id);
    let mut ping = tokio::time::interval(WS_PING_EVERY);
    ping.tick().await;
    let mut last_inbound = tokio::time::Instant::now();
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(text) => {
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            inbound = socket.recv() => match inbound {
                Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => last_inbound = tokio::time::Instant::now(),
                _ => break,
            },
            _ = ping.tick() => {
                if last_inbound.elapsed() > WS_IDLE_TIMEOUT
                    || socket.send(Message::Ping(Vec::new().into())).await.is_err()
                {
                    break;
                }
            },
        }
    }
    hub.unregister(user_id, conn_id);
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_limit() -> i64 {
    100
}

async fn admin_log(
    user: CurrentUser,
    State(state): State<AppState>,
    Query(q): Query<LogQuery>,
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
