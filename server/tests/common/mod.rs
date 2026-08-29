use axum::body::Body;
use axum::http::{header, Request};
use note_server::{api, auth, db, AppState};
use tempfile::TempDir;
use tower::ServiceExt;

/// Builds a config dir holding a UTC user default and a `default` template with
/// one sliding 09:00 event on every weekday. The caller must keep the returned
/// `TempDir` alive for as long as the state that reads it.
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
    res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string()
}

pub async fn app_with_logged_in_user() -> (axum::Router, String, TempDir) {
    let cfg = config_dir();
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", true).unwrap();
    let app = api::router(AppState::new(conn, cfg.path().to_path_buf()));
    let cookie = login(&app, "aki", "pw").await;
    (app, cookie, cfg)
}
