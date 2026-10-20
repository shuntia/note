use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::time::Duration;

/// A signed-in `aitest` session on the scratch server. Only `Scratch::login`
/// makes one, and every request is checked against the scratch URL.
pub struct Client {
    base: String,
    port: u16,
    cookie: String,
    agent: ureq::Agent,
}

pub struct Reply {
    pub status: u16,
    pub body: Value,
}

impl Client {
    pub(crate) fn login(base: &str, port: u16, user: &str, password: &str) -> Result<Self> {
        crate::scratch::check_target(base, port)?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(120)))
            .build()
            .into();
        let resp = agent
            .post(&format!("{base}/api/login"))
            .send_json(serde_json::json!({ "username": user, "password": password }))
            .context("signing in to the scratch server")?;
        if !resp.status().is_success() {
            bail!("scratch sign-in failed: {}", resp.status());
        }
        let cookie = resp
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|c| c.split(';').next().filter(|kv| kv.starts_with("session=")))
            .context("no session cookie")?
            .to_string();
        Ok(Self { base: base.to_string(), port, cookie, agent })
    }

    fn url(&self, path: &str) -> Result<String> {
        let url = format!("{}{path}", self.base);
        crate::scratch::check_target(&url, self.port)?;
        Ok(url)
    }

    pub fn get(&self, path: &str) -> Result<Reply> {
        let resp = self.agent.get(&self.url(path)?).header("Cookie", &self.cookie).call()?;
        read(resp)
    }

    pub fn send(&self, method: &str, path: &str, body: &Value) -> Result<Reply> {
        let url = self.url(path)?;
        let req = match method {
            "POST" => self.agent.post(&url),
            "PUT" => self.agent.put(&url),
            "PATCH" => self.agent.patch(&url),
            other => bail!("unsupported method {other}"),
        };
        read(req.header("Cookie", &self.cookie).send_json(body)?)
    }

    /// A write the seed relies on: anything but a 2xx stops the case.
    pub fn must(&self, method: &str, path: &str, body: &Value) -> Result<Value> {
        let r = self.send(method, path, body)?;
        if !(200..300).contains(&r.status) {
            bail!("{method} {path} answered {}: {}", r.status, r.body);
        }
        Ok(r.body)
    }

    pub fn must_get(&self, path: &str) -> Result<Value> {
        let r = self.get(path)?;
        if !(200..300).contains(&r.status) {
            bail!("GET {path} answered {}: {}", r.status, r.body);
        }
        Ok(r.body)
    }
}

fn read(mut resp: ureq::http::Response<ureq::Body>) -> Result<Reply> {
    let status = resp.status().as_u16();
    let text = resp.body_mut().with_config().limit(32 * 1024 * 1024).read_to_string()?;
    let body = if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::String(text)) };
    Ok(Reply { status, body })
}
