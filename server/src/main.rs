use note_server::{api, config::ServerConfig, db, AppState};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_dir = std::env::var("NOTE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./config"));
    let cfg = ServerConfig::load(&config_dir)?;

    std::fs::create_dir_all(&cfg.data_dir)?;
    let conn = db::open(&cfg.data_dir.join("note.db"))?;
    let app = api::router(AppState::new(conn));

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
