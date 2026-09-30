#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = std::env::var_os("NOTE_VOICE_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("note-voice.toml"));
    let cfg = note_voice::config::VoiceServiceConfig::load(&path)?;
    note_voice::service::run(cfg).await
}
