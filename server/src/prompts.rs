use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The prompts a user may override through the API; callers must check a
/// caller-supplied name against this list before it reaches the filesystem.
pub const EDITABLE: [&str; 2] = ["persona", "planning"];

fn override_path(config_dir: &Path, user: &str, name: &str) -> PathBuf {
    config_dir.join("users").join(user).join("prompts").join(format!("{name}.md"))
}

/// Per-user prompt override with shipped-default fallback, mirroring
/// Template::load's resolution order.
pub fn load(config_dir: &Path, user: &str, name: &str) -> Result<String> {
    let user_path = override_path(config_dir, user, name);
    let default_path = config_dir.join("defaults/prompts").join(format!("{name}.md"));
    let path = if user_path.exists() { user_path } else { default_path };
    std::fs::read_to_string(&path).with_context(|| format!("reading prompt {name} ({})", path.display()))
}

pub fn custom(config_dir: &Path, user: &str, name: &str) -> bool {
    override_path(config_dir, user, name).exists()
}

pub fn save(config_dir: &Path, user: &str, name: &str, content: &str) -> Result<()> {
    let path = override_path(config_dir, user, name);
    std::fs::create_dir_all(path.parent().expect("a prompt file always has a parent"))?;
    crate::context::write_atomic(&path, content)?;
    Ok(())
}

/// Drops the user's override so `load` falls back to the shipped default;
/// having no override to drop is success, not an error.
pub fn reset(config_dir: &Path, user: &str, name: &str) -> Result<()> {
    match std::fs::remove_file(override_path(config_dir, user, name)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
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

    /// The shipped defaults are what an un-overridden server loads, so every
    /// editable name must resolve to a file in the repo's config tree.
    #[test]
    fn every_editable_prompt_ships_a_default() {
        let config = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("config");
        assert!(EDITABLE.contains(&"import"));
        for name in EDITABLE {
            let text = load(&config, "nobody", name).unwrap();
            assert!(!text.trim().is_empty(), "{name} ships an empty prompt");
        }
        assert!(load(&config, "nobody", "import").unwrap().contains("task_split"));
    }

    #[test]
    fn save_overrides_and_reset_restores_the_default() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/prompts/persona.md", "default persona");
        assert!(!custom(tmp.path(), "aki", "persona"));

        save(tmp.path(), "aki", "persona", "aki persona").unwrap();
        assert!(custom(tmp.path(), "aki", "persona"));
        assert_eq!(load(tmp.path(), "aki", "persona").unwrap(), "aki persona");

        save(tmp.path(), "aki", "persona", "aki again").unwrap();
        assert_eq!(load(tmp.path(), "aki", "persona").unwrap(), "aki again");

        reset(tmp.path(), "aki", "persona").unwrap();
        assert!(!custom(tmp.path(), "aki", "persona"));
        assert_eq!(load(tmp.path(), "aki", "persona").unwrap(), "default persona");
        reset(tmp.path(), "aki", "persona").unwrap();
    }
}
