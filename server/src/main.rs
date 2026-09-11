use anyhow::Context;
use note_server::{
    admin, api, auth, channels, config::ServerConfig, db, memory, nightly, providers, runner,
    totp, AppState,
};
use std::path::PathBuf;

const USAGE: &str = "usage: note-server [create-user <name> <password> [--admin] | totp-generate | totp-uri]";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_dir = std::env::var("NOTE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./config"));
    let cfg = ServerConfig::load(&config_dir)?;

    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("totp-generate") => {
            let seed = totp::generate_seed();
            println!("{seed}");
            eprintln!(
                "install as {} and enrol with:\n{}",
                cfg.secrets_dir.join("admin_totp").display(),
                totp::otpauth_uri(&totp::parse_seed(&seed)?, "Note", "admin")
            );
            return Ok(());
        }
        Some("totp-uri") => {
            let (secrets, warnings) = admin::AdminSecrets::load(&cfg.secrets_dir);
            for w in &warnings {
                eprintln!("{w}");
            }
            let seed = secrets.totp_seed.context("no admin_totp seed installed")?;
            println!("{}", totp::otpauth_uri(&seed, "Note", "admin"));
            return Ok(());
        }
        _ => {}
    }

    std::fs::create_dir_all(&cfg.data_dir)?;
    let conn = db::open(&cfg.data_dir.join("note.db"))?;
    memory::reindex_all(&conn, &cfg.data_dir)?;

    match args.get(1).map(String::as_str) {
        Some("create-user") => {
            let name = args.get(2).context(USAGE)?;
            let pass = args.get(3).context(USAGE)?;
            let admin = args.get(4).map(String::as_str) == Some("--admin");
            auth::create_user(&conn, name, pass, admin)?;
            println!("created {name}");
            return Ok(());
        }
        Some(other) => anyhow::bail!("unknown command {other:?}\n{USAGE}"),
        None => {}
    }

    let (secrets, warnings) = admin::AdminSecrets::load(&cfg.secrets_dir);
    for w in &warnings {
        eprintln!("warning: {w}");
        let _ = note_server::log::record(&conn, None, "admin_secret_error", w);
    }
    match secrets.totp_mode() {
        admin::TotpMode::Required => {}
        admin::TotpMode::Missing => {
            let msg = format!(
                "admin panel locked: no admin_totp seed in {}",
                cfg.secrets_dir.display()
            );
            eprintln!("{msg}");
            let _ = note_server::log::record(&conn, None, "admin_locked", &msg);
        }
        admin::TotpMode::PasswordOnly => {
            eprintln!("DEV-INSPECT BUILD: admin elevation is password-only and user data is open; never deploy this binary");
        }
    }

    let (llm, embeddings) = providers::build(&cfg.providers)?;
    let mut state = AppState::new(conn, config_dir, cfg.data_dir.clone())
        .with_providers(llm, embeddings)
        .with_providers_info(admin::ProvidersInfo::from(&cfg.providers))
        .with_admin_secrets(secrets);
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

    let app = api::router_with_web(state, &cfg.web_dir);
    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
