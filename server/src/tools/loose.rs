//! The one forgiving reader every `_find`, `_save` and `_remove` tool parses
//! its arguments with, and the result envelope every write answers in.

use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::Timestamp;
use serde_json::{Map, Value};

pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_TEXT_BYTES: usize = super::MAX_TEXT_BYTES;
pub const MIN_MINUTES: i64 = 5;
pub const MAX_MINUTES: i64 = 24 * 60;

/// An item's index path in the call: `"2"` is the third top item, `"2.0.1"` a
/// nested child.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ref(pub String);

impl Ref {
    #[must_use]
    pub fn top(i: usize) -> Self {
        Self(i.to_string())
    }
    #[must_use]
    pub fn child(&self, i: usize) -> Self {
        Self(format!("{}.{i}", self.0))
    }
}

/// What a write did, item by item; empty lists are left out of the answer.
#[derive(Default, Debug)]
pub struct Envelope {
    pub saved: Vec<Value>,
    pub removed: Vec<Value>,
    pub adjusted: Vec<Value>,
    pub ignored: Vec<Value>,
    pub refused: Vec<Value>,
    pub warnings: Vec<String>,
}

impl Envelope {
    pub fn saved(&mut self, r: &Ref, id: i64, title: &str, op: &str) -> &mut Value {
        self.saved.push(serde_json::json!({ "ref": r.0, "id": id, "title": title, "op": op }));
        self.saved.last_mut().expect("just pushed")
    }
    pub fn removed(&mut self, id: i64, title: &str) {
        self.removed.push(serde_json::json!({ "id": id, "title": title }));
    }
    pub fn adjusted(&mut self, r: Option<&Ref>, field: &str, to: Value, why: impl Into<String>) {
        let mut v = serde_json::json!({ "field": field, "why": why.into() });
        v["to"] = to;
        if let Some(r) = r {
            v["ref"] = r.0.clone().into();
        }
        self.adjusted.push(v);
    }
    pub fn ignored(&mut self, r: Option<&Ref>, field: &str, why: impl Into<String>) {
        let mut v = serde_json::json!({ "field": field, "why": why.into() });
        if let Some(r) = r {
            v["ref"] = r.0.clone().into();
        }
        self.ignored.push(v);
    }
    pub fn refused(&mut self, r: &Ref, why: impl Into<String>) {
        self.refused.push(serde_json::json!({ "ref": r.0, "why": why.into() }));
    }
    /// A refusal of the call as a whole, not of one item.
    pub fn refuse_call(&mut self, why: impl Into<String>) {
        self.refused.push(serde_json::json!({ "why": why.into() }));
    }
    pub fn warn(&mut self, w: impl Into<String>) {
        self.warnings.push(w.into());
    }

    pub fn into_value(self) -> Value {
        let mut out = Map::new();
        out.insert("ok".into(), self.refused.is_empty().into());
        for (key, list) in [
            ("saved", self.saved),
            ("removed", self.removed),
            ("adjusted", self.adjusted),
            ("ignored", self.ignored),
            ("refused", self.refused),
        ] {
            if !list.is_empty() {
                out.insert(key.into(), Value::Array(list));
            }
        }
        if !self.warnings.is_empty() {
            out.insert("warnings".into(), self.warnings.into());
        }
        Value::Object(out)
    }
}

/// Adds `warning` to a tool's answer.
pub fn with_warning(mut v: Value, warning: String) -> Value {
    match &mut v {
        Value::Object(o) => match o.get_mut("warnings") {
            Some(Value::Array(ws)) => ws.push(warning.into()),
            _ => {
                o.insert("warnings".into(), vec![warning].into());
            }
        },
        _ => v = serde_json::json!({ "result": v, "warnings": [warning] }),
    }
    v
}

/// The call's arguments as one object; a JSON string holding the object is read
/// through, and no arguments at all read as `{}`.
pub fn args(raw: &str) -> Result<Map<String, Value>, super::ToolError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(Map::new());
    }
    let v: Value = serde_json::from_str(raw).map_err(|e| super::ToolError::invalid_args(e.to_string()))?;
    match v {
        Value::Object(o) => Ok(o),
        Value::Null => Ok(Map::new()),
        Value::String(s) => match serde_json::from_str(&s) {
            Ok(Value::Object(o)) => Ok(o),
            _ => Err(super::ToolError::invalid_args("arguments must be a JSON object")),
        },
        Value::Array(items) => {
            let mut o = Map::new();
            o.insert("items".into(), Value::Array(items));
            Ok(o)
        }
        _ => Err(super::ToolError::invalid_args("arguments must be a JSON object")),
    }
}

/// An object whose fields are read by their canonical name or an alias; what
/// is never read is reported back as unknown.
pub struct Obj {
    map: Map<String, Value>,
}

impl Obj {
    pub fn new(map: Map<String, Value>) -> Self {
        Self { map }
    }

    /// The first of `names` given with a value other than null; every one of
    /// them is consumed.
    pub fn take(&mut self, names: &[&str]) -> Option<Value> {
        let mut found = None;
        for name in names {
            if let Some(v) = self.map.remove(*name) {
                if found.is_none() && !v.is_null() {
                    found = Some(v);
                }
            }
        }
        found
    }

    /// The first of `names` given with a value other than null, left in place.
    pub fn peek(&self, names: &[&str]) -> Option<&Value> {
        names.iter().find_map(|n| self.map.get(*n).filter(|v| !v.is_null()))
    }

    pub fn has(&self, names: &[&str]) -> bool {
        names.iter().any(|n| self.map.get(*n).is_some_and(|v| !v.is_null()))
    }

    pub fn is_empty(&self) -> bool {
        self.map.values().all(Value::is_null)
    }

    pub fn into_map(self) -> Map<String, Value> {
        self.map
    }

    /// The fields left unread, nulls aside.
    pub fn rest(self) -> Vec<String> {
        self.map.into_iter().filter(|(_, v)| !v.is_null()).map(|(k, _)| k).collect()
    }
}

/// The list a call carries under `key`, under one of `aliases`, or under any
/// key at all when it is the one list or object left; a bare object is a list
/// of itself when it carries one of `item_fields`.
pub fn items(o: &mut Obj, key: &str, aliases: &[&str], item_fields: &[&str]) -> (Vec<Value>, Option<String>) {
    if let Some(v) = o.take(&[key]) {
        return (list(v), None);
    }
    if o.has(item_fields) {
        let whole = std::mem::take(&mut o.map);
        return (vec![Value::Object(whole)], None);
    }
    for alias in aliases {
        if let Some(v) = o.take(&[alias]) {
            return (list(v), Some(format!("read {alias} as {key}")));
        }
    }
    match only_list(o) {
        Some((field, v)) => (list(v), Some(format!("read {field} as {key}"))),
        None => (Vec::new(), None),
    }
}

/// The one field left holding a list or an object, taken out.
pub fn only_list(o: &mut Obj) -> Option<(String, Value)> {
    let mut lists = o.map.iter().filter(|(_, v)| v.is_array() || v.is_object()).map(|(k, _)| k.clone());
    let field = lists.next()?;
    if lists.next().is_some() {
        return None;
    }
    let v = o.map.remove(&field)?;
    Some((field, v))
}

/// The ids a remove names under one of `names`, or in the one list left.
pub fn ids(o: &mut Obj, names: &[&str], env: &mut Envelope) -> Vec<Value> {
    if let Some(v) = o.take(names) {
        return list(v);
    }
    match only_list(o) {
        Some((field, v)) => {
            env.warn(format!("read {field} as {}", names[0]));
            list(v)
        }
        None => Vec::new(),
    }
}

/// A remove that named nothing: no error unless fields came that were not read.
pub fn nothing_removed(env: &mut Envelope) {
    if env.ignored.is_empty() {
        env.warn("nothing to remove: ids was empty");
    } else {
        env.refuse_call("nothing was removed: the ids go in ids, a list");
    }
}

/// Why a yearless day was read as next year's.
pub fn rolled_why(raw: &str, d: Date) -> String {
    format!("{} has gone by this year, so it reads as {}; give the year to mean another", raw.trim(), d.year())
}

/// Keys a model wraps a list in, as in `{"item": [...]}`.
const WRAPPERS: &[&str] = &["item", "items", "element", "elements", "list", "array", "value", "values"];

pub fn list(v: Value) -> Vec<Value> {
    let v = unwrapped(v);
    match v {
        Value::Array(items) => items.into_iter().filter(|v| !v.is_null()).collect(),
        Value::Null => Vec::new(),
        Value::String(s) if s.trim_start().starts_with('[') => match serde_json::from_str(&s) {
            Ok(Value::Array(items)) => items,
            _ => vec![Value::String(s)],
        },
        other => vec![other],
    }
}

/// A lone `{"item": X}` read as `X`.
fn unwrapped(v: Value) -> Value {
    match v {
        Value::Object(o) if o.len() == 1 && WRAPPERS.contains(&o.keys().next().map_or("", String::as_str)) => {
            unwrapped(o.into_iter().next().expect("one entry").1)
        }
        other => other,
    }
}

pub fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

pub fn boolean(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_i64().map(|n| n != 0),
        Value::String(s) => match s.trim().to_lowercase().as_str() {
            "true" | "yes" | "y" | "1" | "on" => Some(true),
            "false" | "no" | "n" | "0" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

pub fn integer(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64)),
        Value::String(s) => s.trim().trim_end_matches('%').trim().parse().ok(),
        _ => None,
    }
}

/// `41`, `"41"`, `"#41"`, `"t41"` and `"t#41"` all name 41.
pub fn id(v: &Value) -> Option<i64> {
    match v {
        Value::Number(_) => integer(v).filter(|n| *n > 0),
        Value::String(s) => {
            let s = s.trim().trim_start_matches('#');
            let s = s.strip_prefix(|c: char| c.is_ascii_alphabetic()).unwrap_or(s);
            let s = s.trim_start_matches(['#', '-', ' ']);
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten().filter(|n| *n > 0)
        }
        _ => None,
    }
}

/// A target named by id, or by a title to be matched.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Id(i64),
    Title(String),
}

pub fn target(v: &Value) -> Option<Target> {
    if let Some(n) = id(v) {
        return Some(Target::Id(n));
    }
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(Target::Title(s.trim().to_string())),
        Value::Object(o) => o.get("id").and_then(target),
        _ => None,
    }
}

/// Case and spacing folded away, which is how a title names a thing.
pub fn fold(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The one of `candidates` whose title folds to `title`, or why there is none.
pub fn by_title(title: &str, candidates: &[(i64, String)], what: &str) -> Result<i64, String> {
    by_title_listed(title, candidates, what, |ids| ids.iter().map(i64::to_string).collect::<Vec<_>>().join(", "))
}

/// `by_title`, with the candidates of an ambiguous title written by `list`.
pub fn by_title_listed(title: &str, candidates: &[(i64, String)], what: &str, list: impl Fn(&[i64]) -> String) -> Result<i64, String> {
    let want = fold(title);
    let hits: Vec<i64> = candidates.iter().filter(|(_, t)| fold(t) == want).map(|(id, _)| *id).collect();
    match hits.as_slice() {
        [id] => Ok(*id),
        [] => Err(format!("no open {what} is titled {title:?}; give its id")),
        many => Err(format!("{} open {what}s are titled {title:?}: {}; give the id", many.len(), list(many))),
    }
}

/// Up to `n` of `candidates` nearest `title`: those sharing a word first, then
/// by edit distance.
pub fn nearest(title: &str, candidates: &[(i64, String)], n: usize) -> Vec<(i64, String)> {
    let want = fold(title);
    let words: Vec<&str> = want.split(' ').filter(|w| w.len() > 2).collect();
    let mut ranked: Vec<(usize, usize, &(i64, String))> = candidates
        .iter()
        .map(|c| {
            let have = fold(&c.1);
            let shared = words.iter().filter(|w| have.contains(**w)).count();
            (usize::MAX - shared, super::distance(&want, &have), c)
        })
        .collect();
    ranked.sort_by_key(|(shared, dist, c)| (*shared, *dist, c.0));
    ranked.into_iter().take(n).map(|(.., c)| c.clone()).collect()
}

/// A title past `MAX_TITLE_CHARS` or the task's byte cap, cut at a word
/// boundary; `None` when it fits.
pub fn cut_title(title: &str) -> Option<String> {
    let by_chars = (title.chars().count() > MAX_TITLE_CHARS).then(|| {
        let hard: String = title.chars().take(MAX_TITLE_CHARS).collect();
        match hard.rfind(char::is_whitespace) {
            Some(at) if at > MAX_TITLE_CHARS / 2 => hard[..at].trim_end().to_string(),
            _ => hard,
        }
    });
    cut_bytes(by_chars.as_deref().unwrap_or(title), crate::tasks::MAX_TITLE_BYTES).or(by_chars)
}

/// `title` cut to `max` bytes, at a word boundary in its second half; `None`
/// when it fits.
pub fn cut_bytes(title: &str, max: usize) -> Option<String> {
    let cut = truncate(title, max)?;
    Some(match cut.rfind(char::is_whitespace) {
        Some(at) if at > max / 2 => cut[..at].trim_end().to_string(),
        _ => cut,
    })
}

/// `s` cut to `max` bytes on a character boundary; `None` when it fits.
pub fn truncate(s: &str, max: usize) -> Option<String> {
    if s.len() <= max {
        return None;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    Some(s[..end].to_string())
}

/// Minutes from `45`, `"45"`, `"90m"`, `"1h30"`, `"1.5h"`, `"1:30"`,
/// `"1時間30分"`, `"2 hours"`; may be zero or negative.
pub fn minutes(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_f64().map(|f| f.round() as i64),
        Value::String(s) => minutes_text(s),
        _ => None,
    }
}

fn minutes_text(raw: &str) -> Option<i64> {
    let s = raw.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }
    if let Some((h, m)) = s.split_once(':') {
        let (h, m) = (h.trim().parse::<i64>().ok()?, m.trim().parse::<i64>().ok()?);
        return (0..60).contains(&m).then_some(h * 60 + m);
    }
    let negative = s.starts_with('-');
    let mut total = 0.0;
    let mut chars = s.trim_start_matches(['-', '+']).chars().peekable();
    let mut any = false;
    while chars.peek().is_some() {
        while chars.peek().is_some_and(|c| c.is_whitespace() || *c == ',') {
            chars.next();
        }
        let mut num = String::new();
        while let Some(c) = chars.peek().copied().filter(|c| c.is_ascii_digit() || *c == '.') {
            num.push(c);
            chars.next();
        }
        if num.is_empty() {
            return None;
        }
        let n: f64 = num.parse().ok()?;
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let mut unit = String::new();
        while let Some(c) = chars.peek().copied().filter(|c| !c.is_ascii_digit() && *c != '.' && !c.is_whitespace()) {
            unit.push(c);
            chars.next();
        }
        let unit = unit.trim_end_matches(['.', ',']);
        let factor = match unit {
            "h" | "hr" | "hrs" | "hour" | "hours" | "時間" => 60.0,
            "" | "m" | "min" | "mins" | "minute" | "minutes" | "分" => 1.0,
            _ => return None,
        };
        total += n * factor;
        any = true;
    }
    if !any {
        return None;
    }
    let total = total.round() as i64;
    Some(if negative { -total } else { total })
}

/// Whole 5 minutes, at least 5 and at most a day.
pub fn round_minutes(m: i64) -> i64 {
    ((m + 2) / 5 * 5).clamp(MIN_MINUTES, MAX_MINUTES)
}

pub const DAY_FORMS: &str = "YYYY-MM-DD, M/D, 11月6日, today, tomorrow or a weekday";
pub const MOMENT_FORMS: &str = "YYYY-MM-DDTHH:MM, HH:MM, 9am, 21時 or +20min";

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

fn weekday(word: &str) -> Option<Weekday> {
    let w = word.trim().trim_end_matches(['.', ',']).to_lowercase();
    let w = w.strip_prefix("next ").unwrap_or(&w).trim().to_string();
    let named = [
        (Weekday::Monday, ["mon", "月"]),
        (Weekday::Tuesday, ["tue", "火"]),
        (Weekday::Wednesday, ["wed", "水"]),
        (Weekday::Thursday, ["thu", "木"]),
        (Weekday::Friday, ["fri", "金"]),
        (Weekday::Saturday, ["sat", "土"]),
        (Weekday::Sunday, ["sun", "日"]),
    ];
    for (day, [en, ja]) in named {
        let full_en = matches!(
            (day, w.as_str()),
            (Weekday::Monday, "monday")
                | (Weekday::Tuesday, "tuesday" | "tues" | "tu")
                | (Weekday::Wednesday, "wednesday")
                | (Weekday::Thursday, "thursday" | "thur" | "thurs" | "th")
                | (Weekday::Friday, "friday")
                | (Weekday::Saturday, "saturday")
                | (Weekday::Sunday, "sunday")
        );
        let ja_forms = [ja.to_string(), format!("{ja}曜"), format!("{ja}曜日")];
        if w == en || full_en || ja_forms.contains(&w) {
            return Some(day);
        }
    }
    None
}

/// The weekday's short name, as the calendar stores it.
pub fn weekday_name(word: &str) -> Option<&'static str> {
    weekday(word).map(|d| crate::calendar::DAY_NAMES[d.to_monday_zero_offset() as usize])
}

/// Days back a yearless date may lie and still be this year's.
const PAST_DAYS: i32 = 60;

/// A yearless month and day as this year's, unless that lies more than
/// `PAST_DAYS` back; `true` when it was read as next year's.
fn on_or_after(today: Date, month: i8, day: i8) -> Option<(Date, bool)> {
    let this = Date::new(today.year(), month, day).ok()?;
    if this >= today || (today - this).get_days() <= PAST_DAYS {
        return Some((this, false));
    }
    Date::new(today.year() + 1, month, day).ok().map(|d| (d, true))
}

/// A day from `YYYY-MM-DD`, `M/D` (this year's, or next year's once more than
/// two months gone), `11月6日`, `Dec 12`, `today`/`tomorrow`/`今日`/`明日`, or a
/// weekday (the next one).
pub fn day(raw: &str, today: Date) -> Option<Date> {
    day_read(raw, today).map(|(d, _)| d)
}

/// The day `raw` names when it carried no year and was read as next year's.
pub fn rolled(raw: &str, today: Date) -> Option<Date> {
    let s = raw.trim();
    let halves = s.rfind([' ', 'T']).filter(|at| *at > 0).map(|at| [&s[..at], &s[at + 1..]]);
    std::iter::once(s)
        .chain(halves.into_iter().flatten())
        .find_map(|part| day_read(part, today).filter(|(_, next)| *next).map(|(d, _)| d))
}

fn day_read(raw: &str, today: Date) -> Option<(Date, bool)> {
    let s = raw.trim().trim_end_matches(['.', ',']).to_lowercase();
    if s.is_empty() {
        return None;
    }
    let plain = |d: Option<Date>| d.map(|d| (d, false));
    match s.as_str() {
        "today" | "今日" | "きょう" | "本日" => return Some((today, false)),
        "tomorrow" | "明日" | "あした" | "あす" => return plain(today.tomorrow().ok()),
        "明後日" | "あさって" => return plain(today.checked_add(jiff::Span::new().days(2)).ok()),
        _ => {}
    }
    if s.len() == 10 {
        if let Ok(d) = s.replace('/', "-").parse::<Date>() {
            return Some((d, false));
        }
    }
    if let Some(d) = weekday(&s) {
        let ahead = (d.to_monday_zero_offset() - today.weekday().to_monday_zero_offset()).rem_euclid(7);
        return plain(today.checked_add(jiff::Span::new().days(i64::from(ahead))).ok());
    }
    if let Some(rest) = s.strip_suffix('日') {
        let (year, rest) = match rest.split_once('年') {
            Some((y, r)) => (Some(y.trim().parse::<i16>().ok()?), r),
            None => (None, rest),
        };
        let (m, d) = rest.split_once('月')?;
        let (m, d) = (m.trim().parse::<i8>().ok()?, d.trim().parse::<i8>().ok()?);
        return match year {
            Some(y) => plain(Date::new(y, m, d).ok()),
            None => on_or_after(today, m, d),
        };
    }
    let parts: Vec<&str> = s.split(['/', '-']).map(str::trim).collect();
    if parts.len() == 2 || parts.len() == 3 {
        let nums: Option<Vec<i32>> = parts.iter().map(|p| p.parse::<i32>().ok()).collect();
        if let Some(n) = nums {
            return match n.as_slice() {
                [m, d] => on_or_after(today, i8::try_from(*m).ok()?, i8::try_from(*d).ok()?),
                [y, m, d] if *y > 31 => plain(Date::new(i16::try_from(*y).ok()?, i8::try_from(*m).ok()?, i8::try_from(*d).ok()?).ok()),
                [m, d, y] => {
                    let y = if *y < 100 { 2000 + y } else { *y };
                    plain(Date::new(i16::try_from(y).ok()?, i8::try_from(*m).ok()?, i8::try_from(*d).ok()?).ok())
                }
                _ => None,
            };
        }
    }
    month_name_day(&s, today)
}

/// `dec 12`, `december 12, 2026`, `12 dec`.
fn month_name_day(text: &str, today: Date) -> Option<(Date, bool)> {
    let words: Vec<&str> = text.split([' ', ',']).filter(|w| !w.is_empty()).collect();
    let month_of = |w: &str| MONTHS.iter().position(|m| w.starts_with(m)).map(|i| i as i8 + 1);
    let (month, day, year) = match words.as_slice() {
        [a, b] => match (month_of(a), month_of(b)) {
            (Some(m), _) => (m, b.trim_end_matches(char::is_alphabetic).parse::<i8>().ok()?, None),
            (_, Some(m)) => (m, a.trim_end_matches(char::is_alphabetic).parse::<i8>().ok()?, None),
            _ => return None,
        },
        [a, b, c] => match (month_of(a), month_of(b)) {
            (Some(m), _) => (m, b.trim_end_matches(char::is_alphabetic).parse::<i8>().ok()?, Some(c.parse::<i16>().ok()?)),
            (_, Some(m)) => (m, a.trim_end_matches(char::is_alphabetic).parse::<i8>().ok()?, Some(c.parse::<i16>().ok()?)),
            _ => return None,
        },
        _ => return None,
    };
    match year {
        Some(y) => Date::new(y, month, day).ok().map(|d| (d, false)),
        None => on_or_after(today, month, day),
    }
}

/// `HH:MM` from `17:00`, `9:05`, `5pm`, `5:30 pm`, `noon`, `21時`, `21時30分`, `17`.
pub fn wall_time(raw: &str) -> Option<String> {
    let s = raw.trim().to_lowercase().replace(' ', "");
    let (h, m): (i64, i64) = match s.as_str() {
        "noon" | "正午" => (12, 0),
        "midnight" => (0, 0),
        _ => {
            if let Some(rest) = s.strip_suffix("am").or_else(|| s.strip_suffix("a.m.")) {
                let (h, m) = hour_minute(rest)?;
                if !(1..=12).contains(&h) {
                    return None;
                }
                (h % 12, m)
            } else if let Some(rest) = s.strip_suffix("pm").or_else(|| s.strip_suffix("p.m.")) {
                let (h, m) = hour_minute(rest)?;
                if !(1..=12).contains(&h) {
                    return None;
                }
                (h % 12 + 12, m)
            } else if let Some((h, rest)) = s.split_once('時') {
                let m = rest.trim_end_matches('分');
                let m = if m.is_empty() { 0 } else if m == "半" { 30 } else { m.parse().ok()? };
                (h.parse().ok()?, m)
            } else {
                hour_minute(&s)?
            }
        }
    };
    ((0..24).contains(&h) && (0..60).contains(&m)).then(|| format!("{h:02}:{m:02}"))
}

fn hour_minute(s: &str) -> Option<(i64, i64)> {
    match s.split_once([':', '.']) {
        Some((h, m)) if m.len() == 2 => Some((h.parse().ok()?, m.parse().ok()?)),
        None if !s.is_empty() && s.len() <= 2 => Some((s.parse().ok()?, 0)),
        None if s.len() == 4 => Some((s[..2].parse().ok()?, s[2..].parse().ok()?)),
        _ => None,
    }
}

/// `+20min`, `+1h`, `in 20 minutes`; a bare `+17` names no unit and is not one.
fn relative(raw: &str) -> Option<i64> {
    let s = raw.trim().to_lowercase();
    let rest = s.strip_prefix('+').or_else(|| s.strip_prefix("in "))?;
    if rest.trim().chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    minutes_text(rest).filter(|m| *m > 0)
}

/// A whole day or a set moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum When {
    Day(Date),
    At(Timestamp),
}

/// A due: a day is a whole day, a time a moment on the user's clock (today,
/// or tomorrow once today's has gone by).
pub fn when(raw: &str, tz: &TimeZone, now: Timestamp) -> Option<When> {
    let s = raw.trim();
    let today = now.to_zoned(tz.clone()).date();
    if let Ok(at) = s.parse::<Timestamp>() {
        return Some(When::At(at));
    }
    if let Some(m) = relative(s) {
        return now.checked_add(jiff::Span::new().minutes(m)).ok().map(When::At);
    }
    if let Some(d) = day(s, today) {
        return Some(When::Day(d));
    }
    if let Ok(local) = s.parse::<jiff::civil::DateTime>() {
        return tz.to_ambiguous_zoned(local).compatible().ok().map(|z| When::At(z.timestamp()));
    }
    let at = |date: Date, wall: &str| crate::zone::instant(tz, date, wall).map(When::At);
    if let Some(wall) = wall_time(s) {
        let today_at = crate::zone::instant(tz, today, &wall)?;
        if today_at > now {
            return Some(When::At(today_at));
        }
        return at(today.tomorrow().ok()?, &wall);
    }
    let split = s.rfind([' ', 'T']).filter(|at| *at > 0)?;
    let (left, right) = (&s[..split], &s[split + 1..]);
    if let (Some(d), Some(wall)) = (day(left, today), wall_time(right)) {
        return at(d, &wall);
    }
    if let (Some(wall), Some(d)) = (wall_time(left), day(right, today)) {
        return at(d, &wall);
    }
    None
}

/// `raw` folded onto one of `allowed`, through `synonyms`.
pub fn choice(raw: &str, allowed: &[&'static str], synonyms: &[(&str, &'static str)]) -> Option<&'static str> {
    let s = raw.trim().to_lowercase().replace([' ', '-'], "_");
    if let Some(a) = allowed.iter().find(|a| **a == s) {
        return Some(a);
    }
    synonyms.iter().find(|(from, _)| *from == s).map(|(_, to)| *to)
}

pub const STATE_SYNONYMS: &[(&str, &str)] = &[
    ("todo", "open"),
    ("to_do", "open"),
    ("new", "open"),
    ("reopen", "open"),
    ("reopened", "open"),
    ("not_started", "open"),
    ("pending", "open"),
    ("未着手", "open"),
    ("doing", "in_progress"),
    ("started", "in_progress"),
    ("start", "in_progress"),
    ("active", "in_progress"),
    ("inprogress", "in_progress"),
    ("progress", "in_progress"),
    ("wip", "in_progress"),
    ("ongoing", "in_progress"),
    ("進行中", "in_progress"),
    ("complete", "done"),
    ("completed", "done"),
    ("finished", "done"),
    ("finish", "done"),
    ("closed", "done"),
    ("reached", "done"),
    ("完了", "done"),
    ("済み", "done"),
    ("drop", "dropped"),
    ("cancelled", "dropped"),
    ("canceled", "dropped"),
    ("cancel", "dropped"),
    ("abandoned", "dropped"),
    ("abandon", "dropped"),
    ("中止", "dropped"),
];

pub const URGENCY_SYNONYMS: &[(&str, &str)] = &[
    ("urgent", "high"),
    ("important", "high"),
    ("asap", "high"),
    ("critical", "high"),
    ("top", "high"),
    ("medium", "normal"),
    ("mid", "normal"),
    ("default", "normal"),
    ("regular", "normal"),
    ("minor", "low"),
];

pub const NOTIFY_SYNONYMS: &[(&str, &str)] = &[
    ("silent", "none"),
    ("off", "none"),
    ("quiet", "none"),
    ("no", "none"),
    ("message", "chat"),
    ("thread", "chat"),
    ("notification", "notify"),
    ("push", "notify"),
    ("on", "notify"),
];

/// Why a field that is not part of a resource's shape went unread, naming the
/// tool that takes it where one obviously does.
pub fn unknown(field: &str, elsewhere: &[(&str, &str)]) -> String {
    match elsewhere.iter().find(|(f, _)| *f == field) {
        Some((_, tool)) => format!("{field} belongs to {tool}"),
        None => "not a field here".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn d(s: &str) -> Date {
        s.parse().unwrap()
    }

    #[test]
    fn durations_read_in_every_form_a_person_writes() {
        for (v, want) in [
            (json!(45), 45),
            (json!("45"), 45),
            (json!("90m"), 90),
            (json!("90 min"), 90),
            (json!("1h30"), 90),
            (json!("1h 30m"), 90),
            (json!("1.5h"), 90),
            (json!("1:30"), 90),
            (json!("1時間30分"), 90),
            (json!("30分"), 30),
            (json!("2 hours"), 120),
            (json!(0), 0),
            (json!(-10), -10),
            (json!(22.4), 22),
        ] {
            assert_eq!(minutes(&v), Some(want), "{v}");
        }
        for v in [json!("soon"), json!("1x"), json!(true), json!("")] {
            assert_eq!(minutes(&v), None, "{v}");
        }
    }

    #[test]
    fn sizes_round_to_five_inside_a_day() {
        assert_eq!(round_minutes(22), 20);
        assert_eq!(round_minutes(23), 25);
        assert_eq!(round_minutes(1), 5);
        assert_eq!(round_minutes(5000), 1440);
        assert_eq!(round_minutes(45), 45);
    }

    #[test]
    fn days_read_as_the_next_one_on_or_after_today() {
        let today = d("2026-10-20");
        assert_eq!(day("2026-11-06", today), Some(d("2026-11-06")));
        assert_eq!(day("11/6", today), Some(d("2026-11-06")));
        assert_eq!(day("10/1", today), Some(d("2026-10-01")), "a day weeks gone stays this year's");
        assert_eq!(day("8/1", today), Some(d("2027-08-01")), "a day months gone is next year's");
        assert_eq!(rolled("8/1", today), Some(d("2027-08-01")));
        assert_eq!(rolled("8/1 17:00", today), Some(d("2027-08-01")));
        assert_eq!(rolled("10/1", today), None);
        assert_eq!(rolled("2027-08-01", today), None);
        assert_eq!(day("10/20", today), Some(d("2026-10-20")));
        assert_eq!(day("11月6日", today), Some(d("2026-11-06")));
        assert_eq!(day("2027年1月3日", today), Some(d("2027-01-03")));
        assert_eq!(day("today", today), Some(today));
        assert_eq!(day("Tomorrow", today), Some(d("2026-10-21")));
        assert_eq!(day("今日", today), Some(today));
        assert_eq!(day("明日", today), Some(d("2026-10-21")));
        assert_eq!(day("friday", today), Some(d("2026-10-23")));
        assert_eq!(day("Tue", today), Some(today), "today is a Tuesday");
        assert_eq!(day("金曜日", today), Some(d("2026-10-23")));
        assert_eq!(day("Dec 12", today), Some(d("2026-12-12")));
        assert_eq!(day("December 12, 2026", today), Some(d("2026-12-12")));
        assert_eq!(day("12/18/2026", today), Some(d("2026-12-18")));
        for bad in ["someday", "2/30", "13/1", ""] {
            assert_eq!(day(bad, today), None, "{bad}");
        }
    }

    #[test]
    fn moments_read_on_the_users_clock() {
        let tz = TimeZone::get("Asia/Tokyo").unwrap();
        let now: Timestamp = "2026-10-20T03:00:00Z".parse().unwrap();
        let at = |s: &str| match when(s, &tz, now) {
            Some(When::At(t)) => t.to_string(),
            other => format!("{other:?}"),
        };
        assert_eq!(at("2026-10-21T09:00:00+09:00"), "2026-10-21T00:00:00Z");
        assert_eq!(at("2026-10-21T17:30"), "2026-10-21T08:30:00Z");
        assert_eq!(at("17:30"), "2026-10-20T08:30:00Z", "noon in Tokyo, so 17:30 is still today");
        assert_eq!(at("9:05"), "2026-10-21T00:05:00Z", "gone by today, so tomorrow");
        assert_eq!(at("9am"), "2026-10-21T00:00:00Z");
        assert_eq!(at("21時"), "2026-10-20T12:00:00Z");
        assert_eq!(at("+20min"), "2026-10-20T03:20:00Z");
        assert_eq!(at("11/6 17:00"), "2026-11-06T08:00:00Z");
        assert_eq!(when("11/6", &tz, now), Some(When::Day(d("2026-11-06"))));
        assert_eq!(when("whenever", &tz, now), None);
        assert_eq!(when("+17", &tz, now), None, "a bare count names no unit");
    }

    #[test]
    fn wall_times_are_zero_padded() {
        for (raw, want) in [
            ("17:00", "17:00"),
            ("9:05", "09:05"),
            ("5pm", "17:00"),
            ("5:30 pm", "17:30"),
            ("12am", "00:00"),
            ("noon", "12:00"),
            ("21時", "21:00"),
            ("21時30分", "21:30"),
            ("17", "17:00"),
            ("1830", "18:30"),
        ] {
            assert_eq!(wall_time(raw).as_deref(), Some(want), "{raw}");
        }
        for bad in ["25:00", "13pm", "late", "9:5"] {
            assert_eq!(wall_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn ids_take_every_form_and_titles_fall_through() {
        for v in [json!(41), json!("41"), json!("#41"), json!("t41"), json!("T#41"), json!(41.0)] {
            assert_eq!(id(&v), Some(41), "{v}");
        }
        assert_eq!(id(&json!("Essay")), None);
        assert_eq!(id(&json!(0)), None);
        assert_eq!(target(&json!("Essay")), Some(Target::Title("Essay".into())));
        assert_eq!(target(&json!("e88")), Some(Target::Id(88)));
    }

    #[test]
    fn a_title_resolves_only_when_one_open_thing_carries_it() {
        let open = vec![(88, "Essay".to_string()), (91, "essay".to_string()), (7, "Lab  Report".to_string())];
        assert_eq!(by_title("lab report", &open, "task"), Ok(7));
        let e = by_title("ESSAY", &open, "task").unwrap_err();
        assert!(e.contains("88, 91"), "{e}");
        let goals = vec![(1, "AP Chemistry final".to_string()), (2, "Visual Arts".to_string()), (3, "Chem lab".to_string())];
        assert_eq!(nearest("chemistry", &goals, 2).iter().map(|g| g.0).collect::<Vec<_>>(), [1, 3]);
        assert!(by_title("Poem", &open, "task").unwrap_err().starts_with("no open task"));
    }

    #[test]
    fn enums_fold_case_and_synonyms() {
        let states = crate::tasks::STATES;
        for (raw, want) in [
            ("Done", "done"),
            ("complete", "done"),
            ("finished", "done"),
            ("Completed", "done"),
            ("cancelled", "dropped"),
            ("abandoned", "dropped"),
            ("doing", "in_progress"),
            ("started", "in_progress"),
            ("in progress", "in_progress"),
        ] {
            assert_eq!(choice(raw, states, STATE_SYNONYMS), Some(want), "{raw}");
        }
        assert_eq!(choice("urgent", crate::tasks::URGENCY, URGENCY_SYNONYMS), Some("high"));
        assert_eq!(choice("HIGH", crate::tasks::URGENCY, URGENCY_SYNONYMS), Some("high"));
        assert_eq!(choice("exploded", states, STATE_SYNONYMS), None);
    }

    #[test]
    fn an_over_long_title_is_cut_at_a_word() {
        let long = "word ".repeat(60);
        let cut = cut_title(&long).unwrap();
        assert!(cut.chars().count() <= MAX_TITLE_CHARS);
        assert!(cut.ends_with("word"));
        assert_eq!(cut_title("short"), None);
        let ja = "あ".repeat(180);
        assert_eq!(cut_title(&ja).unwrap().len(), crate::tasks::MAX_TITLE_BYTES - 2, "under 200 characters, over 500 bytes");
        assert_eq!(truncate("ああ", 4).as_deref(), Some("あ"));
    }

    #[test]
    fn a_list_is_found_under_its_key_an_alias_or_as_a_bare_object() {
        let obj = |v: Value| Obj::new(v.as_object().unwrap().clone());
        let mut o = obj(json!({"tasks": [{"title": "a"}]}));
        assert_eq!(items(&mut o, "tasks", &["items"], &["title"]).0.len(), 1);
        let mut o = obj(json!({"items": [{"title": "a"}, {"title": "b"}]}));
        let (got, warning) = items(&mut o, "tasks", &["items"], &["title"]);
        assert_eq!(got.len(), 2);
        assert!(warning.is_some());
        let mut o = obj(json!({"title": "a", "minutes": 5}));
        assert_eq!(items(&mut o, "tasks", &["items"], &["title"]).0, vec![json!({"title": "a", "minutes": 5})]);
        let mut o = obj(json!({"tasks": {"title": "one"}}));
        assert_eq!(items(&mut o, "tasks", &[], &[]).0, vec![json!({"title": "one"})]);
        let mut o = obj(json!({}));
        assert!(items(&mut o, "tasks", &[], &["title"]).0.is_empty());
        let mut o = obj(json!({"updates": [{"id": 4}], "why": "x"}));
        let (got, warning) = items(&mut o, "tasks", &[], &["title"]);
        assert_eq!((got.len(), warning.as_deref()), (1, Some("read updates as tasks")));
        let mut o = obj(json!({"a": [1], "b": {"id": 2}}));
        assert!(items(&mut o, "tasks", &[], &["title"]).0.is_empty(), "two lists name none");
    }

    #[test]
    fn a_list_wrapped_in_item_is_the_list() {
        assert_eq!(list(json!({"item": ["1", "2"]})), vec![json!("1"), json!("2")]);
        assert_eq!(list(json!({"item": {"id": "5", "due": "10/28"}})), vec![json!({"id": "5", "due": "10/28"})]);
        assert_eq!(list(json!({"items": {"item": [3]}})), vec![json!(3)]);
        assert_eq!(list(json!({"id": 4})), vec![json!({"id": 4})], "a node is not a wrapper");
        let mut o = Obj::new(json!({"tasks": {"item": {"id": "5"}}}).as_object().unwrap().clone());
        assert_eq!(items(&mut o, "tasks", &[], &["id"]).0, vec![json!({"id": "5"})]);
    }

    #[test]
    fn a_field_is_read_by_its_name_or_an_alias_and_the_rest_is_reported() {
        let mut o = Obj::new(json!({"duration_min": 30, "colour": "red", "due": null}).as_object().unwrap().clone());
        assert_eq!(o.take(&["minutes", "duration_min", "duration"]), Some(json!(30)));
        assert_eq!(o.take(&["due"]), None, "null is not given");
        assert_eq!(o.rest(), vec!["colour".to_string()]);
    }

    #[test]
    fn the_envelope_leaves_out_what_is_empty() {
        let mut e = Envelope::default();
        assert_eq!(e.into_value(), json!({"ok": true}));
        e = Envelope::default();
        e.saved(&Ref::top(0), 4, "a", "created");
        e.refused(&Ref::top(1).child(2), "no task 9");
        let v = e.into_value();
        assert_eq!(v["ok"], false);
        assert_eq!(v["refused"][0]["ref"], "1.2");
        assert!(v.get("adjusted").is_none());
        assert_eq!(with_warning(json!({"ok": true}), "w".into())["warnings"], json!(["w"]));
    }
}
