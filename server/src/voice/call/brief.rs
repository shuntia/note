use crate::agent::{system_prompt, SessionDeps};
use crate::providers::Message;
use crate::tools::SessionKind;
use anyhow::Result;
use std::fmt::Write as _;

const THREAD_TAIL: usize = 20;

pub enum Reason {
    CheckIn { title: String, body: String },
    UserCalled,
}

/// The call's fixed system prompt: the voice persona and the user's context,
/// why the call is happening, and the tail of the thread it belongs to.
pub fn build(
    deps: &SessionDeps,
    user_id: i64,
    username: &str,
    reason: &Reason,
    conversation_id: Option<i64>,
    now: jiff::Timestamp,
) -> Result<String> {
    let mut brief = system_prompt(deps, user_id, username, SessionKind::Call, now)?;
    brief.push_str("\n\n# Why this call\n\n");
    match reason {
        Reason::CheckIn { title, body } => {
            let _ = write!(brief, "You called about: {title}. You opened with: {body}");
        }
        Reason::UserCalled => brief.push_str("The user called you."),
    }
    let thread = match conversation_id {
        Some(id) => crate::talk::history(&crate::db_guard(deps.db), id, THREAD_TAIL)?,
        None => Vec::new(),
    };
    if !thread.is_empty() {
        brief.push_str("\n\n# Earlier in this thread\n");
        for message in thread {
            let (who, text) = match &message {
                Message::User(text) => ("you", text),
                Message::Assistant { text, .. } => ("Note", text),
                Message::ToolResult { .. } => continue,
            };
            if !text.trim().is_empty() {
                let _ = write!(brief, "\n{who}: {text}");
            }
        }
    }
    Ok(brief)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::mock::MockLLM;
    use std::path::Path;
    use std::sync::Mutex;

    #[test]
    fn the_brief_names_the_reason_and_the_thread_tail() {
        let conn = crate::db::open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", [])
            .unwrap();
        let now: jiff::Timestamp = "2026-10-02T18:00:00Z".parse().unwrap();
        let thread = crate::talk::create(&conn, 1, "chat", now).unwrap();
        crate::talk::append_text(&conn, thread, "user", "the essay is due friday", now).unwrap();
        crate::talk::append_text(&conn, thread, "assistant", "   ", now).unwrap();
        crate::talk::append_text(&conn, thread, "assistant", "I'll check in thursday", now)
            .unwrap();
        let db = Mutex::new(conn);

        let tmp = tempfile::tempdir().unwrap();
        let prompts = tmp.path().join("defaults/prompts");
        std::fs::create_dir_all(&prompts).unwrap();
        std::fs::write(
            tmp.path().join("defaults/user.toml"),
            "display_name = \"Aki\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n",
        )
        .unwrap();
        let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../config/defaults/prompts/voice.md");
        std::fs::copy(shipped, prompts.join("voice.md")).unwrap();

        let llm = MockLLM::scripted(vec![]);
        let deps = SessionDeps {
            db: &db,
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            llm: &llm,
            embeddings: None,
            search: None,
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            token_id: None,
            thread_note: None,
            share: None,
        };
        let reason = Reason::CheckIn { title: "Essay".into(), body: "How is the essay going?".into() };
        let brief = build(&deps, 1, "aki", &reason, Some(thread), now).unwrap();

        assert!(brief.starts_with("You are Note, on a phone call with Aki."), "{brief}");
        assert!(brief.contains("You called about: Essay. You opened with: How is the essay going?"));
        assert!(brief.contains("you: the essay is due friday"), "{brief}");
        let user_at = brief.find("you: the essay is due friday").expect(&brief);
        let note_at = brief.find("Note: I'll check in thursday").expect(&brief);
        assert!(user_at < note_at, "{brief}");
        assert!(
            brief.ends_with("# Earlier in this thread\n\nyou: the essay is due friday\nNote: I'll check in thursday"),
            "a blank row renders no line: {brief}"
        );
        assert!(!brief.contains("{name}"));
    }

    #[test]
    fn an_empty_thread_leaves_no_thread_section() {
        let conn = crate::db::open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", [])
            .unwrap();
        let now: jiff::Timestamp = "2026-10-02T18:00:00Z".parse().unwrap();
        let thread = crate::talk::create(&conn, 1, "chat", now).unwrap();
        let db = Mutex::new(conn);
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("defaults/prompts")).unwrap();
        std::fs::write(
            tmp.path().join("defaults/user.toml"),
            "display_name = \"Aki\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("defaults/prompts/voice.md"), "call {name}").unwrap();
        let standing = crate::context::standing_path(tmp.path(), "aki");
        std::fs::create_dir_all(standing.parent().unwrap()).unwrap();
        std::fs::write(standing, "write {name} on the form").unwrap();
        let llm = MockLLM::scripted(vec![]);
        let deps = SessionDeps {
            db: &db,
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            llm: &llm,
            embeddings: None,
            search: None,
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            token_id: None,
            thread_note: None,
            share: None,
        };
        let brief = build(&deps, 1, "aki", &Reason::UserCalled, Some(thread), now).unwrap();
        assert!(brief.starts_with("call Aki"));
        assert!(brief.contains("write {name} on the form"), "the user's context is left as written");
        assert!(brief.ends_with("# Why this call\n\nThe user called you."), "{brief}");
    }
}
