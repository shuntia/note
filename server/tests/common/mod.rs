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
        "[[events]]\nkind='checkin_call'\ntime='09:00'\ndays=['mon','tue','wed','thu','fri','sat','sun']\nflexibility='slide'\nslide_window_min=60\nchannel='push'\n",
    );
    write("defaults/prompts/persona.md", "you are note");
    write("defaults/prompts/planning.md", "plan the day");
    write("defaults/prompts/import.md", "brief the assignment");
    write(
        "defaults/prompts/inbox.md",
        "You read one item from a school system and decide what to remember.",
    );
    write("defaults/prompts/summarize.md", "summarise the conversation");
    write("defaults/prompts/harvest.md", "keep what will still matter");
    write("defaults/prompts/review.md", "read the week");
    write("defaults/prompts/trigger.md", "say one thing or stay_quiet");
    write("defaults/prompts/share.md", "you answer for {owner}; stay inside the slice");
    tmp
}

/// A stand-in HTTP server: every request waits for the body the test has queued and
/// is handed back for inspection, so a suite drives the wire in both directions.
#[allow(dead_code)] // only the Matrix suite stands up a fake homeserver
pub struct Fake {
    pub base: String,
    replies: std::sync::mpsc::Sender<String>,
    requests: std::sync::mpsc::Receiver<String>,
}

#[allow(dead_code)] // only the Matrix suite stands up a fake homeserver
impl Fake {
    /// Queued before the call that consumes it; the fake holds the connection
    /// open until one is there.
    pub fn answer(&self, body: &str) {
        self.replies.send(body.to_string()).unwrap();
    }

    pub fn took(&self) -> String {
        self.requests.recv_timeout(std::time::Duration::from_secs(5)).expect("a request")
    }

    /// The method and JSON body of the next request.
    pub fn call(&self) -> (String, serde_json::Value) {
        let raw = self.took();
        let path = raw.split_whitespace().nth(1).unwrap_or_default().to_string();
        let method = path.rsplit('/').next().unwrap_or_default().to_string();
        let (_, body) = raw.split_once("\r\n\r\n").expect("a request with a body");
        (method, serde_json::from_str(body).unwrap_or_else(|e| panic!("body {body:?}: {e}")))
    }

    pub fn silent(&self) -> bool {
        self.requests.recv_timeout(std::time::Duration::from_millis(250)).is_err()
    }
}

#[allow(dead_code)] // only the Matrix suite stands up a fake homeserver
pub fn fake_http() -> Fake {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (replies, next) = std::sync::mpsc::channel::<String>();
    let (seen, requests) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || loop {
        let Ok((mut sock, _)) = listener.accept() else { return };
        let mut raw = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            let Ok(n) = sock.read(&mut buf) else { return };
            raw.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&raw).to_string();
            let Some(head_end) = text.find("\r\n\r\n") else {
                if n == 0 {
                    break;
                }
                continue;
            };
            let want: usize = text[..head_end]
                .lines()
                .find_map(|l| {
                    l.strip_prefix("content-length: ").or(l.strip_prefix("Content-Length: "))
                })
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            if raw.len() >= head_end + 4 + want || n == 0 {
                break;
            }
        }
        let Ok(body) = next.recv() else { return };
        let _ = sock.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
        let _ = seen.send(String::from_utf8_lossy(&raw).to_string());
    });
    Fake { base, replies, requests }
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

#[allow(dead_code)] // only the admin suite swaps the model
pub async fn app_with_admin_seed_and_llm(
    seed: Vec<u8>,
    llm: Arc<dyn LLMProvider>,
) -> (axum::Router, String, AppState, TempDir) {
    build_with(Some(llm), Some(AdminSecrets::with_seed(seed)), None).await
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
