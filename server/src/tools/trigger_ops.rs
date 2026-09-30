use super::{SessionKind, ToolCtx, ToolError};
use crate::triggers::{self, Cancel, Lay, Refusal};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetArgs {
    /// When it should fire: zero-padded `HH:MM`, or `+Nmin` from now.
    pub at: String,
    /// What you will be following up on, in your own words.
    pub prompt: String,
    /// The conversation to say it in. Omit for the day's check-in thread.
    #[serde(default)]
    pub thread: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitUntilArgs {
    /// Zero-padded `HH:MM`, or `+Nmin` from now.
    pub at: String,
    pub prompt: String,
    /// The conversation whose reply calls the follow-up off. Omit for the day's
    /// check-in thread, which then cannot call it off.
    #[serde(default)]
    pub thread: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitForArgs {
    /// The task whose finish calls the follow-up off.
    #[serde(default)]
    pub task_id: Option<i64>,
    /// The plan event whose decision calls the follow-up off.
    #[serde(default)]
    pub event_id: Option<i64>,
    /// How long to wait: zero-padded `HH:MM`, or `+Nmin` from now.
    pub until: String,
    pub prompt: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BudgetArgs {
    /// How many more check-ins today, 1 to 10.
    pub extra: u32,
    /// What the user agreed to, in one line.
    pub reason: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SayArgs {
    /// One or two warm sentences, as the user will read them.
    pub text: String,
    /// The ids of the notes these words are about, when a nudge names any.
    #[serde(default)]
    pub notes: Vec<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuietArgs {
    /// Why saying nothing is the right move, for the log.
    pub reason: String,
}

fn refused(e: Refusal) -> ToolError {
    match e {
        Refusal::CapReached { allowance, spent } => ToolError::cap_reached(format!(
            "{spent} of today's {allowance} check-ins are laid; ask the user, and call \
             trigger_budget after they agree"
        )),
        Refusal::TooSoon { minutes } => ToolError::too_soon(format!(
            "a trigger point must be at least {} minutes ahead, and that one is {minutes}",
            triggers::MIN_LEAD_MIN
        )),
        Refusal::Past { at } => {
            ToolError::past(format!("{at} has gone by; lay it later today"))
        }
        Refusal::NotFound(m) => ToolError::not_found(m),
        Refusal::Rejected(m) => ToolError::rejected(m),
        Refusal::Internal(m) => ToolError::internal(m),
    }
}

/// The day a trigger laid in this session belongs to: the nightly run lays for
/// the day it is planning, and every other session for the day it is in.
fn target_date(ctx: &ToolCtx, kind: SessionKind, now: jiff::Timestamp) -> jiff::civil::Date {
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

fn lay(
    conn: &Connection,
    ctx: &ToolCtx,
    kind: SessionKind,
    at: &str,
    prompt: &str,
    cancel: Option<Cancel>,
    thread: Option<i64>,
) -> Result<serde_json::Value, ToolError> {
    let now = jiff::Timestamp::now();
    let laid = triggers::lay(
        conn,
        &Lay {
            config_dir: ctx.config_dir,
            user_id: ctx.user_id,
            username: ctx.username,
            at,
            prompt,
            date: target_date(ctx, kind, now),
            cancel,
            conversation_id: thread,
            work_session_id: None,
            system: false,
            now,
        },
    )
    .map_err(refused)?;
    Ok(serde_json::json!({
        "event_id": laid.event_id,
        "at": laid.at,
        "cancel_if": laid.cancel_if,
    }))
}

pub fn set(
    conn: &Connection,
    ctx: &ToolCtx,
    kind: SessionKind,
    args: &SetArgs,
) -> Result<serde_json::Value, ToolError> {
    lay(conn, ctx, kind, &args.at, &args.prompt, None, args.thread)
}

pub fn wait_until(
    conn: &Connection,
    ctx: &ToolCtx,
    kind: SessionKind,
    args: &WaitUntilArgs,
) -> Result<serde_json::Value, ToolError> {
    lay(conn, ctx, kind, &args.at, &args.prompt, Some(Cancel::Replied), args.thread)
}

pub fn wait_for(
    conn: &Connection,
    ctx: &ToolCtx,
    kind: SessionKind,
    args: &WaitForArgs,
) -> Result<serde_json::Value, ToolError> {
    let cancel = match (args.task_id, args.event_id) {
        (Some(id), None) => Cancel::TaskDone(id),
        (None, Some(id)) => Cancel::EventDecided(id),
        _ => return Err(ToolError::rejected("name exactly one of task_id or event_id")),
    };
    lay(conn, ctx, kind, &args.until, &args.prompt, Some(cancel), None)
}

/// Raises today's allowance after the user has agreed to it; the receipt says
/// so in the words the user would use.
pub fn budget(
    conn: &Connection,
    ctx: &ToolCtx,
    args: &BudgetArgs,
) -> Result<serde_json::Value, ToolError> {
    if !(1..=triggers::MAX_EXTRA).contains(&args.extra) {
        return Err(ToolError::rejected(format!(
            "extra must be 1 to {}",
            triggers::MAX_EXTRA
        )));
    }
    let reason = args.reason.trim();
    if reason.is_empty() {
        return Err(ToolError::rejected("reason must say what the user agreed to"));
    }
    super::check_text("reason", reason)?;
    let internal = |e: rusqlite::Error| ToolError::internal(e.to_string());
    let date = target_date(ctx, SessionKind::Talk, jiff::Timestamp::now());
    let extra = triggers::add_extra(conn, ctx.user_id, date, args.extra).map_err(internal)?;
    crate::log::record(
        conn,
        Some(ctx.user_id),
        "trigger_budget",
        &format!("+{} today ({reason})", args.extra),
    )
    .map_err(|e| ToolError::internal(e.to_string()))?;
    let allowance = triggers::allowance(conn, ctx.config_dir, ctx.username, ctx.user_id, date)
        .map_err(internal)?;
    let spent = triggers::spent(conn, ctx.user_id, date).map_err(internal)?;
    Ok(serde_json::json!({ "extra": extra, "allowance": allowance, "spent": spent }))
}

/// The trigger session's terminal word. The firing path is what delivers it and
/// writes it to the thread, so the tool only vets the text.
pub fn say(
    _conn: &Connection,
    _ctx: &ToolCtx,
    args: &SayArgs,
) -> Result<serde_json::Value, ToolError> {
    let text = args.text.trim();
    if text.is_empty() || text.len() > triggers::MAX_SAY_BYTES {
        return Err(ToolError::rejected(format!(
            "text must be 1 to {} bytes",
            triggers::MAX_SAY_BYTES
        )));
    }
    Ok(serde_json::json!({ "said": text, "notes": args.notes }))
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
                "display_name = \"X\"\ntimezone = \"{}\"\ntemplate = \"default\"\n\
                 nightly_time = \"22:00\"\n",
                zone().iana_name().unwrap()
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

    /// Far enough ahead that the lead check passes wherever in the day the
    /// suite happens to run; the nightly lays on its own date instead.
    fn soon() -> String {
        "+90min".into()
    }

    #[test]
    fn trigger_set_lays_a_point_with_no_cancel_rule() {
        let (conn, tmp) = env();
        let out = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Talk,
            "trigger_set",
            &format!(r#"{{"at":"{}","prompt":"ask about the essay"}}"#, soon()),
        )
        .unwrap();
        assert!(out["event_id"].as_i64().unwrap() > 0);
        assert!(out["cancel_if"].is_null());
        let (kind, prompt): (String, String) = conn
            .query_row("SELECT kind, prompt FROM events", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((kind.as_str(), prompt.as_str()), ("trigger", "ask about the essay"));
        let logged: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='trigger_laid'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(logged, 1);
    }

    #[test]
    fn the_two_waits_carry_their_own_cancel_rules() {
        let (conn, tmp) = env();
        let out = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Checkin,
            "wait_until",
            &format!(r#"{{"at":"{}","prompt":"did they answer?"}}"#, soon()),
        )
        .unwrap();
        assert_eq!(out["cancel_if"], "replied");

        crate::tasks::create(
            &conn,
            1,
            crate::tasks::NewTask { title: "essay".into(), ..Default::default() },
            "manual",
            crate::tasks::Actor::User,
        )
        .unwrap();
        let out = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Talk,
            "wait_for",
            &format!(r#"{{"task_id":1,"until":"{}","prompt":"still on the essay?"}}"#, soon()),
        )
        .unwrap();
        assert_eq!(out["cancel_if"], "task_done");

        let e = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Talk,
            "wait_for",
            &format!(r#"{{"task_id":1,"event_id":2,"until":"{}","prompt":"x"}}"#, soon()),
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");
    }

    #[test]
    fn the_cap_names_the_way_out_and_the_budget_reopens_it() {
        let (conn, tmp) = env();
        let lay = |at: &str| {
            dispatch(
                &conn,
                &ctx(&tmp),
                SessionKind::Talk,
                "trigger_set",
                &format!(r#"{{"at":"{at}","prompt":"check in"}}"#),
            )
        };
        for n in 1..=4 {
            lay(&format!("+{}min", 60 + n * 10)).unwrap();
        }
        let e = lay("+120min").unwrap_err();
        assert_eq!(e.kind, "cap_reached");
        assert!(e.message.contains("trigger_budget"), "{}", e.message);

        let out = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Talk,
            "trigger_budget",
            r#"{"extra":2,"reason":"they asked to be pushed today"}"#,
        )
        .unwrap();
        assert_eq!(out["extra"], 2);
        assert_eq!(out["allowance"], 6);
        assert_eq!(out["spent"], 4);
        lay("+120min").unwrap();

        let e = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Talk,
            "trigger_budget",
            r#"{"extra":0,"reason":"x"}"#,
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");
    }

    #[test]
    fn a_trigger_too_close_or_loosely_written_is_named_as_such() {
        let (conn, tmp) = env();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "trigger_set",
            r#"{"at":"+2min","prompt":"now"}"#).unwrap_err();
        assert_eq!(e.kind, "too_soon");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "trigger_set",
            r#"{"at":"9:00","prompt":"loose"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "trigger_set",
            r#"{"at":"+30min","prompt":"  "}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
        let events: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(events, 0);
    }

    /// An evening `nightly_time` plans tomorrow, so its triggers land there too
    /// whatever the hour the run happens at.
    #[test]
    fn the_nightly_lays_on_the_day_it_is_planning() {
        let (conn, tmp) = env();
        dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "trigger_set",
            r#"{"at":"09:00","prompt":"after the first block"}"#).unwrap();
        let date: String = conn
            .query_row(
                "SELECT p.date FROM plans p JOIN events e ON e.plan_id = p.id WHERE e.kind='trigger'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let tomorrow = jiff::Timestamp::now().to_zoned(zone()).date().tomorrow().unwrap();
        assert_eq!(date, tomorrow.to_string());
    }

    /// The plan is how the model finds a trigger again: the row has to carry
    /// what it was for, and what would call it off.
    #[test]
    fn plan_list_shows_a_trigger_with_its_prompt_and_its_cancel_rule() {
        let (conn, tmp) = env();
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "trigger_set",
            &format!(r#"{{"at":"{}","prompt":"ask about the essay"}}"#, soon())).unwrap();
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "wait_until",
            &format!(r#"{{"at":"{}","prompt":"did they answer?"}}"#, soon())).unwrap();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Trigger, "plan_list", "{}").unwrap();
        let rows = out["events"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["prompt"], "ask about the essay");
        assert!(rows[0]["cancel_if"].is_null());
        assert_eq!(rows[1]["cancel_if"], "replied");
        assert_eq!(rows[0]["flexibility"], "drop", "the user can drop one from the day");
    }

    #[test]
    fn the_budget_tool_is_out_of_reach_where_the_user_is_not() {
        let (conn, tmp) = env();
        for kind in [SessionKind::Nightly, SessionKind::Trigger] {
            assert!(!crate::tools::registry(kind).contains(&"trigger_budget"), "{kind:?}");
            assert!(dispatch(&conn, &ctx(&tmp), kind, "trigger_budget",
                r#"{"extra":1,"reason":"x"}"#).is_err());
        }
        let raised: i64 = conn
            .query_row("SELECT COUNT(*) FROM trigger_budgets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(raised, 0);
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

        for kind in [SessionKind::Talk, SessionKind::Checkin, SessionKind::Nightly] {
            let e = dispatch(&conn, &ctx(&tmp), kind, "say", r#"{"text":"hi"}"#).unwrap_err();
            assert_eq!(e.kind, "unknown_tool", "{kind:?} can speak out of turn");
        }
    }
    #[test]
    fn say_carries_the_notes_it_names() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Trigger, "say",
            r#"{"text":"the bank closes at five","notes":[3]}"#).unwrap();
        assert_eq!(out["notes"], serde_json::json!([3]));
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Trigger, "say", r#"{"text":"hi"}"#)
            .unwrap();
        assert_eq!(out["notes"], serde_json::json!([]));
    }
}
