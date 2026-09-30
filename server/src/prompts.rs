use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The prompts a user may override through the API. Every function here checks
/// a name against this list, so a caller-supplied name can never become a path
/// of its own choosing.
pub const EDITABLE: [&str; 11] = [
    "persona", "planning", "import", "inbox", "summarize", "harvest", "review", "trigger",
    "search", "title", "share",
];

fn checked(name: &str) -> Result<()> {
    anyhow::ensure!(EDITABLE.contains(&name), "unknown prompt {name:?}");
    Ok(())
}

fn override_path(config_dir: &Path, user: &str, name: &str) -> PathBuf {
    config_dir.join("users").join(user).join("prompts").join(format!("{name}.md"))
}

/// Per-user prompt override with shipped-default fallback, mirroring
/// `Template::load`'s resolution order.
pub fn load(config_dir: &Path, user: &str, name: &str) -> Result<String> {
    checked(name)?;
    let user_path = override_path(config_dir, user, name);
    let default_path = crate::config::defaults_dir(config_dir).join("prompts").join(format!("{name}.md"));
    let path = if user_path.exists() { user_path } else { default_path };
    std::fs::read_to_string(&path).with_context(|| format!("reading prompt {name} ({})", path.display()))
}

pub fn custom(config_dir: &Path, user: &str, name: &str) -> bool {
    checked(name).is_ok() && override_path(config_dir, user, name).exists()
}

pub fn save(config_dir: &Path, user: &str, name: &str, content: &str) -> Result<()> {
    checked(name)?;
    let path = override_path(config_dir, user, name);
    std::fs::create_dir_all(path.parent().expect("a prompt file always has a parent"))?;
    crate::context::write_atomic(&path, content)?;
    Ok(())
}

/// Drops the user's override so `load` falls back to the shipped default;
/// having no override to drop is success, not an error.
pub fn reset(config_dir: &Path, user: &str, name: &str) -> Result<()> {
    checked(name)?;
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
        let err = load(tmp.path(), "aki", "planning").unwrap_err().to_string();
        assert!(err.contains("planning"), "{err}");
    }

    /// The allow-list lives here rather than in each caller, so forgetting it
    /// upstream cannot turn a name into a path.
    #[test]
    fn a_name_outside_the_allow_list_never_reaches_the_filesystem() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/prompts/persona.md", "default persona");
        for bad in ["../x", "../../etc/passwd", "persona.md", "", "secrets"] {
            assert!(save(tmp.path(), "aki", bad, "owned").is_err(), "saved {bad:?}");
            assert!(load(tmp.path(), "aki", bad).is_err(), "loaded {bad:?}");
            assert!(reset(tmp.path(), "aki", bad).is_err(), "reset {bad:?}");
            assert!(!custom(tmp.path(), "aki", bad));
        }
        assert!(!tmp.path().join("users/aki/prompts").exists());
        assert!(!tmp.path().join("users/aki/x.md").exists());
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
        // the import prompt names the tools that session actually has
        let import = load(&config, "nobody", "import").unwrap();
        for name in crate::tools::registry(crate::tools::SessionKind::Import) {
            assert!(import.contains(name), "the import prompt never mentions {name}");
        }
        for gone in ["task_update", "task_split"] {
            assert!(!import.contains(gone), "the import prompt still calls for {gone}");
        }
        // the inbox prompt names the tools that session actually has
        let inbox = load(&config, "nobody", "inbox").unwrap();
        for name in crate::tools::registry(crate::tools::SessionKind::Inbox) {
            assert!(inbox.contains(name), "the inbox prompt never mentions {name}");
        }
        let summarize = load(&config, "nobody", "summarize").unwrap();
        for name in crate::tools::registry(crate::tools::SessionKind::Summarize) {
            assert!(summarize.contains(name), "the summarize prompt never mentions {name}");
        }
        let harvest = load(&config, "nobody", "harvest").unwrap();
        for name in crate::tools::registry(crate::tools::SessionKind::Harvest) {
            assert!(harvest.contains(name), "the harvest prompt never mentions {name}");
        }
        // the night's two halves: the episodic record is mechanical, the rest distilled
        for kind in ["semantic", "procedural", "episodic"] {
            assert!(harvest.contains(kind), "the harvest prompt never names {kind} memory");
        }
        assert!(harvest.contains("supersede"), "the harvest prompt never says how a fact changes");
        let review = load(&config, "nobody", "review").unwrap();
        for name in crate::tools::registry(crate::tools::SessionKind::Review) {
            assert!(review.contains(name), "the review prompt never mentions {name}");
        }
        // the trigger prompt names the two ways that session can end
        let trigger = load(&config, "nobody", "trigger").unwrap();
        for name in ["say", "stay_quiet", "wait_until", "wait_for"] {
            assert!(trigger.contains(name), "the trigger prompt never mentions {name}");
        }
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
