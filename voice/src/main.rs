#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("livekit=info,libwebrtc=warn")).init();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let path = std::env::var_os("NOTE_VOICE_CONFIG").map_or_else(|| std::path::PathBuf::from("note-voice.toml"), std::path::PathBuf::from);
    let cfg = note_voice::config::VoiceServiceConfig::load(&path)?;
    note_voice::service::run(cfg).await
}
