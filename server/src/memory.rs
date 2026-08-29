use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const CATEGORIES: [&str; 3] = ["semantic", "episodic", "procedural"];

#[derive(Debug, Clone)]
pub struct MemoryFile {
    pub id: String,
    pub category: String,
    pub summary: String,
    pub body: String,
    pub supersedes: Option<String>,
    pub created: String,
    pub archived: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct QueryHit {
    pub id: String,
    pub category: String,
    pub summary: String,
}

/// Distinguishes writes rejected by the supersede-never-delete rule from
/// infrastructure failures, so the tool layer can reject vs. 500 correctly.
#[derive(Debug, Error)]
pub enum WriteError {
    #[error("memory {0} is archived and immutable")]
    Archived(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Ids appear in file paths; anything but a lowercase-UUID shape is rejected
/// before any filesystem access.
pub fn valid_id(id: &str) -> bool {
    id.len() == 36 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f' | b'-'))
}

fn user_root(data_dir: &Path, user: &str) -> PathBuf {
    data_dir.join("memory").join(user)
}

fn render(f: &MemoryFile) -> String {
    let mut fm = format!(
        "---\nid: {}\ncategory: {}\nsummary: {}\ncreated: {}\n",
        f.id, f.category, f.summary, f.created
    );
    if let Some(s) = &f.supersedes {
        fm.push_str(&format!("supersedes: {s}\n"));
    }
    format!("{fm}---\n\n{}\n", f.body)
}

fn parse(raw: &str, archived: bool) -> Result<MemoryFile> {
    let rest = raw.strip_prefix("---\n").context("missing frontmatter")?;
    let (fm, body) = rest.split_once("\n---\n").context("unterminated frontmatter")?;
    let mut f = MemoryFile {
        id: String::new(),
        category: String::new(),
        summary: String::new(),
        body: body.trim().to_string(),
        supersedes: None,
        created: String::new(),
        archived,
    };
    for line in fm.lines() {
        let Some((k, v)) = line.split_once(": ") else { continue };
        match k {
            "id" => f.id = v.into(),
            "category" => f.category = v.into(),
            "summary" => f.summary = v.into(),
            "created" => f.created = v.into(),
            "supersedes" => f.supersedes = Some(v.into()),
            _ => {}
        }
    }
    if !valid_id(&f.id) {
        bail!("bad or missing id in memory file frontmatter");
    }
    if !CATEGORIES.contains(&f.category.as_str()) {
        bail!("bad category {:?} in memory file {}", f.category, f.id);
    }
    Ok(f)
}

fn locate(data_dir: &Path, user: &str, id: &str) -> Option<(PathBuf, bool)> {
    if !valid_id(id) {
        return None;
    }
    let name = format!("{id}.md");
    for cat in CATEGORIES {
        let p = user_root(data_dir, user).join(cat).join(&name);
        if p.exists() {
            return Some((p, false));
        }
    }
    let p = user_root(data_dir, user).join("archive").join(&name);
    p.exists().then_some((p, true))
}

fn index_insert(conn: &Connection, user: &str, f: &MemoryFile, path: &Path) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO memory_index (user, id, category, summary, archived, path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        (user, &f.id, &f.category, &f.summary, f.archived as i64, path.to_string_lossy()),
    )?;
    conn.execute("DELETE FROM memory_fts WHERE user = ?1 AND id = ?2", (user, &f.id))?;
    if !f.archived {
        conn.execute(
            "INSERT INTO memory_fts (user, id, summary, body) VALUES (?1, ?2, ?3, ?4)",
            (user, &f.id, &f.summary, &f.body),
        )?;
    }
    Ok(())
}

fn one_line(summary: &str) -> String {
    summary.replace(['\n', '\r'], " ").trim().to_string()
}

pub fn add(conn: &Connection, data_dir: &Path, user: &str, category: &str, summary: &str, body: &str) -> Result<String> {
    if !CATEGORIES.contains(&category) {
        bail!("invalid category: {category}");
    }
    let f = MemoryFile {
        id: uuid::Uuid::new_v4().to_string(),
        category: category.into(),
        summary: one_line(summary),
        body: body.trim().into(),
        supersedes: None,
        created: jiff::Timestamp::now().to_string(),
        archived: false,
    };
    let dir = user_root(data_dir, user).join(category);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.md", f.id));
    std::fs::write(&path, render(&f))?;
    index_insert(conn, user, &f, &path)?;
    Ok(f.id)
}

pub fn read(data_dir: &Path, user: &str, id: &str) -> Result<Option<MemoryFile>> {
    let Some((path, archived)) = locate(data_dir, user, id) else {
        return Ok(None);
    };
    Ok(Some(parse(&std::fs::read_to_string(path)?, archived)?))
}

pub fn update(conn: &Connection, data_dir: &Path, user: &str, id: &str, summary: &str, body: &str) -> Result<Option<()>, WriteError> {
    let Some((path, archived)) = locate(data_dir, user, id) else {
        return Ok(None);
    };
    if archived {
        return Err(WriteError::Archived(id.into()));
    }
    let mut f = parse(&std::fs::read_to_string(&path)?, false)?;
    f.summary = one_line(summary);
    f.body = body.trim().into();
    std::fs::write(&path, render(&f))?;
    index_insert(conn, user, &f, &path)?;
    Ok(Some(()))
}

pub fn supersede(conn: &Connection, data_dir: &Path, user: &str, old_id: &str, summary: &str, body: &str) -> Result<Option<String>, WriteError> {
    let Some((old_path, archived)) = locate(data_dir, user, old_id) else {
        return Ok(None);
    };
    if archived {
        return Err(WriteError::Archived(old_id.into()));
    }
    let mut old = parse(&std::fs::read_to_string(&old_path)?, false)?;
    let new = MemoryFile {
        id: uuid::Uuid::new_v4().to_string(),
        category: old.category.clone(),
        summary: one_line(summary),
        body: body.trim().into(),
        supersedes: Some(old.id.clone()),
        created: jiff::Timestamp::now().to_string(),
        archived: false,
    };
    let new_path = user_root(data_dir, user).join(&new.category).join(format!("{}.md", new.id));
    std::fs::write(&new_path, render(&new))?;
    let arch_dir = user_root(data_dir, user).join("archive");
    std::fs::create_dir_all(&arch_dir)?;
    let arch_path = arch_dir.join(format!("{}.md", old.id));
    std::fs::rename(&old_path, &arch_path)?;
    old.archived = true;
    index_insert(conn, user, &old, &arch_path)?;
    index_insert(conn, user, &new, &new_path)?;
    Ok(Some(new.id))
}

/// Lexical search over non-archived facts. The raw query is reduced to quoted
/// alphanumeric tokens, so model- or user-supplied strings can never produce
/// an FTS5 syntax error.
pub fn query(conn: &Connection, user: &str, q: &str, limit: i64) -> Result<Vec<QueryHit>> {
    let tokens: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(16)
        .map(|t| format!("\"{t}\""))
        .collect();
    if tokens.is_empty() {
        return Ok(vec![]);
    }
    let mut stmt = conn.prepare(
        "SELECT f.id, i.category, i.summary
         FROM memory_fts f
         JOIN memory_index i ON i.user = f.user AND i.id = f.id
         WHERE memory_fts MATCH ?1 AND f.user = ?2 AND i.archived = 0
         ORDER BY rank LIMIT ?3",
    )?;
    let rows = stmt.query_map((tokens.join(" OR "), user, limit), |r| {
        Ok(QueryHit { id: r.get(0)?, category: r.get(1)?, summary: r.get(2)? })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Rebuilds one user's index rows from their files. Unparseable files are
/// logged and skipped so one corrupt fact cannot hide the rest.
pub fn reindex_user(conn: &Connection, data_dir: &Path, user: &str) -> Result<()> {
    conn.execute("DELETE FROM memory_index WHERE user = ?1", [user])?;
    conn.execute("DELETE FROM memory_fts WHERE user = ?1", [user])?;
    let dirs = CATEGORIES.iter().map(|c| (*c, false)).chain([("archive", true)]);
    for (dir_name, archived) in dirs {
        let dir = user_root(data_dir, user).join(dir_name);
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "md") {
                continue;
            }
            match std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|raw| parse(&raw, archived)) {
                Ok(f) => index_insert(conn, user, &f, &path)?,
                Err(e) => {
                    let _ = crate::log::record(conn, None, "memory_index_error",
                        &format!("{}: {e}", path.display()));
                }
            }
        }
    }
    Ok(())
}

pub fn reindex_all(conn: &Connection, data_dir: &Path) -> Result<()> {
    let root = data_dir.join("memory");
    let Ok(entries) = std::fs::read_dir(&root) else { return Ok(()) };
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            reindex_user(conn, data_dir, &entry.file_name().to_string_lossy())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        (crate::db::open_memory().unwrap(), tempfile::tempdir().unwrap())
    }

    #[test]
    fn add_read_roundtrip() {
        let (conn, tmp) = env();
        let id = add(&conn, tmp.path(), "aki", "semantic", "likes tea", "green tea, no sugar").unwrap();
        let f = read(tmp.path(), "aki", &id).unwrap().unwrap();
        assert_eq!(f.summary, "likes tea");
        assert_eq!(f.body, "green tea, no sugar");
        assert_eq!(f.category, "semantic");
        assert!(!f.archived);
        assert!(tmp.path().join("memory/aki/semantic").join(format!("{id}.md")).exists());
    }

    #[test]
    fn query_finds_by_body_token_and_excludes_archived() {
        let (conn, tmp) = env();
        let id = add(&conn, tmp.path(), "aki", "semantic", "dentist", "molar hurts on tuesdays").unwrap();
        let hits = query(&conn, "aki", "molar", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);

        let new_id = supersede(&conn, tmp.path(), "aki", &id, "dentist", "molar fixed").unwrap().unwrap();
        let hits = query(&conn, "aki", "molar", 10).unwrap();
        assert_eq!(hits.len(), 1, "old fact must leave the index");
        assert_eq!(hits[0].id, new_id);
    }

    #[test]
    fn supersede_archives_the_old_file_and_links_it() {
        let (conn, tmp) = env();
        let old = add(&conn, tmp.path(), "aki", "episodic", "s", "b").unwrap();
        let new = supersede(&conn, tmp.path(), "aki", &old, "s2", "b2").unwrap().unwrap();
        assert!(tmp.path().join("memory/aki/archive").join(format!("{old}.md")).exists());
        assert!(!tmp.path().join("memory/aki/episodic").join(format!("{old}.md")).exists());
        let f = read(tmp.path(), "aki", &new).unwrap().unwrap();
        assert_eq!(f.supersedes.as_deref(), Some(old.as_str()));
        // archived facts are readable but immutable
        let archived = read(tmp.path(), "aki", &old).unwrap().unwrap();
        assert!(archived.archived);
        assert!(matches!(
            update(&conn, tmp.path(), "aki", &old, "x", "y"),
            Err(WriteError::Archived(_))
        ));
        assert!(matches!(
            supersede(&conn, tmp.path(), "aki", &old, "x", "y"),
            Err(WriteError::Archived(_))
        ));
    }

    #[test]
    fn update_rewrites_in_place() {
        let (conn, tmp) = env();
        let id = add(&conn, tmp.path(), "aki", "procedural", "s", "b").unwrap();
        update(&conn, tmp.path(), "aki", &id, "s revised", "b revised").unwrap().unwrap();
        let f = read(tmp.path(), "aki", &id).unwrap().unwrap();
        assert_eq!(f.summary, "s revised");
        assert_eq!(query(&conn, "aki", "revised", 10).unwrap().len(), 1);
    }

    #[test]
    fn reindex_rebuilds_from_files_alone() {
        let (conn, tmp) = env();
        let id = add(&conn, tmp.path(), "aki", "semantic", "findme", "needle haystack").unwrap();
        conn.execute("DELETE FROM memory_index", []).unwrap();
        conn.execute("DELETE FROM memory_fts WHERE user='aki'", []).unwrap();
        assert!(query(&conn, "aki", "needle", 10).unwrap().is_empty());
        reindex_user(&conn, tmp.path(), "aki").unwrap();
        assert_eq!(query(&conn, "aki", "needle", 10).unwrap()[0].id, id);
    }

    #[test]
    fn ids_are_validated_before_touching_the_filesystem() {
        let (conn, tmp) = env();
        for bad in ["../../../etc/passwd", "..", "x/y", "", "A-UPPER-ID-000000000000000000000000000"] {
            assert!(!valid_id(bad), "{bad:?} must be invalid");
            assert!(read(tmp.path(), "aki", bad).unwrap().is_none());
            assert!(update(&conn, tmp.path(), "aki", bad, "s", "b").unwrap().is_none());
        }
    }

    #[test]
    fn hostile_query_strings_never_error() {
        let (conn, tmp) = env();
        add(&conn, tmp.path(), "aki", "semantic", "s", "b").unwrap();
        for q in ["\"unbalanced", "a OR OR", "(((", "*", "co-lu:mn NEAR/x", "", "   "] {
            query(&conn, "aki", q, 10).unwrap();
        }
    }
}
