use super::{ToolCtx, ToolError};
use crate::calendar::{self, CalendarError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

pub const MAX_DAYS_AHEAD: u32 = 14;

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Day {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Day {
    fn as_str(self) -> &'static str {
        match self {
            Day::Mon => "mon",
            Day::Tue => "tue",
            Day::Wed => "wed",
            Day::Thu => "thu",
            Day::Fri => "fri",
            Day::Sat => "sat",
            Day::Sun => "sun",
        }
    }
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Fixed,
    Busy,
    Note,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Fixed => "fixed",
            Kind::Busy => "busy",
            Kind::Note => "note",
        }
    }
}

fn mask(days: &[Day]) -> i64 {
    days.iter().fold(0, |m, d| {
        m | calendar::day_mask(&[d.as_str()]).expect("a Day is always a known day name")
    })
}

fn failed(e: CalendarError) -> ToolError {
    match e {
        CalendarError::Invalid(m) => ToolError::rejected(m),
        e @ CalendarError::TooMany => ToolError::rejected(e.to_string()),
        e @ CalendarError::NotFound(_) => ToolError::not_found(e.to_string()),
        CalendarError::Db(e) => ToolError::internal(e.to_string()),
    }
}

/// The calendar belongs to the user, not to one imported task or inbox item, so
/// a scoped session never reaches it.
fn unscoped(ctx: &ToolCtx) -> Result<(), ToolError> {
    if ctx.task_scope.is_some() || ctx.inbox_source.is_some() {
        return Err(ToolError::forbidden("the calendar is out of this session's scope"));
    }
    Ok(())
}

/// Days reach the model as names and the database as a bitmask.
fn row(e: &calendar::Entry) -> serde_json::Value {
    serde_json::json!({
        "entry_id": e.id,
        "title": e.title,
        "kind": e.kind,
        "quiet": e.quiet,
        "start_time": e.start_time,
        "end_time": e.end_time,
        "days": e.day_names,
        "on_date": e.on_date,
        "from_date": e.from_date,
        "until_date": e.until_date,
        "skipped_dates": e.exceptions,
    })
}

fn today(ctx: &ToolCtx) -> jiff::civil::Date {
    let tz = crate::config::UserConfig::load(ctx.config_dir, ctx.username)
        .ok()
        .and_then(|c| jiff::tz::TimeZone::get(&c.timezone).ok())
        .unwrap_or(jiff::tz::TimeZone::UTC);
    jiff::Timestamp::now().to_zoned(tz).date()
}

fn parse_date(field: &str, raw: &str) -> Result<jiff::civil::Date, ToolError> {
    raw.parse()
        .map_err(|_| ToolError::rejected(format!("{field} must be YYYY-MM-DD, got {raw:?}")))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {
    /// First day to read, YYYY-MM-DD. Defaults to today.
    #[serde(default)]
    pub date: Option<String>,
    /// How many days from `date`, 1 to 14. Defaults to 1.
    #[serde(default)]
    pub days: Option<u32>,
}

pub fn list(
    conn: &Connection,
    ctx: &ToolCtx,
    args: ListArgs,
) -> Result<serde_json::Value, ToolError> {
    unscoped(ctx)?;
    let start = match args.date.as_deref() {
        Some(d) => parse_date("date", d)?,
        None => today(ctx),
    };
    let span = args.days.unwrap_or(1);
    if !(1..=MAX_DAYS_AHEAD).contains(&span) {
        return Err(ToolError::rejected(format!("days must be in 1..={MAX_DAYS_AHEAD}")));
    }
    let mut days = Vec::with_capacity(span as usize);
    let mut date = start;
    for _ in 0..span {
        let occurrences = calendar::occurrences(conn, ctx.user_id, date)
            .map_err(|e| ToolError::internal(e.to_string()))?;
        days.push(serde_json::json!({ "date": date.to_string(), "occurrences": occurrences }));
        date = date.tomorrow().map_err(|e| ToolError::internal(e.to_string()))?;
    }
    Ok(serde_json::json!({ "days": days }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddArgs {
    /// What it is, as the user would say it: "school", "swim practice".
    pub title: String,
    pub kind: Kind,
    /// Whether deliveries wait for the window to end. Defaults to true, and is
    /// always false for a note.
    #[serde(default)]
    pub quiet: Option<bool>,
    /// Zero-padded HH:MM in the user's timezone.
    pub start_time: String,
    pub end_time: String,
    /// The weekdays it repeats on. Leave it out for a one-off entry and give
    /// on_date instead.
    #[serde(default)]
    pub days: Option<Vec<Day>>,
    /// The single date a one-off entry happens, YYYY-MM-DD.
    #[serde(default)]
    pub on_date: Option<String>,
    /// Optional first and last date a repeating entry is valid, YYYY-MM-DD.
    #[serde(default)]
    pub from_date: Option<String>,
    #[serde(default)]
    pub until_date: Option<String>,
}

pub fn add(conn: &Connection, ctx: &ToolCtx, args: AddArgs) -> Result<serde_json::Value, ToolError> {
    unscoped(ctx)?;
    super::check_text("title", &args.title)?;
    let fields = calendar::Fields {
        title: args.title,
        kind: args.kind.as_str().into(),
        quiet: args.quiet,
        start_time: args.start_time,
        end_time: args.end_time,
        days: Some(args.days.as_deref().map_or(0, mask)),
        on_date: args.on_date,
        from_date: args.from_date,
        until_date: args.until_date,
    };
    calendar::create(conn, ctx.user_id, fields).map(|e| row(&e)).map_err(failed)
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateArgs {
    pub entry_id: i64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<Kind>,
    #[serde(default)]
    pub quiet: Option<bool>,
    #[serde(default)]
    pub start_time: Option<String>,
    #[serde(default)]
    pub end_time: Option<String>,
    /// The weekdays it repeats on; giving them clears a one-off date.
    #[serde(default)]
    pub days: Option<Vec<Day>>,
    /// The single date a one-off entry happens; giving it clears the weekdays.
    #[serde(default)]
    pub on_date: Option<String>,
    /// An empty string clears a validity bound.
    #[serde(default)]
    pub from_date: Option<String>,
    #[serde(default)]
    pub until_date: Option<String>,
}

pub fn update(
    conn: &Connection,
    ctx: &ToolCtx,
    args: UpdateArgs,
) -> Result<serde_json::Value, ToolError> {
    unscoped(ctx)?;
    if let Some(title) = &args.title {
        super::check_text("title", title)?;
    }
    let patch = calendar::Patch {
        title: args.title,
        kind: args.kind.map(|k| k.as_str().to_string()),
        quiet: args.quiet,
        start_time: args.start_time,
        end_time: args.end_time,
        days: args.days.as_deref().map(mask),
        on_date: args.on_date,
        from_date: args.from_date,
        until_date: args.until_date,
    };
    calendar::update(conn, ctx.user_id, args.entry_id, patch).map(|e| row(&e)).map_err(failed)
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveArgs {
    pub entry_id: i64,
}

pub fn remove(
    conn: &Connection,
    ctx: &ToolCtx,
    args: RemoveArgs,
) -> Result<serde_json::Value, ToolError> {
    unscoped(ctx)?;
    match calendar::delete(conn, ctx.user_id, args.entry_id) {
        Ok(true) => Ok(serde_json::json!({ "removed": true })),
        Ok(false) => Err(ToolError::not_found(format!("no calendar entry {}", args.entry_id))),
        Err(e) => Err(ToolError::internal(e.to_string())),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkipArgs {
    pub entry_id: i64,
    /// The date this entry does not happen, YYYY-MM-DD.
    pub date: String,
}

pub fn skip(
    conn: &Connection,
    ctx: &ToolCtx,
    args: SkipArgs,
) -> Result<serde_json::Value, ToolError> {
    unscoped(ctx)?;
    calendar::skip(conn, ctx.user_id, args.entry_id, &args.date).map_err(failed)?;
    Ok(serde_json::json!({ "entry_id": args.entry_id, "skipped": args.date }))
}

#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, registry, schemas, PreparedVectors, SessionKind, ToolCtx};
    use rusqlite::Connection;

    fn env() -> (Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("defaults")).unwrap();
        std::fs::write(
            tmp.path().join("defaults/user.toml"),
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n",
        )
        .unwrap();
        (conn, tmp)
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir) -> ToolCtx<'a> {
        ToolCtx {
            config_dir: tmp.path(), data_dir: tmp.path(), user_id: 1, username: "aki",
            vectors: PreparedVectors::default(), task_scope: None, inbox_source: None,
        }
    }

    const SCHOOL: &str = r#"{"title":"school","kind":"fixed","start_time":"08:15",
        "end_time":"15:30","days":["mon","tue","wed","thu","fri"]}"#;

    #[test]
    fn a_commitment_goes_in_by_day_name_and_comes_back_by_day_name() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "calendar_add", SCHOOL).unwrap();
        assert_eq!(out["days"], serde_json::json!(["mon", "tue", "wed", "thu", "fri"]));
        assert_eq!(out["quiet"], true, "a fixed commitment is quiet unless it is told otherwise");

        let mask: i64 =
            conn.query_row("SELECT days FROM calendar_entries", [], |r| r.get(0)).unwrap();
        assert_eq!(mask, 31);

        let id = out["entry_id"].as_i64().unwrap();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "calendar_update",
            &format!(r#"{{"entry_id":{id},"days":["sat","sun"],"quiet":false}}"#)).unwrap();
        assert_eq!(out["days"], serde_json::json!(["sat", "sun"]));
        assert_eq!(out["quiet"], false);
    }

    #[test]
    fn a_note_is_added_but_never_quiet() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "calendar_add",
            r#"{"title":"bin day","kind":"note","quiet":true,"start_time":"07:00",
                "end_time":"08:00","days":["wed"]}"#).unwrap();
        assert_eq!(out["quiet"], false);
    }

    #[test]
    fn listing_walks_the_days_asked_for() {
        let (conn, tmp) = env();
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "calendar_add", SCHOOL).unwrap();
        // 2026-09-18 is a Friday, so the third day of the span is a Sunday
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "calendar_list",
            r#"{"date":"2026-09-18","days":3}"#).unwrap();
        let days = out["days"].as_array().unwrap();
        assert_eq!(days.len(), 3);
        assert_eq!(days[0]["date"], "2026-09-18");
        assert_eq!(days[0]["occurrences"][0]["title"], "school");
        assert_eq!(days[0]["occurrences"][0]["start"], "08:15");
        assert!(days[2]["occurrences"].as_array().unwrap().is_empty());

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "calendar_list", r#"{"days":15}"#)
            .unwrap_err();
        assert_eq!(e.kind, "rejected");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "calendar_list",
            r#"{"date":"18/09/2026"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
    }

    #[test]
    fn listing_without_a_date_reads_today_in_the_users_timezone() {
        let (conn, tmp) = env();
        let out =
            dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "calendar_list", "{}").unwrap();
        let today = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date().to_string();
        assert_eq!(out["days"][0]["date"], today);
    }

    #[test]
    fn skipping_a_date_leaves_the_entry_and_removes_the_day() {
        let (conn, tmp) = env();
        let id = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "calendar_add", SCHOOL).unwrap()
            ["entry_id"].as_i64().unwrap();
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "calendar_skip",
            &format!(r#"{{"entry_id":{id},"date":"2026-09-18"}}"#)).unwrap();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "calendar_list",
            r#"{"date":"2026-09-18"}"#).unwrap();
        assert!(out["days"][0]["occurrences"].as_array().unwrap().is_empty());

        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "calendar_remove",
            &format!(r#"{{"entry_id":{id}}}"#)).unwrap();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "calendar_remove",
            &format!(r#"{{"entry_id":{id}}}"#)).unwrap_err();
        assert_eq!(e.kind, "not_found");
    }

    #[test]
    fn a_malformed_entry_is_rejected_and_writes_nothing() {
        let (conn, tmp) = env();
        for args in [
            r#"{"title":"x","kind":"fixed","start_time":"08:15","end_time":"07:00","days":["mon"]}"#,
            r#"{"title":"x","kind":"party","start_time":"08:15","end_time":"09:00","days":["mon"]}"#,
            r#"{"title":"x","kind":"fixed","start_time":"08:15","end_time":"09:00"}"#,
        ] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "calendar_add", args)
                .unwrap_err();
            assert!(matches!(e.kind, "rejected" | "invalid_args"), "{args} gave {e:?}");
        }
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "calendar_add",
            r#"{"title":"x","kind":"fixed","start_time":"08:15","end_time":"09:00",
                "days":["moonday"]}"#).unwrap_err();
        assert_eq!(e.kind, "invalid_args");
        let n: i64 =
            conn.query_row("SELECT COUNT(*) FROM calendar_entries", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn a_checkin_reads_the_calendar_and_changes_nothing() {
        assert_eq!(
            registry(SessionKind::Checkin).iter().filter(|t| t.starts_with("calendar_")).count(),
            1
        );
        let (conn, tmp) = env();
        dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "calendar_list", "{}").unwrap();
        for name in ["calendar_add", "calendar_update", "calendar_remove", "calendar_skip"] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, name, SCHOOL).unwrap_err();
            assert_eq!(e.kind, "forbidden", "{name} is reachable from a check-in");
        }
    }

    #[test]
    fn no_calendar_tool_reaches_an_import_or_inbox_session() {
        for kind in [SessionKind::Import, SessionKind::Inbox] {
            assert!(!registry(kind).iter().any(|t| t.starts_with("calendar_")), "{kind:?}");
        }
    }

    #[test]
    fn a_scoped_session_is_refused_even_on_a_surface_that_carries_the_tool() {
        let (conn, tmp) = env();
        let scoped = ToolCtx { task_scope: Some(1), ..ctx(&tmp) };
        let e = dispatch(&conn, &scoped, SessionKind::Talk, "calendar_add", SCHOOL).unwrap_err();
        assert_eq!(e.kind, "forbidden");
        let e = dispatch(&conn, &scoped, SessionKind::Talk, "calendar_list", "{}").unwrap_err();
        assert_eq!(e.kind, "forbidden");

        let inbox = ToolCtx { inbox_source: Some("lms:post:1".into()), ..ctx(&tmp) };
        let e = dispatch(&conn, &inbox, SessionKind::Talk, "calendar_add", SCHOOL).unwrap_err();
        assert_eq!(e.kind, "forbidden");
    }

    #[test]
    fn the_add_schema_offers_day_names_and_kinds() {
        let schema = schemas(SessionKind::Talk)
            .into_iter()
            .find(|s| s["name"] == "calendar_add")
            .unwrap();
        let text = schema["input_schema"].to_string();
        for word in ["mon", "sun", "fixed", "busy", "note"] {
            assert!(text.contains(word), "the schema never mentions {word}");
        }
        let description = schema["description"].as_str().unwrap();
        assert!(description.contains("held"), "{description}");
    }
}
