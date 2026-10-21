use super::{SessionKind, ToolCtx, ToolError};
use crate::triggers;
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SayArgs {
    /// One or two warm sentences, as the user will read them.
    pub text: String,
    /// The ids of the working notes these words are about, when a nudge names any.
    #[serde(default, alias = "notes")]
    pub memory: Vec<String>,
    /// Wait up to this many minutes, 5 to 120, for an answer; this session
    /// then goes on, told whether the user answered or acted.
    #[serde(default)]
    pub await_min: Option<u32>,
    /// The tasks these words push on, by id; omit to take the wake-up's own.
    #[serde(default)]
    pub task_ids: Vec<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuietArgs {
    /// Why saying nothing is the right move, for the log.
    pub reason: String,
}

/// The day a trigger laid in this session belongs to: the nightly run lays for
/// the day it is planning, and every other session for the day it is in.
pub(crate) fn target_date(ctx: &ToolCtx, kind: SessionKind, now: jiff::Timestamp) -> jiff::civil::Date {
    let cfg = crate::config::UserConfig::load(ctx.config_dir, ctx.username).ok();
    let tz = cfg
        .as_ref()
        .and_then(|c| jiff::tz::TimeZone::get(&c.timezone).ok())
        .unwrap_or(jiff::tz::TimeZone::UTC);
    let local = now.to_zoned(tz);
    match (kind, cfg) {
        (SessionKind::Nightly, Some(cfg)) => crate::nightly::plan_date(&local, &cfg.nightly_time),
        _ => local.date(),
    }
}

/// The IANA zone a tool's wall times are given in, when the model names one.
pub(crate) fn zone_arg(tz: Option<&str>) -> Result<Option<jiff::tz::TimeZone>, ToolError> {
    tz.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| {
            jiff::tz::TimeZone::get(t).map_err(|_| {
                ToolError::rejected(format!("tz must be an IANA zone name like America/New_York, got {t:?}"))
            })
        })
        .transpose()
}

/// The trigger session's terminal word. The firing path is what delivers it and
/// writes it to the thread, so the tool only vets the text.
pub fn say(args: &SayArgs) -> Result<serde_json::Value, ToolError> {
    let text = args.text.trim();
    if text.is_empty() || text.len() > triggers::MAX_SAY_BYTES {
        return Err(ToolError::rejected(format!("text must be 1 to {} bytes", triggers::MAX_SAY_BYTES)));
    }
    let mut out = serde_json::json!({ "said": text, "memory": args.memory });
    if !args.task_ids.is_empty() {
        out["task_ids"] = serde_json::json!(args.task_ids);
    }
    if let Some(min) = args.await_min {
        if !triggers::AWAIT_MIN.contains(&min) {
            return Err(ToolError::rejected(format!(
                "await_min must be {} to {}",
                triggers::AWAIT_MIN.start(),
                triggers::AWAIT_MIN.end()
            )));
        }
        out["await_min"] = min.into();
    }
    Ok(out)
}

pub fn stay_quiet(
    _conn: &Connection,
    _ctx: &ToolCtx,
    args: &QuietArgs,
) -> Result<serde_json::Value, ToolError> {
    let reason = args.reason.trim();
    if reason.is_empty() {
        return Err(ToolError::rejected("reason must say why nothing is the right move"));
    }
    super::check_text("reason", reason)?;
    Ok(serde_json::json!({ "quiet": true, "reason": reason }))
}

#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, PreparedVectors, SessionKind, ToolCtx};

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        env_in(zone().iana_name().unwrap())
    }

    fn env_in(timezone: &str) -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("defaults/user.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            p,
            format!(
                "display_name = \"X\"\ntimezone = \"{timezone}\"\ntemplate = \"default\"\n\
                 nightly_time = \"22:00\"\ntriggers_per_day = 4\n"
            ),
        )
        .unwrap();
        (conn, tmp)
    }

    /// The test user's zone, pinned to midday so the offsets below stay on the
    /// day that laid them.
    fn zone() -> jiff::tz::TimeZone {
        crate::triggers::midday_zone()
    }

    fn ctx(tmp: &tempfile::TempDir) -> ToolCtx<'_> {
        ToolCtx {
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            user_id: 1,
            username: "aki",
            vectors: PreparedVectors::default(),
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            share: None,
            share_thread: None,
        }
    }

    #[test]
    fn the_terminal_pair_vets_its_text_and_belongs_to_the_trigger_session_alone() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Trigger, "say",
            r#"{"text":"  how did the essay go?  "}"#).unwrap();
        assert_eq!(out["said"], "how did the essay go?");
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Trigger, "stay_quiet",
            r#"{"reason":"they answered two minutes ago"}"#).unwrap();
        assert_eq!(out["quiet"], true);

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Trigger, "say", r#"{"text":"  "}"#)
            .unwrap_err();
        assert_eq!(e.kind, "rejected");
        let big = format!(r#"{{"text":"{}"}}"#, "x".repeat(1201));
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Trigger, "say", &big).unwrap_err();
        assert_eq!(e.kind, "rejected");

        for kind in [SessionKind::Talk, SessionKind::Nightly] {
            let e = dispatch(&conn, &ctx(&tmp), kind, "say", r#"{"text":"hi"}"#).unwrap_err();
            assert_eq!(e.kind, "unknown_tool", "{kind:?} can speak out of turn");
        }
    }
    #[test]
    fn say_carries_the_notes_it_names() {
        let (conn, tmp) = env();
        let id = "00000000-0000-4000-8000-000000000003";
        for field in ["memory", "notes"] {
            let out = dispatch(&conn, &ctx(&tmp), SessionKind::Trigger, "say",
                &format!(r#"{{"text":"the bank closes at five","{field}":["{id}"]}}"#)).unwrap();
            assert_eq!(out["memory"], serde_json::json!([id]), "{field}");
        }
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Trigger, "say", r#"{"text":"hi"}"#)
            .unwrap();
        assert_eq!(out["memory"], serde_json::json!([]));
    }

    #[test]
    fn say_can_wait_for_an_answer() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Trigger, "say",
            r#"{"text":"start the essay now","await_min":15}"#).unwrap();
        assert_eq!(out["await_min"].as_u64(), Some(15));
        for args in [r#"{"text":"hi","await_min":2}"#, r#"{"text":"hi","await_min":500}"#] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Trigger, "say", args).unwrap_err();
            assert_eq!(e.kind, "rejected", "{args}");
        }
    }

    #[test]
    fn say_only_writes() {
        let (conn, tmp) = env();
        for args in [r#"{"text":"got a minute?","ring":true}"#, r#"{"text":"got a minute?","asked":true}"#] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Trigger, "say", args).unwrap_err();
            assert_eq!(e.kind, "invalid_args", "{args}");
        }
    }
}
