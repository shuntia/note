use anyhow::Context;
use note_server::{
    api, auth, config::ServerConfig, db, memory, nightly, providers, runner, AppState,
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
    let state =
        AppState::new(conn, config_dir, cfg.data_dir.clone()).with_providers(llm, embeddings);
    runner::spawn(state.clone());
    nightly::spawn(state.clone());

    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
