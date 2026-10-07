use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use thiserror::Error;
use std::fmt::Write as _;

pub const CATEGORIES: [&str; 3] = ["semantic", "episodic", "procedural"];

pub const NOTE: &str = "note";

/// Where live files sit: the long-term categories, then Note's working notes.
const LIVE_DIRS: [&str; 4] = [CATEGORIES[0], CATEGORIES[1], CATEGORIES[2], NOTE];

#[derive(Debug, Clone)]
pub struct MemoryFile {
    pub id: String,
    pub category: String,
    pub summary: String,
    pub body: String,
    pub supersedes: Option<String>,
    /// The date after which the fact stops mattering, swept a day after it
    /// passes; on a working note, the RFC 3339 instant its window closes.
    pub until: Option<String>,
    pub created: String,
    pub archived: bool,
    /// What wrote the fact when it was not the user's own sessions; `note` for
    /// Note's built-in knowledge of itself.
    pub source: Option<String>,
    /// A working note's window opening and bookkeeping, RFC 3339 instants.
    pub from: Option<String>,
    pub touched_at: Option<String>,
    pub last_nudged_at: Option<String>,
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

pub(crate) fn user_root(data_dir: &Path, user: &str) -> PathBuf {
    data_dir.join("memory").join(user)
}

fn render(f: &MemoryFile) -> String {
    let mut fm = format!(
        "---\nid: {}\ncategory: {}\nsummary: {}\ncreated: {}\n",
        f.id, f.category, f.summary, f.created
    );
    if let Some(s) = &f.supersedes {
        let _ = writeln!(fm, "supersedes: {s}");
    }
    if let Some(u) = &f.until {
        let _ = writeln!(fm, "until: {u}");
    }
    if let Some(s) = &f.source {
        let _ = writeln!(fm, "source: {s}");
    }
    for (key, value) in [("from", &f.from), ("touched_at", &f.touched_at), ("last_nudged_at", &f.last_nudged_at)] {
        if let Some(v) = value {
            let _ = writeln!(fm, "{key}: {v}");
        }
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
        until: None,
        created: String::new(),
        archived,
        source: None,
        from: None,
        touched_at: None,
        last_nudged_at: None,
    };
    for line in fm.lines() {
        let Some((k, v)) = line.split_once(": ") else { continue };
        match k {
            "id" => f.id = v.into(),
            "category" => f.category = v.into(),
            "summary" => f.summary = v.into(),
            "created" => f.created = v.into(),
            "supersedes" => f.supersedes = Some(v.into()),
            "until" => f.until = Some(v.into()),
            "source" => f.source = Some(v.into()),
            "from" => f.from = Some(v.into()),
            "touched_at" => f.touched_at = Some(v.into()),
            "last_nudged_at" => f.last_nudged_at = Some(v.into()),
            _ => {}
        }
    }
    if !valid_id(&f.id) {
        bail!("bad or missing id in memory file frontmatter");
    }
    if !LIVE_DIRS.contains(&f.category.as_str()) {
        bail!("bad category {:?} in memory file {}", f.category, f.id);
    }
    Ok(f)
}

fn locate(data_dir: &Path, user: &str, id: &str) -> Option<(PathBuf, bool)> {
    if !valid_id(id) {
        return None;
    }
    let name = format!("{id}.md");
    for cat in LIVE_DIRS {
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
        "INSERT OR REPLACE INTO memory_index (user, id, category, summary, archived, path, until)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        (user, &f.id, &f.category, &f.summary, i64::from(f.archived), path.to_string_lossy(), &f.until),
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

pub(crate) fn one_line(summary: &str) -> String {
    summary.replace(['\n', '\r'], " ").trim().to_string()
}

/// The exact text shape the vector index embeds for a fact, shared with the
/// tool-path prepare pass so prepared vectors match index-time vectors.
pub(crate) fn embed_text(summary: &str, body: &str) -> String {
    let body = body.trim();
    let cut = body
        .char_indices()
        .map(|(i, _)| i)
        .find(|&i| i >= MAX_EMBED_BODY_CHARS)
        .unwrap_or(body.len());
    format!("{}\n{}", one_line(summary), &body[..cut])
}

/// What the embedding model is given of a body; the endpoint refuses inputs
/// past its batch size, and the summary plus this much carries the meaning.
pub const MAX_EMBED_BODY_CHARS: usize = 2200;

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

/// Live facts the index knows but the vector table does not: everything written
/// before embeddings existed, or while the endpoint was down. Embeds them in
/// batches and stores each vector; returns how many were filled.
pub fn backfill_vectors(
    db: &std::sync::Mutex<Connection>,
    data_dir: &Path,
    embeddings: &dyn crate::providers::EmbeddingsProvider,
) -> Result<usize> {
    let missing: Vec<(String, String)> = {
        let conn = crate::db_guard(db);
        let mut stmt = conn.prepare(
            "SELECT i.user, i.id FROM memory_index i
             LEFT JOIN memory_vectors v ON v.user = i.user AND v.id = i.id
             WHERE i.archived = 0 AND i.category != 'note' AND v.id IS NULL ORDER BY i.rowid",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut filled = 0;
    for batch in missing.chunks(16) {
        let mut texts = Vec::new();
        let mut keys = Vec::new();
        for (user, id) in batch {
            if let Some(f) = read(data_dir, user, id)? {
                texts.push(embed_text(&f.summary, &f.body));
                keys.push((user.clone(), id.clone()));
            }
        }
        if texts.is_empty() {
            continue;
        }
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let vectors = match embeddings.embed(&refs) {
            Ok(v) => v.into_iter().map(Some).collect::<Vec<_>>(),
            Err(_) => refs
                .iter()
                .map(|t| embeddings.embed(&[t]).ok().and_then(|mut v| v.pop()))
                .collect(),
        };
        let conn = crate::db_guard(db);
        for ((user, id), v) in keys.iter().zip(vectors.iter()) {
            if v.is_some() {
                store_vector(&conn, user, id, v.as_deref());
                filled += 1;
            }
        }
    }
    Ok(filled)
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
    add_until(conn, data_dir, user, &Fact { category, summary, body, until: None }, vector)
}

/// What a memory says, before it has an id: `until` is an expiry date
/// (`YYYY-MM-DD`) past which the nightly sweep archives it.
#[derive(Debug, Clone, Copy)]
pub struct Fact<'a> {
    pub category: &'a str,
    pub summary: &'a str,
    pub body: &'a str,
    pub until: Option<&'a str>,
}

/// `add` with an expiry date (`YYYY-MM-DD`) written into the file, so a fact
/// that stops mattering can be swept without the writer tracking it.
pub fn add_until(
    conn: &Connection,
    data_dir: &Path,
    user: &str,
    fact: &Fact,
    vector: Option<&[f32]>,
) -> Result<String> {
    write_new(conn, data_dir, user, fact, None, vector)
}

/// `add` for a fact Note writes on its own behalf, marked with `source`.
pub fn add_sourced(
    conn: &Connection,
    data_dir: &Path,
    user: &str,
    fact: &Fact,
    source: &str,
) -> Result<String> {
    write_new(conn, data_dir, user, fact, Some(source), None)
}

fn write_new(
    conn: &Connection,
    data_dir: &Path,
    user: &str,
    fact: &Fact,
    source: Option<&str>,
    vector: Option<&[f32]>,
) -> Result<String> {
    let Fact { category, summary, body, until } = *fact;
    if !CATEGORIES.contains(&category) {
        bail!("invalid category: {category}");
    }
    let f = MemoryFile {
        id: uuid::Uuid::new_v4().to_string(),
        category: category.into(),
        summary: one_line(summary),
        body: body.trim().into(),
        supersedes: None,
        until: until.map(str::to_owned),
        created: jiff::Timestamp::now().to_string(),
        archived: false,
        source: source.map(str::to_owned),
        from: None,
        touched_at: None,
        last_nudged_at: None,
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
    f.source = None;
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
    supersede_sourced(conn, data_dir, user, old_id, (summary, body), None, vector)
}

/// `supersede` whose replacement carries `source`; the category is kept.
pub fn supersede_sourced(
    conn: &Connection,
    data_dir: &Path,
    user: &str,
    old_id: &str,
    (summary, body): (&str, &str),
    source: Option<&str>,
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
        until: None,
        created: jiff::Timestamp::now().to_string(),
        archived: false,
        source: source.map(str::to_owned),
        from: None,
        touched_at: None,
        last_nudged_at: None,
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

/// Moves one live fact into the user's archive, where it stays readable and
/// immutable. Returns whether anything moved: an unknown or already-archived
/// id is a no-op, so a re-run is harmless.
pub fn archive(conn: &Connection, data_dir: &Path, user: &str, id: &str) -> Result<bool> {
    let Some((path, archived)) = locate(data_dir, user, id) else {
        return Ok(false);
    };
    if archived {
        return Ok(false);
    }
    let mut f = parse(&std::fs::read_to_string(&path)?, false)?;
    let arch_dir = user_root(data_dir, user).join("archive");
    std::fs::create_dir_all(&arch_dir)?;
    let arch_path = arch_dir.join(format!("{id}.md"));
    std::fs::rename(&path, &arch_path)?;
    f.archived = true;
    index_insert(conn, user, &f, &arch_path)?;
    conn.execute("DELETE FROM memory_vectors WHERE user = ?1 AND id = ?2", (user, id))?;
    Ok(true)
}

/// Writes `f` into its category's directory and indexes it, replacing a live
/// file with the same id.
pub fn put(conn: &Connection, data_dir: &Path, user: &str, f: &MemoryFile) -> Result<()> {
    if !valid_id(&f.id) {
        bail!("bad memory id {:?}", f.id);
    }
    if !LIVE_DIRS.contains(&f.category.as_str()) {
        bail!("invalid category: {}", f.category);
    }
    let dir = user_root(data_dir, user).join(&f.category);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.md", f.id));
    write_atomic(&path, &render(f))?;
    index_insert(conn, user, f, &path)?;
    Ok(())
}

/// Deletes a live file with its index, search and vector rows; an archived or
/// unknown id is left alone.
pub fn remove(conn: &Connection, data_dir: &Path, user: &str, id: &str) -> Result<bool> {
    let Some((path, false)) = locate(data_dir, user, id) else {
        return Ok(false);
    };
    std::fs::remove_file(&path)?;
    conn.execute("DELETE FROM memory_index WHERE user = ?1 AND id = ?2", (user, id))?;
    conn.execute("DELETE FROM memory_fts WHERE user = ?1 AND id = ?2", (user, id))?;
    conn.execute("DELETE FROM memory_vectors WHERE user = ?1 AND id = ?2", (user, id))?;
    Ok(true)
}

/// Every readable live file of one category, in no particular order; the
/// unreadable ones go to the event log.
pub fn live_files(conn: &Connection, data_dir: &Path, user: &str, category: &str) -> Result<Vec<MemoryFile>> {
    let Ok(entries) = std::fs::read_dir(user_root(data_dir, user).join(category)) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        match std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|raw| parse(&raw, false)) {
            Ok(f) => out.push(f),
            Err(e) => {
                let _ = crate::log::record(conn, None, "memory_index_error", &format!("{}: {e}", path.display()));
            }
        }
    }
    Ok(out)
}

/// Archives the user's live facts whose `until` is more than one day past, so
/// a quiz date still shows up on the day after it. Returns how many moved.
pub fn archive_expired(
    conn: &Connection,
    data_dir: &Path,
    user: &str,
    today: jiff::civil::Date,
) -> Result<usize> {
    let cutoff = today.yesterday().unwrap_or(today).to_string();
    let mut stmt = conn.prepare(
        "SELECT id FROM memory_index
         WHERE user = ?1 AND archived = 0 AND category != 'note' AND until IS NOT NULL AND until < ?2",
    )?;
    let ids: Vec<String> = stmt
        .query_map((user, &cutoff), |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut n = 0;
    for id in ids {
        if archive(conn, data_dir, user, &id)? {
            n += 1;
        }
    }
    if n > 0 {
        let _ = crate::log::record(conn, None, "memory_expired", &format!("{user}: {n} archived"));
    }
    Ok(n)
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
         WHERE memory_fts MATCH ?1 AND f.user = ?2 AND i.archived = 0 AND i.category != 'note'
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
         WHERE v.user = ?1 AND i.archived = 0 AND i.category != 'note'",
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
        "SELECT COUNT(*) FROM memory_index WHERE user = ?1 AND archived = 0 AND category != 'note'",
        [user],
        |r| r.get(0),
    )?)
}

/// The same count split by category, in `CATEGORIES` order, for a surface that
/// says what kind of memory a user is carrying.
pub fn live_count_by_category(conn: &Connection, user: &str) -> Result<[i64; CATEGORIES.len()]> {
    let mut stmt = conn.prepare(
        "SELECT category, COUNT(*) FROM memory_index
         WHERE user = ?1 AND archived = 0 GROUP BY category",
    )?;
    let mut out = [0i64; CATEGORIES.len()];
    for row in stmt.query_map([user], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (category, n) = row?;
        if let Some(i) = CATEGORIES.iter().position(|c| *c == category) {
            out[i] = n;
        }
    }
    Ok(out)
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
         WHERE user = ?1 AND archived = 0 AND category != 'note' AND (?2 IS NULL OR category = ?2)
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
    let dirs = LIVE_DIRS.iter().map(|c| (*c, false)).chain([("archive", true)]);
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
    fn an_until_date_survives_a_reindex() {
        let (conn, tmp) = env();
        let id = add_until(&conn, tmp.path(), "aki", &Fact { category: "semantic", summary: "quiz", body: "chapter 4 quiz", until: Some("2026-09-25") }, None).unwrap();
        let plain = add(&conn, tmp.path(), "aki", "semantic", "rule", "bring a pencil", None).unwrap();
        let until = |id: &str| -> Option<String> {
            conn.query_row("SELECT until FROM memory_index WHERE user='aki' AND id=?1", [id],
                |r| r.get(0)).unwrap()
        };
        assert_eq!(until(&id).as_deref(), Some("2026-09-25"));
        assert_eq!(until(&plain), None);

        reindex_user(&conn, tmp.path(), "aki").unwrap();
        assert_eq!(until(&id).as_deref(), Some("2026-09-25"), "until must live in the file");
        assert_eq!(until(&plain), None);
        assert_eq!(read(tmp.path(), "aki", &id).unwrap().unwrap().until.as_deref(), Some("2026-09-25"));
    }

    #[test]
    fn archive_moves_a_live_fact_out_of_the_index_and_prunes_its_vector() {
        use crate::providers::EmbeddingsProvider;
        let (conn, tmp) = env();
        let e = crate::providers::mock::MockEmbeddings;
        let v = e.embed(&[&embed_text("s", "b")]).unwrap();
        let id = add(&conn, tmp.path(), "aki", "semantic", "s", "b", Some(&v[0])).unwrap();
        assert!(archive(&conn, tmp.path(), "aki", &id).unwrap());
        assert!(tmp.path().join("memory/aki/archive").join(format!("{id}.md")).exists());
        assert!(list(&conn, "aki", None, 50).unwrap().is_empty());
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM memory_vectors WHERE id=?1", [&id],
            |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        // archiving twice, or an id that never existed, is a no-op
        assert!(!archive(&conn, tmp.path(), "aki", &id).unwrap());
        assert!(!archive(&conn, tmp.path(), "aki", "00000000-0000-4000-8000-000000000000").unwrap());
    }

    #[test]
    fn archive_expired_keeps_yesterday_and_archives_the_day_before() {
        let (conn, tmp) = env();
        let today: jiff::civil::Date = "2026-09-16".parse().unwrap();
        let live = |d: &str| add_until(&conn, tmp.path(), "aki", &Fact { category: "semantic", summary: d, body: d, until: Some(d) }, None).unwrap();
        let today_id = live("2026-09-16");
        let yesterday = live("2026-09-15");
        let two_days = live("2026-09-14");
        let standing = add(&conn, tmp.path(), "aki", "semantic", "rule", "bring a pencil", None).unwrap();

        assert_eq!(archive_expired(&conn, tmp.path(), "aki", today).unwrap(), 1);
        let live_ids: Vec<String> =
            list(&conn, "aki", None, 50).unwrap().into_iter().map(|h| h.id).collect();
        assert!(live_ids.contains(&today_id));
        assert!(live_ids.contains(&yesterday), "a fact one day past is still kept");
        assert!(live_ids.contains(&standing), "a standing fact never expires");
        assert!(!live_ids.contains(&two_days));
        assert!(read(tmp.path(), "aki", &two_days).unwrap().unwrap().archived);

        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM event_log WHERE kind = 'memory_expired'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // a sweep that archives nothing logs nothing
        assert_eq!(archive_expired(&conn, tmp.path(), "aki", today).unwrap(), 0);
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM event_log WHERE kind = 'memory_expired'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn archive_expired_leaves_other_users_alone() {
        let (conn, tmp) = env();
        let today: jiff::civil::Date = "2026-09-16".parse().unwrap();
        let bo = add_until(&conn, tmp.path(), "bo", &Fact { category: "semantic", summary: "s", body: "b", until: Some("2026-01-01") }, None).unwrap();
        add_until(&conn, tmp.path(), "aki", &Fact { category: "semantic", summary: "s", body: "b", until: Some("2026-01-01") }, None).unwrap();
        assert_eq!(archive_expired(&conn, tmp.path(), "aki", today).unwrap(), 1);
        assert!(!read(tmp.path(), "bo", &bo).unwrap().unwrap().archived);
    }

    #[test]
    fn hostile_query_strings_never_error() {
        let (conn, tmp) = env();
        add(&conn, tmp.path(), "aki", "semantic", "s", "b", None).unwrap();
        for q in ["\"unbalanced", "a OR OR", "(((", "*", "co-lu:mn NEAR/x", "", "   "] {
            query(&conn, "aki", q, 10, None).unwrap();
        }
    }

    const NOTE_ID: &str = "00000000-0000-4000-8000-000000000001";

    fn a_note(id: &str) -> MemoryFile {
        MemoryFile {
            id: id.into(),
            category: NOTE.into(),
            summary: "call the bank before five".into(),
            body: String::new(),
            supersedes: None,
            until: Some("2026-10-07T17:00:00+09:00".into()),
            created: "2026-10-07T00:00:00Z".into(),
            archived: false,
            source: None,
            from: Some("2026-10-07T09:00:00+09:00".into()),
            touched_at: Some("2026-10-07T01:00:00Z".into()),
            last_nudged_at: Some("2026-10-07T02:00:00Z".into()),
        }
    }

    #[test]
    fn a_note_file_round_trips_its_window_and_stamps_through_a_reindex() {
        let (conn, tmp) = env();
        put(&conn, tmp.path(), "aki", &a_note(NOTE_ID)).unwrap();
        assert!(tmp.path().join("memory/aki/note").join(format!("{NOTE_ID}.md")).exists());
        let check = || {
            let f = read(tmp.path(), "aki", NOTE_ID).unwrap().unwrap();
            assert_eq!(f.category, "note");
            assert_eq!(f.body, "");
            assert_eq!(f.from.as_deref(), Some("2026-10-07T09:00:00+09:00"));
            assert_eq!(f.until.as_deref(), Some("2026-10-07T17:00:00+09:00"));
            assert_eq!(f.touched_at.as_deref(), Some("2026-10-07T01:00:00Z"));
            assert_eq!(f.last_nudged_at.as_deref(), Some("2026-10-07T02:00:00Z"));
        };
        check();
        reindex_user(&conn, tmp.path(), "aki").unwrap();
        check();
        let indexed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_index WHERE user = 'aki' AND id = ?1 AND category = 'note'",
                [NOTE_ID],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indexed, 1, "a reindex keeps the note in the index");
        assert_eq!(live_files(&conn, tmp.path(), "aki", NOTE).unwrap().len(), 1);
        assert!(live_files(&conn, tmp.path(), "nobody", NOTE).unwrap().is_empty());
    }

    #[test]
    fn an_unreadable_live_file_is_logged_not_listed() {
        let conn = crate::db::open_memory().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let dir = user_root(tmp.path(), "aki").join(NOTE);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("broken.md"), "no front matter").unwrap();
        assert!(live_files(&conn, tmp.path(), "aki", NOTE).unwrap().is_empty());
        let logged: String = conn
            .query_row("SELECT detail FROM event_log WHERE kind = 'memory_index_error'", [], |r| r.get(0))
            .unwrap();
        assert!(logged.contains("broken.md"), "{logged}");
    }

    #[test]
    fn notes_stay_out_of_search_browse_counts_and_the_expiry_sweep() {
        let (conn, tmp) = env();
        put(&conn, tmp.path(), "aki", &a_note(NOTE_ID)).unwrap();
        let fact = add(&conn, tmp.path(), "aki", "semantic", "bank hours", "the bank closes at five", None).unwrap();
        let hits: Vec<String> =
            query(&conn, "aki", "bank", 10, None).unwrap().into_iter().map(|h| h.id).collect();
        assert_eq!(hits, vec![fact.clone()]);
        let listed: Vec<String> =
            list(&conn, "aki", None, 50).unwrap().into_iter().map(|h| h.id).collect();
        assert_eq!(listed, vec![fact]);
        assert_eq!(live_count(&conn, "aki").unwrap(), 1);
        assert_eq!(archive_expired(&conn, tmp.path(), "aki", "2026-12-01".parse().unwrap()).unwrap(), 0);
        assert!(!read(tmp.path(), "aki", NOTE_ID).unwrap().unwrap().archived);
    }

    #[test]
    fn remove_deletes_a_live_file_and_its_rows_but_never_an_archived_one() {
        let (conn, tmp) = env();
        put(&conn, tmp.path(), "aki", &a_note(NOTE_ID)).unwrap();
        assert!(remove(&conn, tmp.path(), "aki", NOTE_ID).unwrap());
        assert!(read(tmp.path(), "aki", NOTE_ID).unwrap().is_none());
        let rows: i64 = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM memory_index WHERE id = ?1)
                      + (SELECT COUNT(*) FROM memory_fts WHERE id = ?1)",
                [NOTE_ID],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0);
        assert!(!remove(&conn, tmp.path(), "aki", NOTE_ID).unwrap());

        let old = add(&conn, tmp.path(), "aki", "semantic", "s", "b", None).unwrap();
        archive(&conn, tmp.path(), "aki", &old).unwrap();
        assert!(!remove(&conn, tmp.path(), "aki", &old).unwrap());
        assert!(read(tmp.path(), "aki", &old).unwrap().is_some());
    }

    #[test]
    fn put_refuses_a_bad_id_or_category() {
        let (conn, tmp) = env();
        assert!(put(&conn, tmp.path(), "aki", &a_note("../x")).is_err());
        let mut f = a_note(NOTE_ID);
        f.category = "archive".into();
        assert!(put(&conn, tmp.path(), "aki", &f).is_err());
    }
}
