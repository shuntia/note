use super::loose::{self, Envelope, Obj, Ref, Target};
use super::{ToolCtx, ToolError};
use crate::calendar::{self, CalendarError};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::fmt::Write as _;

pub const MAX_DAYS_AHEAD: i64 = 14;

const KIND_SYNONYMS: &[(&str, &str)] = &[
    ("hard", "fixed"),
    ("class", "fixed"),
    ("school", "fixed"),
    ("work", "fixed"),
    ("commitment", "fixed"),
    ("soft", "busy"),
    ("event", "busy"),
    ("appointment", "busy"),
    ("info", "note"),
    ("informational", "note"),
    ("reminder", "note"),
    ("open", "free"),
    ("task_time", "free"),
];

fn internal(e: impl std::fmt::Display) -> ToolError {
    ToolError::internal(e.to_string())
}

/// The calendar belongs to the user, not to one imported task or inbox item, so
/// a scoped session never reaches it.
fn unscoped(ctx: &ToolCtx) -> Result<(), ToolError> {
    if ctx.task_scope.is_some() || ctx.inbox_source.is_some() {
        return Err(ToolError::forbidden("the calendar is out of this session's scope"));
    }
    Ok(())
}

fn today(ctx: &ToolCtx) -> jiff::civil::Date {
    jiff::Timestamp::now().to_zoned(crate::triggers::timezone(ctx.config_dir, ctx.username)).date()
}

/// How an entry recurs, in a few words.
fn when(e: &calendar::Entry) -> String {
    let mut out = match &e.on_date {
        Some(on) => format!("{on} {}-{}", e.start_time, e.end_time),
        None => format!("{} {}-{}", e.day_names.join(","), e.start_time, e.end_time),
    };
    if let Some(from) = &e.from_date {
        let _ = write!(out, " from {from}");
    }
    if let Some(until) = &e.until_date {
        let _ = write!(out, " until {until}");
    }
    out
}

fn row(e: &calendar::Entry) -> Value {
    let mut v = json!({
        "id": e.id,
        "title": e.title,
        "kind": e.kind,
        "start": e.start_time,
        "end": e.end_time,
        "quiet": e.quiet,
    });
    match &e.on_date {
        Some(on) => v["on"] = on.clone().into(),
        None => v["days"] = json!(e.day_names),
    }
    if let Some(from) = &e.from_date {
        v["from"] = from.clone().into();
    }
    if let Some(until) = &e.until_date {
        v["until"] = until.clone().into();
    }
    if !e.exceptions.is_empty() {
        v["skipped"] = json!(e.exceptions);
    }
    if e.external_id.is_some() {
        v["imported"] = true.into();
    }
    v
}

/// `calendar_find`: every entry with its rule, and what lands on each day asked for.
pub fn find(conn: &Connection, ctx: &ToolCtx, raw: &str) -> Result<Value, ToolError> {
    unscoped(ctx)?;
    let mut o = Obj::new(loose::args(raw)?);
    let mut warnings = Vec::new();
    let today = today(ctx);
    let mut start = match o.take(&["date", "day", "from", "on"]).and_then(|v| loose::text(&v)) {
        Some(raw) => loose::day(&raw, today).unwrap_or_else(|| {
            warnings.push(format!("date {raw:?} was ignored: a day is {}", loose::DAY_FORMS));
            today
        }),
        None => today,
    };
    let mut span = o.take(&["days", "span", "count"]).and_then(|v| loose::integer(&v)).unwrap_or(1);
    if !(1..=MAX_DAYS_AHEAD).contains(&span) {
        warnings.push(format!("days is 1 to {MAX_DAYS_AHEAD}; read {}", span.clamp(1, MAX_DAYS_AHEAD)));
        span = span.clamp(1, MAX_DAYS_AHEAD);
    }
    for field in o.rest() {
        warnings.push(format!("{field} was ignored: not a calendar_find field"));
    }
    if let Some(scope) = &ctx.share {
        let (first, end) = scope.horizon(today);
        let last = end.yesterday().map_err(internal)?;
        if start < first || start > last {
            warnings.push(format!("this link shares {first} to {last}"));
            start = first;
        }
        let room = i64::from((end - start).get_days().max(1));
        if span > room {
            warnings.push(format!("this link shares {first} to {last}"));
            span = room;
        }
    }
    let mut days = Vec::new();
    let mut date = start;
    for _ in 0..span {
        let occurrences = calendar::occurrences(conn, ctx.user_id, date).map_err(internal)?;
        days.push(json!({ "date": date.to_string(), "occurrences": occurrences }));
        date = date.tomorrow().map_err(internal)?;
    }
    let mut out = if ctx.share.is_some() {
        json!({ "days": days })
    } else {
        let entries = calendar::list(conn, ctx.user_id).map_err(internal)?;
        json!({ "total": entries.len(), "items": entries.iter().map(row).collect::<Vec<_>>(), "days": days })
    };
    if !warnings.is_empty() {
        out["warnings"] = warnings.into();
    }
    Ok(out)
}

/// One entry of a call, read.
#[derive(Default)]
struct Entry {
    target: Option<Target>,
    title: Option<String>,
    kind: Option<&'static str>,
    start: Option<String>,
    end: Option<String>,
    days: Option<i64>,
    on: Option<String>,
    from: Option<String>,
    until: Option<String>,
    quiet: Option<bool>,
    tz: Option<String>,
    skip: Vec<jiff::civil::Date>,
    unskip: Vec<jiff::civil::Date>,
    new: bool,
}

impl Entry {
    fn changes_fields(&self) -> bool {
        self.title.is_some() || self.changes_rule()
    }

    /// Changes anything besides the title and the skipped days.
    fn changes_rule(&self) -> bool {
        self.kind.is_some()
            || self.start.is_some()
            || self.end.is_some()
            || self.days.is_some()
            || self.on.is_some()
            || self.from.is_some()
            || self.until.is_some()
            || self.quiet.is_some()
    }
}

/// Weekday names, a list or one string: `["tue","thu"]`, `"tue,thu"`,
/// `"Tuesdays and Thursdays"`, `"weekdays"`, `"daily"`.
fn day_mask(v: &Value) -> Result<i64, String> {
    let words: Vec<String> = match v {
        Value::Array(_) | Value::Object(_) => loose::list(v.clone()).iter().filter_map(loose::text).collect(),
        Value::String(s) => vec![s.clone()],
        _ => return Err("days are weekday names such as [\"tue\", \"thu\"]".into()),
    };
    let mut names = Vec::new();
    for word in words {
        let w = word.to_lowercase();
        match w.trim() {
            "weekdays" | "weekday" | "平日" => names.extend(["mon", "tue", "wed", "thu", "fri"]),
            "weekends" | "weekend" | "週末" | "土日" => names.extend(["sat", "sun"]),
            "daily" | "every day" | "everyday" | "all" | "毎日" => names.extend(calendar::DAY_NAMES),
            _ => {
                for part in w.split([',', ' ', '+', '/', '&', '、', '・']).filter(|p| !p.is_empty() && *p != "and" && *p != "every") {
                    let part = part.trim_end_matches('s');
                    match loose::weekday_name(part).or_else(|| loose::weekday_name(&format!("{part}s"))) {
                        Some(n) => names.push(n),
                        None => return Err(format!("{part:?} is not a weekday")),
                    }
                }
            }
        }
    }
    if names.is_empty() {
        return Err("days are weekday names such as [\"tue\", \"thu\"]".into());
    }
    calendar::day_mask(&names).map_err(|e| e.to_string())
}

fn read_entry(obj: &mut Obj, r: &Ref, env: &mut Envelope, today: jiff::civil::Date) -> Entry {
    let mut entry = Entry::default();
    let day_field = |obj: &mut Obj, env: &mut Envelope, field: &str, names: &[&str]| -> Option<String> {
        let v = obj.take(names)?;
        let raw = loose::text(&v).unwrap_or_default();
        if raw.trim().is_empty() {
            return Some(String::new());
        }
        let day = loose::day(&raw, today);
        match day {
            None => env.ignored(Some(r), field, format!("a day is {}", loose::DAY_FORMS)),
            Some(d) if loose::rolled(&raw, today).is_some() => env.adjusted(Some(r), field, d.to_string().into(), loose::rolled_why(&raw, d)),
            Some(_) => {}
        }
        day.map(|d| d.to_string())
    };
    entry.on = day_field(obj, env, "on", &["on", "on_date", "date", "day"]);
    entry.from = day_field(obj, env, "from", &["from", "from_date", "start_date", "starts_on"]);
    entry.until = day_field(obj, env, "until", &["until", "until_date", "end_date", "ends_on", "through"]);
    if let Some(v) = obj.take(&["id", "entry_id", "event_id", "calendar_id"]) {
        entry.target = loose::target(&v);
    }
    entry.title = obj.take(&["title", "name", "summary"]).and_then(|v| loose::text(&v)).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    if let Some(t) = &entry.title {
        if t.chars().count() > calendar::MAX_TITLE_CHARS {
            let cut: String = t.chars().take(calendar::MAX_TITLE_CHARS).collect();
            env.adjusted(Some(r), "title", cut.clone().into(), "a calendar title stops at 80 characters");
            entry.title = Some(cut.trim_end().to_string());
        }
    }
    if let Some(v) = obj.take(&["kind", "type"]) {
        match loose::text(&v).and_then(|s| loose::choice(&s, &calendar::KINDS, KIND_SYNONYMS)) {
            Some(k) => entry.kind = Some(k),
            None => env.ignored(Some(r), "kind", "kind is fixed, busy, note or free"),
        }
    }
    let time = |obj: &mut Obj, env: &mut Envelope, field: &str, names: &[&str]| -> Option<String> {
        let v = obj.take(names)?;
        let raw = loose::text(&v).unwrap_or_default();
        if let Some(t) = loose::wall_time(&raw) {
            return Some(t);
        }
        if let Some((a, b)) = raw.split_once(['-', '–', '~', '〜']).or_else(|| raw.split_once(" to ")) {
            if let (Some(a), Some(b)) = (loose::wall_time(a), loose::wall_time(b)) {
                return Some(format!("{a}-{b}"));
            }
        }
        env.ignored(Some(r), field, "a time is HH:MM, 5pm or 17時");
        None
    };
    let start = time(obj, env, "start", &["start", "start_time", "starts", "begin", "from_time", "time"]);
    let end = time(obj, env, "end", &["end", "end_time", "ends", "until_time", "to"]);
    if let Some((a, b)) = start.as_deref().and_then(|s| s.split_once('-')) {
        entry.start = Some(a.to_string());
        entry.end = end.or_else(|| Some(b.to_string()));
    } else {
        entry.start = start;
        entry.end = end;
    }
    if let Some(v) = obj.take(&["days", "weekdays", "repeat", "every", "on_days"]) {
        match day_mask(&v) {
            Ok(m) => entry.days = Some(m),
            Err(why) => env.ignored(Some(r), "days", why),
        }
    }
    if let Some(v) = obj.take(&["quiet", "do_not_disturb", "dnd", "hold"]) {
        entry.quiet = loose::boolean(&v);
    }
    entry.tz = obj.take(&["tz", "timezone", "zone"]).and_then(|v| loose::text(&v)).filter(|t| !t.trim().is_empty());
    let dates = |obj: &mut Obj, env: &mut Envelope, field: &str, names: &[&str]| -> Vec<jiff::civil::Date> {
        let Some(v) = obj.take(names) else { return Vec::new() };
        let mut out = Vec::new();
        for raw in loose::list(v).iter().filter_map(loose::text) {
            match loose::day(&raw, today) {
                Some(d) => out.push(d),
                None => env.ignored(Some(r), field, format!("{raw:?} is not a day: {}", loose::DAY_FORMS)),
            }
        }
        out
    };
    const SKIP: &[&str] = &["skip", "skip_dates", "skipped", "except", "cancel"];
    const UNSKIP: &[&str] = &["unskip", "restore", "unskip_dates"];
    for (field, names) in [("skip", SKIP), ("unskip", UNSKIP)] {
        let flag = obj.peek(names).filter(|v| v.is_boolean() || v.is_string()).and_then(loose::boolean);
        let days = match flag {
            Some(on) => {
                obj.take(names);
                match (on, entry.on.take().filter(|d| !d.is_empty()).and_then(|d| d.parse().ok())) {
                    (true, Some(d)) => vec![d],
                    (true, None) => {
                        env.ignored(Some(r), field, format!("{field}: true takes its day from date; or list the days in {field}"));
                        Vec::new()
                    }
                    (false, _) => Vec::new(),
                }
            }
            None => dates(obj, env, field, names),
        };
        if field == "skip" {
            entry.skip = days;
        } else {
            entry.unskip = days;
        }
    }
    entry.new = obj.take(&["new", "also"]).and_then(|v| loose::boolean(&v)).unwrap_or(false);
    entry
}

/// An entry already on the calendar under the same title whose time overlaps
/// on a day both reach.
fn same_entry(conn: &Connection, user_id: i64, title: &str, start: &str, end: &str, days: i64, on_date: Option<&str>) -> Result<Option<calendar::Entry>, ToolError> {
    let weekday_bit = |d: &str| -> i64 { d.parse::<jiff::civil::Date>().map_or(0, |d| 1 << (d.weekday().to_monday_zero_offset())) };
    let entries = calendar::list(conn, user_id).map_err(internal)?;
    Ok(entries.into_iter().find(|e| {
        let shared_day = match (days, e.days) {
            (0, 0) => on_date.is_some() && e.on_date.as_deref() == on_date,
            (0, theirs) => on_date.is_some_and(|d| theirs & weekday_bit(d) != 0),
            (ours, 0) => e.on_date.as_deref().is_some_and(|d| ours & weekday_bit(d) != 0),
            (ours, theirs) => ours & theirs != 0,
        };
        shared_day && super::same_title(&e.title, title) && e.start_time.as_str() < end && start < e.end_time.as_str()
    }))
}

fn refusal(e: CalendarError) -> Result<String, ToolError> {
    match e {
        CalendarError::Db(e) => Err(internal(e)),
        other => Ok(other.to_string()),
    }
}

/// `calendar_save`: entries added or changed, and dates skipped or restored.
pub fn save(conn: &Connection, ctx: &ToolCtx, raw: &str) -> Result<Value, ToolError> {
    unscoped(ctx)?;
    let mut o = Obj::new(loose::args(raw)?);
    let mut env = Envelope::default();
    let (entries, warning) = loose::items(
        &mut o,
        "entries",
        &["items", "events", "calendar", "list", "data", "entry"],
        &["title", "id", "entry_id", "start", "start_time", "days", "on", "on_date", "skip"],
    );
    if let Some(w) = warning {
        env.warn(w);
    }
    let rest = o.rest();
    for field in &rest {
        env.ignored(None, field, "not a calendar_save field");
    }
    if entries.is_empty() {
        if rest.is_empty() {
            env.refuse_call(
                r#"nothing was saved: entries was empty. entries is a list, like [{"title":"School","kind":"fixed","days":["mon","tue","wed","thu","fri"],"start":"08:30","end":"15:00"}]"#,
            );
        } else {
            env.refuse_call("nothing was saved: the entries go in entries, a list");
        }
        return Ok(env.into_value());
    }
    let today = today(ctx);
    for (i, v) in entries.into_iter().enumerate() {
        let r = Ref::top(i);
        let Value::Object(map) = v else {
            env.refused(&r, "an entry is an object with a title and times");
            continue;
        };
        let mut eo = Obj::new(map);
        let mut e = read_entry(&mut eo, &r, &mut env, today);
        for field in eo.rest() {
            env.ignored(Some(&r), &field, "not a calendar entry field");
        }
        if e.target.is_none() && !e.changes_rule() && (!e.skip.is_empty() || !e.unskip.is_empty()) {
            e.target = e.title.take().map(Target::Title);
        }
        let target = match e.target.take() {
            Some(Target::Id(id)) => {
                let found = calendar::get(conn, ctx.user_id, id).map_err(internal)?;
                if found.is_none() {
                    env.refused(&r, format!("no calendar entry {id}"));
                    continue;
                }
                found
            }
            Some(Target::Title(t)) => {
                let all: Vec<(i64, String)> = calendar::list(conn, ctx.user_id).map_err(internal)?.into_iter().map(|x| (x.id, x.title)).collect();
                match loose::by_title(&t, &all, "calendar entry") {
                    Ok(id) => {
                        env.adjusted(Some(&r), "id", id.into(), format!("{t:?} is entry {id}"));
                        calendar::get(conn, ctx.user_id, id).map_err(internal)?
                    }
                    Err(why) => {
                        env.refused(&r, why.replace("no open ", "no "));
                        continue;
                    }
                }
            }
            None => None,
        };
        let entry = match target {
            Some(found) => match update(conn, ctx, &r, &mut env, found, &e)? {
                Some(x) => x,
                None => continue,
            },
            None => match create(conn, ctx, &r, &mut env, &e)? {
                Some(x) => x,
                None => continue,
            },
        };
        let before = entry.0.exceptions.clone();
        for d in &e.skip {
            if !occurs(&entry.0, *d) {
                env.ignored(Some(&r), "skip", format!("{} doesn't happen on {d} ({})", entry.0.title, rule(&entry.0)));
                continue;
            }
            if let Err(err) = calendar::skip(conn, ctx.user_id, entry.0.id, &d.to_string()) {
                env.ignored(Some(&r), "skip", refusal(err)?);
            }
        }
        for d in &e.unskip {
            if let Err(err) = calendar::unskip(conn, ctx.user_id, entry.0.id, &d.to_string()) {
                env.ignored(Some(&r), "unskip", refusal(err)?);
            }
        }
        let fresh = calendar::get(conn, ctx.user_id, entry.0.id).map_err(internal)?.unwrap_or(entry.0);
        let skips_changed = fresh.exceptions != before;
        let op = if entry.1 == "unchanged" && skips_changed { "updated" } else { entry.1 };
        let row = env.saved(&r, fresh.id, &fresh.title, op);
        row["when"] = when(&fresh).into();
        if !e.skip.is_empty() || !e.unskip.is_empty() {
            row["skipped"] = json!(fresh.exceptions);
        }
    }
    Ok(env.into_value())
}

fn occurs(e: &calendar::Entry, d: jiff::civil::Date) -> bool {
    let day = d.to_string();
    if e.days == 0 {
        return e.on_date.as_deref() == Some(day.as_str());
    }
    e.days & (1 << d.weekday().to_monday_zero_offset()) != 0
        && e.from_date.as_deref().is_none_or(|f| f <= day.as_str())
        && e.until_date.as_deref().is_none_or(|u| u >= day.as_str())
}

/// `tue, thu`, or a one-off's day.
fn rule(e: &calendar::Entry) -> String {
    match &e.on_date {
        Some(on) if e.days == 0 => on.clone(),
        _ => e.day_names.join(", "),
    }
}

/// A one-off's date and times given in `tz`, restated on the user's own clock.
fn in_zone(ctx: &ToolCtx, tz: &str, on: &str, start: &str, end: &str) -> Result<Result<(String, String, String), String>, ToolError> {
    let Ok(date) = on.parse::<jiff::civil::Date>() else { return Ok(Err(format!("{on:?} is not a day"))) };
    match super::in_user_zone(ctx, Some(tz), date, &[start, end]) {
        Ok((d, walls)) => Ok(Ok((d.to_string(), walls[0].clone(), walls[1].clone()))),
        Err(e) if e.kind == "internal" => Err(e),
        Err(e) => Ok(Err(e.message)),
    }
}

fn create(conn: &Connection, ctx: &ToolCtx, r: &Ref, env: &mut Envelope, entry: &Entry) -> Result<Option<(calendar::Entry, &'static str)>, ToolError> {
    let mut missing = Vec::new();
    if entry.title.is_none() {
        missing.push("title");
    }
    if entry.start.is_none() {
        missing.push("start");
    }
    if entry.end.is_none() {
        missing.push("end");
    }
    if entry.days.is_none() && entry.on.as_deref().is_none_or(str::is_empty) {
        missing.push("days or on");
    }
    if !missing.is_empty() {
        env.refused(r, format!("a new entry needs {}; to change one, give its id", missing.join(", ")));
        return Ok(None);
    }
    let title = entry.title.clone().unwrap_or_default();
    let (mut start, mut end) = (entry.start.clone().unwrap_or_default(), entry.end.clone().unwrap_or_default());
    let mut on = entry.on.clone().filter(|o| !o.is_empty());
    let days = entry.days.unwrap_or(0);
    if days > 0 && on.is_some() {
        env.ignored(Some(r), "on", "days makes it repeat; on is for a one-off");
        on = None;
    }
    if let Some(tz) = &entry.tz {
        match &on {
            Some(day) => match in_zone(ctx, tz, day, &start, &end)? {
                Ok((d, s, x)) => {
                    on = Some(d);
                    start = s;
                    end = x;
                }
                Err(why) => {
                    env.refused(r, why);
                    return Ok(None);
                }
            },
            None => env.ignored(Some(r), "tz", "a repeating entry keeps the user's own clock"),
        }
    }
    if !entry.new {
        if let Some(found) = same_entry(conn, ctx.user_id, &title, &start, &end, days, on.as_deref())? {
            if found.days > 0 && days == 0 {
                env.adjusted(Some(r), "id", found.id.into(), format!("{:?} already happens then ({}), so nothing was added; new: true adds another", found.title, rule(&found)));
                return Ok(Some((found, "unchanged")));
            }
            env.adjusted(Some(r), "id", found.id.into(), format!("entry {} {:?} already covers that time, so it was updated; new: true adds another", found.id, found.title));
            let as_update = Entry { title: None, on, start: Some(start), end: Some(end), tz: None, ..clone_entry(entry) };
            return update(conn, ctx, r, env, found, &as_update);
        }
    }
    if let (Some(day), true) = (on.as_deref(), days == 0) {
        if let Ok(date) = day.parse() {
            if super::has_gone_by(ctx, date, &end) {
                env.refused(r, format!("{day} {end} has gone by; put a one-off on a day still ahead"));
                return Ok(None);
            }
        }
    }
    let (from, until) = if days > 0 {
        (entry.from.clone().filter(|x| !x.is_empty()), entry.until.clone().filter(|x| !x.is_empty()))
    } else {
        if entry.from.is_some() || entry.until.is_some() {
            env.ignored(Some(r), "until", "from and until bound a repeating entry");
        }
        (None, None)
    };
    let fields = calendar::Fields {
        title,
        kind: entry.kind.unwrap_or("busy").into(),
        quiet: entry.quiet,
        start_time: start,
        end_time: end,
        days: Some(days),
        on_date: on,
        from_date: from,
        until_date: until,
        external_id: None,
    };
    match calendar::create(conn, ctx.user_id, fields) {
        Ok(made) => Ok(Some((made, "created"))),
        Err(err) => {
            env.refused(r, refusal(err)?);
            Ok(None)
        }
    }
}

fn clone_entry(e: &Entry) -> Entry {
    Entry {
        target: e.target.clone(),
        title: e.title.clone(),
        kind: e.kind,
        start: e.start.clone(),
        end: e.end.clone(),
        days: e.days,
        on: e.on.clone(),
        from: e.from.clone(),
        until: e.until.clone(),
        quiet: e.quiet,
        tz: e.tz.clone(),
        skip: e.skip.clone(),
        unskip: e.unskip.clone(),
        new: e.new,
    }
}

fn update(conn: &Connection, ctx: &ToolCtx, r: &Ref, env: &mut Envelope, found: calendar::Entry, entry: &Entry) -> Result<Option<(calendar::Entry, &'static str)>, ToolError> {
    if !entry.changes_fields() {
        return Ok(Some((found, "unchanged")));
    }
    if found.external_id.is_some() {
        env.refused(r, format!("entry {} is imported from another calendar, which rewrites it; skip its dates instead", found.id));
        return Ok(None);
    }
    if found.days > 0 && entry.days.is_none() && entry.on.as_deref().is_some_and(|o| !o.is_empty()) {
        env.refused(r, format!("{} repeats ({}); on makes a one-off; to skip a date use skip", found.title, rule(&found)));
        return Ok(None);
    }
    let (mut start, mut end, mut on) = (entry.start.clone(), entry.end.clone(), entry.on.clone());
    if let Some(tz) = &entry.tz {
        let repeating = entry.days.is_some_and(|d| d > 0) || (entry.on.is_none() && found.days > 0);
        if repeating {
            env.ignored(Some(r), "tz", "a repeating entry keeps the user's own clock");
        } else {
            let day = on.clone().or(found.on_date.clone()).unwrap_or_default();
            let s = start.clone().unwrap_or(found.start_time.clone());
            let x = end.clone().unwrap_or(found.end_time.clone());
            match in_zone(ctx, tz, &day, &s, &x)? {
                Ok((d, s, x)) => {
                    on = Some(d);
                    start = Some(s);
                    end = Some(x);
                }
                Err(why) => {
                    env.refused(r, why);
                    return Ok(None);
                }
            }
        }
    }
    let patch = calendar::Patch {
        title: entry.title.clone(),
        kind: entry.kind.map(str::to_string),
        quiet: entry.quiet,
        start_time: start,
        end_time: end,
        days: entry.days,
        on_date: on,
        from_date: entry.from.clone(),
        until_date: entry.until.clone(),
        external_id: None,
    };
    match calendar::update(conn, ctx.user_id, found.id, patch) {
        Ok(entry) => Ok(Some((entry, "updated"))),
        Err(err) => {
            env.refused(r, refusal(err)?);
            Ok(None)
        }
    }
}

/// `calendar_remove`: entries gone for good.
pub fn remove(conn: &Connection, ctx: &ToolCtx, raw: &str) -> Result<Value, ToolError> {
    unscoped(ctx)?;
    let mut o = Obj::new(loose::args(raw)?);
    let mut env = Envelope::default();
    let ids = loose::ids(&mut o, &["ids", "id", "entry_id", "entry_ids", "event_id", "entries"], &mut env);
    for field in o.rest() {
        env.ignored(None, &field, "not a calendar_remove field");
    }
    if ids.is_empty() {
        loose::nothing_removed(&mut env);
        return Ok(env.into_value());
    }
    let all: Vec<(i64, String)> = calendar::list(conn, ctx.user_id).map_err(internal)?.into_iter().map(|x| (x.id, x.title)).collect();
    for (i, v) in ids.into_iter().enumerate() {
        let r = Ref::top(i);
        let id = match loose::target(&v) {
            Some(Target::Id(id)) => id,
            Some(Target::Title(t)) => match loose::by_title(&t, &all, "calendar entry") {
                Ok(id) => id,
                Err(why) => {
                    env.refused(&r, why.replace("no open ", "no "));
                    continue;
                }
            },
            None => {
                env.refused(&r, "an entry is named by its id");
                continue;
            }
        };
        let Some(found) = calendar::get(conn, ctx.user_id, id).map_err(internal)? else {
            env.refused(&r, format!("no calendar entry {id}"));
            continue;
        };
        if found.external_id.is_some() {
            env.refused(&r, format!("entry {id} is imported from another calendar, which would bring it back; skip its dates instead"));
            continue;
        }
        if calendar::delete(conn, ctx.user_id, id).map_err(internal)? {
            env.removed(id, &found.title);
        }
    }
    Ok(env.into_value())
}

#[cfg(test)]
mod tests {
    use crate::tools::testkit::{call, ctx, env, saved, talk};
    use crate::tools::{dispatch, registry, schemas, SessionKind, ToolCtx};
    use serde_json::json;

    const SCHOOL: &str = r#"{"entries":[{"title":"school","kind":"fixed","start":"08:15","end":"15:30","days":["mon","tue","wed","thu","fri"]}]}"#;

    fn entry(conn: &rusqlite::Connection, id: i64) -> crate::calendar::Entry {
        crate::calendar::get(conn, 1, id).unwrap().unwrap()
    }

    /// Pins the user to a zone whose clock reads midday, so an hour either side
    /// of now stays on today's date.
    fn pin_to_midday(tmp: &tempfile::TempDir) -> jiff::civil::Date {
        let zone = crate::triggers::midday_zone();
        std::fs::write(
            tmp.path().join("defaults/user.toml"),
            format!("display_name = \"X\"\ntimezone = \"{}\"\ntemplate = \"default\"\n", zone.iana_name().unwrap()),
        )
        .unwrap();
        jiff::Timestamp::now().to_zoned(zone).date()
    }

    #[test]
    fn a_commitment_goes_in_by_day_name_and_reads_back_with_its_rule() {
        let (conn, tmp) = env("UTC");
        let out = call(&conn, &tmp, SessionKind::Talk, "calendar_save", SCHOOL).unwrap();
        let id = saved(&out, "0");
        assert_eq!(out["saved"][0]["when"], "mon,tue,wed,thu,fri 08:15-15:30");
        assert!(entry(&conn, id).quiet, "a fixed commitment is quiet unless told otherwise");
        assert_eq!(entry(&conn, id).days, 31);
        talk(&conn, &tmp, "calendar_save", json!({"entries": [{"id": id, "days": "weekends", "quiet": false}]}));
        assert_eq!((entry(&conn, id).day_names, entry(&conn, id).quiet), (vec!["sat", "sun"], false));
        let out = talk(&conn, &tmp, "calendar_find", json!({"date": "2026-09-18", "days": 3}));
        assert_eq!(out["items"][0]["days"], json!(["sat", "sun"]));
        assert!(out["days"][0]["occurrences"].as_array().unwrap().is_empty(), "2026-09-18 is a Friday");
        assert_eq!(out["days"][1]["occurrences"][0]["title"], "school");
        let out = talk(&conn, &tmp, "calendar_find", json!({"days": 30}));
        assert_eq!(out["days"].as_array().unwrap().len(), 14);
        assert!(out["warnings"][0].as_str().unwrap().contains("1 to 14"));
    }

    #[test]
    fn swim_practice_twice_a_week_until_a_date_is_one_quiet_entry() {
        let (conn, tmp) = env("America/Los_Angeles");
        let out = talk(&conn, &tmp, "calendar_save", json!({"entries": [{"title": "Swim practice", "kind": "busy",
            "start": "5pm", "end": "6:30pm", "days": "Tuesdays and Thursdays", "until": "December 12", "quiet": true}]}));
        assert_eq!(out["ok"], true, "{out}");
        let e = entry(&conn, saved(&out, "0"));
        assert_eq!((e.start_time.as_str(), e.end_time.as_str()), ("17:00", "18:30"));
        assert_eq!(e.day_names, vec!["tue", "thu"]);
        assert!(e.until_date.as_deref().unwrap().ends_with("-12-12"));
        assert!(e.quiet);
        let out = talk(&conn, &tmp, "calendar_save", json!({"entries": [{"title": "swim  practice", "start": "17:30", "end": "19:00", "days": ["tue"]}]}));
        assert_eq!(out["saved"][0]["id"], e.id, "the same commitment again is updated, not doubled");
        assert!(out["adjusted"][0]["why"].as_str().unwrap().contains("already covers"));
        let out = talk(&conn, &tmp, "calendar_save", json!({"entries": [{"title": "swim practice", "start": "17:30", "end": "19:00", "days": ["tue"], "new": true}]}));
        assert_ne!(saved(&out, "0"), e.id);
    }

    #[test]
    fn a_time_range_in_one_field_is_split_and_bad_fields_are_ignored() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "calendar_save", json!({"title": "piano", "start": "16:00-17:00", "days": "wed", "kind": "lesson", "colour": "red"}));
        let e = entry(&conn, saved(&out, "0"));
        assert_eq!((e.start_time.as_str(), e.end_time.as_str(), e.kind.as_str()), ("16:00", "17:00", "busy"));
        let ignored: Vec<&str> = out["ignored"].as_array().unwrap().iter().map(|i| i["field"].as_str().unwrap()).collect();
        assert_eq!(ignored, ["kind", "colour"]);
        let out = talk(&conn, &tmp, "calendar_save", json!({"entries": [{"title": "x", "start": "09:00"}, {"title": "y", "start": "10:00", "end": "09:00", "days": ["mon"]}]}));
        assert_eq!(out["ok"], false);
        assert!(out["refused"][0]["why"].as_str().unwrap().contains("needs end, days or on"));
        assert!(out["refused"][1]["why"].as_str().unwrap().contains("end after it starts"));
        assert_eq!(talk(&conn, &tmp, "calendar_save", json!({"entries": []}))["ok"], false, "an empty save is refused");
    }

    #[test]
    fn skip_and_unskip_take_dates_off_and_back() {
        let (conn, tmp) = env("UTC");
        let id = saved(&call(&conn, &tmp, SessionKind::Talk, "calendar_save", SCHOOL).unwrap(), "0");
        let out = talk(&conn, &tmp, "calendar_save", json!({"entries": [{"id": id, "skip": ["2099-09-18", "2099-09-21"]}]}));
        assert_eq!(out["saved"][0]["op"], "updated");
        assert_eq!(entry(&conn, id).exceptions, ["2099-09-18", "2099-09-21"]);
        assert_eq!(out["saved"][0]["skipped"], json!(["2099-09-18", "2099-09-21"]));
        let again = talk(&conn, &tmp, "calendar_save", json!({"entries": [{"id": id, "skip": "2099-09-21"}]}));
        assert_eq!(again["saved"][0]["op"], "unchanged", "{again}");
        talk(&conn, &tmp, "calendar_save", json!({"entries": [{"id": "School", "unskip": ["2099-09-18"]}]}));
        assert_eq!(entry(&conn, id).exceptions, ["2099-09-21"]);
        let out = talk(&conn, &tmp, "calendar_save", json!({"entries": [{"entry_id": id, "skip": "2099-09-22"}]}));
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(entry(&conn, id).exceptions, ["2099-09-21", "2099-09-22"]);
    }

    #[test]
    fn a_repeating_entry_stays_repeating_and_skips_only_its_own_days() {
        let (conn, tmp) = env("UTC");
        let gym = saved(&talk(&conn, &tmp, "calendar_save", json!({"entries": [{"title": "Gym", "start": "18:00", "end": "19:00", "days": ["tue", "thu"]}]})), "0");
        let out = talk(&conn, &tmp, "calendar_save", json!({"entries": [{"id": gym, "on": "2099-01-08"}]}));
        assert_eq!(out["ok"], false);
        assert!(out["refused"][0]["why"].as_str().unwrap().contains("to skip a date use skip"), "{out}");
        assert_eq!(entry(&conn, gym).days, 2 | 8);
        let out = talk(&conn, &tmp, "calendar_save", json!({"entries": [{"id": gym, "date": "2099-01-08", "skip": true}]}));
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!((entry(&conn, gym).days, entry(&conn, gym).exceptions.clone()), (2 | 8, vec!["2099-01-08".to_string()]));
        let out = talk(&conn, &tmp, "calendar_save", json!({"entries": [{"title": "gym", "skip": ["2099-01-13"]}]}));
        assert_eq!(saved(&out, "0"), gym, "{out}");
        assert_eq!(entry(&conn, gym).exceptions.len(), 2);
        let out = talk(&conn, &tmp, "calendar_save", json!({"entries": [{"id": gym, "skip": ["2099-01-07"]}]}));
        assert_eq!(out["ignored"][0]["why"], "Gym doesn't happen on 2099-01-07 (tue, thu)");
        assert_eq!(out["saved"][0]["op"], "unchanged");
        let out = talk(&conn, &tmp, "calendar_save", json!({"entries": [{"title": "Gym", "on": "2099-01-15", "start": "18:00", "end": "19:00"}]}));
        assert_eq!(saved(&out, "0"), gym, "a one-off inside a repeating entry is already there");
        assert_eq!(entry(&conn, gym).days, 2 | 8);
    }

    #[test]
    fn days_wrapped_in_item_still_name_the_weekdays() {
        let (conn, tmp) = env("UTC");
        let out = talk(&conn, &tmp, "calendar_save", json!({"entries": {"item": [{"title": "Pills", "kind": "note", "start": "08:00", "end": "08:05", "days": {"item": ["mon", "wed"]}}]}}));
        assert_eq!(entry(&conn, saved(&out, "0")).days, 1 | 4, "{out}");
    }

    #[test]
    fn an_imported_entry_is_skipped_never_rewritten_or_removed() {
        let (conn, tmp) = env("UTC");
        let fields = crate::calendar::Fields {
            title: "Chemistry".into(), kind: "fixed".into(), start_time: "09:00".into(), end_time: "10:00".into(),
            days: Some(1), ..Default::default()
        };
        let (e, _) = crate::calendar::upsert_by_external(&conn, 1, "ics:1", fields).unwrap();
        let out = talk(&conn, &tmp, "calendar_save", json!({"entries": [{"id": e.id, "title": "Chem"}]}));
        assert!(out["refused"][0]["why"].as_str().unwrap().contains("imported"));
        let out = talk(&conn, &tmp, "calendar_save", json!({"entries": [{"id": e.id, "skip": ["2099-01-05"]}]}));
        assert_eq!(out["ok"], true);
        let out = talk(&conn, &tmp, "calendar_remove", json!({"ids": [e.id]}));
        assert_eq!(out["ok"], false);
        assert_eq!(entry(&conn, e.id).title, "Chemistry");
    }

    #[test]
    fn a_one_off_that_has_ended_is_refused_and_another_zone_lands_on_the_users_clock() {
        let (conn, tmp) = env("UTC");
        let today = pin_to_midday(&tmp);
        let out = talk(&conn, &tmp, "calendar_save", json!({"entries": [
            {"title": "dentist", "start": "10:00", "end": "11:00", "on": today.to_string()},
            {"title": "dentist", "start": "13:00", "end": "14:00", "on": "today"}
        ]}));
        assert!(out["refused"][0]["why"].as_str().unwrap().contains("gone by"));
        assert_eq!(out["saved"][0]["ref"], "1");
        std::fs::write(tmp.path().join("defaults/user.toml"), "display_name = \"X\"\ntimezone = \"Europe/London\"\ntemplate = \"default\"\n").unwrap();
        let out = talk(&conn, &tmp, "calendar_save", json!({"entries": [{"title": "call", "start": "14:00", "end": "15:00", "on": "2099-07-02", "tz": "Asia/Tokyo"},
            {"title": "gym", "start": "14:00", "end": "15:00", "days": ["mon"], "tz": "Asia/Tokyo"}]}));
        let e = entry(&conn, saved(&out, "0"));
        assert_eq!((e.on_date.as_deref(), e.start_time.as_str()), (Some("2099-07-02"), "06:00"));
        assert_eq!(entry(&conn, saved(&out, "1")).start_time, "14:00", "a repeating entry keeps the user's clock");
        assert!(out["ignored"].as_array().unwrap().iter().any(|i| i["ref"] == "1" && i["field"] == "tz"));
    }

    #[test]
    fn a_remove_takes_ids_or_titles() {
        let (conn, tmp) = env("UTC");
        let id = saved(&call(&conn, &tmp, SessionKind::Talk, "calendar_save", SCHOOL).unwrap(), "0");
        let out = talk(&conn, &tmp, "calendar_remove", json!({"entry_id": id}));
        assert_eq!(out["removed"][0]["id"], id);
        let out = talk(&conn, &tmp, "calendar_remove", json!({"ids": [id, "nothing"]}));
        assert_eq!(out["refused"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_wake_up_reads_the_calendar_and_changes_nothing() {
        assert_eq!(registry(SessionKind::Trigger).iter().filter(|t| t.starts_with("calendar_")).count(), 1);
        let (conn, tmp) = env("UTC");
        call(&conn, &tmp, SessionKind::Trigger, "calendar_find", "{}").unwrap();
        for name in ["calendar_save", "calendar_remove"] {
            assert_eq!(call(&conn, &tmp, SessionKind::Trigger, name, SCHOOL).unwrap_err().kind, "forbidden", "{name}");
        }
        for kind in [SessionKind::Import, SessionKind::Inbox] {
            assert!(!registry(kind).iter().any(|t| t.starts_with("calendar_")), "{kind:?}");
        }
        let scoped = ToolCtx { task_scope: Some(1), ..ctx(&tmp) };
        assert_eq!(dispatch(&conn, &scoped, SessionKind::Talk, "calendar_save", SCHOOL).unwrap_err().kind, "forbidden");
    }

    #[test]
    fn a_share_reads_only_its_horizon_and_no_rules() {
        let (conn, tmp) = env("UTC");
        call(&conn, &tmp, SessionKind::Talk, "calendar_save", SCHOOL).unwrap();
        let sctx = ToolCtx { share: Some(crate::shares::ShareScope::default()), ..ctx(&tmp) };
        let out = dispatch(&conn, &sctx, SessionKind::Share, "calendar_find", r#"{"days":14}"#).unwrap();
        assert_eq!(out["days"].as_array().unwrap().len(), crate::shares::ShareScope::default().horizon_days as usize);
        assert!(out.get("items").is_none());
        assert!(out["warnings"].is_array());
    }

    #[test]
    fn the_save_schema_offers_day_names_kinds_and_skip() {
        let schema = schemas(SessionKind::Talk).iter().find(|s| s["name"] == "calendar_save").unwrap();
        let text = schema["input_schema"].to_string();
        for word in ["mon", "sun", "fixed", "busy", "note", "free", "skip", "unskip"] {
            assert!(text.contains(word), "the schema never mentions {word}");
        }
        let quiet = &schema["input_schema"]["properties"]["entries"]["items"]["properties"]["quiet"]["description"];
        assert!(quiet.as_str().unwrap().contains("Hold deliveries"));
    }
}
