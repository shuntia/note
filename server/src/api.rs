use crate::AppState;
use axum::{routing::get, Router};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state)
}
