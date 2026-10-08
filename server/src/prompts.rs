use anyhow::{Context, Result};
use crate::text::Lang;
use std::path::{Path, PathBuf};

/// The prompts a user may override through the API. Every function here checks
/// a name against this list, so a caller-supplied name can never become a path
/// of its own choosing.
pub const EDITABLE: [&str; 12] = [
    "persona", "planning", "import", "inbox", "summarize", "harvest", "review", "trigger",
    "search", "title", "share", "voice",
];

fn checked(name: &str) -> Result<()> {
    anyhow::ensure!(EDITABLE.contains(&name), "unknown prompt {name:?}");
    Ok(())
}

fn override_path(config_dir: &Path, user: &str, name: &str) -> PathBuf {
    config_dir.join("users").join(user).join("prompts").join(format!("{name}.md"))
}

/// `load_in` the user's own language.
pub fn load(config_dir: &Path, user: &str, name: &str) -> Result<String> {
    load_in(config_dir, user, name, Lang::for_user(config_dir, user))
}

/// The user's override, else the shipped default in `lang`, else the English one.
pub fn load_in(config_dir: &Path, user: &str, name: &str, lang: Lang) -> Result<String> {
    checked(name)?;
    let user_path = override_path(config_dir, user, name);
    let path = if user_path.exists() { user_path } else { default_path(config_dir, name, lang) };
    std::fs::read_to_string(&path).with_context(|| format!("reading prompt {name} ({})", path.display()))
}

/// English defaults sit in `prompts/`, every other language in `prompts/<code>/`.
fn default_path(config_dir: &Path, name: &str, lang: Lang) -> PathBuf {
    let dir = crate::config::defaults_dir(config_dir).join("prompts");
    let file = format!("{name}.md");
    let localised = dir.join(lang.code()).join(&file);
    if lang != Lang::En && localised.exists() { localised } else { dir.join(file) }
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

    fn shipped() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("config")
    }

    /// The shipped defaults are what an un-overridden server loads, so every
    /// editable name must resolve to a file in the repo's config tree, in
    /// every language, naming the tools its session has.
    #[test]
    fn every_editable_prompt_ships_a_default() {
        let config = shipped();
        assert!(EDITABLE.contains(&"import"));
        for lang in [Lang::En, Lang::Ja] {
            let load = |name| load_in(&config, "nobody", name, lang).unwrap();
            for name in EDITABLE {
                assert!(!load(name).trim().is_empty(), "{name} ({lang:?}) ships an empty prompt");
            }
            use crate::tools::{registry, SessionKind as K};
            for (name, kind) in [
                ("import", K::Import),
                ("inbox", K::Inbox),
                ("summarize", K::Summarize),
                ("harvest", K::Harvest),
                ("review", K::Review),
            ] {
                let text = load(name);
                for tool in registry(kind) {
                    assert!(text.contains(tool), "the {name} prompt ({lang:?}) never mentions {tool}");
                }
            }
            let import = load("import");
            for gone in ["task_update", "task_split"] {
                assert!(!import.contains(gone), "the import prompt ({lang:?}) still calls for {gone}");
            }
            // the night's two halves: the episodic record is mechanical, the rest distilled
            let harvest = load("harvest");
            for kind in ["semantic", "procedural", "episodic", "supersede"] {
                assert!(harvest.contains(kind), "the harvest prompt ({lang:?}) never names {kind}");
            }
            // the trigger prompt names the two ways that session can end
            let trigger = load("trigger");
            for name in [
                "say", "stay_quiet", "wait_until", "wait_for", "`ring`",
                "order_set", "order_move", "order_drop", "note_write", "trigger_set",
            ] {
                assert!(trigger.contains(name), "the trigger prompt ({lang:?}) never mentions {name}");
            }
            let planning = load("planning");
            assert!(planning.contains("order_set"), "the planning prompt ({lang:?}) never sets the order");
            for name in EDITABLE {
                assert!(!load(name).contains("plan_auto"), "the {name} prompt ({lang:?}) still calls plan_auto");
            }
        }
    }

    /// Every `{word}` the code fills in.
    fn placeholders(text: &str) -> std::collections::BTreeSet<&str> {
        text.split('{')
            .skip(1)
            .filter_map(|rest| rest.split_once('}').map(|(word, _)| word))
            .filter(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
            .collect()
    }

    #[test]
    fn every_translation_fills_the_same_placeholders() {
        let dir = shipped().join("defaults/prompts");
        for name in EDITABLE {
            let en = std::fs::read_to_string(dir.join(format!("{name}.md"))).unwrap();
            let ja = std::fs::read_to_string(dir.join("ja").join(format!("{name}.md")))
                .unwrap_or_else(|_| panic!("{name} ships no Japanese default"));
            assert_eq!(placeholders(&en), placeholders(&ja), "{name}");
        }
        let share = std::fs::read_to_string(dir.join("share.md")).unwrap();
        assert_eq!(placeholders(&share).into_iter().collect::<Vec<_>>(), ["owner"]);
    }

    #[test]
    fn a_japanese_reader_gets_the_japanese_default_until_they_override_it() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/prompts/persona.md", "english persona");
        write(tmp.path(), "defaults/prompts/ja/persona.md", "日本語のペルソナ");
        write(tmp.path(), "defaults/prompts/planning.md", "english planning");
        write(tmp.path(), "defaults/user.toml", "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");
        write(tmp.path(), "users/aki/user.toml", "language = \"ja\"\n");
        assert_eq!(load(tmp.path(), "aki", "persona").unwrap(), "日本語のペルソナ");
        assert_eq!(load(tmp.path(), "bob", "persona").unwrap(), "english persona");
        assert_eq!(load(tmp.path(), "aki", "planning").unwrap(), "english planning");
        assert_eq!(load_in(tmp.path(), "aki", "persona", Lang::En).unwrap(), "english persona");
        save(tmp.path(), "aki", "persona", "aki persona").unwrap();
        assert_eq!(load(tmp.path(), "aki", "persona").unwrap(), "aki persona");
        reset(tmp.path(), "aki", "persona").unwrap();
        assert_eq!(load(tmp.path(), "aki", "persona").unwrap(), "日本語のペルソナ");
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
