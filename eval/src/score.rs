use crate::snapshot::Session;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Score {
    pub rounds: u64,
    pub refused: u64,
    pub tool_calls: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
}

/// Refused calls: each errored call, plus each entry of any `refused` list a
/// call's result carries.
pub fn refused<'a>(sessions: impl IntoIterator<Item = &'a Session>) -> u64 {
    sessions
        .into_iter()
        .flat_map(|s| s.calls.iter())
        .map(|c| u64::from(c.is_error) + refused_entries(&c.result))
        .sum()
}

fn refused_entries(v: &Value) -> u64 {
    match v {
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| match (k.as_str(), v) {
                ("refused", Value::Array(a)) => a.len() as u64,
                _ => refused_entries(v),
            })
            .sum(),
        Value::Array(a) => a.iter().map(refused_entries).sum(),
        _ => 0,
    }
}

/// Rounds and refusals count the case's own session kind; tokens and cost
/// count every session the case caused.
pub fn score(sessions: &[Session], kind: &str) -> Score {
    let own: Vec<&Session> = sessions.iter().filter(|s| s.kind == kind).collect();
    Score {
        rounds: own.iter().map(|s| s.turns).sum(),
        refused: refused(own.iter().copied()),
        tool_calls: own.iter().map(|s| s.calls.len() as u64).sum(),
        prompt_tokens: sessions.iter().map(|s| s.prompt_tokens).sum(),
        completion_tokens: sessions.iter().map(|s| s.completion_tokens).sum(),
        cost: sessions.iter().map(|s| s.cost).sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Call;

    #[test]
    fn errors_and_refused_entries_both_count() {
        let calls = vec![
            Call { name: "a".into(), is_error: true, ..Default::default() },
            Call { name: "b".into(), result: serde_json::json!({ "done": [1], "refused": [{ "id": 2 }, { "id": 3 }] }), ..Default::default() },
            Call { name: "c".into(), result: serde_json::json!({ "results": [{ "refused": [4] }] }), ..Default::default() },
        ];
        let talk = Session { kind: "Talk".into(), turns: 3, calls, prompt_tokens: 100, completion_tokens: 10, ..Default::default() };
        let title = Session { kind: "Title".into(), turns: 1, prompt_tokens: 5, ..Default::default() };
        let s = score(&[talk, title], "Talk");
        assert_eq!((s.rounds, s.refused, s.tool_calls), (3, 4, 3));
        assert_eq!(s.prompt_tokens, 105);
    }
}
