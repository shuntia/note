use anyhow::{Context, Result};
use std::path::Path;

/// Per-user prompt override with shipped-default fallback, mirroring
/// Template::load's resolution order.
pub fn load(config_dir: &Path, user: &str, name: &str) -> Result<String> {
    let user_path = config_dir.join("users").join(user).join("prompts").join(format!("{name}.md"));
    let default_path = config_dir.join("defaults/prompts").join(format!("{name}.md"));
    let path = if user_path.exists() { user_path } else { default_path };
    std::fs::read_to_string(&path).with_context(|| format!("reading prompt {name} ({})", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn user_prompt_overrides_default() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/prompts/persona.md", "default persona");
        write(tmp.path(), "users/aki/prompts/persona.md", "aki persona");
        assert_eq!(load(tmp.path(), "aki", "persona").unwrap(), "aki persona");
        assert_eq!(load(tmp.path(), "bob", "persona").unwrap(), "default persona");
        let err = load(tmp.path(), "aki", "missing").unwrap_err().to_string();
        assert!(err.contains("missing"), "{err}");
    }
}
