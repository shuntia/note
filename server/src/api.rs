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
        .route("/api/plan/today", get(plan_today))
        .route("/api/events/{id}/shift", post(event_shift))
        .route("/api/events/{id}/done", post(event_done))
        .route("/api/events/{id}/drop", post(event_drop))
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
