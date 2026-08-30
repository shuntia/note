use anyhow::Context;
use note_server::{
    api, auth, channels, config::ServerConfig, db, memory, nightly, providers, runner, AppState,
};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_dir = std::env::var("NOTE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./config"));
    let cfg = ServerConfig::load(&config_dir)?;

    std::fs::create_dir_all(&cfg.data_dir)?;
    let conn = db::open(&cfg.data_dir.join("note.db"))?;
    memory::reindex_all(&conn, &cfg.data_dir)?;

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("create-user") {
        let name = args
            .get(2)
            .context("usage: create-user <name> <password> [--admin]")?;
        let pass = args
            .get(3)
            .context("usage: create-user <name> <password> [--admin]")?;
        let admin = args.get(4).map(String::as_str) == Some("--admin");
        auth::create_user(&conn, name, pass, admin)?;
        println!("created {name}");
        return Ok(());
    }

    let (llm, embeddings) = providers::build(&cfg.providers)?;
    let mut state =
        AppState::new(conn, config_dir, cfg.data_dir.clone()).with_providers(llm, embeddings);
    state.secure_cookies = cfg.public_base_url.starts_with("https://");
    if let Some(wp) = &cfg.channels.webpush {
        let pem = std::fs::read(&wp.vapid_pem_file)
            .with_context(|| format!("reading {}", wp.vapid_pem_file.display()))?;
        let public_key = channels::webpush::public_key_b64(&pem)?;
        let ch = channels::webpush::WebPushChannel::new(state.db.clone(), pem, wp.subject.clone())?;
        state = state.with_webpush(ch, public_key);
    }
    runner::spawn(state.clone());
    nightly::spawn(state.clone());

    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
