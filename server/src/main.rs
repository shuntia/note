use note_server::{api, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = api::router(AppState::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
