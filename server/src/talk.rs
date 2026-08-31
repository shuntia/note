use crate::providers::Message;
use anyhow::Result;
use rusqlite::Connection;

const MAX_TITLE_CHARS: usize = 60;

pub fn create(conn: &Connection, user_id: i64, title: &str, now: jiff::Timestamp) -> Result<i64> {
    conn.execute(
        "INSERT INTO conversations (user_id, title, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?3)",
        (user_id, title, now.to_string()),
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn owned(conn: &Connection, user_id: i64, id: i64) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM conversations WHERE id = ?1 AND user_id = ?2",
        (id, user_id),
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

pub fn touch(conn: &Connection, id: i64, now: jiff::Timestamp) -> Result<()> {
    conn.execute(
        "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
        (now.to_string(), id),
    )?;
    Ok(())
}

/// `role` is `"user"` or `"assistant"`; the table's CHECK rejects anything else.
pub fn append_text(
    conn: &Connection,
    conversation_id: i64,
    role: &str,
    content: &str,
    now: jiff::Timestamp,
) -> Result<()> {
    conn.execute(
        "INSERT INTO talk_messages (conversation_id, role, content, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        (conversation_id, role, content, now.to_string()),
    )?;
    Ok(())
}

/// `result` is the tool output as it went back to the model, and lands in
/// `content` so every row carries its displayable text in the same column.
pub fn append_tool(
    conn: &Connection,
    conversation_id: i64,
    tool_name: &str,
    tool_args: &str,
    result: &str,
    is_error: bool,
    now: jiff::Timestamp,
) -> Result<()> {
    conn.execute(
        "INSERT INTO talk_messages
            (conversation_id, role, content, tool_name, tool_args, is_error, created_at)
         VALUES (?1, 'tool', ?2, ?3, ?4, ?5, ?6)",
        (conversation_id, result, tool_name, tool_args, is_error, now.to_string()),
    )?;
    Ok(())
}

/// The last `limit` text turns, oldest-first. Tool rows are dropped because a
/// replayed transcript has no live call ids to pair its results against.
pub fn history(conn: &Connection, conversation_id: i64, limit: usize) -> Result<Vec<Message>> {
    let mut stmt = conn.prepare(
        "SELECT role, content FROM talk_messages
         WHERE conversation_id = ?1 AND role IN ('user','assistant')
         ORDER BY id DESC LIMIT ?2",
    )?;
    let mut msgs = stmt
        .query_map((conversation_id, limit as i64), |r| {
            let role: String = r.get(0)?;
            let content: String = r.get(1)?;
            Ok(match role.as_str() {
                "user" => Message::User(content),
                _ => Message::Assistant { text: content, tool_calls: vec![] },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    msgs.reverse();
    Ok(msgs)
}

/// A conversation title derived from its opening message: whitespace collapsed
/// to single spaces and at most `MAX_TITLE_CHARS` chars, ellipsis included.
pub fn title_from(message: &str) -> String {
    let collapsed = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_TITLE_CHARS {
        return collapsed;
    }
    let mut title: String = collapsed.chars().take(MAX_TITLE_CHARS - 1).collect();
    title.push('…');
    title
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn_with_conversation() -> Connection {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        create(&conn, 1, "chat", jiff::Timestamp::now()).unwrap();
        conn
    }

    fn now() -> jiff::Timestamp {
        jiff::Timestamp::now()
    }

    #[test]
    fn title_from_passes_short_messages_through() {
        assert_eq!(title_from("call mom"), "call mom");
    }

    #[test]
    fn title_from_collapses_whitespace_runs() {
        assert_eq!(title_from("  call\n\n  mom   today \t"), "call mom today");
    }

    #[test]
    fn title_from_truncates_on_a_char_boundary() {
        let title = title_from(&"日".repeat(100));
        assert!(title.ends_with('…'));
        assert_eq!(title.chars().count(), MAX_TITLE_CHARS);
        assert!(title.chars().take(MAX_TITLE_CHARS - 1).all(|c| c == '日'));

        let exact = "a".repeat(MAX_TITLE_CHARS);
        assert_eq!(title_from(&exact), exact);
    }

    #[test]
    fn the_role_check_rejects_unknown_roles() {
        let conn = conn_with_conversation();
        assert!(append_text(&conn, 1, "system", "x", now()).is_err());
    }

    #[test]
    fn history_excludes_tool_rows_and_maps_roles_oldest_first() {
        let conn = conn_with_conversation();
        append_text(&conn, 1, "user", "hi", now()).unwrap();
        append_tool(&conn, 1, "task_create", r#"{"title":"x"}"#, "{}", false, now()).unwrap();
        append_text(&conn, 1, "assistant", "done", now()).unwrap();

        let msgs = history(&conn, 1, 10).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(matches!(&msgs[0], Message::User(t) if t == "hi"));
        assert!(
            matches!(&msgs[1], Message::Assistant { text, tool_calls } if text == "done" && tool_calls.is_empty())
        );
    }

    #[test]
    fn history_limit_takes_the_last_rows() {
        let conn = conn_with_conversation();
        for i in 0..5 {
            append_text(&conn, 1, "user", &format!("m{i}"), now()).unwrap();
        }
        let msgs = history(&conn, 1, 2).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(matches!(&msgs[0], Message::User(t) if t == "m3"));
        assert!(matches!(&msgs[1], Message::User(t) if t == "m4"));
    }

    #[test]
    fn history_is_scoped_to_its_conversation() {
        let conn = conn_with_conversation();
        create(&conn, 1, "other", now()).unwrap();
        append_text(&conn, 1, "user", "mine", now()).unwrap();
        append_text(&conn, 2, "user", "theirs", now()).unwrap();
        let msgs = history(&conn, 1, 10).unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(matches!(&msgs[0], Message::User(t) if t == "mine"));
    }

    #[test]
    fn owned_is_false_for_another_user_and_for_absent_ids() {
        let conn = conn_with_conversation();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('bo', 'x', 'member')",
            [],
        )
        .unwrap();
        assert!(owned(&conn, 1, 1).unwrap());
        assert!(!owned(&conn, 2, 1).unwrap());
        assert!(!owned(&conn, 1, 99).unwrap());
    }

    #[test]
    fn touch_bumps_updated_at_without_moving_created_at() {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        let start: jiff::Timestamp = "2026-08-30T09:00:00Z".parse().unwrap();
        let id = create(&conn, 1, "chat", start).unwrap();
        let later: jiff::Timestamp = "2026-08-30T10:00:00Z".parse().unwrap();
        touch(&conn, id, later).unwrap();
        let (created, updated): (String, String) = conn
            .query_row("SELECT created_at, updated_at FROM conversations WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(created, start.to_string());
        assert_eq!(updated, later.to_string());
    }

    #[test]
    fn deleting_a_conversation_cascades_to_its_messages() {
        let conn = conn_with_conversation();
        append_text(&conn, 1, "user", "hi", now()).unwrap();
        append_tool(&conn, 1, "task_create", "{}", "{}", true, now()).unwrap();
        conn.execute("DELETE FROM conversations WHERE id = 1", []).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM talk_messages WHERE conversation_id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn append_tool_stores_the_call_beside_its_result() {
        let conn = conn_with_conversation();
        append_tool(&conn, 1, "task_create", r#"{"title":"x"}"#, r#"{"id":1}"#, true, now()).unwrap();
        let (role, content, name, args, is_error): (String, String, String, String, bool) = conn
            .query_row(
                "SELECT role, content, tool_name, tool_args, is_error FROM talk_messages WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(role, "tool");
        assert_eq!(content, r#"{"id":1}"#);
        assert_eq!(name, "task_create");
        assert_eq!(args, r#"{"title":"x"}"#);
        assert!(is_error);
    }

    #[test]
    fn append_text_leaves_the_tool_columns_null() {
        let conn = conn_with_conversation();
        append_text(&conn, 1, "user", "hi", now()).unwrap();
        let (name, args): (Option<String>, Option<String>) = conn
            .query_row("SELECT tool_name, tool_args FROM talk_messages WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert!(name.is_none());
        assert!(args.is_none());
    }
}
