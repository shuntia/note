use crate::providers::{Effort, LLMProvider};
use crate::tools::SessionKind;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

/// A model and reasoning effort asked for over the provider's own; `None`
/// leaves either to the next in line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pick {
    pub model: Option<String>,
    pub effort: Option<Effort>,
}

impl Pick {
    pub fn of_user(cfg: &crate::config::UserConfig) -> Self {
        Self { model: cfg.model.clone(), effort: cfg.effort() }
    }
}

/// What a session of `kind` asks for: the conversation's pick, else the
/// user's, each model only while the provider still offers it. Calls and
/// share visitors run on the provider's own.
pub fn resolve(llm: &dyn LLMProvider, kind: SessionKind, conversation: &Pick, user: &Pick) -> Pick {
    if matches!(kind, SessionKind::Call | SessionKind::Share) {
        return Pick::default();
    }
    let offered = llm.offered();
    let model = [&conversation.model, &user.model]
        .into_iter()
        .flatten()
        .find(|m| offered.contains(m))
        .cloned();
    Pick { model, effort: conversation.effort.or(user.effort) }
}

/// `model` while the provider still offers it.
pub fn still_offered(llm: &dyn LLMProvider, model: Option<String>) -> Option<String> {
    model.filter(|m| llm.offered().contains(m))
}

/// A pick from the API, or the name of the field that is not on offer.
pub fn parse(llm: &dyn LLMProvider, model: Option<String>, effort: Option<String>) -> Result<Pick, &'static str> {
    if model.as_ref().is_some_and(|m| !llm.offered().contains(m)) {
        return Err("model");
    }
    let effort = match effort {
        None => None,
        Some(e) => Some(Effort::parse(&e).ok_or("effort")?),
    };
    Ok(Pick { model, effort })
}

/// A blank or unknown effort reads as unset.
pub fn conversation_pick(conn: &Connection, conversation_id: i64) -> Result<Pick> {
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row("SELECT model, effort FROM conversations WHERE id = ?1", [conversation_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let (model, effort) = row.unwrap_or_default();
    Ok(Pick { model, effort: effort.as_deref().and_then(Effort::parse) })
}

/// False when the conversation is not the user's.
pub fn set_conversation_pick(conn: &Connection, user_id: i64, conversation_id: i64, pick: &Pick) -> Result<bool> {
    let n = conn.execute(
        "UPDATE conversations SET model = ?1, effort = ?2 WHERE id = ?3 AND user_id = ?4",
        rusqlite::params![pick.model, pick.effort.map(Effort::as_str), conversation_id, user_id],
    )?;
    Ok(n == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ChatRequest, ChatResponse};

    struct Offers;

    impl LLMProvider for Offers {
        fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse> {
            Ok(ChatResponse::default())
        }

        fn model(&self) -> Option<String> {
            Some("base".into())
        }

        fn offered(&self) -> Vec<String> {
            vec!["base".into(), "a/fast".into(), "b/deep".into()]
        }
    }

    fn pick(model: Option<&str>, effort: Option<Effort>) -> Pick {
        Pick { model: model.map(String::from), effort }
    }

    #[test]
    fn the_conversation_wins_over_the_user_and_the_user_over_the_provider() {
        let conv = pick(Some("a/fast"), Some(Effort::High));
        let user = pick(Some("b/deep"), Some(Effort::Off));
        assert_eq!(resolve(&Offers, SessionKind::Talk, &conv, &user), conv);
        assert_eq!(resolve(&Offers, SessionKind::Talk, &Pick::default(), &user), user);
        assert_eq!(resolve(&Offers, SessionKind::Nightly, &Pick::default(), &Pick::default()), Pick::default());
        let half = pick(None, Some(Effort::Low));
        assert_eq!(resolve(&Offers, SessionKind::Trigger, &half, &user), pick(Some("b/deep"), Some(Effort::Low)));
    }

    #[test]
    fn calls_and_share_visitors_keep_the_provider_default() {
        let conv = pick(Some("a/fast"), Some(Effort::High));
        for kind in [SessionKind::Call, SessionKind::Share] {
            assert_eq!(resolve(&Offers, kind, &conv, &conv), Pick::default());
        }
    }

    #[test]
    fn a_model_no_longer_offered_falls_through() {
        let conv = pick(Some("gone/model"), None);
        let user = pick(Some("b/deep"), None);
        assert_eq!(resolve(&Offers, SessionKind::Talk, &conv, &user).model.as_deref(), Some("b/deep"));
        assert_eq!(resolve(&Offers, SessionKind::Talk, &conv, &Pick::default()).model, None);
        let mock = crate::providers::mock::NullLLM;
        assert_eq!(resolve(&mock, SessionKind::Talk, &user, &user).model, None);
    }

    #[test]
    fn a_conversation_keeps_its_pick_and_only_its_owner_changes_it() {
        let conn = crate::db::open_memory().unwrap();
        conn.execute_batch(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aitest','h','member'), ('other','h','member');
             INSERT INTO conversations (user_id, title, created_at, updated_at) VALUES (1, 't', 'x', 'x');",
        )
        .unwrap();
        assert_eq!(conversation_pick(&conn, 1).unwrap(), Pick::default());
        let p = pick(Some("a/fast"), Some(Effort::Medium));
        assert!(set_conversation_pick(&conn, 1, 1, &p).unwrap());
        assert_eq!(conversation_pick(&conn, 1).unwrap(), p);
        assert!(!set_conversation_pick(&conn, 2, 1, &Pick::default()).unwrap());
        assert!(set_conversation_pick(&conn, 1, 1, &Pick::default()).unwrap());
        assert_eq!(conversation_pick(&conn, 1).unwrap(), Pick::default());
    }
}
