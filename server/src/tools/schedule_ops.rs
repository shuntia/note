use super::{ToolCtx, ToolError};
use rusqlite::{Connection, OptionalExtension};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SlideArgs {
    pub event_id: i64,
    pub minutes: i64,
}

/// The plan date and current wall time of an owned event; the pair every
/// calendar check needs before it can say where a move would land.
fn event_day(conn: &Connection, user_id: i64, event_id: i64) -> Option<(jiff::civil::Date, String)> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT p.date, e.wall_time FROM events e JOIN plans p ON p.id = e.plan_id
             WHERE e.id = ?1 AND p.user_id = ?2",
            (event_id, user_id),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten();
    row.and_then(|(date, wall)| Some((date.parse().ok()?, wall)))
}

/// Refuses a target range that runs into a hard commitment on the user's
/// calendar, naming the one it hits.
fn check_calendar(
    conn: &Connection,
    user_id: i64,
    date: jiff::civil::Date,
    start: &str,
    end: &str,
) -> Result<(), ToolError> {
    let clash = crate::calendar::conflict(conn, user_id, date, start, end)
        .map_err(|e| ToolError::internal(e.to_string()))?;
    match clash {
        Some(why) if start == end => Err(ToolError::rejected(format!("{start} is {why}"))),
        Some(why) => Err(ToolError::rejected(format!("{start}-{end} is {why}"))),
        None => Ok(()),
    }
}

/// Why an owned event refused a slide or snooze, so the model is pointed at
/// the tool that can do it rather than told the event does not exist.
fn refusal(conn: &Connection, user_id: i64, event_id: i64, verb: &str) -> ToolError {
    match crate::plan::event_gate(conn, user_id, event_id) {
        Ok(Some((flex, _))) => {
            if crate::plan::block_shape(conn, user_id, event_id).ok().flatten().is_some() {
                ToolError::rejected(format!(
                    "event {event_id} is a block, which cannot {verb}; move or resize it with schedule_reshape"
                ))
            } else {
                ToolError::rejected(format!("event {event_id} has flexibility '{flex}' and cannot {verb}"))
            }
        }
        Ok(None) => ToolError::not_found(format!("no event {event_id} for this user; plan_list shows a day's ids")),
        Err(e) => ToolError::internal(e.to_string()),
    }
}

pub fn slide(conn: &Connection, ctx: &ToolCtx, args: SlideArgs) -> Result<serde_json::Value, ToolError> {
    if let Some((date, wall)) = event_day(conn, ctx.user_id, args.event_id) {
        let target = crate::templates::wall_add(&wall, args.minutes);
        check_calendar(conn, ctx.user_id, date, &target, &target)?;
    }
    match crate::plan::shift(conn, ctx.user_id, args.event_id, args.minutes) {
        Ok(Some(())) => Ok(serde_json::json!({ "ok": true })),
        Ok(None) => Err(refusal(conn, ctx.user_id, args.event_id, "slide")),
        Err(e @ (crate::plan::ShiftError::OutOfWindow { .. } | crate::plan::ShiftError::Decided { .. })) => {
            Err(ToolError::rejected(e.to_string()))
        }
        Err(e) => Err(ToolError::internal(e.to_string())),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnoozeArgs {
    pub event_id: i64,
    pub minutes: i64,
}

pub fn snooze(conn: &Connection, ctx: &ToolCtx, args: SnoozeArgs) -> Result<serde_json::Value, ToolError> {
    if !(1..=1440).contains(&args.minutes) {
        return Err(ToolError::rejected("snooze minutes must be in 1..=1440"));
    }
    match crate::plan::snooze(conn, ctx.user_id, args.event_id, args.minutes) {
        Ok(Some(())) => Ok(serde_json::json!({ "ok": true })),
        Ok(None) => Err(refusal(conn, ctx.user_id, args.event_id, "be snoozed")),
        Err(e @ crate::plan::ShiftError::Decided { .. }) => Err(ToolError::rejected(e.to_string())),
        Err(e) => Err(ToolError::internal(e.to_string())),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReshapeArgs {
    pub event_id: i64,
    /// New start, zero-padded HH:MM. Omit to keep the current one.
    #[serde(default)]
    pub start: Option<String>,
    /// New end, zero-padded HH:MM. Omit to keep the current one.
    #[serde(default)]
    pub end: Option<String>,
}

pub fn reshape(conn: &Connection, ctx: &ToolCtx, args: ReshapeArgs) -> Result<serde_json::Value, ToolError> {
    if args.start.is_none() && args.end.is_none() {
        return Err(ToolError::rejected("give a new start, a new end, or both"));
    }
    for (field, value) in [("start", &args.start), ("end", &args.end)] {
        if let Some(v) = value {
            if !crate::templates::valid_time(v) {
                return Err(ToolError::rejected(format!(
                    "{field} must be zero-padded HH:MM, got {v:?}"
                )));
            }
        }
    }
    let shape = crate::plan::block_shape(conn, ctx.user_id, args.event_id)
        .map_err(|e| ToolError::internal(e.to_string()))?;
    let Some((cur_start, cur_end, _)) = shape else {
        return Err(ToolError::not_found(format!(
            "no block {} for this user; only blocks have a start and an end, and plan_list shows which events are blocks", args.event_id
        )));
    };
    let start = args.start.unwrap_or(cur_start);
    let end = args.end.unwrap_or(cur_end);
    if end <= start {
        return Err(ToolError::rejected(format!("a block must end after it starts, got {start}-{end}")));
    }
    if let Some((date, _)) = event_day(conn, ctx.user_id, args.event_id) {
        check_calendar(conn, ctx.user_id, date, &start, &end)?;
    }
    match crate::plan::reshape(conn, ctx.user_id, args.event_id, &start, &end) {
        Ok(Some(())) => Ok(serde_json::json!({ "start": start, "end": end })),
        Ok(None) => Err(ToolError::not_found(format!("no block {}", args.event_id))),
        Err(e @ crate::plan::ShiftError::Decided { .. }) => Err(ToolError::rejected(e.to_string())),
        Err(e) => Err(ToolError::internal(e.to_string())),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DropArgs {
    pub event_id: i64,
    /// The event this one moved to, when the drop is a reschedule rather than an
    /// abandonment. Add the replacement first, then name its id here.
    #[serde(default)]
    pub moved_to_event_id: Option<i64>,
}

/// The agent may only drop events the template marked `flexibility = 'drop'`,
/// and only while they are still undecided; abandoning anything else — or
/// rewriting a decision the user already made — is the user's call, not the
/// model's.
pub fn drop_event(conn: &Connection, ctx: &ToolCtx, args: DropArgs) -> Result<serde_json::Value, ToolError> {
    if let Some(target) = args.moved_to_event_id {
        if target == args.event_id {
            return Err(ToolError::rejected("an event cannot have moved to itself"));
        }
        match crate::plan::event_gate(conn, ctx.user_id, target) {
            Ok(None) => {
                return Err(ToolError::rejected(format!(
                    "no event {target} to have moved to; add the replacement first"
                )))
            }
            Ok(Some(_)) => {}
            Err(e) => return Err(ToolError::internal(e.to_string())),
        }
    }
    match crate::plan::event_gate(conn, ctx.user_id, args.event_id) {
        Ok(None) => return Err(ToolError::not_found(format!("no event {}", args.event_id))),
        Ok(Some((flex, _))) if flex != "drop" => {
            return Err(ToolError::rejected(format!(
                "event {} has flexibility '{flex}'; only droppable events can be dropped by the agent",
                args.event_id
            )));
        }
        Ok(Some((_, status))) if status == "done" || status == "dropped" => {
            return Err(ToolError::rejected(format!(
                "event {} is already '{status}'; the user decided it",
                args.event_id
            )));
        }
        Ok(Some(_)) => {}
        Err(e) => return Err(ToolError::internal(e.to_string())),
    }
    match crate::plan::set_status(conn, ctx.user_id, args.event_id, "dropped") {
        Ok(Some(())) => {}
        Ok(None) => return Err(ToolError::not_found(format!("no event {}", args.event_id))),
        Err(e) => return Err(ToolError::internal(e.to_string())),
    }
    if let Some(target) = args.moved_to_event_id {
        crate::plan::set_moved_to(conn, ctx.user_id, args.event_id, target)
            .map_err(|e| ToolError::internal(e.to_string()))?;
    }
    Ok(serde_json::json!({ "ok": true }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Flexibility { Fixed, Slide, Drop }

impl Flexibility {
    fn as_str(&self) -> &'static str {
        match self {
            Flexibility::Fixed => "fixed",
            Flexibility::Slide => "slide",
            Flexibility::Drop => "drop",
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Channel { Push, Voice }

impl Channel {
    fn as_str(&self) -> &'static str {
        match self {
            Channel::Push => "push",
            Channel::Voice => "voice",
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InsertArgs {
    pub date: String,
    pub kind: String,
    pub time: String,
    pub flexibility: Flexibility,
    #[serde(default)]
    pub slide_window_min: i64,
    pub channel: Channel,
}

/// Adds an event to an already-generated day plan; creating the plan itself
/// stays with the nightly job, so a missing plan is a rejection, not an insert.
pub fn insert(conn: &Connection, ctx: &ToolCtx, args: InsertArgs) -> Result<serde_json::Value, ToolError> {
    let kind = args.kind.trim();
    if kind.is_empty() || kind.len() > 100 {
        return Err(ToolError::rejected("kind must be 1..=100 characters"));
    }
    if !crate::templates::valid_time(&args.time) {
        return Err(ToolError::rejected(format!("time must be zero-padded HH:MM, got {:?}", args.time)));
    }
    if !(0..=720).contains(&args.slide_window_min) {
        return Err(ToolError::rejected("slide_window_min must be in 0..=720"));
    }
    let date: jiff::civil::Date = args.date.parse()
        .map_err(|_| ToolError::rejected(format!("date must be YYYY-MM-DD, got {:?}", args.date)))?;
    let plan_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM plans WHERE user_id = ?1 AND date = ?2",
            (ctx.user_id, date.to_string()),
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| ToolError::internal(e.to_string()))?;
    let Some(plan_id) = plan_id else {
        return Err(ToolError::rejected(format!("no plan exists for {date}; generate it first")));
    };
    check_calendar(conn, ctx.user_id, date, &args.time, &args.time)?;
    conn.execute(
        "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, flexibility, slide_window_min, channel, origin)
         VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, 'agent')",
        (plan_id, kind, &args.time, args.flexibility.as_str(), args.slide_window_min, args.channel.as_str()),
    )
    .map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(serde_json::json!({ "event_id": conn.last_insert_rowid() }))
}

#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, SessionKind, ToolCtx};

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        let tmpl = crate::templates::Template {
            events: vec![
                crate::templates::TemplateEvent {
                    kind: "checkin_call".into(), time: "09:00".into(),
                    days: vec!["mon".into()], flexibility: Some("slide".into()),
                    slide_window_min: Some(60), channel: "voice".into(), ..Default::default()
                },
                crate::templates::TemplateEvent {
                    kind: "nudge".into(), time: "14:00".into(),
                    days: vec!["mon".into()], flexibility: Some("drop".into()),
                    slide_window_min: Some(0), channel: "push".into(), ..Default::default()
                },
                crate::templates::TemplateEvent {
                    kind: "Work time".into(), time: "09:30".into(),
                    days: vec!["mon".into()], entry: crate::templates::Entry::Block,
                    end_time: Some("12:30".into()), channel: "push".into(), ..Default::default()
                },
            ],
        };
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, 1, &tmpl, date).unwrap();
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir) -> ToolCtx<'a> {
        ToolCtx { config_dir: tmp.path(), data_dir: tmp.path(), user_id: 1, username: "aki", vectors: crate::tools::PreparedVectors::default(), task_scope: None, inbox_source: None }
    }


    /// School covers the middle of the Monday the fixture plans.
    fn school(conn: &rusqlite::Connection) {
        crate::calendar::create(conn, 1, crate::calendar::Fields {
            title: "school".into(), kind: "fixed".into(),
            start_time: "09:30".into(), end_time: "15:30".into(),
            days: Some(crate::calendar::day_mask(&["mon", "tue", "wed", "thu", "fri"]).unwrap()),
            ..Default::default()
        }).unwrap();
    }

    #[test]
    fn a_slide_into_a_fixed_commitment_is_refused_by_name() {
        let (conn, tmp) = env();
        school(&conn);
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "schedule_slide",
            r#"{"event_id":1,"minutes":30}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
        assert_eq!(e.message, "09:30 is inside school 09:30-15:30");
        let wall: String = conn
            .query_row("SELECT wall_time FROM events WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(wall, "09:00", "a refused slide leaves the event alone");

        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "schedule_slide",
            r#"{"event_id":1,"minutes":-30}"#).unwrap();
    }

    #[test]
    fn an_insert_inside_a_fixed_commitment_is_refused() {
        let (conn, tmp) = env();
        school(&conn);
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "schedule_insert",
            r#"{"date":"2026-08-31","kind":"nudge","time":"10:00","flexibility":"slide","channel":"push"}"#,
        ).unwrap_err();
        assert_eq!(e.kind, "rejected");
        assert!(e.message.contains("inside school 09:30-15:30"), "{}", e.message);

        dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "schedule_insert",
            r#"{"date":"2026-08-31","kind":"nudge","time":"16:00","flexibility":"slide","channel":"push"}"#,
        ).unwrap();
    }

    #[test]
    fn a_reshape_into_a_fixed_commitment_is_refused() {
        let (conn, tmp) = env();
        school(&conn);
        let block: i64 = conn
            .query_row("SELECT id FROM events WHERE end_wall_time IS NOT NULL", [], |r| r.get(0))
            .unwrap();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "schedule_reshape",
            &format!(r#"{{"event_id":{block},"start":"13:00","end":"14:00"}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");
        assert!(e.message.contains("inside school 09:30-15:30"), "{}", e.message);

        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "schedule_reshape",
            &format!(r#"{{"event_id":{block},"start":"07:00","end":"09:30"}}"#)).unwrap();
    }

    
    #[test]
    fn slide_snooze_and_window_rejection() {
        let (conn, tmp) = env();
        dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_slide",
            r#"{"event_id":1,"minutes":30}"#).unwrap();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_slide",
            r#"{"event_id":1,"minutes":45}"#).unwrap_err();
        assert_eq!(e.kind, "rejected"); // cumulative 75 > window 60
        dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_snooze",
            r#"{"event_id":1,"minutes":15}"#).unwrap();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_snooze",
            r#"{"event_id":1,"minutes":0}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_slide",
            r#"{"event_id":99,"minutes":5}"#).unwrap_err();
        assert_eq!(e.kind, "not_found");
    }

    #[test]
    fn snooze_on_a_decided_event_is_rejected_not_missing() {
        let (conn, tmp) = env();
        conn.execute("UPDATE events SET status='done' WHERE id=1", []).unwrap();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_snooze",
            r#"{"event_id":1,"minutes":10}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_snooze",
            r#"{"event_id":99,"minutes":10}"#).unwrap_err();
        assert_eq!(e.kind, "not_found");
    }

    #[test]
    fn drop_only_applies_to_droppable_events() {
        let (conn, tmp) = env();
        // event 1 is flexibility=slide → agent cannot drop it
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_drop",
            r#"{"event_id":1}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
        // event 2 is flexibility=drop
        dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_drop",
            r#"{"event_id":2}"#).unwrap();
        let status: String = conn
            .query_row("SELECT status FROM events WHERE id=2", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "dropped");
    }

    #[test]
    fn drop_leaves_a_user_decision_alone() {
        let (conn, tmp) = env();
        conn.execute("UPDATE events SET status='done' WHERE id=2", []).unwrap();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_drop",
            r#"{"event_id":2}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
        let status: String = conn
            .query_row("SELECT status FROM events WHERE id=2", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "done");
    }

    #[test]
    fn a_drop_can_name_the_event_it_moved_to() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "schedule_insert",
            r#"{"date":"2026-08-31","kind":"nudge","time":"17:00","flexibility":"drop","channel":"push"}"#).unwrap();
        let moved = out["event_id"].as_i64().unwrap();
        dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "schedule_drop",
            &format!(r#"{{"event_id":2,"moved_to_event_id":{moved}}}"#)).unwrap();

        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let evs = crate::plan::events_for(&conn, 1, date).unwrap();
        let dropped = evs.iter().find(|e| e.id == 2).unwrap();
        assert_eq!(dropped.status, "dropped");
        let to = dropped.moved_to.as_ref().unwrap();
        assert_eq!((to.event_id, to.wall_time.as_str(), to.kind.as_str()), (moved, "17:00", "nudge"));
        assert_eq!(to.date, "2026-08-31");
        assert!(evs.iter().find(|e| e.id == 1).unwrap().moved_to.is_none());
    }

    #[test]
    fn a_drop_cannot_point_at_itself_or_an_event_that_is_not_there() {
        let (conn, tmp) = env();
        for raw in [
            r#"{"event_id":2,"moved_to_event_id":2}"#,
            r#"{"event_id":2,"moved_to_event_id":9999}"#,
        ] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_drop", raw)
                .unwrap_err();
            assert_eq!(e.kind, "rejected", "{raw}");
        }
        let status: String =
            conn.query_row("SELECT status FROM events WHERE id=2", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "pending", "a rejected drop must change nothing");
    }

    #[test]
    fn the_agent_reshapes_a_block_but_not_a_routine() {
        let (conn, tmp) = env();
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "schedule_reshape",
            r#"{"event_id":3,"start":"10:00","end":"13:00"}"#).unwrap();
        let shape = |id: i64| -> (String, String) {
            conn.query_row("SELECT wall_time, end_wall_time FROM events WHERE id=?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
        };
        assert_eq!(shape(3), ("10:00".to_string(), "13:00".to_string()));

        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "schedule_reshape",
            r#"{"event_id":3,"end":"14:00"}"#).unwrap();
        assert_eq!(shape(3), ("10:00".to_string(), "14:00".to_string()));

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "schedule_reshape",
            r#"{"event_id":1,"start":"10:00","end":"11:00"}"#).unwrap_err();
        assert_eq!(e.kind, "not_found", "a routine has no shape to change");

        for (raw, why) in [
            (r#"{"event_id":3}"#, "nothing to change"),
            (r#"{"event_id":3,"start":"9:00"}"#, "unpadded start"),
            (r#"{"event_id":3,"start":"15:00","end":"14:00"}"#, "end before start"),
            (r#"{"event_id":3,"start":"15:00"}"#, "start past the stored end"),
        ] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "schedule_reshape", raw)
                .unwrap_err();
            assert_eq!(e.kind, "rejected", "{why}");
        }
        assert_eq!(shape(3), ("10:00".to_string(), "14:00".to_string()));
    }

    #[test]
    fn a_block_moves_only_by_reshaping() {
        let (conn, tmp) = env();
        for (tool, raw) in [
            ("schedule_slide", r#"{"event_id":3,"minutes":30}"#),
            ("schedule_snooze", r#"{"event_id":3,"minutes":30}"#),
        ] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, tool, raw).unwrap_err();
            assert_eq!(e.kind, "rejected", "{tool} moved a block");
            assert!(e.message.contains("schedule_reshape"), "{tool}: {}", e.message);
        }
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "schedule_slide",
            r#"{"event_id":999,"minutes":5}"#).unwrap_err();
        assert_eq!(e.kind, "not_found");
        let (start, end): (String, String) = conn
            .query_row("SELECT wall_time, end_wall_time FROM events WHERE id=3", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((start.as_str(), end.as_str()), ("09:30", "12:30"));
    }

    #[test]
    fn a_decided_block_is_left_alone() {
        let (conn, tmp) = env();
        conn.execute("UPDATE events SET status='done' WHERE id=3", []).unwrap();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "schedule_reshape",
            r#"{"event_id":3,"start":"10:00"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
    }

    #[test]
    fn no_schedule_tool_accepts_an_alert_flag() {
        let (conn, tmp) = env();
        for (tool, raw) in [
            ("schedule_reshape", r#"{"event_id":3,"start":"10:00","alert":false}"#),
            ("schedule_slide", r#"{"event_id":1,"minutes":5,"alert":false}"#),
            ("schedule_snooze", r#"{"event_id":1,"minutes":5,"alert":false}"#),
            ("schedule_drop", r#"{"event_id":2,"alert":false}"#),
            ("schedule_insert", r#"{"date":"2026-08-31","kind":"nudge","time":"16:00","flexibility":"drop","channel":"push","alert":false}"#),
        ] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, tool, raw).unwrap_err();
            assert_eq!(e.kind, "invalid_args", "{tool} accepted an alert flag");
        }
        let bells: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT alert FROM events ORDER BY id").unwrap();
            stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(bells, vec![1, 1, 0]);
    }

    #[test]
    fn insert_is_nightly_only_and_validated() {
        let (conn, tmp) = env();
        let ok = r#"{"date":"2026-08-31","kind":"nudge","time":"16:30","flexibility":"drop","channel":"push"}"#;
        for kind in [SessionKind::Checkin, SessionKind::Talk] {
            let e = dispatch(&conn, &ctx(&tmp), kind, "schedule_insert", ok).unwrap_err();
            assert_eq!(e.kind, "forbidden");
        }
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "schedule_insert", ok).unwrap();
        assert!(out["event_id"].as_i64().unwrap() > 0);

        for (raw, why) in [
            (r#"{"date":"2026-08-31","kind":"nudge","time":"9:00","flexibility":"drop","channel":"push"}"#, "unpadded time"),
            (r#"{"date":"not-a-date","kind":"nudge","time":"09:00","flexibility":"drop","channel":"push"}"#, "bad date"),
            (r#"{"date":"2026-09-01","kind":"nudge","time":"09:00","flexibility":"drop","channel":"push"}"#, "no plan for date"),
            (r#"{"date":"2026-08-31","kind":"","time":"09:00","flexibility":"drop","channel":"push"}"#, "empty kind"),
            (r#"{"date":"2026-08-31","kind":"nudge","time":"09:00","flexibility":"drop","slide_window_min":-5,"channel":"push"}"#, "negative window"),
        ] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "schedule_insert", raw).unwrap_err();
            assert_eq!(e.kind, "rejected", "{why}");
        }
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "schedule_insert",
            r#"{"date":"2026-08-31","kind":"nudge","time":"09:00","flexibility":"soft","channel":"push"}"#).unwrap_err();
        assert_eq!(e.kind, "invalid_args");
    }
}
