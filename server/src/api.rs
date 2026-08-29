use crate::auth::{self, CurrentUser};
use crate::AppState;
use axum::extract::{Path, State};
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
