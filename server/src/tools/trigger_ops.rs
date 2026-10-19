use super::{SessionKind, ToolCtx, ToolError};
use crate::triggers::{self, Cancel, Lay, Refusal};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetArgs {
    /// When it should fire: zero-padded `HH:MM` on `date`, or `+Nmin` from now.
    pub at: String,
    /// What you will be following up on, in your own words.
    pub prompt: String,
    /// The conversation to say it in. Omit for the day's check-in thread.
    #[serde(default)]
    pub thread: Option<i64>,
    /// The day `at` falls on, YYYY-MM-DD: today (the default) or any later day.
    #[serde(default)]
    pub date: Option<String>,
    /// The IANA zone `at` and `date` are read in, when not the user's own.
    #[serde(default)]
    pub tz: Option<String>,
    /// A pending trigger this one takes the place of; it is dropped.
    #[serde(default)]
    pub replace: Option<i64>,
    /// Lay it even though another trigger sits within 20 minutes of it.
    #[serde(default)]
    pub also: bool,
    /// What calls it off before it fires: reply, the user writing back in
    /// `thread` (the day's check-in thread cannot); task, `task_id` finished or
    /// dropped; event, `event_id` settled.
    #[serde(default)]
    pub cancel_on: Option<CancelOn>,
    #[serde(default)]
    pub task_id: Option<i64>,
    #[serde(default)]
    pub event_id: Option<i64>,
    /// A check on how the open work session is going: free, and dropped when
    /// the session ends. Leave it off for anything meant to stand on its own,
    /// like a reminder.
    #[serde(default)]
    pub session_check: bool,
    /// Ring when it fires: what you then say opens a call if the user is free
    /// for one (the check `call_user` makes), else it goes as a message.
    #[serde(default)]
    pub ring: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CancelOn {
    Reply,
    Task,
    Event,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BudgetArgs {
    /// How many more check-ins that day, 1 to 10.
    pub extra: u32,
    /// What the user agreed to, in one line.
    pub reason: String,
    /// The day to raise, YYYY-MM-DD. Omit for today.
    #[serde(default)]
    pub date: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SayArgs {
    /// One or two warm sentences, as the user will read them.
    pub text: String,
    /// The ids of the working notes these words are about, when a nudge names any.
    #[serde(default)]
    pub notes: Vec<String>,
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

fn refused(e: Refusal) -> ToolError {
    match e {
        Refusal::CapReached { date, allowance, spent } => ToolError::cap_reached(format!(
            "{spent} of {date}'s {allowance} check-ins are laid; ask the user, and call \
             trigger_budget for that date after they agree"
        )),
        Refusal::Past { at } => {
            ToolError::past(format!("{at} has gone by; pick a time still ahead, or a later date"))
        }
        Refusal::NotFound(m) => ToolError::not_found(m),
        Refusal::Rejected(m) => ToolError::rejected(m),
        Refusal::Internal(m) => ToolError::internal(m),
    }
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

/// A day the model named for a trigger, refused once gone by in `zone`.
fn checked_day(raw: &str, zone: &jiff::tz::TimeZone, now: jiff::Timestamp) -> Result<jiff::civil::Date, ToolError> {
    let date: jiff::civil::Date = raw
        .trim()
        .parse()
        .map_err(|_| ToolError::rejected(format!("date must be YYYY-MM-DD, got {raw:?}")))?;
    let today = now.to_zoned(zone.clone()).date();
    if date < today {
        return Err(ToolError::past(format!("{date} has gone by; today is {today}")));
    }
    Ok(date)
}

struct Aim<'a> {
    at: &'a str,
    date: Option<&'a str>,
    tz: Option<&'a str>,
    replace: Option<i64>,
    also: bool,
}

/// The plan date and wall time a trigger is laid with, restated in the user's
/// own zone when the model aimed it in another.
fn placed(
    ctx: &ToolCtx,
    kind: SessionKind,
    aim: &Aim,
    now: jiff::Timestamp,
) -> Result<(jiff::civil::Date, String), ToolError> {
    let zone = zone_arg(aim.tz)?;
    if aim.at.starts_with('+') {
        if aim.date.is_some() || zone.is_some() {
            return Err(ToolError::rejected("date and tz go with an HH:MM time, not a +Nmin offset"));
        }
        return Ok((target_date(ctx, kind, now), aim.at.to_string()));
    }
    let user = triggers::timezone(ctx.config_dir, ctx.username);
    let date = match (aim.date, &zone) {
        (Some(raw), _) => checked_day(raw, zone.as_ref().unwrap_or(&user), now)?,
        (None, Some(there)) if kind != SessionKind::Nightly => now.to_zoned(there.clone()).date(),
        (None, _) => target_date(ctx, kind, now),
    };
    let Some(there) = zone else {
        return Ok((date, aim.at.to_string()));
    };
    crate::zone::translate(date, aim.at, &there, &user).ok_or_else(|| {
        ToolError::rejected(format!("at must be zero-padded HH:MM or +Nmin, got {:?}", aim.at))
    })
}

fn drop_replaced(conn: &Connection, ctx: &ToolCtx, id: i64, now: jiff::Timestamp) -> Result<(), ToolError> {
    let internal = |e: rusqlite::Error| ToolError::internal(e.to_string());
    let n = conn
        .execute(
            "UPDATE events SET status = 'dropped', decided_at = ?1
             WHERE id = ?2 AND kind = ?3 AND status IN ('pending', 'snoozed')
               AND plan_id IN (SELECT id FROM plans WHERE user_id = ?4)",
            (now.to_string(), id, triggers::KIND, ctx.user_id),
        )
        .map_err(internal)?;
    if n == 0 {
        return Err(ToolError::not_found(format!(
            "no pending trigger {id} for this user; plan_list shows a day's triggers"
        )));
    }
    crate::log::record(conn, Some(ctx.user_id), "trigger_replaced", &format!("event {id}"))
        .map_err(|e| ToolError::internal(e.to_string()))
}

/// Lays the trigger unless one already waits within `NEAR_MIN` minutes of it,
/// which the model is shown instead, so a follow-up is not laid twice.
#[allow(clippy::too_many_arguments)]
fn lay(
    conn: &Connection,
    ctx: &ToolCtx,
    kind: SessionKind,
    aim: &Aim,
    prompt: &str,
    cancel: Option<Cancel>,
    thread: Option<i64>,
    session_check: bool,
    ring: bool,
) -> Result<serde_json::Value, ToolError> {
    let now = jiff::Timestamp::now();
    let (date, at) = placed(ctx, kind, aim, now)?;
    if let Some(id) = aim.replace {
        drop_replaced(conn, ctx, id, now)?;
    }
    let lay = Lay {
        config_dir: ctx.config_dir,
        user_id: ctx.user_id,
        username: ctx.username,
        at: &at,
        prompt,
        date,
        cancel,
        conversation_id: thread,
        work_session_id: None,
        session_check,
        system: false,
        now,
    };
    if !aim.also {
        let target = triggers::aimed_at(&lay).map_err(refused)?;
        let tz = triggers::timezone(ctx.config_dir, ctx.username);
        let near = triggers::near(conn, ctx.user_id, &tz, target)
            .map_err(|e| ToolError::internal(e.to_string()))?;
        if let Some(n) = near {
            return Err(ToolError::duplicate(format!(
                "trigger {} is already laid for {} {} ({:?}); drop it with schedule_drop, pass \
                 replace: {} to lay this one in its place, or also: true when both are wanted",
                n.event_id, n.date, n.at, n.prompt, n.event_id
            )));
        }
    }
    let laid = triggers::lay(conn, &lay).map_err(refused)?;
    if ring {
        triggers::set_ring(conn, laid.event_id).map_err(|e| ToolError::internal(e.to_string()))?;
    }
    let (date, work_session): (String, Option<i64>) = conn
        .query_row(
            "SELECT p.date, e.work_session_id FROM events e JOIN plans p ON p.id = e.plan_id WHERE e.id = ?1",
            [laid.event_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| ToolError::internal(e.to_string()))?;
    let mut out = serde_json::json!({
        "event_id": laid.event_id,
        "date": date,
        "at": laid.at,
        "cancel_if": laid.cancel_if,
    });
    if session_check {
        out["work_session"] = work_session.into();
    }
    if ring {
        out["ring"] = serde_json::json!(true);
    }
    Ok(out)
}

pub fn set(
    conn: &Connection,
    ctx: &ToolCtx,
    kind: SessionKind,
    args: &SetArgs,
) -> Result<serde_json::Value, ToolError> {
    let aim = Aim {
        at: &args.at,
        date: args.date.as_deref(),
        tz: args.tz.as_deref(),
        replace: args.replace,
        also: args.also,
    };
    let cancel = match (&args.cancel_on, args.task_id, args.event_id) {
        (None, None, None) => None,
        (Some(CancelOn::Reply), None, None) => Some(Cancel::Replied),
        (Some(CancelOn::Task), Some(id), None) => Some(Cancel::TaskDone(id)),
        (Some(CancelOn::Event), None, Some(id)) => Some(Cancel::EventDecided(id)),
        _ => {
            return Err(ToolError::rejected(
                "task_id goes with cancel_on task, event_id with cancel_on event, and reply takes neither",
            ))
        }
    };
    lay(conn, ctx, kind, &aim, &args.prompt, cancel, args.thread, args.session_check, args.ring)
}

/// Raises a day's allowance after the user has agreed to it; the receipt says
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
    let now = jiff::Timestamp::now();
    let date = match args.date.as_deref() {
        Some(raw) => checked_day(raw, &triggers::timezone(ctx.config_dir, ctx.username), now)?,
        None => target_date(ctx, SessionKind::Talk, now),
    };
    let base = triggers::allowance_of(ctx.config_dir, ctx.username);
    let raised = triggers::allowance_over(conn, base, ctx.user_id, date).map_err(internal)? - base;
    if raised + args.extra > base {
        return Err(ToolError::cap_reached(format!(
            "{date} is already raised by {raised}; a day reaches at most twice its {base} check-ins"
        )));
    }
    let extra = triggers::add_extra(conn, ctx.user_id, date, args.extra).map_err(internal)?;
    crate::log::record(
        conn,
        Some(ctx.user_id),
        "trigger_budget",
        &format!("+{} on {date} ({reason})", args.extra),
    )
    .map_err(|e| ToolError::internal(e.to_string()))?;
    let allowance = triggers::allowance(conn, ctx.config_dir, ctx.username, ctx.user_id, date)
        .map_err(internal)?;
    let spent = triggers::spent(conn, ctx.user_id, date).map_err(internal)?;
    Ok(serde_json::json!({ "extra": extra, "allowance": allowance, "spent": spent }))
}

/// The trigger session's terminal word. The firing path is what delivers it and
/// writes it to the thread, so the tool only vets the text.
pub fn say(args: &SayArgs) -> Result<serde_json::Value, ToolError> {
    let text = args.text.trim();
    if text.is_empty() || text.len() > triggers::MAX_SAY_BYTES {
        return Err(ToolError::rejected(format!("text must be 1 to {} bytes", triggers::MAX_SAY_BYTES)));
    }
    let mut out = serde_json::json!({ "said": text, "notes": args.notes });
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
    fn a_trigger_is_laid_to_ring_only_when_asked() {
        let (conn, tmp) = env();
        for (args, channel) in [(r#","ring":true"#, "call"), ("", "push")] {
            let json = format!(r#"{{"at":"{}","prompt":"call about the essay","also":true{args}}}"#, soon());
            let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "trigger_set", &json).unwrap();
            assert_eq!(out["ring"].as_bool(), (channel == "call").then_some(true), "{out}");
            let laid: String = conn
                .query_row("SELECT channel FROM events WHERE id = ?1", [out["event_id"].as_i64().unwrap()], |r| r.get(0))
                .unwrap();
            assert_eq!(laid, channel);
        }
    }

    #[test]
    fn cancel_on_carries_its_own_cancel_rule() {
        let (conn, tmp) = env();
        let out = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Talk,
            "trigger_set",
            &format!(r#"{{"at":"{}","prompt":"did they answer?","cancel_on":"reply"}}"#, soon()),
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
            "trigger_set",
            r#"{"task_id":1,"cancel_on":"task","at":"+150min","prompt":"still on the essay?"}"#,
        )
        .unwrap();
        assert_eq!(out["cancel_if"], "task_done");

        let e = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Talk,
            "trigger_set",
            &format!(r#"{{"task_id":1,"event_id":2,"cancel_on":"task","at":"{}","prompt":"x"}}"#, soon()),
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");
        for raw in [r#""task_id":1"#, r#""cancel_on":"event""#, r#""cancel_on":"reply","task_id":1"#] {
            let args = format!(r#"{{{raw},"at":"{}","prompt":"x"}}"#, soon());
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "trigger_set", &args).unwrap_err();
            assert_eq!(e.kind, "rejected", "{args}");
        }
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
            lay(&format!("+{}min", 60 + n * 30)).unwrap();
        }
        let e = lay("+240min").unwrap_err();
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
        lay("+240min").unwrap();

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
    fn a_trigger_loosely_written_is_named_as_such() {
        let (conn, tmp) = env();
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
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "trigger_set",
            r#"{"at":"+150min","prompt":"did they answer?","cancel_on":"reply"}"#).unwrap();
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

        for kind in [SessionKind::Talk, SessionKind::Nightly] {
            let e = dispatch(&conn, &ctx(&tmp), kind, "say", r#"{"text":"hi"}"#).unwrap_err();
            assert_eq!(e.kind, "unknown_tool", "{kind:?} can speak out of turn");
        }
    }
    #[test]
    fn say_carries_the_notes_it_names() {
        let (conn, tmp) = env();
        let id = "00000000-0000-4000-8000-000000000003";
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Trigger, "say",
            &format!(r#"{{"text":"the bank closes at five","notes":["{id}"]}}"#)).unwrap();
        assert_eq!(out["notes"], serde_json::json!([id]));
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Trigger, "say", r#"{"text":"hi"}"#)
            .unwrap();
        assert_eq!(out["notes"], serde_json::json!([]));
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

    fn day_in(zone: &str, days: i64) -> jiff::civil::Date {
        let tz = jiff::tz::TimeZone::get(zone).unwrap();
        jiff::Timestamp::now().to_zoned(tz).date().checked_add(jiff::Span::new().days(days)).unwrap()
    }

    fn placed(conn: &rusqlite::Connection, id: i64) -> (String, String) {
        conn.query_row(
            "SELECT p.date, e.wall_time FROM events e JOIN plans p ON p.id = e.plan_id WHERE e.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    /// A call asked for tomorrow at eleven lands on tomorrow's plan and spends
    /// tomorrow's allowance, not today's.
    #[test]
    fn a_trigger_for_a_later_day_lands_there_and_spends_that_days_budget() {
        let (conn, tmp) = env_in("America/Los_Angeles");
        let tomorrow = day_in("America/Los_Angeles", 1);
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "trigger_set",
            &format!(r#"{{"at":"11:00","date":"{tomorrow}","prompt":"they asked for a call"}}"#)).unwrap();
        let id = out["event_id"].as_i64().unwrap();
        assert_eq!(placed(&conn, id), (tomorrow.to_string(), "11:00".into()));
        assert_eq!(out["date"], tomorrow.to_string());
        assert_eq!(crate::triggers::spent(&conn, 1, tomorrow).unwrap(), 1);
        assert_eq!(crate::triggers::spent(&conn, 1, day_in("America/Los_Angeles", 0)).unwrap(), 0);
    }

    #[test]
    fn a_date_gone_by_or_beside_an_offset_is_refused() {
        let (conn, tmp) = env_in("America/Los_Angeles");
        let set = |date: jiff::civil::Date, at: &str| {
            dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "trigger_set",
                &format!(r#"{{"at":"{at}","date":"{date}","prompt":"x"}}"#)).unwrap_err()
        };
        assert_eq!(set(day_in("America/Los_Angeles", -1), "11:00").kind, "past");
        assert_eq!(set(day_in("America/Los_Angeles", 2), "+90min").kind, "rejected");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "trigger_set",
            r#"{"at":"11:00","tz":"Mars/Olympus","prompt":"x"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
        let events: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(events, 0);
    }

    /// A day half a year off holds only the trigger until that day is planned,
    /// which then brings the template in force at the time, and a move of zone
    /// carries it like any other.
    #[test]
    fn a_trigger_half_a_year_off_waits_on_a_bare_day() {
        let (conn, tmp) = env_in("America/Los_Angeles");
        std::fs::create_dir_all(tmp.path().join("defaults/templates")).unwrap();
        std::fs::write(
            tmp.path().join("defaults/templates/default.toml"),
            "[[events]]\nkind = \"meds\"\ntime = \"08:00\"\ndays = [\"mon\",\"tue\",\"wed\",\"thu\",\"fri\",\"sat\",\"sun\"]\nflexibility = \"fixed\"\nchannel = \"push\"\n",
        )
        .unwrap();
        let far = day_in("America/Los_Angeles", 180);
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "trigger_set",
            &format!(r#"{{"at":"11:00","date":"{far}","prompt":"renew the passport"}}"#)).unwrap();
        let id = out["event_id"].as_i64().unwrap();
        let kinds = |conn: &rusqlite::Connection| -> Vec<String> {
            crate::plan::events_for(conn, 1, far).unwrap().into_iter().map(|e| e.kind).collect()
        };
        assert_eq!(kinds(&conn), ["trigger"]);
        assert!(!crate::plan::exists(&conn, 1, far).unwrap());
        let today = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "plan_list", "{}").unwrap();
        assert_eq!(today["later_trigger_days"], serde_json::json!([{ "date": far.to_string(), "triggers": 1 }]));

        crate::zone::rezone(&conn, tmp.path(), "aki", 1, "America/Los_Angeles", "America/New_York",
            jiff::Timestamp::now()).unwrap();
        assert_eq!(placed(&conn, id), (far.to_string(), "14:00".into()));

        let tmpl = crate::templates::Template::load(tmp.path(), "aki", "default").unwrap();
        crate::plan::generate(&conn, 1, &tmpl, far).unwrap();
        assert_eq!(kinds(&conn), ["meds", "trigger"]);
        assert!(crate::plan::exists(&conn, 1, far).unwrap());
        crate::plan::generate(&conn, 1, &tmpl, far).unwrap();
        assert_eq!(kinds(&conn).len(), 2, "a planned day is generated once");
    }

    /// Eleven in New York is eight where the account lives.
    #[test]
    fn a_time_given_in_another_zone_is_stored_in_the_users_own() {
        let (conn, tmp) = env_in("America/Los_Angeles");
        let there = day_in("America/New_York", 2);
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "trigger_set",
            &format!(r#"{{"at":"11:00","date":"{there}","tz":"America/New_York","prompt":"x"}}"#)).unwrap();
        assert_eq!(placed(&conn, out["event_id"].as_i64().unwrap()), (there.to_string(), "08:00".into()));
        assert_eq!(out["at"], "08:00");
    }

    #[test]
    fn a_second_trigger_beside_a_pending_one_is_shown_the_first() {
        let (conn, tmp) = env();
        let set = |args: &str| dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "trigger_set", args);
        let first = set(r#"{"at":"+90min","prompt":"ask about the essay"}"#).unwrap()["event_id"]
            .as_i64()
            .unwrap();
        let e = set(r#"{"at":"+105min","prompt":"essay?"}"#).unwrap_err();
        assert_eq!(e.kind, "duplicate");
        assert!(e.message.contains(&format!("replace: {first}")), "{}", e.message);
        assert!(e.message.contains("ask about the essay"), "{}", e.message);
        set(r#"{"at":"+120min","prompt":"a separate thing"}"#).unwrap();

        set(r#"{"at":"+75min","prompt":"both wanted","also":true}"#).unwrap();
        let replaced = set(&format!(r#"{{"at":"+95min","prompt":"essay, sharper","replace":{first},"also":true}}"#))
            .unwrap();
        let status: String = conn
            .query_row("SELECT status FROM events WHERE id = ?1", [first], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "dropped");
        assert!(replaced["event_id"].as_i64().unwrap() > first);
        let e = set(r#"{"at":"+300min","prompt":"x","replace":999}"#).unwrap_err();
        assert_eq!(e.kind, "not_found");
    }

    #[test]
    fn a_day_is_raised_to_at_most_twice_its_allowance() {
        let (conn, tmp) = env();
        let raise = |n: u32| dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "trigger_budget",
            &format!(r#"{{"extra":{n},"reason":"they asked"}}"#));
        raise(3).unwrap();
        assert_eq!(raise(2).unwrap_err().kind, "cap_reached");
        assert_eq!(raise(1).unwrap()["allowance"], 8);
        assert_eq!(raise(1).unwrap_err().kind, "cap_reached");
    }

    #[test]
    fn the_budget_raises_the_day_it_names() {
        let (conn, tmp) = env_in("America/Los_Angeles");
        let tomorrow = day_in("America/Los_Angeles", 1);
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "trigger_budget",
            &format!(r#"{{"extra":2,"reason":"a long day tomorrow","date":"{tomorrow}"}}"#)).unwrap();
        assert_eq!(out["allowance"], 6);
        let today = crate::triggers::allowance(&conn, tmp.path(), "aki", 1, day_in("America/Los_Angeles", 0))
            .unwrap();
        assert_eq!(today, 4);
    }
}
