use crate::auth::{self, CurrentUser};
use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/login", post(login))
        .route("/api/me", get(me))
        .route("/api/tasks", get(tasks_list).post(tasks_create))
        .route("/api/tasks/{id}", patch(tasks_update))
        .route("/api/talk", post(talk))
        .route("/api/plan/today", get(plan_today))
        .route("/api/events/{id}/shift", post(event_shift))
        .route("/api/events/{id}/snooze", post(event_snooze))
        .route("/api/events/{id}/done", post(event_done))
        .route("/api/events/{id}/drop", post(event_drop))
        .route("/api/admin/log", get(admin_log))
        .route("/api/admin/users", post(admin_create_user))
        .with_state(state)
}

#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}

async fn login(State(state): State<AppState>, Json(req): Json<LoginReq>) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match auth::login(&conn, &req.username, &req.password) {
        Ok(Some(token)) => (
            StatusCode::OK,
            [(
                header::SET_COOKIE,
                format!("session={token}; HttpOnly; Path=/; SameSite=Lax"),
            )],
        )
            .into_response(),
        Ok(None) => StatusCode::UNAUTHORIZED.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn me(user: CurrentUser) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "username": user.username, "admin": user.admin }))
}

#[derive(Deserialize)]
struct CreateTaskReq {
    title: String,
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
    Json(req): Json<CreateTaskReq>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tasks::create(&conn, user.id, &req.title, "manual") {
        Ok(t) => Json(t).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
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
        Err(crate::tasks::UpdateError::InvalidState(_)) => StatusCode::BAD_REQUEST.into_response(),
        Err(crate::tasks::UpdateError::Db(_)) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
struct TalkReq {
    message: String,
}

const MAX_TALK_MESSAGE: usize = 16 * 1024;

/// A session makes synchronous provider calls and blocking DB writes, so it
/// runs off the async executor.
async fn talk(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<TalkReq>,
) -> impl IntoResponse {
    let message = req.message.trim().to_string();
    if message.is_empty() || message.len() > MAX_TALK_MESSAGE {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let result = tokio::task::spawn_blocking(move || {
        let deps = crate::agent::SessionDeps {
            db: &state.db,
            config_dir: &state.config_dir,
            data_dir: &state.data_dir,
            llm: state.llm.as_ref(),
            embeddings: state.embeddings.as_deref(),
        };
        crate::agent::run_session(
            &deps,
            user.id,
            &user.username,
            crate::tools::SessionKind::Talk,
            &message,
        )
    })
    .await;
    match result {
        Ok(Ok(out)) => Json(serde_json::json!({ "reply": out.reply })).into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
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
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
struct SnoozeReq {
    minutes: i64,
}

/// The range is checked here so a bad `minutes` is a 400 while a failure inside
/// `plan::snooze` — which rejects the same range as defense in depth — stays a 500.
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
struct CreateUserReq {
    username: String,
    password: String,
    admin: bool,
}

async fn admin_create_user(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<CreateUserReq>,
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
