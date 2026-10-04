use anyhow::Context;
use note_server::{
    admin, api, auth, channels, config::ServerConfig, db, memory, nightly, providers, runner,
    search, security, summaries, totp, AppState,
};
use std::path::PathBuf;

const USAGE: &str = "usage: note-server [create-user <name> <password> [--admin] [--test] | set-category <name> <member|test> | totp-generate | totp-uri]";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_dir = std::env::var("NOTE_CONFIG_DIR").map_or_else(|_| PathBuf::from("./config"), PathBuf::from);
    let cfg = match std::env::var_os("NOTE_SERVER_CONFIG") {
        Some(path) => ServerConfig::load_file(std::path::Path::new(&path))?,
        None => ServerConfig::load(&config_dir)?,
    };

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
            let flags = &args[4.min(args.len())..];
            let admin = flags.iter().any(|f| f == "--admin");
            let category = if flags.iter().any(|f| f == "--test") {
                note_server::config::CATEGORY_TEST
            } else {
                note_server::config::CATEGORY_MEMBER
            };
            if let Some(unknown) = flags.iter().find(|f| *f != "--admin" && *f != "--test") {
                anyhow::bail!("unknown flag {unknown:?}\n{USAGE}");
            }
            auth::create_user(&conn, name, pass, admin)?;
            auth::set_category(&conn, name, category)?;
            println!("created {name} ({category})");
            return Ok(());
        }
        Some("set-category") => {
            let name = args.get(2).context(USAGE)?;
            let category = args.get(3).context(USAGE)?;
            anyhow::ensure!(auth::set_category(&conn, name, category)?, "no user {name}");
            println!("{name} is now {category}");
            return Ok(());
        }
        Some(other) => anyhow::bail!("unknown command {other:?}\n{USAGE}"),
        None => {}
    }

    let (secrets, warnings) = admin::AdminSecrets::load(&cfg.secrets_dir);
    let secrets = secrets.require_second_factor(cfg.admin.require_second_factor);
    for w in &warnings {
        eprintln!("warning: {w}");
        let _ = note_server::log::record(&conn, None, "admin_secret_error", w);
    }
    match secrets.totp_mode() {
        admin::TotpMode::Required => {}
        admin::TotpMode::Missing => {
            let msg = format!(
                "admin panel locked for accounts with no passkey or authenticator app: no admin_totp seed in {}",
                cfg.secrets_dir.display()
            );
            eprintln!("{msg}");
            let _ = note_server::log::record(&conn, None, "admin_locked", &msg);
        }
        admin::TotpMode::PasswordOnly if admin::INSPECT => {
            eprintln!("DEV-INSPECT BUILD: admin elevation is password-only and user data is open; never deploy this binary");
        }
        admin::TotpMode::PasswordOnly => {
            eprintln!("admin elevation is password-only: [admin] require_second_factor = false in server.toml");
        }
    }

    let (passkeys, passkey_warning) = security::PasskeyService::build(
        &cfg.public_base_url,
        cfg.admin.rp_id.as_deref(),
        cfg.admin.rp_origin.as_deref(),
    );
    if let Some(w) = &passkey_warning {
        eprintln!("warning: {w}");
        let _ = note_server::log::record(&conn, None, "passkeys_unavailable", w);
    } else if !passkeys.available() {
        eprintln!("passkeys are registered but browsers will refuse them: {} is not https", cfg.public_base_url);
    }

    let (llm, embeddings) = providers::build(&cfg.providers)?;
    if let Some(model) = db::server_setting(&conn, admin::LLM_MODEL_SETTING)? {
        llm.set_model(&model);
    }
    let mut state = AppState::new(conn, config_dir, cfg.data_dir.clone())
        .with_providers(llm, embeddings)
        .with_providers_info(admin::ProvidersInfo::from(&cfg.providers))
        .with_admin_secrets(secrets)
        .with_passkeys(passkeys)
        .with_limits(&cfg.limits)
        .with_public_base_url(&cfg.public_base_url)
        .with_idle_summary_min(cfg.idle_summary_min())
        .with_inbox_refresh(cfg.inbox.refresh_signal.clone());
    state.secure_cookies = cfg.public_base_url.starts_with("https://");
    if let Some(wp) = &cfg.channels.webpush {
        let pem = std::fs::read(&wp.vapid_pem_file)
            .with_context(|| format!("reading {}", wp.vapid_pem_file.display()))?;
        let public_key = channels::webpush::public_key_b64(&pem)?;
        let ch = channels::webpush::WebPushChannel::new(state.db.clone(), pem, wp.subject.clone())?;
        state = state.with_webpush(ch, public_key);
    }
    if let Some(s) = &cfg.search {
        state = state.with_search(std::sync::Arc::new(search::SearxngSearch::new(s)));
    }
    if let Some(v) = &cfg.voice {
        let voice_llm = providers::build_voice(&cfg.providers, v, &state.llm)?;
        state = state.with_voice_llm(voice_llm, note_server::voice::call::CallSettings::from(v));
        let voice = note_server::voice::Voice::new(state.db.clone());
        state = state.with_voice(voice.clone());
        voice
            .listen(&v.socket)
            .with_context(|| format!("listening for the voice service on {}", v.socket.display()))?;
        voice.spawn_sweeper();
    }
    if let Some(embeddings) = state.embeddings.clone() {
        let db = state.db.clone();
        let data_dir = state.data_dir.clone();
        tokio::task::spawn_blocking(move || {
            match memory::backfill_vectors(&db, &data_dir, embeddings.as_ref()) {
                Ok(0) => {}
                Ok(n) => eprintln!("memory: embedded {n} fact(s) that had no vector"),
                Err(e) => eprintln!("memory: vector backfill failed: {e:#}"),
            }
        });
    }
    runner::spawn(state.clone());
    nightly::spawn(state.clone());
    summaries::spawn(state.clone());

    let app = api::router_with_web(state, &cfg.web_dir);
    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
