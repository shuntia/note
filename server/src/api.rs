use crate::auth::{self, CurrentUser, TaskPrincipal};
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
        .route("/api/tasks/{id}", patch(tasks_update).delete(tasks_delete))
        .route("/api/tasks/{id}/split", post(task_split))
        .route("/api/tasks/{id}/flatten", post(task_flatten))
        .route("/api/tasks/{id}/agent", post(task_agent))
        .route("/api/tokens", get(tokens_list).post(tokens_create))
        .route("/api/tokens/{id}", axum::routing::delete(tokens_revoke))
        .route("/api/talk", post(talk))
        .route("/api/conversations", get(conversations_list))
        .route(
            "/api/conversations/{id}",
            patch(conversation_rename).delete(conversation_delete),
        )
        .route("/api/conversations/{id}/messages", get(conversation_messages))
        .route("/api/settings", get(settings_get).put(settings_put))
        .route("/api/notify/test", post(notify_test))
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
        .route("/api/events/{id}/alert", post(event_alert))
        .route("/api/events/{id}/move_tomorrow", post(event_move_tomorrow))
        .route("/api/ws", get(ws_connect))
        .route("/api/push/subscribe", post(push_subscribe))
        .route("/api/push/unsubscribe", post(push_unsubscribe))
        .route("/api/push/vapid_public_key", get(vapid_public_key))
        .nest("/api/security", crate::security::routes())
        .nest("/api/admin", crate::admin::routes())
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

/// Admission first, then the hash, then the limiter: the semaphore bounds what
/// an unauthenticated flood can spend, and counting an attempt only once a
/// verification has failed means guessing traffic against a username can never
/// lock its owner out.
async fn login(State(state): State<AppState>, Json(req): Json<LoginReq>) -> impl IntoResponse {
    let Ok(slot) = state.login_slots.clone().try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "2")],
            Json(serde_json::json!({ "error": "too many sign-ins in flight; try again" })),
        )
            .into_response();
    };
    let db = state.db.clone();
    let (username, password) = (req.username.clone(), req.password);
    let result = tokio::task::spawn_blocking(move || {
        // held inside the closure so a cancelled request still occupies the slot
        // until the hash it started actually finishes
        let _slot = slot;
        auth::login(&db, &username, &password)
    })
    .await;
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
        Ok(Ok(None)) => {
            if !state.login_limiter.try_attempt(&req.username, jiff::Timestamp::now()) {
                return StatusCode::TOO_MANY_REQUESTS.into_response();
            }
            StatusCode::UNAUTHORIZED.into_response()
        }
        Ok(Err(e)) => log_login_error(&state, &e.to_string()),
        Err(e) => log_login_error(&state, &format!("login task failed: {e}")),
    }
}

fn log_login_error(state: &AppState, detail: &str) -> axum::response::Response {
    let conn = state.db();
    let _ = crate::log::record(&conn, None, "login_error", detail);
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

async fn logout(State(state): State<AppState>, headers: axum::http::HeaderMap) -> impl IntoResponse {
    let jar = axum_extra::extract::CookieJar::from_headers(&headers);
    if let Some(c) = jar.get("session") {
        let conn = state.db();
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

async fn tasks_list(user: TaskPrincipal, State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.db();
    match crate::tasks::list(&conn, user.id) {
        Ok(ts) => Json(ts).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn tasks_create(
    user: TaskPrincipal,
    State(state): State<AppState>,
    Json(req): Json<crate::tasks::NewTask>,
) -> impl IntoResponse {
    let conn = state.db();
    match crate::tasks::create(&conn, user.id, req, "manual", crate::tasks::Actor::User) {
        Ok(t) => Json(t).into_response(),
        Err(e) => task_error(e),
    }
}

async fn tasks_update(
    user: TaskPrincipal,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(patch): Json<crate::tasks::TaskPatch>,
) -> impl IntoResponse {
    let conn = state.db();
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
    user: TaskPrincipal,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<SplitReq>,
) -> impl IntoResponse {
    let conn = state.db();
    match crate::tasks::split(&conn, user.id, id, req.steps, crate::tasks::Actor::User) {
        Ok(Some(n)) => Json(n).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => task_error(e),
    }
}

async fn task_flatten(
    user: TaskPrincipal,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.db();
    match crate::tasks::flatten(&conn, user.id, id) {
        Ok(Some((task, removed))) => {
            Json(serde_json::json!({ "task": task, "removed": removed })).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => task_error(e),
    }
}

async fn tasks_delete(
    user: TaskPrincipal,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.db();
    match crate::tasks::delete(&conn, user.id, id) {
        Ok(true) => StatusCode::NO_CONTENT,
        Ok(false) => StatusCode::NOT_FOUND,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

const MAX_BRIEF_CONTEXT: usize = 32 * 1024;

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct BriefReq {
    context: String,
}

fn brief_error(status: StatusCode, message: &str) -> axum::response::Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// The per-user daily spend ceiling both agent routes sit behind. Sessions are
/// counted from the `agent_session` rows they already write, so the ceiling
/// needs no state of its own.
fn daily_cap_reached(state: &AppState, user_id: i64) -> bool {
    let cap = state.agent_sessions_per_day;
    if cap == 0 {
        return false;
    }
    let since = jiff::Timestamp::now() - jiff::Span::new().hours(24);
    let conn = state.db();
    crate::log::agent_sessions_since(&conn, user_id, since).unwrap_or(0) >= cap
}

fn daily_cap_response() -> axum::response::Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({ "error": "daily session limit reached" })),
    )
        .into_response()
}

/// Turns a busy gate into the response both agent routes give: one session per
/// user, `MAX_CONCURRENT_TALKS` across the server.
fn session_busy_response(busy: crate::TalkBusy) -> axum::response::Response {
    match busy {
        crate::TalkBusy::UserBusy => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": "a session is already in progress" })),
        )
            .into_response(),
        crate::TalkBusy::Full => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "5")],
            Json(serde_json::json!({ "error": "the server is at capacity" })),
        )
            .into_response(),
    }
}

fn log_brief_error(state: &AppState, user_id: i64, detail: &str) {
    let conn = state.db();
    let _ = crate::log::record(&conn, Some(user_id), "task_agent_error", detail);
}

/// The task as DATA for the model: everything the importer wrote, plus the
/// steps that already exist so a re-brief knows not to try splitting again.
fn brief_message(node: &crate::tasks::TaskNode, context: &str) -> String {
    let t = &node.task;
    let mut m = format!(
        "Task id: {}\nTitle: {}\nState: {}\nDescription: {}\nNotes: {}\n",
        t.id, t.title, t.state, t.description, t.notes
    );
    if !node.children.is_empty() {
        m.push_str("Steps:\n");
        for c in &node.children {
            m.push_str(&format!("- {}\n", c.title));
        }
    }
    if !context.trim().is_empty() {
        m.push_str("\nContext:\n");
        m.push_str(context);
        m.push('\n');
    }
    m
}

/// Briefs one imported task in a fresh agent session scoped to that task, with
/// no history and nothing kept as a conversation. Any failure restores the task
/// and its steps exactly as they were, so a caller can retry on the same id.
async fn task_agent(
    user: TaskPrincipal,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let req: BriefReq = if body.iter().all(|b| b.is_ascii_whitespace()) {
        BriefReq::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => return brief_error(StatusCode::BAD_REQUEST, &e.to_string()),
        }
    };
    if req.context.len() > MAX_BRIEF_CONTEXT {
        return brief_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("context must be at most {MAX_BRIEF_CONTEXT} bytes"),
        );
    }
    if daily_cap_reached(&state, user.id) {
        return daily_cap_response();
    }
    let permit = match state.talk_gate.try_enter(user.id) {
        Ok(p) => p,
        Err(busy) => return session_busy_response(busy),
    };
    let token_id = match user.via {
        auth::Credential::Token(id) => Some(id),
        auth::Credential::Session => None,
    };
    let (opening, snap) = {
        let conn = state.db();
        let failed = |detail: String| {
            let _ = crate::log::record(&conn, Some(user.id), "task_agent_error", &detail);
            brief_error(StatusCode::INTERNAL_SERVER_ERROR, "the task could not be briefed")
        };
        let node = match crate::tasks::node(&conn, user.id, id) {
            Ok(Some(n)) => n,
            Ok(None) => return brief_error(StatusCode::NOT_FOUND, "task not found"),
            Err(e) => return failed(e.to_string()),
        };
        if node.task.parent_id.is_some() {
            return brief_error(
                StatusCode::CONFLICT,
                "only a top-level task can be briefed, not one of its steps",
            );
        }
        match crate::tasks::snapshot(&conn, user.id, id) {
            Ok(snap) => (brief_message(&node, &req.context), snap),
            Err(e) => return failed(e.to_string()),
        }
    };

    let rollback = snap.clone();
    let err_state = state.clone();
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
            task_scope: Some(id),
            token_id,
        };
        let session = crate::agent::run_session(
            &deps,
            user.id,
            &user.username,
            crate::tools::SessionKind::Import,
            jiff::Timestamp::now(),
            &[],
            &opening,
        );
        let conn = state.db();
        // `None` is a rolled-back session: the caller answers 502.
        match session {
            Ok(out) => match crate::tasks::node(&conn, user.id, id) {
                Ok(Some(node)) => Ok(Some((out.steps, node))),
                Ok(None) => {
                    crate::tasks::restore(&conn, &snap)?;
                    anyhow::bail!("task {id} disappeared during the session")
                }
                Err(e) => {
                    crate::tasks::restore(&conn, &snap)?;
                    Err(e.into())
                }
            },
            Err(e) => {
                crate::tasks::restore(&conn, &snap)?;
                let _ =
                    crate::log::record(&conn, Some(user.id), "task_agent_error", &e.to_string());
                Ok(None)
            }
        }
    })
    .await;

    match result {
        Ok(Ok(Some((steps, node)))) => {
            let outcome = if node.task.state == "dropped" {
                "dropped"
            } else if steps.iter().any(|s| !s.is_error) {
                "briefed"
            } else {
                "unchanged"
            };
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
                "task_id": id,
                "outcome": outcome,
                "steps": steps,
                "task": node,
            }))
            .into_response()
        }
        Ok(Ok(None)) => {
            brief_error(StatusCode::BAD_GATEWAY, "the assistant is unavailable; try again")
        }
        Ok(Err(e)) => {
            log_brief_error(&err_state, user.id, &e.to_string());
            brief_error(StatusCode::INTERNAL_SERVER_ERROR, "the task could not be briefed")
        }
        Err(e) => {
            let detail = format!("brief task failed: {e}");
            {
                let conn = err_state.db();
                let _ = crate::tasks::restore(&conn, &rollback);
            }
            log_brief_error(&err_state, user.id, &detail);
            brief_error(StatusCode::INTERNAL_SERVER_ERROR, "the task could not be briefed")
        }
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
struct NewTokenReq {
    name: String,
}

async fn tokens_list(user: CurrentUser, State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.db();
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
    let conn = state.db();
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
    let conn = state.db();
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
        let conn = state.db();
        match crate::talk::owned(&conn, user.id, id) {
            Ok(true) => {}
            Ok(false) => return conversation_not_found(),
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
    if daily_cap_reached(&state, user.id) {
        return daily_cap_response();
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
        Err(busy) => return session_busy_response(busy),
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
            task_scope: None,
            token_id: None,
        };
        let now = jiff::Timestamp::now();
        let history = match req_conversation {
            Some(id) => {
                let conn = state.db();
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
        let conn = state.db();
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
                let conn = crate::db_guard(&err_db);
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
                let conn = crate::db_guard(&err_db);
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
    let conn = state.db();
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
    let conn = state.db();
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
    let conn = state.db();
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
    let conn = state.db();
    match crate::talk::owned(&conn, user.id, id) {
        Ok(true) => {}
        Ok(false) => return conversation_not_found(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
    match crate::talk::messages_json(&conn, id) {
        Ok(v) => Json(v).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

const MAX_DISPLAY_NAME: usize = 64;
const COUNTERS: [&str; 2] = ["remaining", "elapsed"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AlertPatch {
    index: usize,
    alert: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsPatch {
    display_name: Option<String>,
    timezone: Option<String>,
    nightly_time: Option<String>,
    template: Option<String>,
    show_arc_between_sessions: Option<bool>,
    counter: Option<String>,
    nightly_enabled: Option<bool>,
    checkins_enabled: Option<bool>,
    ntfy_topic: Option<String>,
    alerts: Option<Vec<AlertPatch>>,
}

/// The prefix the user's default topic is built from, whether or not the
/// channel is configured, so Settings can always name the topic to subscribe to.
fn ntfy_topic_prefix(state: &AppState) -> &str {
    state
        .ntfy_topic_prefix
        .as_deref()
        .unwrap_or(crate::config::DEFAULT_NTFY_TOPIC_PREFIX)
}

fn settings_body(
    state: &AppState,
    cfg: &crate::config::UserConfig,
    user: &CurrentUser,
    schedule: Vec<crate::templates::ScheduleRow>,
) -> serde_json::Value {
    let category = user.category.as_str();
    let features = cfg.features(category);
    serde_json::json!({
        "display_name": cfg.display_name,
        "timezone": cfg.timezone,
        "nightly_time": cfg.nightly_time,
        "template": cfg.template,
        "show_arc_between_sessions": cfg.show_arc_between_sessions,
        "counter": cfg.counter,
        "category": category,
        "nightly_enabled": features.nightly,
        "checkins_enabled": features.checkins,
        "ntfy_enabled": state.ntfy_topic_prefix.is_some(),
        "ntfy_topic": cfg.ntfy_topic_for(ntfy_topic_prefix(state), &user.username),
        "schedule": schedule,
    })
}

/// A template that no longer parses contributes no rows rather than failing the
/// request: the user needs Settings to reach the picker and choose another one.
fn schedule_rows(
    state: &AppState,
    user: &str,
    template: &str,
) -> Vec<crate::templates::ScheduleRow> {
    crate::templates::Template::load(&state.config_dir, user, template)
        .map(|t| t.rows())
        .unwrap_or_default()
}

fn invalid_field(field: &str, requirement: &str) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": format!("{field} {requirement}") })),
    )
        .into_response()
}

fn unprocessable_field(field: &str, requirement: &str) -> axum::response::Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
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
    let schedule = schedule_rows(&state, &user.username, &cfg.template);
    let mut body = settings_body(&state, &cfg, &user, schedule);
    body["templates"] = serde_json::json!(templates);
    body["timezones"] = serde_json::json!(zones);
    Json(body).into_response()
}

/// Merges the supplied subset into the effective values and rewrites the user's
/// file, rejecting the first invalid field without touching disk. Bell toggles
/// apply to the template the request leaves selected, and land in the user's own
/// copy of it. The read,
/// merge and write run under the DB lock: settings writes are rare, and the
/// guard is the cheapest serializer that stops two concurrent PUTs from each
/// writing a file built from the values they read before the other landed.
async fn settings_put(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<SettingsPatch>,
) -> impl IntoResponse {
    let templates = crate::templates::available(&state.config_dir, &user.username);
    let _serializer = state.db();
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
    if let Some(show) = req.show_arc_between_sessions {
        cfg.show_arc_between_sessions = show;
    }
    if let Some(counter) = req.counter {
        if !COUNTERS.contains(&counter.as_str()) {
            return invalid_field("counter", "must be remaining or elapsed");
        }
        cfg.counter = counter;
    }
    if let Some(on) = req.nightly_enabled {
        cfg.nightly_enabled = Some(on);
    }
    if let Some(on) = req.checkins_enabled {
        cfg.checkins_enabled = Some(on);
    }
    if let Some(topic) = req.ntfy_topic {
        let topic = topic.trim();
        if topic.is_empty() {
            cfg.ntfy_topic = None;
        } else if !crate::channels::ntfy::valid_topic(topic) {
            return unprocessable_field(
                "ntfy_topic",
                "must be 1 to 64 characters of letters, digits, _ or -",
            );
        } else {
            cfg.ntfy_topic = Some(topic.to_string());
        }
    }
    if let Some(alerts) = req.alerts {
        let changes: Vec<(usize, bool)> = alerts.iter().map(|a| (a.index, a.alert)).collect();
        if let Err(e) =
            crate::templates::set_alerts(&state.config_dir, &user.username, &cfg.template, &changes)
        {
            return invalid_field("alerts", &e.to_string());
        }
    }
    let schedule = schedule_rows(&state, &user.username, &cfg.template);
    match cfg.save(&state.config_dir, &user.username) {
        Ok(()) => Json(settings_body(&state, &cfg, &user, schedule)).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Runs the delivery ladder the way a fired event does, so the reply names the
/// channel that would actually reach the user right now.
async fn notify_test(user: CurrentUser, State(state): State<AppState>) -> impl IntoResponse {
    let msg = crate::channels::OutboundMessage {
        title: "Note".into(),
        body: "Test notification".into(),
        urgency: crate::channels::Urgency::Normal,
        event_id: None,
    };
    let db = state.db.clone();
    let ladder = state.channels.clone();
    let via = tokio::task::spawn_blocking(move || {
        crate::channels::deliver_via(&db, &ladder, user.id, &user.username, &msg)
    })
    .await;
    match via {
        Ok(Some(name)) => Json(serde_json::json!({ "via": name })).into_response(),
        Ok(None) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": "no channel could reach you" })),
        )
            .into_response(),
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
    let conn = state.db();
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
    let conn = state.db();
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
    let conn = state.db();
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
    let conn = state.db();
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
    let conn = state.db();
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AlertReq {
    alert: bool,
}

async fn event_alert(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<AlertReq>,
) -> impl IntoResponse {
    let conn = state.db();
    match crate::plan::set_alert(&conn, user.id, id, req.alert) {
        Ok(Some(())) => StatusCode::OK.into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(crate::plan::AlertRefused::Block) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn event_move_tomorrow(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let ucfg = match crate::config::UserConfig::load(&state.config_dir, &user.username) {
        Ok(c) => c,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let tmpl = match crate::templates::Template::load(&state.config_dir, &user.username, &ucfg.template) {
        Ok(t) => t,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let conn = state.db();
    match crate::plan::move_to_tomorrow(&conn, user.id, id, &tmpl) {
        Ok(Some((event_id, date))) => {
            Json(serde_json::json!({ "event_id": event_id, "date": date.to_string() })).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(crate::plan::ShiftError::Decided { .. }) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn event_set(state: &AppState, user: &CurrentUser, id: i64, status: &str) -> axum::response::Response {
    let conn = state.db();
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

/// The server posts to a stored endpoint on every delivery, so the host is
/// vetted here — once, before it is stored — rather than at delivery time.
async fn push_subscribe(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<SubscribeReq>,
) -> impl IntoResponse {
    if req.endpoint.len() > MAX_ENDPOINT_LEN
        || req.keys.p256dh.len() > MAX_P256DH_LEN
        || req.keys.auth.len() > MAX_AUTH_LEN
        || req.keys.p256dh.is_empty()
        || req.keys.auth.is_empty()
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let endpoint = req.endpoint.clone();
    let vetted = tokio::task::spawn_blocking(move || crate::net::push_endpoint_ok(&endpoint)).await;
    match vetted {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({ "error": reason })),
            )
                .into_response()
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
    let conn = state.db();
    match crate::push_subs::add(&conn, user.id, &req.endpoint, &req.keys.p256dh, &req.keys.auth) {
        Ok(crate::push_subs::Added::Stored) => StatusCode::OK.into_response(),
        Ok(crate::push_subs::Added::Taken) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "that endpoint belongs to another account" })),
        )
            .into_response(),
        Ok(crate::push_subs::Added::TooMany) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": format!("at most {} devices per account", crate::push_subs::MAX_PER_USER)
            })),
        )
            .into_response(),
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
    let conn = state.db();
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
    WsAdmission(user): WsAdmission,
    State(state): State<AppState>,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_pump(socket, state.hub.clone(), user.id))
}

/// A session allowed to open a socket: not started by a foreign page, and not
/// already holding the cap. It sits ahead of the upgrade extractor in the
/// handler's arguments so both refusals answer as plain HTTP.
struct WsAdmission(CurrentUser);

impl axum::extract::FromRequestParts<AppState> for WsAdmission {
    type Rejection = axum::response::Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if !crate::net::fetch_site_ok(&parts.headers) {
            return Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "error": "cross-site request refused" })),
            )
                .into_response());
        }
        let user = CurrentUser::from_request_parts(parts, state)
            .await
            .map_err(|s| s.into_response())?;
        if state.hub.at_capacity(user.id) {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({ "error": "too many open connections" })),
            )
                .into_response());
        }
        Ok(WsAdmission(user))
    }
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

    let Some((conn_id, mut rx)) = hub.register(user_id) else {
        return;
    };
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

