//! Note's built-in knowledge of itself: the default memories shipped in
//! `<defaults>/memory/<lang>/<slug>.md`, seeded into every user's memory.
//!
//! A per-user marker records which language was seeded and, per slug, the id
//! and content hash of the copy written. A slug the marker lacks is added; a
//! changed default supersedes the seeded copy only while that copy is live and
//! still hashes as written, so anything the user or assistant touched stays.

use crate::memory::{self, Fact};
use crate::text::Lang;
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const SOURCE: &str = "note";
const MARKER: &str = "builtin.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltIn {
    pub slug: String,
    pub category: String,
    pub summary: String,
    pub body: String,
}

impl BuiltIn {
    fn hash(&self) -> String {
        content_hash(&self.category, &self.summary, &self.body)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Marker {
    lang: String,
    seeded: BTreeMap<String, Seeded>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Seeded {
    id: String,
    hash: String,
}

/// Hashes a fact as the memory store normalises it, so a file read back
/// compares equal to the default it was written from.
fn content_hash(category: &str, summary: &str, body: &str) -> String {
    let mut h = Sha256::new();
    for part in [category, &memory::one_line(summary), body.trim()] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    data_encoding::HEXLOWER.encode(&h.finalize())
}

fn lang_dir(defaults_dir: &Path, lang: Lang) -> PathBuf {
    defaults_dir.join("memory").join(lang.code())
}

fn parse(slug: &str, raw: &str) -> Result<BuiltIn> {
    let rest = raw.strip_prefix("---\n").context("missing frontmatter")?;
    let (fm, body) = rest.split_once("\n---\n").context("unterminated frontmatter")?;
    let mut category = None;
    let mut summary = None;
    for line in fm.lines() {
        match line.split_once(": ") {
            Some(("category", v)) => category = Some(v.trim().to_string()),
            Some(("summary", v)) => summary = Some(v.trim().to_string()),
            _ => {}
        }
    }
    let category = category.context("no category")?;
    anyhow::ensure!(memory::CATEGORIES.contains(&category.as_str()), "bad category {category:?}");
    let summary = summary.filter(|s| !s.is_empty()).context("no summary")?;
    let body = body.trim().to_string();
    anyhow::ensure!(!body.is_empty(), "empty body");
    Ok(BuiltIn { slug: slug.into(), category, summary, body })
}

/// The shipped defaults for `lang`, by slug; none when the directory is absent.
pub fn load(defaults_dir: &Path, lang: Lang) -> Result<Vec<BuiltIn>> {
    let dir = lang_dir(defaults_dir, lang);
    let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let slug = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let raw = std::fs::read_to_string(&path)?;
        out.push(parse(&slug, &raw).with_context(|| path.display().to_string())?);
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(out)
}

fn marker_path(data_dir: &Path, user: &str) -> PathBuf {
    memory::user_root(data_dir, user).join(MARKER)
}

fn read_marker(data_dir: &Path, user: &str) -> Result<Option<Marker>> {
    match std::fs::read_to_string(marker_path(data_dir, user)) {
        Ok(raw) => Ok(Some(serde_json::from_str(&raw)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn write_marker(data_dir: &Path, user: &str, marker: &Marker) -> Result<()> {
    let path = marker_path(data_dir, user);
    std::fs::create_dir_all(path.parent().context("marker has no parent")?)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(marker)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Whether the seeded copy is still live and exactly as it was written.
fn untouched(data_dir: &Path, user: &str, seeded: &Seeded) -> Result<bool> {
    Ok(memory::read(data_dir, user, &seeded.id)?
        .is_some_and(|f| !f.archived && content_hash(&f.category, &f.summary, &f.body) == seeded.hash))
}

/// Brings one user's built-in memories up to the shipped defaults. `lang`
/// applies only to a first seeding; after that the marker's language holds.
/// Returns how many facts were written. Vectors are left to the backfill.
pub fn seed_user(conn: &Connection, data_dir: &Path, defaults_dir: &Path, user: &str, lang: Lang) -> Result<usize> {
    let existing = read_marker(data_dir, user)?;
    let lang = existing
        .as_ref()
        .and_then(|m| Lang::from_setting(&m.lang))
        .unwrap_or(lang);
    let defaults = load(defaults_dir, lang)?;
    if defaults.is_empty() {
        return Ok(0);
    }
    let mut marker = existing.unwrap_or_else(|| Marker { lang: lang.code().into(), ..Marker::default() });
    let mut written = 0;
    for d in &defaults {
        let hash = d.hash();
        let id = match marker.seeded.get(&d.slug) {
            None => {
                let fact = Fact { category: &d.category, summary: &d.summary, body: &d.body, until: None };
                memory::add_sourced(conn, data_dir, user, &fact, SOURCE)?
            }
            Some(s) if s.hash == hash || !untouched(data_dir, user, s)? => continue,
            Some(s) => match memory::supersede_sourced(conn, data_dir, user, &s.id, (&d.summary, &d.body), Some(SOURCE), None)? {
                Some(id) => id,
                None => continue,
            },
        };
        marker.seeded.insert(d.slug.clone(), Seeded { id, hash });
        write_marker(data_dir, user, &marker)?;
        written += 1;
    }
    Ok(written)
}

/// Seeds a newly created account in its language. A failure is logged, never
/// raised: the account exists either way, and the next start retries.
pub fn seed_new_user(conn: &Connection, data_dir: &Path, config_dir: &Path, user: &str) {
    let defaults_dir = crate::config::defaults_dir(config_dir);
    let lang = Lang::for_user(config_dir, user);
    if let Err(e) = seed_user(conn, data_dir, &defaults_dir, user, lang) {
        let _ = crate::log::record(conn, None, "builtin_memory_error", &format!("{user}: {e:#}"));
    }
}

/// Seeds every account; run at start so existing users and changed defaults
/// catch up. Returns how many facts were written in all.
pub fn seed_all(conn: &Connection, data_dir: &Path, config_dir: &Path) -> Result<usize> {
    let defaults_dir = crate::config::defaults_dir(config_dir);
    let users: Vec<String> = {
        let mut stmt = conn.prepare("SELECT username FROM users ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut written = 0;
    for user in users {
        match seed_user(conn, data_dir, &defaults_dir, &user, Lang::for_user(config_dir, &user)) {
            Ok(n) => written += n,
            Err(e) => {
                let _ = crate::log::record(conn, None, "builtin_memory_error", &format!("{user}: {e:#}"));
            }
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Env {
        conn: Connection,
        data: tempfile::TempDir,
        defaults: tempfile::TempDir,
    }

    fn env() -> Env {
        Env {
            conn: crate::db::open_memory().unwrap(),
            data: tempfile::tempdir().unwrap(),
            defaults: tempfile::tempdir().unwrap(),
        }
    }

    fn put(env: &Env, lang: &str, slug: &str, summary: &str, body: &str) {
        let dir = env.defaults.path().join("memory").join(lang);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{slug}.md")),
            format!("---\ncategory: procedural\nsummary: {summary}\n---\n\n{body}\n"),
        )
        .unwrap();
    }

    fn seed(env: &Env, lang: Lang) -> usize {
        seed_user(&env.conn, env.data.path(), env.defaults.path(), "aki", lang).unwrap()
    }

    fn live(env: &Env) -> Vec<memory::MemoryFile> {
        memory::list(&env.conn, "aki", None, 100)
            .unwrap()
            .into_iter()
            .map(|h| memory::read(env.data.path(), "aki", &h.id).unwrap().unwrap())
            .collect()
    }

    fn seeded_id(env: &Env, slug: &str) -> String {
        read_marker(env.data.path(), "aki").unwrap().unwrap().seeded[slug].id.clone()
    }

    fn shipped() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("config/defaults")
    }

    #[test]
    fn a_new_user_is_seeded_in_their_language_and_indexed() {
        let conn = crate::db::open_memory().unwrap();
        let data = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(shipped(), config.path().join("defaults")).unwrap();
        crate::auth::create_user(&conn, "aki", "pw", false).unwrap();
        crate::text::remember_seen(config.path(), "aki", Lang::Ja).unwrap();
        seed_new_user(&conn, data.path(), config.path(), "aki");

        let ja = load(&shipped(), Lang::Ja).unwrap();
        let facts: Vec<_> = memory::list(&conn, "aki", None, 100)
            .unwrap()
            .into_iter()
            .map(|h| memory::read(data.path(), "aki", &h.id).unwrap().unwrap())
            .collect();
        assert_eq!(facts.len(), ja.len());
        assert!(facts.iter().all(|f| f.source.as_deref() == Some(SOURCE)));
        let mut summaries: Vec<_> = facts.iter().map(|f| f.summary.clone()).collect();
        let mut expected: Vec<_> = ja.iter().map(|d| d.summary.clone()).collect();
        summaries.sort();
        expected.sort();
        assert_eq!(summaries, expected);

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_fts WHERE user = 'aki'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n as usize, ja.len());
    }

    #[test]
    fn the_shipped_languages_hold_the_same_slugs_and_all_parse() {
        let en = load(&shipped(), Lang::En).unwrap();
        let ja = load(&shipped(), Lang::Ja).unwrap();
        assert!(en.len() >= 10, "expected the shipped set, found {}", en.len());
        let slugs = |v: &[BuiltIn]| v.iter().map(|d| d.slug.clone()).collect::<Vec<_>>();
        assert_eq!(slugs(&en), slugs(&ja));
        for (e, j) in en.iter().zip(&ja) {
            assert_eq!(e.category, j.category, "{} differs in category", e.slug);
        }
    }

    #[test]
    fn backfill_seeds_existing_users_once() {
        let conn = crate::db::open_memory().unwrap();
        let data = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(shipped(), config.path().join("defaults")).unwrap();
        crate::auth::create_user(&conn, "aki", "pw", false).unwrap();
        crate::auth::create_user(&conn, "bo", "pw", false).unwrap();
        let per_user = load(&shipped(), Lang::En).unwrap().len();

        assert_eq!(seed_all(&conn, data.path(), config.path()).unwrap(), 2 * per_user);
        assert_eq!(seed_all(&conn, data.path(), config.path()).unwrap(), 0);
        assert_eq!(memory::live_count(&conn, "aki").unwrap() as usize, per_user);
        assert_eq!(memory::live_count(&conn, "bo").unwrap() as usize, per_user);
    }

    #[test]
    fn a_new_default_is_seeded_later_on_its_own() {
        let env = env();
        put(&env, "en", "tasks", "how tasks work", "say add a task");
        assert_eq!(seed(&env, Lang::En), 1);
        let first = seeded_id(&env, "tasks");
        put(&env, "en", "goals", "how goals work", "say add a goal");
        assert_eq!(seed(&env, Lang::En), 1);
        assert_eq!(seeded_id(&env, "tasks"), first);
        assert_eq!(live(&env).len(), 2);
    }

    #[test]
    fn a_changed_default_supersedes_an_untouched_copy() {
        let env = env();
        put(&env, "en", "tasks", "how tasks work", "say add a task");
        seed(&env, Lang::En);
        let old = seeded_id(&env, "tasks");
        put(&env, "en", "tasks", "how tasks work", "say add a task, or tap +");
        assert_eq!(seed(&env, Lang::En), 1);
        let new = seeded_id(&env, "tasks");
        assert_ne!(new, old);
        let f = memory::read(env.data.path(), "aki", &new).unwrap().unwrap();
        assert_eq!(f.body, "say add a task, or tap +");
        assert_eq!(f.supersedes.as_deref(), Some(old.as_str()));
        assert_eq!(f.source.as_deref(), Some(SOURCE));
        assert!(memory::read(env.data.path(), "aki", &old).unwrap().unwrap().archived);
        assert_eq!(seed(&env, Lang::En), 0);
    }

    #[test]
    fn a_changed_default_leaves_an_edited_or_superseded_copy_alone() {
        let env = env();
        put(&env, "en", "edited", "a", "one");
        put(&env, "en", "replaced", "b", "two");
        seed(&env, Lang::En);
        let edited = seeded_id(&env, "edited");
        let replaced = seeded_id(&env, "replaced");
        memory::update(&env.conn, env.data.path(), "aki", &edited, "a", "one, as I put it", None).unwrap();
        let mine = memory::supersede(&env.conn, env.data.path(), "aki", &replaced, "b", "mine", None)
            .unwrap()
            .unwrap();

        put(&env, "en", "edited", "a", "one, revised");
        put(&env, "en", "replaced", "b", "two, revised");
        assert_eq!(seed(&env, Lang::En), 0);
        let f = memory::read(env.data.path(), "aki", &edited).unwrap().unwrap();
        assert_eq!(f.body, "one, as I put it");
        assert!(!f.archived);
        assert_eq!(f.source, None);
        assert_eq!(memory::read(env.data.path(), "aki", &mine).unwrap().unwrap().body, "mine");
        assert_eq!(live(&env).len(), 2);
    }

    #[test]
    fn a_later_language_change_keeps_the_seeded_language() {
        let env = env();
        put(&env, "en", "tasks", "how tasks work", "say add a task");
        put(&env, "ja", "tasks", "タスクの使い方", "タスクを追加と言う");
        put(&env, "ja", "goals", "目標の使い方", "目標を追加と言う");
        seed(&env, Lang::En);
        assert_eq!(seed(&env, Lang::Ja), 0);
        let facts = live(&env);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].summary, "how tasks work");
    }

    #[test]
    fn the_source_survives_a_reindex_and_the_marker_is_not_a_memory() {
        let env = env();
        put(&env, "en", "tasks", "how tasks work", "say add a task");
        seed(&env, Lang::En);
        memory::reindex_user(&env.conn, env.data.path(), "aki").unwrap();
        let facts = live(&env);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].source.as_deref(), Some(SOURCE));
    }
}
