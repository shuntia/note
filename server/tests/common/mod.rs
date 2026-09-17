use axum::body::Body;
use axum::http::{header, Request};
use note_server::admin::AdminSecrets;
use note_server::providers::LLMProvider;
use note_server::security::PasskeyService;
use note_server::{api, auth, db, AppState};
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

/// Builds a config dir holding a UTC user default, a `default` template with
/// one sliding 09:00 event on every weekday, and the editable prompts. The caller
/// must keep the returned `TempDir` alive for as long as the state that reads it.
pub fn config_dir() -> TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let write = |rel: &str, c: &str| {
        let p = tmp.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, c).unwrap();
    };
    write(
        "defaults/user.toml",
        "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n",
    );
    write(
        "defaults/templates/default.toml",
        "[[events]]\nkind='checkin_call'\ntime='09:00'\ndays=['mon','tue','wed','thu','fri','sat','sun']\nflexibility='slide'\nslide_window_min=60\nchannel='voice'\n",
    );
    write("defaults/prompts/persona.md", "you are note");
    write("defaults/prompts/planning.md", "plan the day");
    write("defaults/prompts/import.md", "brief the assignment");
    write(
        "defaults/prompts/inbox.md",
        "You read one item from a school system and decide what to remember.",
    );
    tmp
}

pub async fn login(app: &axum::Router, username: &str, password: &str) -> String {
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"username":"{username}","password":"{password}"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    res.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

#[allow(dead_code)] // every test binary compiles this module; the static-file suite only needs the config dir
pub async fn app_with_logged_in_user() -> (axum::Router, String, TempDir) {
    let (app, cookie, _state, cfg) = app_with_logged_in_user_and_state().await;
    (app, cookie, cfg)
}

#[allow(dead_code)] // every test binary compiles this module; only the talk suite scripts a model
pub async fn app_with_logged_in_user_and_llm(
    llm: Arc<dyn LLMProvider>,
) -> (axum::Router, String, TempDir) {
    let (app, cookie, _state, cfg) = build(Some(llm)).await;
    (app, cookie, cfg)
}

#[allow(dead_code)] // every test binary compiles this module; only the talk suite scripts a model
pub async fn app_with_logged_in_user_llm_and_state(
    llm: Arc<dyn LLMProvider>,
) -> (axum::Router, String, AppState, TempDir) {
    build(Some(llm)).await
}

#[allow(dead_code)] // every test binary compiles this module; only the talk suite reaches into the state
pub async fn app_with_logged_in_user_and_state() -> (axum::Router, String, AppState, TempDir) {
    build(None).await
}

#[allow(dead_code)] // only the admin suite installs a TOTP seed
pub async fn app_with_admin_seed(seed: Vec<u8>) -> (axum::Router, String, AppState, TempDir) {
    build_with(None, Some(AdminSecrets::with_seed(seed)), None).await
}

#[allow(dead_code)] // only the admin suite elevates on the password alone
pub async fn app_with_password_only_admin() -> (axum::Router, String, AppState, TempDir) {
    build_with(
        None,
        Some(AdminSecrets::default().require_second_factor(false)),
        None,
    )
    .await
}

/// The origin the passkey-capable states are built for; tests hand it to the
/// software authenticator as the page's origin.
#[allow(dead_code)] // only the security suite drives a software authenticator
pub const ORIGIN: &str = "https://note.example.net";

/// No shared seed: the only second factors are the ones a user enrols.
#[allow(dead_code)] // only the security suite drives a software authenticator
pub async fn app_with_passkeys() -> (axum::Router, String, AppState, TempDir) {
    build_with(
        None,
        None,
        Some(PasskeyService::build(ORIGIN, None, None).0),
    )
    .await
}

#[allow(dead_code)] // only the security suite drives a software authenticator
pub async fn app_with_seed_and_passkeys(
    seed: Vec<u8>,
) -> (axum::Router, String, AppState, TempDir) {
    build_with(
        None,
        Some(AdminSecrets::with_seed(seed)),
        Some(PasskeyService::build(ORIGIN, None, None).0),
    )
    .await
}

async fn build(llm: Option<Arc<dyn LLMProvider>>) -> (axum::Router, String, AppState, TempDir) {
    build_with(llm, None, None).await
}

async fn build_with(
    llm: Option<Arc<dyn LLMProvider>>,
    secrets: Option<AdminSecrets>,
    passkeys: Option<PasskeyService>,
) -> (axum::Router, String, AppState, TempDir) {
    let cfg = config_dir();
    let dir = cfg.path().to_path_buf();
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", true).unwrap();
    let mut state = AppState::new(conn, dir.clone(), dir);
    if let Some(llm) = llm {
        state = state.with_providers(llm, None);
    }
    if let Some(secrets) = secrets {
        state = state.with_admin_secrets(secrets);
    }
    if let Some(passkeys) = passkeys {
        state = state.with_passkeys(passkeys);
    }
    let app = api::router(state.clone());
    let cookie = login(&app, "aki", "pw").await;
    (app, cookie, state, cfg)
}
