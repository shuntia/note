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
        .route(
            "/api/tasks/by-external/{external_id}",
            axum::routing::put(task_upsert).delete(task_delete_by_external),
        )
        .route("/api/tasks/{id}/split", post(task_split))
        .route("/api/tasks/{id}/flatten", post(task_flatten))
        .route("/api/tasks/{id}/agent", post(task_agent))
        .route("/api/agent/inbox", post(agent_inbox))
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
        .route(
            "/api/telegram/link",
            post(telegram_link).delete(telegram_unlink),
        )
        .route("/api/notify/test", post(notify_test))
        .route(
            "/api/prompts/{name}",
            get(prompt_get).put(prompt_put).delete(prompt_delete),
        )
        .route("/api/memory", get(memory_list))
        .route("/api/memory/{id}", get(memory_read))
        .route("/api/plan/today", get(plan_today))
        .route("/api/plan/range", get(plan_range))
        .route("/api/plan/{date}/allocate", post(plan_allocate))
        .route("/api/day/{date}", get(day_view))
        .route("/api/debrief", get(debrief))
        .route("/api/events/{id}/shift", post(event_shift))
        .route("/api/events/{id}/snooze", post(event_snooze))
        .route("/api/events/{id}/done", post(event_done))
        .route("/api/events/{id}/drop", post(event_drop))
        .route("/api/events/{id}/alert", post(event_alert))
        .route("/api/events/{id}/move_tomorrow", post(event_move_tomorrow))
        .route("/api/sessions", post(work_session_start))
        .route("/api/sessions/open", get(work_session_open))
        .route("/api/sessions/{id}/end", post(work_session_end))
        .route("/api/sessions/{id}/pause", post(work_session_pause))
        .route("/api/sessions/{id}/resume", post(work_session_resume))
        .route("/api/sessions/{id}/step", post(work_session_step))
        .route("/api/sessions/{id}/skip_break", post(work_session_skip_break))
        .route("/api/ws", get(ws_connect))
        .route("/api/push/subscribe", post(push_subscribe))
        .route("/api/push/unsubscribe", post(push_unsubscribe))
        .route("/api/push/vapid_public_key", get(vapid_public_key))
        .merge(calendar_router())
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

/// A cookie is the user typing; a token is a script mirroring another system,
/// so what it writes is an import and never passes for something the user did.
fn default_source(via: &auth::Credential) -> &'static str {
    match via {
        auth::Credential::Session => "manual",
        auth::Credential::Token(_) => "import",
    }
}

async fn tasks_create(
    user: TaskPrincipal,
    State(state): State<AppState>,
    Json(req): Json<crate::tasks::NewTask>,
) -> impl IntoResponse {
    if matches!(user.via, auth::Credential::Token(_)) && req.source.as_deref() == Some("manual") {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "a token cannot write source manual" })),
        )
            .into_response();
    }
    let conn = state.db();
    let source = default_source(&user.via);
    match crate::tasks::create(&conn, user.id, req, source, crate::tasks::Actor::User) {
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

/// Mirrors one task from another system. The body is a task without its
/// `external_id`, which the path carries; a task the user deleted is declined
/// rather than made again.
async fn task_upsert(
    user: TaskPrincipal,
    State(state): State<AppState>,
    Path(external_id): Path<String>,
    Json(req): Json<crate::tasks::NewTask>,
) -> impl IntoResponse {
    if req.external_id.as_deref().is_some_and(|id| id != external_id) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "external_id belongs to the path, not the body" })),
        )
            .into_response();
    }
    let conn = state.db();
    match crate::tasks::upsert(&conn, user.id, &external_id, req) {
        Ok(crate::tasks::Upsert::Created(n)) => (StatusCode::CREATED, Json(n)).into_response(),
        Ok(crate::tasks::Upsert::Updated(n)) => Json(n).into_response(),
        Ok(crate::tasks::Upsert::Declined { deleted_at }) => (
            StatusCode::GONE,
            Json(serde_json::json!({
                "error": "the user deleted this task; it is not recreated",
                "external_id": external_id,
                "deleted_at": deleted_at,
            })),
        )
            .into_response(),
        Err(e) => task_error(e),
    }
}

async fn task_delete_by_external(
    user: TaskPrincipal,
    State(state): State<AppState>,
    Path(external_id): Path<String>,
) -> impl IntoResponse {
    let conn = state.db();
    match crate::tasks::delete_by_external(&conn, user.id, &external_id) {
        Ok(true) => StatusCode::NO_CONTENT,
        Ok(false) => StatusCode::NOT_FOUND,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
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
pub(crate) fn daily_cap_reached(state: &AppState, user_id: i64) -> bool {
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
fn brief_message(node: &crate::tasks::TaskNode, context: &str, tz: &jiff::tz::TimeZone) -> String {
    let t = &node.task;
    let mut m = format!(
        "Task id: {}\nTitle: {}\nState: {}\nDescription: {}\nNotes: {}\n",
        t.id, t.title, t.state, t.description, t.notes
    );
    if let Some(at) = t.due_at.as_deref().and_then(|d| d.parse::<jiff::Timestamp>().ok()) {
        m.push_str(&format!(
            "Due: {}\n",
            at.to_zoned(tz.clone()).strftime("%Y-%m-%d %H:%M")
        ));
    }
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
            Err(_) => return brief_error(StatusCode::BAD_REQUEST, "malformed JSON body"),
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
        let tz = crate::config::UserConfig::load(&state.config_dir, &user.username)
            .ok()
            .and_then(|c| jiff::tz::TimeZone::get(&c.timezone).ok())
            .unwrap_or(jiff::tz::TimeZone::UTC);
        match crate::tasks::snapshot(&conn, user.id, id) {
            Ok(snap) => (brief_message(&node, &req.context, &tz), snap),
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
            inbox_source: None,
            memory_source: None,
            token_id,
            thread_note: None,
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

const MAX_SOURCE_ID: usize = 200;
const INBOX_KINDS: [&str; 2] = ["announcement", "material"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InboxReq {
    source_id: String,
    kind: String,
    #[serde(default)]
    context: String,
}

/// Source ids appear in the session's scope check and in the source map, so
/// they are held to a shape a caller cannot smuggle anything through.
fn valid_source_id(id: &str) -> bool {
    (1..=MAX_SOURCE_ID).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'.' | b'_' | b'-'))
}

/// Judges one learning-management item in a fresh agent session scoped to its
/// source id, with no history and nothing kept as a conversation. Memory is
/// written only by the terminal decision, so any failure before it leaves the
/// source's previous facts exactly as they were.
async fn agent_inbox(
    user: TaskPrincipal,
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let req: InboxReq = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(_) => return brief_error(StatusCode::BAD_REQUEST, "malformed JSON body"),
    };
    if !valid_source_id(&req.source_id) {
        return brief_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("source_id must be 1 to {MAX_SOURCE_ID} characters of A-Za-z0-9:._-"),
        );
    }
    if !INBOX_KINDS.contains(&req.kind.as_str()) {
        return brief_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "kind must be \"announcement\" or \"material\"",
        );
    }
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
    let source_id = req.source_id.clone();
    let opening = format!("Source: {}\nKind: {}\n\n{}", req.source_id, req.kind, req.context);
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
            task_scope: None,
            inbox_source: Some(req.source_id.clone()),
            memory_source: None,
            token_id,
            thread_note: None,
        };
        crate::agent::run_session(
            &deps,
            user.id,
            &user.username,
            crate::tools::SessionKind::Inbox,
            jiff::Timestamp::now(),
            &[],
            &opening,
        )
    })
    .await;

    let session = match result {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            log_inbox_error(&err_state, user.id, &e.to_string());
            return brief_error(StatusCode::BAD_GATEWAY, "the assistant is unavailable; try again");
        }
        Err(e) => {
            log_inbox_error(&err_state, user.id, &format!("inbox session failed: {e}"));
            return brief_error(StatusCode::INTERNAL_SERVER_ERROR, "the item could not be read");
        }
    };
    let Some(decision) = session
        .steps
        .iter()
        .rev()
        .find(|s| s.name == "inbox_decide" && !s.is_error)
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s.result).ok())
    else {
        log_inbox_error(&err_state, user.id, &format!("{source_id}: the session never decided"));
        return brief_error(StatusCode::BAD_GATEWAY, "the assistant reached no decision; try again");
    };
    let steps: Vec<_> = session
        .steps
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
        "source_id": source_id,
        "outcome": decision["outcome"],
        "reason": decision["reason"],
        "memory_ids": decision["memory_ids"],
        "steps": steps,
    }))
    .into_response()
}

fn log_inbox_error(state: &AppState, user_id: i64, detail: &str) {
    let conn = state.db();
    let _ = crate::log::record(&conn, Some(user_id), "agent_inbox_error", detail);
}

fn task_error(e: crate::tasks::UpdateError) -> axum::response::Response {
    use crate::tasks::UpdateError as E;
    match e {
        E::InvalidState(_) => StatusCode::BAD_REQUEST.into_response(),
        E::InvalidDuration(m) | E::InvalidHierarchy(m) | E::Invalid(m) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": m })),
        )
            .into_response(),
        E::NowFull(m) | E::ExternalIdTaken(m) => {
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

/// A session makes synchronous provider calls and blocking DB writes, so it
/// runs off the async executor.
async fn talk(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<TalkReq>,
) -> impl IntoResponse {
    use crate::talk::TurnError as E;
    let turn = crate::talk::run_turn(
        &state,
        user.id,
        &user.username,
        req.conversation_id,
        &req.message,
        crate::talk::Via::Web,
    )
    .await;
    match turn {
        Ok(turn) => {
            let steps: Vec<_> = turn
                .steps
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
                "conversation_id": turn.conversation_id,
                "reply": turn.reply,
                "steps": steps,
                "reasoning": turn.reasoning,
                "thought_ms": turn.thought_ms,
            }))
            .into_response()
        }
        Err(E::Blank) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "message must be non-blank and at most 16384 bytes" })),
        )
            .into_response(),
        Err(E::NotFound) => conversation_not_found(),
        Err(E::DailyCap) => daily_cap_response(),
        Err(E::Busy(crate::TalkBusy::UserBusy)) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "a reply is already in progress" })),
        )
            .into_response(),
        Err(E::Busy(busy)) => session_busy_response(busy),
        Err(E::Unavailable) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": "the assistant is unavailable; try again" })),
        )
            .into_response(),
        Err(E::Internal) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
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
        "SELECT id, title, updated_at, summary, via FROM conversations
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
                "summary": r.get::<_, Option<String>>(3)?,
                "via": r.get::<_, String>(4)?,
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
    triggers_per_day: Option<u32>,
    pomodoro_enabled: Option<bool>,
    pomodoro_work_min: Option<u32>,
    pomodoro_break_min: Option<u32>,
    alerts: Option<Vec<AlertPatch>>,
}

/// A day's worth of check-ins Note may start on its own; more than this and it
/// is not a companion any more.
const MAX_TRIGGERS_PER_DAY: u32 = 20;
const POMODORO_WORK_MIN: std::ops::RangeInclusive<u32> = 5..=120;
const POMODORO_BREAK_MIN: std::ops::RangeInclusive<u32> = 1..=60;

/// `telegram_linked` is read by the caller, which already holds the DB guard on
/// the write path.
fn settings_body(
    state: &AppState,
    cfg: &crate::config::UserConfig,
    user: &CurrentUser,
    schedule: Vec<crate::templates::ScheduleRow>,
    telegram_linked: bool,
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
        "telegram_enabled": state.telegram.is_some(),
        "telegram_linked": telegram_linked,
        "telegram_bot": state.telegram.as_ref().map(|ch| ch.bot()).unwrap_or_default(),
        "triggers_per_day": cfg.triggers_per_day(),
        "pomodoro_enabled": cfg.pomodoro_enabled(),
        "pomodoro_work_min": cfg.pomodoro_work_min(),
        "pomodoro_break_min": cfg.pomodoro_break_min(),
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
    let linked = {
        let conn = state.db();
        crate::telegram::chat_for_user(&conn, user.id).unwrap_or(None).is_some()
    };
    let mut body = settings_body(&state, &cfg, &user, schedule, linked);
    body["templates"] = serde_json::json!(templates);
    body["timezones"] = serde_json::json!(zones);
    Json(body).into_response()
}

/// Merges the supplied subset into the effective values and rewrites the user's
/// file, rejecting the first invalid field without touching disk. Bell toggles
/// apply to the template the request leaves selected, and land in the user's own
/// copy of it. The read, merge and write run under the DB lock: settings writes
/// are rare, and the guard is the cheapest serializer that stops two concurrent
/// PUTs from each writing a file built from the values they read before the
/// other landed.
async fn settings_put(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<SettingsPatch>,
) -> impl IntoResponse {
    let templates = crate::templates::available(&state.config_dir, &user.username);
    let conn = state.db();
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
    if let Some(n) = req.triggers_per_day {
        if n > MAX_TRIGGERS_PER_DAY {
            return invalid_field(
                "triggers_per_day",
                &format!("must be 0 to {MAX_TRIGGERS_PER_DAY}"),
            );
        }
        cfg.triggers_per_day = Some(n);
    }
    if let Some(on) = req.pomodoro_enabled {
        cfg.pomodoro_enabled = Some(on);
    }
    if let Some(n) = req.pomodoro_work_min {
        if !POMODORO_WORK_MIN.contains(&n) {
            return invalid_field(
                "pomodoro_work_min",
                &format!("must be {} to {}", POMODORO_WORK_MIN.start(), POMODORO_WORK_MIN.end()),
            );
        }
        cfg.pomodoro_work_min = Some(n);
    }
    if let Some(n) = req.pomodoro_break_min {
        if !POMODORO_BREAK_MIN.contains(&n) {
            return invalid_field(
                "pomodoro_break_min",
                &format!("must be {} to {}", POMODORO_BREAK_MIN.start(), POMODORO_BREAK_MIN.end()),
            );
        }
        cfg.pomodoro_break_min = Some(n);
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
        Ok(()) => {
            let linked = crate::telegram::chat_for_user(&conn, user.id).unwrap_or(None).is_some();
            Json(settings_body(&state, &cfg, &user, schedule, linked)).into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// A fresh code and the deep link that carries it to the bot; issuing one
/// replaces whatever code the user was last given.
async fn telegram_link(user: CurrentUser, State(state): State<AppState>) -> impl IntoResponse {
    let Some(ch) = state.telegram.clone() else {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "telegram is not configured" })),
        )
            .into_response();
    };
    let conn = state.db();
    match crate::telegram::issue_code(&conn, user.id, jiff::Timestamp::now()) {
        Ok(code) => {
            let bot = ch.bot();
            Json(serde_json::json!({
                "code": code,
                "bot": bot,
                "url": format!("https://t.me/{bot}?start={code}"),
            }))
            .into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn telegram_unlink(user: CurrentUser, State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.db();
    match crate::telegram::unlink(&conn, user.id) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
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
        conversation_id: None,
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
/// What wrote this fact on the user's behalf — an inbox item, a harvest —
/// empty for one the user's own sessions wrote.
fn memory_sources(state: &AppState, user_id: i64, memory_id: &str) -> Vec<String> {
    let conn = state.db();
    let Ok(mut stmt) = conn.prepare(
        "SELECT source_id FROM memory_sources WHERE user_id = ?1 AND memory_id = ?2
         ORDER BY source_id",
    ) else {
        return Vec::new();
    };
    stmt.query_map((user_id, memory_id), |r| r.get(0))
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<String>>>())
        .unwrap_or_default()
}

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
            "sources": memory_sources(&state, user.id, &id),
        }))
        .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
struct PlanQuery {
    date: Option<String>,
    /// Adds the day's calendar occurrences beside the events, so a client can
    /// draw both from one call.
    calendar: Option<String>,
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
    let Ok(evs) = crate::plan::events_for(&conn, user.id, date) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    if !truthy(q.calendar.as_deref()) {
        return Json(evs).into_response();
    }
    match crate::calendar::occurrences(&conn, user.id, date) {
        Ok(occurrences) => {
            Json(serde_json::json!({ "events": evs, "calendar": occurrences })).into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// A query flag a client sets by presence: `?calendar`, `?calendar=1` and
/// `?calendar=true` all mean yes, an explicit `0` or `false` means no.
fn truthy(value: Option<&str>) -> bool {
    matches!(value, Some("" | "1" | "true" | "yes"))
}

const MAX_RANGE_DAYS: i32 = 14;

/// The user's timezone and template, the two things every day-shaped read needs.
fn day_context(
    state: &AppState,
    username: &str,
) -> Result<(jiff::tz::TimeZone, crate::templates::Template), StatusCode> {
    let ucfg = crate::config::UserConfig::load(&state.config_dir, username)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let tz = jiff::tz::TimeZone::get(&ucfg.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
    let tmpl = crate::templates::Template::load(&state.config_dir, username, &ucfg.template)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((tz, tmpl))
}

/// One local day, whole: its plan, its calendar, the free time left in it, and
/// — for today and the days behind it — what has already happened.
async fn day_view(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(date): Path<String>,
) -> impl IntoResponse {
    let Ok(date) = date.parse::<jiff::civil::Date>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let (tz, tmpl) = match day_context(&state, &user.username) {
        Ok(c) => c,
        Err(status) => return status.into_response(),
    };
    let now = jiff::Timestamp::now();
    let today = now.to_zoned(tz.clone()).date();
    let conn = state.db();
    if crate::plan::generate(&conn, user.id, &tmpl, date).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let (Ok(events), Ok(occurrences)) = (
        crate::plan::events_for(&conn, user.id, date),
        crate::calendar::occurrences(&conn, user.id, date),
    ) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let free: Vec<serde_json::Value> = crate::allocate::free_windows(&occurrences)
        .iter()
        .map(|w| serde_json::json!({ "start": w.start_wall(), "end": w.end_wall() }))
        .collect();
    let quiet_now = if today == date {
        match crate::calendar::quiet_window(&conn, user.id, &tz, now) {
            Ok(w) => w.map(|w| w.end),
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    } else {
        None
    };
    let history = if date > today {
        Vec::new()
    } else {
        match crate::day::history(&conn, user.id, &tz, date, now) {
            Ok(h) => h,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    };
    Json(serde_json::json!({
        "date": date.to_string(),
        "events": events,
        "calendar": occurrences,
        "free": free,
        "quiet_now": quiet_now,
        "history": history,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct RangeQuery {
    from: String,
    to: String,
}

/// The plans that already exist across a span of days; a day with no plan is
/// left out rather than generated, so reading a week never invents one.
async fn plan_range(
    user: CurrentUser,
    State(state): State<AppState>,
    Query(q): Query<RangeQuery>,
) -> impl IntoResponse {
    let (Ok(from), Ok(to)) =
        (q.from.parse::<jiff::civil::Date>(), q.to.parse::<jiff::civil::Date>())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let span = (to - from).get_days();
    if span < 0 || span >= MAX_RANGE_DAYS {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let conn = state.db();
    let mut days = serde_json::Map::new();
    let mut date = from;
    while date <= to {
        match crate::plan::exists(&conn, user.id, date) {
            Ok(true) => match crate::plan::events_for(&conn, user.id, date) {
                Ok(evs) => {
                    days.insert(date.to_string(), serde_json::to_value(evs).unwrap_or_default());
                }
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            },
            Ok(false) => {}
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
        let Ok(next) = date.tomorrow() else { break };
        date = next;
    }
    Json(serde_json::json!({ "days": days })).into_response()
}

/// Lays the user's open tasks into that day's free time, replacing the
/// automatic blocks an earlier run left that have not started.
async fn plan_allocate(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(date): Path<String>,
) -> impl IntoResponse {
    let Ok(date) = date.parse::<jiff::civil::Date>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let (tz, tmpl) = match day_context(&state, &user.username) {
        Ok(c) => c,
        Err(status) => return status.into_response(),
    };
    let now = jiff::Timestamp::now();
    if date < now.to_zoned(tz.clone()).date() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "a day that is over cannot be filled" })),
        )
            .into_response();
    }
    let conn = state.db();
    if crate::plan::generate(&conn, user.id, &tmpl, date).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    match crate::allocate::run(&conn, user.id, &tz, date, now) {
        Ok(out) => Json(serde_json::json!({
            "plan_date": date.to_string(),
            "placed": out.placed,
            "cleared": out.cleared,
        }))
        .into_response(),
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewWorkSession {
    #[serde(default)]
    task_id: Option<i64>,
    #[serde(default)]
    event_id: Option<i64>,
    title: String,
    #[serde(default)]
    planned_min: Option<i64>,
    #[serde(default)]
    step_index: Option<i64>,
    #[serde(default)]
    step_count: Option<i64>,
    #[serde(default)]
    step_name: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EndWorkSession {
    outcome: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionStep {
    step_index: i64,
    step_name: String,
}

/// Opens a work session, stopping whatever was still running. The server lays
/// the first progress check itself, so accountability starts with the session
/// rather than with the agent's next turn.
async fn work_session_start(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<NewWorkSession>,
) -> impl IntoResponse {
    let conn = state.db();
    let started = crate::work::start(
        &conn,
        &state.config_dir,
        user.id,
        &user.username,
        crate::work::NewSession {
            task_id: req.task_id,
            event_id: req.event_id,
            title: req.title,
            planned_min: req.planned_min,
            step_index: req.step_index,
            step_count: req.step_count,
            step_name: req.step_name,
            notes: req.notes,
        },
        jiff::Timestamp::now(),
    );
    match started {
        Ok(session) => Json(session).into_response(),
        Err(crate::work::StartError::Invalid(m)) => {
            (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": m })))
                .into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Closes the session and takes its waiting checks with it. Ending one that is
/// already over is success: the client says stop once, whatever it lost track of.
async fn work_session_end(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<EndWorkSession>,
) -> impl IntoResponse {
    if req.outcome != "done" && req.outcome != "stopped" {
        return unprocessable_field("outcome", "must be done or stopped");
    }
    let conn = state.db();
    let ended = crate::work::end(
        &conn,
        &state.config_dir,
        user.id,
        &user.username,
        Some(id),
        &req.outcome,
        jiff::Timestamp::now(),
    );
    match ended {
        Ok(ended) => Json(serde_json::json!({ "ended": ended })).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// The four ways a client moves the session the server holds. Each answers with
/// the whole session, so the face repaints from one reply, and a stale id is a
/// 404 rather than a silent write to whatever is running now.
fn session_reply(
    moved: rusqlite::Result<Option<crate::work::Session>>,
) -> axum::response::Response {
    match moved {
        Ok(Some(session)) => Json(session).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn work_session_pause(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.db();
    session_reply(crate::work::pause(&conn, user.id, id, jiff::Timestamp::now()))
}

async fn work_session_resume(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.db();
    session_reply(crate::work::resume(&conn, user.id, id, jiff::Timestamp::now()))
}

async fn work_session_step(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<SessionStep>,
) -> impl IntoResponse {
    if req.step_index < 0 {
        return unprocessable_field("step_index", "must not be negative");
    }
    if req.step_name.len() > crate::work::MAX_TITLE_BYTES {
        return unprocessable_field(
            "step_name",
            &format!("must be at most {} bytes", crate::work::MAX_TITLE_BYTES),
        );
    }
    let conn = state.db();
    session_reply(crate::work::set_step(&conn, user.id, id, req.step_index, &req.step_name))
}

async fn work_session_skip_break(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.db();
    let skipped = crate::work::skip_break(&conn, user.id, id, jiff::Timestamp::now());
    drop(conn);
    if matches!(skipped, Ok(Some(_))) {
        state.hub.broadcast_changed(user.id);
    }
    session_reply(skipped)
}

async fn work_session_open(user: CurrentUser, State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.db();
    match crate::work::open(&conn, user.id) {
        Ok(session) => Json(session).into_response(),
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

fn calendar_router() -> Router<AppState> {
    Router::new()
        .route("/api/calendar", get(calendar_list).post(calendar_create))
        .route("/api/calendar/{id}", patch(calendar_update).delete(calendar_delete))
        .route("/api/calendar/{id}/skip", post(calendar_skip))
        .route("/api/calendar/{id}/skip/{date}", axum::routing::delete(calendar_unskip))
        .route("/api/calendar/day/{date}", get(calendar_day))
}

fn calendar_error(status: StatusCode, message: &str) -> axum::response::Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

fn calendar_failed(e: crate::calendar::CalendarError) -> axum::response::Response {
    use crate::calendar::CalendarError as E;
    match e {
        E::Invalid(m) => calendar_error(StatusCode::UNPROCESSABLE_ENTITY, &m),
        e @ E::TooMany => calendar_error(StatusCode::CONFLICT, &e.to_string()),
        e @ E::NotFound(_) => calendar_error(StatusCode::NOT_FOUND, &e.to_string()),
        E::Db(_) => calendar_error(StatusCode::INTERNAL_SERVER_ERROR, "database error"),
    }
}

/// Unknown fields and wrong types are the caller's mistake to fix, so they read
/// as validation failures rather than as broken syntax.
fn calendar_body<T: serde::de::DeserializeOwned>(
    body: &axum::body::Bytes,
) -> Result<T, (StatusCode, String)> {
    serde_json::from_slice(body).map_err(|e| match e.classify() {
        serde_json::error::Category::Data => (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
        _ => (StatusCode::BAD_REQUEST, "malformed JSON body".into()),
    })
}

async fn calendar_list(user: CurrentUser, State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.db();
    match crate::calendar::list(&conn, user.id) {
        Ok(entries) => Json(serde_json::json!({ "entries": entries })).into_response(),
        Err(_) => calendar_error(StatusCode::INTERNAL_SERVER_ERROR, "database error"),
    }
}

async fn calendar_create(
    user: CurrentUser,
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let fields = match calendar_body::<crate::calendar::Fields>(&body) {
        Ok(f) => f,
        Err((status, why)) => return calendar_error(status, &why),
    };
    let conn = state.db();
    match crate::calendar::create(&conn, user.id, fields) {
        Ok(entry) => (StatusCode::CREATED, Json(entry)).into_response(),
        Err(e) => calendar_failed(e),
    }
}

async fn calendar_update(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let patch = match calendar_body::<crate::calendar::Patch>(&body) {
        Ok(p) => p,
        Err((status, why)) => return calendar_error(status, &why),
    };
    let conn = state.db();
    match crate::calendar::update(&conn, user.id, id, patch) {
        Ok(entry) => Json(entry).into_response(),
        Err(e) => calendar_failed(e),
    }
}

async fn calendar_delete(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.db();
    match crate::calendar::delete(&conn, user.id, id) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => calendar_error(StatusCode::NOT_FOUND, "no such calendar entry"),
        Err(_) => calendar_error(StatusCode::INTERNAL_SERVER_ERROR, "database error"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SkipReq {
    date: String,
}

async fn calendar_skip(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let req = match calendar_body::<SkipReq>(&body) {
        Ok(r) => r,
        Err((status, why)) => return calendar_error(status, &why),
    };
    let conn = state.db();
    match crate::calendar::skip(&conn, user.id, id, &req.date) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => calendar_failed(e),
    }
}

async fn calendar_unskip(
    user: CurrentUser,
    State(state): State<AppState>,
    Path((id, date)): Path<(i64, String)>,
) -> impl IntoResponse {
    let conn = state.db();
    match crate::calendar::unskip(&conn, user.id, id, &date) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => calendar_failed(e),
    }
}

/// One local day of the calendar. `quiet_now` answers only for today, where
/// "now" means anything at all.
async fn calendar_day(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(date): Path<String>,
) -> impl IntoResponse {
    let Ok(date) = date.parse::<jiff::civil::Date>() else {
        return calendar_error(StatusCode::BAD_REQUEST, "date must be YYYY-MM-DD");
    };
    let Ok(ucfg) = crate::config::UserConfig::load(&state.config_dir, &user.username) else {
        return calendar_error(StatusCode::INTERNAL_SERVER_ERROR, "unreadable user config");
    };
    let tz = jiff::tz::TimeZone::get(&ucfg.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
    let now = jiff::Timestamp::now();
    let conn = state.db();
    let Ok(occurrences) = crate::calendar::occurrences(&conn, user.id, date) else {
        return calendar_error(StatusCode::INTERNAL_SERVER_ERROR, "database error");
    };
    let quiet_now = if now.to_zoned(tz.clone()).date() == date {
        match crate::calendar::quiet_window(&conn, user.id, &tz, now) {
            Ok(w) => w.map(|w| w.end),
            Err(_) => return calendar_error(StatusCode::INTERNAL_SERVER_ERROR, "database error"),
        }
    } else {
        None
    };
    Json(serde_json::json!({
        "date": date.to_string(),
        "occurrences": occurrences,
        "quiet_now": quiet_now,
    }))
    .into_response()
}
