use axum::http::{header, HeaderValue};
use base64::Engine;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// The script sources that allow the shell's inline scripts by hash.
pub fn script_hashes(shell: &str) -> String {
    inline_scripts(shell).fold(String::new(), |mut out, body| {
        let digest = Sha256::digest(body.as_bytes());
        let _ = write!(out, " 'sha256-{}'", base64::engine::general_purpose::STANDARD.encode(digest));
        out
    })
}

/// The connect sources name the request's own host so its sockets pass on
/// browsers whose `'self'` does not cover ws(s).
fn policy(scripts: &str, host: Option<&str>) -> String {
    let sockets = host
        .filter(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b)))
        .map(|h| format!(" wss://{h} ws://{h}"))
        .unwrap_or_default();
    format!(
        "default-src 'self'; script-src 'self'{scripts}; img-src 'self' data: blob:; media-src 'self' blob:; \
         connect-src 'self'{sockets}; style-src 'self' 'unsafe-inline'; font-src 'self' data:; \
         worker-src 'self' blob:; manifest-src 'self'; frame-ancestors 'none'; base-uri 'none'; \
         form-action 'self'; object-src 'none'"
    )
}

fn inline_scripts(html: &str) -> impl Iterator<Item = &str> {
    let mut rest = html;
    std::iter::from_fn(move || loop {
        let start = rest.find("<script")?;
        let open_end = start + rest[start..].find('>')? + 1;
        let tag = &rest[start..open_end];
        let close = open_end + rest[open_end..].find("</script>")?;
        let body = &rest[open_end..close];
        rest = &rest[close + "</script>".len()..];
        if !tag.contains(" src=") {
            return Some(body);
        }
    })
}

/// Every response is `nosniff` and keeps its referrer on this origin unless
/// it names its own policy; HTML pages also carry the CSP.
pub fn harden(res: &mut axum::response::Response, scripts: &str, host: Option<&str>) {
    let is_html = res
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"text/html"));
    let h = res.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    if !h.contains_key(header::REFERRER_POLICY) {
        h.insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    }
    if is_html {
        if let Ok(v) = HeaderValue::from_str(&policy(scripts, host)) {
            h.insert(header::CONTENT_SECURITY_POLICY, v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_scripts_are_allowed_by_hash_and_sourced_ones_are_skipped() {
        let shell = "<script>a()</script><script type=\"module\" src=\"/x.js\"></script><script>\n b()\n</script>";
        let found: Vec<&str> = inline_scripts(shell).collect();
        assert_eq!(found, vec!["a()", "\n b()\n"]);
        let p = policy(&script_hashes(shell), None);
        let digest = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(b"a()"));
        assert!(p.contains(&format!("script-src 'self' 'sha256-{digest}' 'sha256-")), "{p}");
        assert!(!p.contains("unsafe-eval"));
    }

    #[test]
    fn sockets_name_only_a_plain_host() {
        assert!(policy("", Some("note.example:3271")).contains("connect-src 'self' wss://note.example:3271 ws://note.example:3271;"));
        assert!(policy("", Some("evil; script-src *")).contains("connect-src 'self';"));
        assert!(policy("", None).contains("connect-src 'self';"));
    }
}
