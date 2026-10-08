use anyhow::{Context, Result};
use crate::text::Lang;
use std::path::{Path, PathBuf};

/// The shipped prompts. They belong to the app and are never overridden; what a
/// user adds goes in `about`.
pub const NAMES: [&str; 12] = [
    "persona", "planning", "import", "inbox", "summarize", "harvest", "review", "trigger",
    "search", "title", "share", "voice",
];

pub const MAX_ABOUT_BYTES: usize = 8 * 1024;

/// The shipped prompt in `lang`, else the English one.
pub fn load_in(config_dir: &Path, name: &str, lang: Lang) -> Result<String> {
    anyhow::ensure!(NAMES.contains(&name), "unknown prompt {name:?}");
    let path = default_path(config_dir, name, lang);
    std::fs::read_to_string(&path).with_context(|| format!("reading prompt {name} ({})", path.display()))
}

/// English defaults sit in `prompts/`, every other language in `prompts/<code>/`.
fn default_path(config_dir: &Path, name: &str, lang: Lang) -> PathBuf {
    let dir = crate::config::defaults_dir(config_dir).join("prompts");
    let file = format!("{name}.md");
    let localised = dir.join(lang.code()).join(&file);
    if lang != Lang::En && localised.exists() { localised } else { dir.join(file) }
}

fn about_path(config_dir: &Path, user: &str) -> PathBuf {
    config_dir.join("users").join(user).join("about.md")
}

/// What the user wrote about themselves, added after the shipped prompt.
pub fn about(config_dir: &Path, user: &str) -> Option<String> {
    let text = std::fs::read_to_string(about_path(config_dir, user)).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Blank text removes it; having nothing to remove is success.
pub fn save_about(config_dir: &Path, user: &str, content: &str) -> Result<()> {
    let path = about_path(config_dir, user);
    if content.trim().is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(path.parent().expect("an about file always has a parent"))?;
    crate::context::write_atomic(&path, content.trim())?;
    Ok(())
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
    fn an_old_override_file_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/prompts/persona.md", "default persona");
        write(tmp.path(), "users/aki/prompts/persona.md", "aki persona");
        assert_eq!(load_in(tmp.path(), "persona", Lang::En).unwrap(), "default persona");
        let err = load_in(tmp.path(), "planning", Lang::En).unwrap_err().to_string();
        assert!(err.contains("planning"), "{err}");
        for bad in ["../x", "../../etc/passwd", "persona.md", "", "secrets"] {
            assert!(load_in(tmp.path(), bad, Lang::En).is_err(), "loaded {bad:?}");
        }
    }

    #[test]
    fn about_is_saved_trimmed_and_blank_removes_it() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(about(tmp.path(), "aki"), None);
        save_about(tmp.path(), "aki", "  I have ADHD.\n").unwrap();
        assert_eq!(about(tmp.path(), "aki").as_deref(), Some("I have ADHD."));
        assert_eq!(about(tmp.path(), "bob"), None);
        save_about(tmp.path(), "aki", "   ").unwrap();
        assert_eq!(about(tmp.path(), "aki"), None);
        save_about(tmp.path(), "aki", "").unwrap();
    }

    #[test]
    fn a_japanese_reader_gets_the_japanese_default() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/prompts/persona.md", "english persona");
        write(tmp.path(), "defaults/prompts/ja/persona.md", "日本語のペルソナ");
        write(tmp.path(), "defaults/prompts/planning.md", "english planning");
        assert_eq!(load_in(tmp.path(), "persona", Lang::Ja).unwrap(), "日本語のペルソナ");
        assert_eq!(load_in(tmp.path(), "persona", Lang::En).unwrap(), "english persona");
        assert_eq!(load_in(tmp.path(), "planning", Lang::Ja).unwrap(), "english planning");
    }

    fn shipped() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("config")
    }

    /// The shipped defaults are what the server loads, so every
    /// editable name must resolve to a file in the repo's config tree, in
    /// every language, naming the tools its session has.
    #[test]
    fn every_prompt_ships_a_default() {
        let config = shipped();
        assert!(NAMES.contains(&"import"));
        for lang in [Lang::En, Lang::Ja] {
            let load = |name| load_in(&config, name, lang).unwrap();
            for name in NAMES {
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
            for name in NAMES {
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
        for name in NAMES {
            let en = std::fs::read_to_string(dir.join(format!("{name}.md"))).unwrap();
            let ja = std::fs::read_to_string(dir.join("ja").join(format!("{name}.md")))
                .unwrap_or_else(|_| panic!("{name} ships no Japanese default"));
            assert_eq!(placeholders(&en), placeholders(&ja), "{name}");
        }
        let share = std::fs::read_to_string(dir.join("share.md")).unwrap();
        assert_eq!(placeholders(&share).into_iter().collect::<Vec<_>>(), ["owner"]);
    }
}
