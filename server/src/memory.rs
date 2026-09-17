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

/// The exact text shape the vector index embeds for a fact, shared with the
/// tool-path prepare pass so prepared vectors match index-time vectors.
pub(crate) fn embed_text(summary: &str, body: &str) -> String {
    format!("{}\n{}", one_line(summary), body.trim())
}

/// Writes through a sibling temp file so a crash mid-write can never leave a
/// half-rendered fact where the index expects a whole one.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>().0.iter().copied().map(f32::from_le_bytes).collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na * nb) }
}

/// A missing vector degrades to no index entry — the write itself already
/// succeeded, and embed-failure logging happens on the dispatch path.
fn store_vector(conn: &Connection, user: &str, id: &str, vector: Option<&[f32]>) {
    let Some(v) = vector else { return };
    let _ = conn.execute(
        "INSERT OR REPLACE INTO memory_vectors (user, id, vector) VALUES (?1, ?2, ?3)",
        (user, id, vec_to_blob(v)),
    );
}

pub fn add(
    conn: &Connection,
    data_dir: &Path,
    user: &str,
    category: &str,
    summary: &str,
    body: &str,
    vector: Option<&[f32]>,
) -> Result<String> {
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
    write_atomic(&path, &render(&f))?;
    index_insert(conn, user, &f, &path)?;
    store_vector(conn, user, &f.id, vector);
    Ok(f.id)
}

pub fn read(data_dir: &Path, user: &str, id: &str) -> Result<Option<MemoryFile>> {
    let Some((path, archived)) = locate(data_dir, user, id) else {
        return Ok(None);
    };
    Ok(Some(parse(&std::fs::read_to_string(path)?, archived)?))
}

pub fn read_raw(data_dir: &Path, user: &str, id: &str) -> Result<Option<String>> {
    let Some((path, _)) = locate(data_dir, user, id) else {
        return Ok(None);
    };
    Ok(Some(std::fs::read_to_string(path)?))
}

/// Replaces the file verbatim and re-indexes it; the content must still parse
/// as a memory file, and the archive flag follows the directory it lives in.
pub fn write_raw(conn: &Connection, data_dir: &Path, user: &str, id: &str, raw: &str) -> Result<Option<()>> {
    let Some((path, archived)) = locate(data_dir, user, id) else {
        return Ok(None);
    };
    let f = parse(raw, archived)?;
    anyhow::ensure!(f.id == id, "frontmatter id {} does not match {id}", f.id);
    write_atomic(&path, raw)?;
    index_insert(conn, user, &f, &path)?;
    Ok(Some(()))
}

pub fn update(
    conn: &Connection,
    data_dir: &Path,
    user: &str,
    id: &str,
    summary: &str,
    body: &str,
    vector: Option<&[f32]>,
) -> Result<Option<()>, WriteError> {
    let Some((path, archived)) = locate(data_dir, user, id) else {
        return Ok(None);
    };
    if archived {
        return Err(WriteError::Archived(id.into()));
    }
    let mut f = parse(&std::fs::read_to_string(&path)?, false)?;
    f.summary = one_line(summary);
    f.body = body.trim().into();
    write_atomic(&path, &render(&f))?;
    index_insert(conn, user, &f, &path)?;
    store_vector(conn, user, &f.id, vector);
    Ok(Some(()))
}

pub fn supersede(
    conn: &Connection,
    data_dir: &Path,
    user: &str,
    old_id: &str,
    summary: &str,
    body: &str,
    vector: Option<&[f32]>,
) -> Result<Option<String>, WriteError> {
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
    write_atomic(&new_path, &render(&new))?;
    let arch_dir = user_root(data_dir, user).join("archive");
    std::fs::create_dir_all(&arch_dir)?;
    let arch_path = arch_dir.join(format!("{}.md", old.id));
    std::fs::rename(&old_path, &arch_path)?;
    old.archived = true;
    index_insert(conn, user, &old, &arch_path)?;
    index_insert(conn, user, &new, &new_path)?;
    conn.execute("DELETE FROM memory_vectors WHERE user = ?1 AND id = ?2", (user, &old.id))?;
    store_vector(conn, user, &new.id, vector);
    Ok(Some(new.id))
}

/// Lexical search over non-archived facts. The raw query is reduced to quoted
/// alphanumeric tokens, so model- or user-supplied strings can never produce
/// an FTS5 syntax error.
fn lexical_query(conn: &Connection, user: &str, q: &str, limit: i64) -> Result<Vec<QueryHit>> {
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

/// Ids of the user's live facts by cosine similarity, best first.
fn vector_query(conn: &Connection, user: &str, qv: &[f32], limit: usize) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT v.id, v.vector
         FROM memory_vectors v
         JOIN memory_index i ON i.user = v.user AND i.id = v.id
         WHERE v.user = ?1 AND i.archived = 0",
    )?;
    let rows = stmt.query_map([user], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
    let mut scored: Vec<(String, f32)> = rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(|(id, blob)| {
            let score = cosine(qv, &blob_to_vec(&blob));
            (id, score)
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(limit);
    Ok(scored.into_iter().map(|(id, _)| id).collect())
}

fn hit_for(conn: &Connection, user: &str, id: &str, lexical: &[QueryHit]) -> Result<QueryHit> {
    if let Some(h) = lexical.iter().find(|h| h.id == id) {
        return Ok(QueryHit { id: h.id.clone(), category: h.category.clone(), summary: h.summary.clone() });
    }
    let (category, summary) = conn.query_row(
        "SELECT category, summary FROM memory_index WHERE user = ?1 AND id = ?2",
        (user, id),
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )?;
    Ok(QueryHit { id: id.to_string(), category, summary })
}

/// Reciprocal rank fusion of the lexical and vector arms; degrades to the
/// lexical ranking alone whenever the vector arm returns nothing.
pub fn query(
    conn: &Connection,
    user: &str,
    q: &str,
    limit: i64,
    query_vec: Option<&[f32]>,
) -> Result<Vec<QueryHit>> {
    let take = limit as usize;
    let lexical = lexical_query(conn, user, q, limit.max(32))?;
    let vector = match query_vec {
        Some(qv) => vector_query(conn, user, qv, 32)?,
        None => vec![],
    };
    if vector.is_empty() {
        return Ok(lexical.into_iter().take(take).collect());
    }
    let mut scores: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for (rank, hit) in lexical.iter().enumerate() {
        *scores.entry(hit.id.clone()).or_default() += 1.0 / (60.0 + rank as f64);
    }
    for (rank, id) in vector.iter().enumerate() {
        *scores.entry(id.clone()).or_default() += 1.0 / (60.0 + rank as f64);
    }
    let mut ids: Vec<(String, f64)> = scores.into_iter().collect();
    ids.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ids.truncate(take);
    ids.into_iter().map(|(id, _)| hit_for(conn, user, &id, &lexical)).collect()
}

/// How many live facts a user holds. The situational block says so, so a
/// session knows before searching whether there is anything to find.
pub fn live_count(conn: &Connection, user: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM memory_index WHERE user = ?1 AND archived = 0",
        [user],
        |r| r.get(0),
    )?)
}

/// Newest-first browse over a user's live facts, with an optional exact
/// category filter — the no-search counterpart to `query`.
pub fn list(
    conn: &Connection,
    user: &str,
    category: Option<&str>,
    limit: usize,
) -> Result<Vec<QueryHit>> {
    let mut stmt = conn.prepare(
        "SELECT id, category, summary FROM memory_index
         WHERE user = ?1 AND archived = 0 AND (?2 IS NULL OR category = ?2)
         ORDER BY rowid DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map((user, category, limit as i64), |r| {
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
    conn.execute(
        "DELETE FROM memory_vectors WHERE user = ?1
         AND id NOT IN (SELECT id FROM memory_index WHERE user = ?1 AND archived = 0)",
        [user],
    )?;
    Ok(())
}

pub fn reindex_all(conn: &Connection, data_dir: &Path) -> Result<()> {
    let root = data_dir.join("memory");
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) => {
            if root.exists() {
                let _ = crate::log::record(conn, None, "memory_index_error",
                    &format!("{}: {e}", root.display()));
            }
            return Ok(());
        }
    };
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
        let id = add(&conn, tmp.path(), "aki", "semantic", "likes tea", "green tea, no sugar", None).unwrap();
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
        let id = add(&conn, tmp.path(), "aki", "semantic", "dentist", "molar hurts on tuesdays", None).unwrap();
        let hits = query(&conn, "aki", "molar", 10, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);

        let new_id = supersede(&conn, tmp.path(), "aki", &id, "dentist", "molar fixed", None).unwrap().unwrap();
        let hits = query(&conn, "aki", "molar", 10, None).unwrap();
        assert_eq!(hits.len(), 1, "old fact must leave the index");
        assert_eq!(hits[0].id, new_id);
    }

    #[test]
    fn supersede_archives_the_old_file_and_links_it() {
        let (conn, tmp) = env();
        let old = add(&conn, tmp.path(), "aki", "episodic", "s", "b", None).unwrap();
        let new = supersede(&conn, tmp.path(), "aki", &old, "s2", "b2", None).unwrap().unwrap();
        assert!(tmp.path().join("memory/aki/archive").join(format!("{old}.md")).exists());
        assert!(!tmp.path().join("memory/aki/episodic").join(format!("{old}.md")).exists());
        let f = read(tmp.path(), "aki", &new).unwrap().unwrap();
        assert_eq!(f.supersedes.as_deref(), Some(old.as_str()));
        // archived facts are readable but immutable
        let archived = read(tmp.path(), "aki", &old).unwrap().unwrap();
        assert!(archived.archived);
        assert!(matches!(
            update(&conn, tmp.path(), "aki", &old, "x", "y", None),
            Err(WriteError::Archived(_))
        ));
        assert!(matches!(
            supersede(&conn, tmp.path(), "aki", &old, "x", "y", None),
            Err(WriteError::Archived(_))
        ));
    }

    #[test]
    fn update_rewrites_in_place() {
        let (conn, tmp) = env();
        let id = add(&conn, tmp.path(), "aki", "procedural", "s", "b", None).unwrap();
        update(&conn, tmp.path(), "aki", &id, "s revised", "b revised", None).unwrap().unwrap();
        let f = read(tmp.path(), "aki", &id).unwrap().unwrap();
        assert_eq!(f.summary, "s revised");
        assert_eq!(query(&conn, "aki", "revised", 10, None).unwrap().len(), 1);
    }

    #[test]
    fn reindex_rebuilds_from_files_alone() {
        let (conn, tmp) = env();
        let id = add(&conn, tmp.path(), "aki", "semantic", "findme", "needle haystack", None).unwrap();
        conn.execute("DELETE FROM memory_index", []).unwrap();
        conn.execute("DELETE FROM memory_fts WHERE user='aki'", []).unwrap();
        assert!(query(&conn, "aki", "needle", 10, None).unwrap().is_empty());
        reindex_user(&conn, tmp.path(), "aki").unwrap();
        assert_eq!(query(&conn, "aki", "needle", 10, None).unwrap()[0].id, id);
    }

    #[test]
    fn ids_are_validated_before_touching_the_filesystem() {
        let (conn, tmp) = env();
        for bad in ["../../../etc/passwd", "..", "x/y", "", "A-UPPER-ID-000000000000000000000000000"] {
            assert!(!valid_id(bad), "{bad:?} must be invalid");
            assert!(read(tmp.path(), "aki", bad).unwrap().is_none());
            assert!(update(&conn, tmp.path(), "aki", bad, "s", "b", None).unwrap().is_none());
        }
    }

    #[test]
    fn vectors_written_on_add_and_pruned_on_supersede() {
        use crate::providers::EmbeddingsProvider;
        let (conn, tmp) = env();
        let e = crate::providers::mock::MockEmbeddings;
        let v = e.embed(&[&embed_text("abc", "abc")]).unwrap();
        let id = add(&conn, tmp.path(), "aki", "semantic", "abc", "abc", Some(&v[0])).unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_vectors WHERE user='aki' AND id=?1", [&id], |r| r.get(0),
        ).unwrap();
        assert_eq!(n, 1);
        let new_id = supersede(&conn, tmp.path(), "aki", &id, "abc", "abc", Some(&v[0])).unwrap().unwrap();
        let old_n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_vectors WHERE id=?1", [&id], |r| r.get(0),
        ).unwrap();
        assert_eq!(old_n, 0, "superseded vector must be pruned");
        let new_n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_vectors WHERE id=?1", [&new_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(new_n, 1);
    }

    #[test]
    fn hybrid_query_ranks_vector_similar_fact_first() {
        use crate::providers::EmbeddingsProvider;
        let (conn, tmp) = env();
        let e = crate::providers::mock::MockEmbeddings;
        // lexically, neither fact contains the query token "aaab"; only vectors can rank them
        let cv = e.embed(&[&embed_text("zz", "aaaa")]).unwrap();
        let close = add(&conn, tmp.path(), "aki", "semantic", "zz", "aaaa", Some(&cv[0])).unwrap();
        let fv = e.embed(&[&embed_text("zz", "hhhh")]).unwrap();
        let _far = add(&conn, tmp.path(), "aki", "semantic", "zz", "hhhh", Some(&fv[0])).unwrap();
        let qv = e.embed(&["aaab"]).unwrap();
        let hits = query(&conn, "aki", "aaab", 2, Some(&qv[0])).unwrap();
        assert!(!hits.is_empty(), "vector arm must contribute hits with no lexical match");
        assert_eq!(hits[0].id, close);
    }

    #[test]
    fn query_without_vector_stays_lexical_even_when_vectors_exist() {
        use crate::providers::EmbeddingsProvider;
        let (conn, tmp) = env();
        let e = crate::providers::mock::MockEmbeddings;
        let v = e.embed(&[&embed_text("dentist", "molar hurts")]).unwrap();
        let id = add(&conn, tmp.path(), "aki", "semantic", "dentist", "molar hurts", Some(&v[0])).unwrap();
        let hits = query(&conn, "aki", "molar", 10, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);
    }

    #[test]
    fn query_without_provider_stays_lexical() {
        let (conn, tmp) = env();
        let id = add(&conn, tmp.path(), "aki", "semantic", "dentist", "molar", None).unwrap();
        let hits = query(&conn, "aki", "molar", 10, None).unwrap();
        assert_eq!(hits[0].id, id);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM memory_vectors", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn limit_above_the_candidate_pool_is_honoured() {
        let (conn, tmp) = env();
        for i in 0..40 {
            add(&conn, tmp.path(), "aki", "semantic", "s", &format!("needle {i}"), None).unwrap();
        }
        assert_eq!(query(&conn, "aki", "needle", 40, None).unwrap().len(), 40);
        assert_eq!(query(&conn, "aki", "needle", 10, None).unwrap().len(), 10);
    }

    #[test]
    fn atomic_write_leaves_no_tmp_files() {
        let (conn, tmp) = env();
        add(&conn, tmp.path(), "aki", "semantic", "s", "b", None).unwrap();
        let dir = tmp.path().join("memory/aki/semantic");
        let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap()
            .flatten()
            .filter(|f| f.path().extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn list_returns_newest_first_and_filters_category() {
        let (conn, tmp) = env();
        let a = add(&conn, tmp.path(), "aki", "semantic", "fact a", "body a", None).unwrap();
        let b = add(&conn, tmp.path(), "aki", "episodic", "fact b", "body b", None).unwrap();
        let c = add(&conn, tmp.path(), "aki", "semantic", "fact c", "body c", None).unwrap();
        add(&conn, tmp.path(), "other", "semantic", "fact d", "body d", None).unwrap();

        let all = list(&conn, "aki", None, 50).unwrap();
        assert_eq!(
            all.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
            vec![c.as_str(), b.as_str(), a.as_str()]
        );
        assert_eq!(all[0].summary, "fact c");

        let sem = list(&conn, "aki", Some("semantic"), 50).unwrap();
        assert_eq!(sem.len(), 2);
        assert!(sem.iter().all(|h| h.category == "semantic"));

        assert_eq!(list(&conn, "aki", None, 1).unwrap().len(), 1);
        assert!(list(&conn, "nobody", None, 50).unwrap().is_empty());
    }

    #[test]
    fn list_excludes_archived() {
        let (conn, tmp) = env();
        let old = add(&conn, tmp.path(), "aki", "semantic", "old", "old body", None).unwrap();
        let new = supersede(&conn, tmp.path(), "aki", &old, "new", "new body", None).unwrap().unwrap();
        let ids: Vec<String> = list(&conn, "aki", None, 50).unwrap().into_iter().map(|h| h.id).collect();
        assert_eq!(ids, vec![new]);
    }

    #[test]
    fn hostile_query_strings_never_error() {
        let (conn, tmp) = env();
        add(&conn, tmp.path(), "aki", "semantic", "s", "b", None).unwrap();
        for q in ["\"unbalanced", "a OR OR", "(((", "*", "co-lu:mn NEAR/x", "", "   "] {
            query(&conn, "aki", q, 10, None).unwrap();
        }
    }
}
