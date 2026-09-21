use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const PREFIX: &str = "note_";
pub const MAX_PER_USER: usize = 20;
pub const MAX_NAME_LEN: usize = 64;
const TOUCH_INTERVAL_SECS: i64 = 60;

#[derive(Debug, Serialize)]
pub struct TokenInfo {
    pub id: i64,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

/// The only response that ever carries the plaintext secret.
#[derive(Debug, Serialize)]
pub struct Created {
    #[serde(flatten)]
    pub info: TokenInfo,
    pub token: String,
}

#[derive(Debug, Error)]
pub enum CreateError {
    #[error("name must be 1 to {MAX_NAME_LEN} characters")]
    InvalidName,
    #[error("at most {MAX_PER_USER} tokens per user")]
    TooMany,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub token_id: i64,
    pub user_id: i64,
    pub username: String,
}

pub fn generate_secret() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("os rng");
    format!(
        "{PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

/// Secrets are 256 random bits, so an unsalted digest is a safe lookup key.
pub fn hash_secret(secret: &str) -> String {
    data_encoding::HEXLOWER.encode(&Sha256::digest(secret.as_bytes()))
}

const COLS: &str = "id, name, created_at, last_used_at";

fn row_to_info(r: &rusqlite::Row) -> rusqlite::Result<TokenInfo> {
    Ok(TokenInfo {
        id: r.get(0)?,
        name: r.get(1)?,
        created_at: r.get(2)?,
        last_used_at: r.get(3)?,
    })
}

pub fn create(conn: &Connection, user_id: i64, name: &str) -> Result<Created, CreateError> {
    let name = name.trim();
    let len = name.chars().count();
    if len == 0 || len > MAX_NAME_LEN {
        return Err(CreateError::InvalidName);
    }
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM api_tokens WHERE user_id = ?1",
        [user_id],
        |r| r.get(0),
    )?;
    if count as usize >= MAX_PER_USER {
        return Err(CreateError::TooMany);
    }
    let token = generate_secret();
    let created_at = jiff::Timestamp::now().to_string();
    conn.execute(
        "INSERT INTO api_tokens (user_id, name, token_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
        (user_id, name, hash_secret(&token), &created_at),
    )?;
    let info = TokenInfo {
        id: conn.last_insert_rowid(),
        name: name.to_string(),
        created_at,
        last_used_at: None,
    };
    Ok(Created { info, token })
}

pub fn list(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<TokenInfo>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM api_tokens WHERE user_id = ?1 ORDER BY id"
    ))?;
    let rows = stmt.query_map([user_id], row_to_info)?;
    rows.collect()
}

/// Returns the removed token, or `None` when the id is not this user's.
pub fn revoke(conn: &Connection, user_id: i64, id: i64) -> rusqlite::Result<Option<TokenInfo>> {
    let info = conn
        .query_row(
            &format!("SELECT {COLS} FROM api_tokens WHERE id = ?1 AND user_id = ?2"),
            (id, user_id),
            row_to_info,
        )
        .optional()?;
    if info.is_some() {
        conn.execute("DELETE FROM api_tokens WHERE id = ?1", [id])?;
    }
    Ok(info)
}

/// Looks the presented secret up by digest. A disabled owner resolves to
/// `None` exactly like an unknown token. Stamps `last_used_at` when it is
/// unset or older than `TOUCH_INTERVAL_SECS`, so a busy client does not write
/// on every request.
pub fn resolve(
    conn: &Connection,
    secret: &str,
    now: jiff::Timestamp,
) -> rusqlite::Result<Option<Resolved>> {
    let row: Option<(i64, i64, String, bool, Option<String>)> = conn
        .query_row(
            "SELECT t.id, t.user_id, u.username, u.disabled, t.last_used_at
             FROM api_tokens t JOIN users u ON u.id = t.user_id
             WHERE t.token_hash = ?1",
            [hash_secret(secret)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((token_id, user_id, username, disabled, last_used_at)) = row else {
        return Ok(None);
    };
    if disabled {
        return Ok(None);
    }
    let stale = match last_used_at.and_then(|s| s.parse::<jiff::Timestamp>().ok()) {
        Some(last) => (now.as_second() - last.as_second()) > TOUCH_INTERVAL_SECS,
        None => true,
    };
    if stale {
        conn.execute(
            "UPDATE api_tokens SET last_used_at = ?1 WHERE id = ?2",
            (now.to_string(), token_id),
        )?;
    }
    Ok(Some(Resolved {
        token_id,
        user_id,
        username,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_user() -> (Connection, i64) {
        let conn = crate::db::open_memory().unwrap();
        let id = crate::auth::create_user(&conn, "aki", "pw", false).unwrap();
        (conn, id)
    }

    fn t0() -> jiff::Timestamp {
        "2026-09-15T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn secrets_carry_the_prefix_and_differ() {
        let a = generate_secret();
        let b = generate_secret();
        assert!(a.starts_with(PREFIX));
        assert_eq!(a.len(), PREFIX.len() + 43);
        assert_ne!(a, b);
    }

    #[test]
    fn create_stores_only_the_hash_and_resolves_the_plaintext() {
        let (conn, uid) = db_with_user();
        let made = create(&conn, uid, "  cli  ").unwrap();
        assert_eq!(made.info.name, "cli");
        assert!(made.token.starts_with(PREFIX));
        let stored: String = conn
            .query_row(
                "SELECT token_hash FROM api_tokens WHERE id = ?1",
                [made.info.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, hash_secret(&made.token));
        assert_ne!(stored, made.token);

        let r = resolve(&conn, &made.token, t0()).unwrap().unwrap();
        assert_eq!(r.user_id, uid);
        assert_eq!(r.token_id, made.info.id);
        assert_eq!(r.username, "aki");
        assert!(resolve(&conn, "note_nope", t0()).unwrap().is_none());
    }

    #[test]
    fn name_is_validated_and_count_is_capped() {
        let (conn, uid) = db_with_user();
        assert!(matches!(
            create(&conn, uid, "   "),
            Err(CreateError::InvalidName)
        ));
        assert!(matches!(
            create(&conn, uid, &"a".repeat(MAX_NAME_LEN + 1)),
            Err(CreateError::InvalidName)
        ));
        assert!(create(&conn, uid, &"あ".repeat(MAX_NAME_LEN)).is_ok());
        for i in 1..MAX_PER_USER {
            create(&conn, uid, &format!("t{i}")).unwrap();
        }
        assert!(matches!(
            create(&conn, uid, "one more"),
            Err(CreateError::TooMany)
        ));
    }

    #[test]
    fn list_and_revoke_are_scoped_to_the_owner() {
        let (conn, uid) = db_with_user();
        let other = crate::auth::create_user(&conn, "bo", "pw", false).unwrap();
        let mine = create(&conn, uid, "mine").unwrap();
        let theirs = create(&conn, other, "theirs").unwrap();

        let names: Vec<String> = list(&conn, uid)
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, vec!["mine"]);

        assert!(revoke(&conn, uid, theirs.info.id).unwrap().is_none());
        assert!(resolve(&conn, &theirs.token, t0()).unwrap().is_some());

        let gone = revoke(&conn, uid, mine.info.id).unwrap().unwrap();
        assert_eq!(gone.name, "mine");
        assert!(resolve(&conn, &mine.token, t0()).unwrap().is_none());
        assert!(list(&conn, uid).unwrap().is_empty());
    }

    #[test]
    fn disabled_users_tokens_do_not_resolve() {
        let (conn, uid) = db_with_user();
        let made = create(&conn, uid, "cli").unwrap();
        conn.execute("UPDATE users SET disabled = 1 WHERE id = ?1", [uid])
            .unwrap();
        assert!(resolve(&conn, &made.token, t0()).unwrap().is_none());
    }

    #[test]
    fn last_used_is_written_at_most_once_a_minute() {
        let (conn, uid) = db_with_user();
        let made = create(&conn, uid, "cli").unwrap();
        let last = |conn: &Connection| -> Option<String> {
            conn.query_row(
                "SELECT last_used_at FROM api_tokens WHERE id = ?1",
                [made.info.id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert!(last(&conn).is_none());
        resolve(&conn, &made.token, t0()).unwrap();
        assert_eq!(last(&conn).unwrap(), t0().to_string());
        let soon = t0() + jiff::Span::new().seconds(30);
        resolve(&conn, &made.token, soon).unwrap();
        assert_eq!(last(&conn).unwrap(), t0().to_string());
        let later = t0() + jiff::Span::new().seconds(TOUCH_INTERVAL_SECS + 1);
        resolve(&conn, &made.token, later).unwrap();
        assert_eq!(last(&conn).unwrap(), later.to_string());
    }
}
