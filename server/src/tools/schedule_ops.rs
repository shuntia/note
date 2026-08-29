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

pub fn slide(conn: &Connection, ctx: &ToolCtx, args: SlideArgs) -> Result<serde_json::Value, ToolError> {
    match crate::plan::shift(conn, ctx.user_id, args.event_id, args.minutes) {
        Ok(Some(())) => Ok(serde_json::json!({ "ok": true })),
        Ok(None) => Err(ToolError::not_found(format!(
            "no slideable event {} for this user", args.event_id
        ))),
        Err(e @ crate::plan::ShiftError::OutOfWindow { .. }) => Err(ToolError::rejected(e.to_string())),
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
        Ok(None) => Err(ToolError::not_found(format!(
            "no snoozable event {} for this user", args.event_id
        ))),
        Err(e) => Err(ToolError::internal(e.to_string())),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DropArgs {
    pub event_id: i64,
}

/// The agent may only drop events the template marked `flexibility = 'drop'`,
/// and only while they are still undecided; abandoning anything else — or
/// rewriting a decision the user already made — is the user's call, not the
/// model's.
pub fn drop_event(conn: &Connection, ctx: &ToolCtx, args: DropArgs) -> Result<serde_json::Value, ToolError> {
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
        Ok(Some(())) => Ok(serde_json::json!({ "ok": true })),
        Ok(None) => Err(ToolError::not_found(format!("no event {}", args.event_id))),
        Err(e) => Err(ToolError::internal(e.to_string())),
    }
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
    conn.execute(
        "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, flexibility, slide_window_min, channel)
         VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6)",
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
                    days: vec!["mon".into()], flexibility: "slide".into(),
                    slide_window_min: 60, channel: "voice".into(),
                },
                crate::templates::TemplateEvent {
                    kind: "nudge".into(), time: "14:00".into(),
                    days: vec!["mon".into()], flexibility: "drop".into(),
                    slide_window_min: 0, channel: "push".into(),
                },
            ],
        };
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, 1, &tmpl, date).unwrap();
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir) -> ToolCtx<'a> {
        ToolCtx { config_dir: tmp.path(), data_dir: tmp.path(), user_id: 1, username: "aki" }
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
