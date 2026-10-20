use crate::models::Model;
use anyhow::{bail, ensure, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const USER: &str = "aitest";
const LIVE_PORT: u16 = 3271;
const PASSWORD: &str = "eval-scratch-password";

/// What one scratch server is started with; everything else is fixed here.
pub struct Spec<'a> {
    pub repo: &'a Path,
    pub server_bin: &'a Path,
    pub key_file: &'a Path,
    pub model: &'a Model,
    pub models: &'a [Model],
    pub base_url: &'a str,
    pub user_toml: String,
}

/// A note-server on a random loopback port over a fresh temp dir holding only
/// `aitest`. Dropping it stops the server and deletes the dir.
pub struct Scratch {
    dir: tempfile::TempDir,
    port: u16,
    child: Child,
    pub log_path: PathBuf,
}

impl Scratch {
    pub fn start(spec: &Spec) -> Result<Self> {
        let dir = tempfile::Builder::new().prefix("note-eval-").tempdir().context("creating the scratch dir")?;
        let root = dir.path().canonicalize()?;
        ensure!(std::fs::read_dir(&root)?.next().is_none(), "scratch dir {} is not empty", root.display());
        let config_dir = root.join("config");
        let data_dir = root.join("data");
        check_paths(spec.repo, &root, &[&config_dir, &data_dir])?;

        copy_dir(&spec.repo.join("config/defaults"), &config_dir.join("defaults"))?;
        let user_dir = config_dir.join("users").join(USER);
        let source = spec.repo.join("config/users").join(USER);
        if source.is_dir() {
            copy_dir(&source, &user_dir)?;
        }
        std::fs::create_dir_all(user_dir.join("templates"))?;
        std::fs::write(user_dir.join("templates/eval.toml"), "events = []\n")?;
        std::fs::write(user_dir.join("user.toml"), &spec.user_toml)?;

        let port = free_port()?;
        check_port(port)?;
        let server_toml = root.join("server.toml");
        std::fs::write(&server_toml, server_toml_text(spec, port, &root, &data_dir)?)?;

        let command = |args: &[&str]| {
            let mut c = Command::new(spec.server_bin);
            c.args(args)
                .current_dir(&root)
                .env("NOTE_CONFIG_DIR", &config_dir)
                .env("NOTE_SERVER_CONFIG", &server_toml);
            c
        };
        let made = command(&["create-user", USER, PASSWORD, "--test"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .context("running note-server create-user")?;
        ensure!(made.status.success(), "create-user failed: {}", String::from_utf8_lossy(&made.stderr));
        check_users(&usernames(&data_dir.join("note.db"))?)?;

        let log_path = root.join("server.log");
        let log = std::fs::File::create(&log_path)?;
        let child = command(&[])
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .context("starting the scratch note-server")?;
        let scratch = Self { dir, port, child, log_path };
        scratch.wait_healthy()?;
        Ok(scratch)
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn db_path(&self) -> PathBuf {
        self.dir.path().join("data/note.db")
    }

    /// The scratch DB, after checking that it still holds `aitest` alone.
    pub fn db(&self) -> Result<rusqlite::Connection> {
        let conn = rusqlite::Connection::open(self.db_path())?;
        conn.busy_timeout(Duration::from_secs(10))?;
        check_users(&names_in(&conn)?)?;
        Ok(conn)
    }

    pub fn login(&self) -> Result<crate::client::Client> {
        crate::client::Client::login(&self.base_url(), self.port, USER, PASSWORD)
    }

    fn wait_healthy(&self) -> Result<()> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(2)))
            .build()
            .into();
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(60) {
            if agent.get(&format!("{}/healthz", self.base_url())).call().is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        bail!("the scratch server never answered /healthz; see {}", self.log_path.display())
    }

    /// Copies the server log and DB out before the dir goes.
    pub fn keep(&self, into: &Path, stem: &str) -> Result<()> {
        std::fs::create_dir_all(into)?;
        let _ = std::fs::copy(&self.log_path, into.join(format!("{stem}.log")));
        let conn = rusqlite::Connection::open(self.db_path())?;
        let out = into.join(format!("{stem}.db"));
        let _ = std::fs::remove_file(&out);
        conn.execute("VACUUM INTO ?1", [out.to_string_lossy()])?;
        Ok(())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn server_toml_text(spec: &Spec, port: u16, root: &Path, data_dir: &Path) -> Result<String> {
    ensure!(spec.key_file.is_absolute(), "the key file path must be absolute");
    ensure!(spec.models.iter().any(|m| m.id == spec.model.id), "the case model is not one of the configured models");
    let models: Vec<toml::Value> = spec
        .models
        .iter()
        .map(|m| {
            toml::Value::Table(toml::toml! {
                id = (m.id.clone())
                name = (m.name.clone())
                desc = "free"
            })
        })
        .collect();
    let llm = toml::toml! {
        kind = "openai"
        base_url = (spec.base_url.to_string())
        model = (spec.model.id.clone())
        api_key_file = (spec.key_file.to_string_lossy().into_owned())
        models = (models)
    };
    let mut doc = toml::toml! {
        bind_addr = (format!("127.0.0.1:{port}"))
        public_base_url = (format!("http://127.0.0.1:{port}"))
        data_dir = (data_dir.to_string_lossy().into_owned())
        web_dir = (root.join("no-web").to_string_lossy().into_owned())
        secrets_dir = (root.join("secrets").to_string_lossy().into_owned())
        [agent]
        idle_summary_min = 0
    };
    doc.insert("providers".into(), toml::Value::Table(toml::toml! { llm = (llm) }));
    Ok(toml::to_string(&doc)?)
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

pub fn check_port(port: u16) -> Result<()> {
    ensure!(port != LIVE_PORT, "port {LIVE_PORT} is the live server's; refusing");
    ensure!(port != 0, "no port");
    Ok(())
}

/// The scratch server's URL, and nothing else, is a target.
pub fn check_target(url: &str, port: u16) -> Result<()> {
    check_port(port)?;
    let expected = format!("http://127.0.0.1:{port}");
    ensure!(
        url == expected || url.starts_with(&format!("{expected}/")),
        "{url} is not the scratch server {expected}; refusing"
    );
    Ok(())
}

/// Every scratch path lies inside the fresh temp root and outside the repo's
/// own `data/` and `config/users/`.
pub fn check_paths(repo: &Path, root: &Path, paths: &[&Path]) -> Result<()> {
    let repo = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let forbidden = [repo.join("data"), repo.join("config/users")];
    ensure!(!root.starts_with(&repo), "scratch root {} lies inside the repo", root.display());
    for p in paths {
        ensure!(p.starts_with(root), "{} lies outside the scratch dir {}", p.display(), root.display());
        for f in &forbidden {
            ensure!(!p.starts_with(f), "{} lies under {}", p.display(), f.display());
        }
    }
    Ok(())
}

pub fn check_users(names: &[String]) -> Result<()> {
    if let Some(other) = names.iter().find(|n| *n != USER) {
        bail!("the scratch DB holds user {other:?}; only {USER} may exist");
    }
    ensure!(names.len() == 1, "the scratch DB must hold exactly {USER}, found {names:?}");
    Ok(())
}

fn usernames(db: &Path) -> Result<Vec<String>> {
    names_in(&rusqlite::Connection::open(db)?)
}

fn names_in(conn: &rusqlite::Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT username FROM users ORDER BY id")?;
    let names = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(names)
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_port_is_refused() {
        assert!(check_port(3271).is_err());
        assert!(check_port(40123).is_ok());
    }

    #[test]
    fn only_the_scratch_url_is_a_target() {
        assert!(check_target("http://127.0.0.1:40123/api/talk", 40123).is_ok());
        assert!(check_target("http://127.0.0.1:40123", 40123).is_ok());
        assert!(check_target("http://127.0.0.1:3271/api/talk", 40123).is_err());
        assert!(check_target("https://note.shuntia.net/api/talk", 40123).is_err());
        assert!(check_target("http://127.0.0.1:401234/api", 40123).is_err());
        assert!(check_target("http://localhost:40123/api", 40123).is_err());
        assert!(check_target("http://127.0.0.1:3271/", 3271).is_err());
    }

    #[test]
    fn paths_outside_the_scratch_dir_or_in_the_repo_data_are_refused() {
        let repo = Path::new("/r/note");
        let root = Path::new("/tmp/note-eval-x");
        assert!(check_paths(repo, root, &[&root.join("data"), &root.join("config")]).is_ok());
        assert!(check_paths(repo, root, &[Path::new("/r/note/data")]).is_err());
        assert!(check_paths(repo, root, &[Path::new("/tmp/other/data")]).is_err());
        assert!(check_paths(repo, Path::new("/r/note/data/x"), &[Path::new("/r/note/data/x/db")]).is_err());
    }

    #[test]
    fn any_user_but_aitest_is_refused() {
        assert!(check_users(&["aitest".into()]).is_ok());
        assert!(check_users(&["aitest".into(), "shuntia".into()]).is_err());
        assert!(check_users(&["admin".into()]).is_err());
        assert!(check_users(&[]).is_err());
    }

    #[test]
    fn the_server_config_names_the_free_models_alone() {
        let m = |id: &str| Model { short: "s".into(), id: id.into(), name: "n".into() };
        let models = [m("a/b:free"), m("c/d:free")];
        let spec = Spec {
            repo: Path::new("/r"),
            server_bin: Path::new("/bin/true"),
            key_file: Path::new("/k/free.key"),
            model: &models[1],
            models: &models,
            base_url: crate::models::OPENROUTER,
            user_toml: String::new(),
        };
        let text = server_toml_text(&spec, 40123, Path::new("/tmp/x"), Path::new("/tmp/x/data")).unwrap();
        let v: toml::Value = toml::from_str(&text).unwrap();
        let llm = &v["providers"]["llm"];
        assert_eq!(llm["model"].as_str(), Some("c/d:free"));
        assert_eq!(llm["api_key_file"].as_str(), Some("/k/free.key"));
        let ids: Vec<_> = llm["models"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["a/b:free", "c/d:free"]);
        assert_eq!(v["bind_addr"].as_str(), Some("127.0.0.1:40123"));
        assert_eq!(v["data_dir"].as_str(), Some("/tmp/x/data"));
    }
}
