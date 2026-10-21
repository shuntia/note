//! One checker per case, judging a `Snapshot` by meaning rather than by
//! schema: a goal is a goal row or a root standing for one, a unit is whatever
//! hangs directly under it, and a day's list is its ordered items and blocks.

use crate::snapshot::{fold, GoalRef, Node, Snapshot};
use jiff::civil::{Date, Time};
use serde::Serialize;
use std::collections::BTreeMap;

/// What a case's seed left behind for its checker: ids by role and dates by name.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Fixture {
    pub today: Option<Date>,
    pub ids: BTreeMap<String, i64>,
    pub dates: BTreeMap<String, Date>,
    pub lists: BTreeMap<String, Vec<i64>>,
}

impl Fixture {
    fn id(&self, key: &str) -> i64 {
        self.ids.get(key).copied().unwrap_or(-1)
    }
    fn date(&self, key: &str) -> Option<Date> {
        self.dates.get(key).copied()
    }
    fn list(&self, key: &str) -> &[i64] {
        self.lists.get(key).map_or(&[], Vec::as_slice)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Verdict {
    pub pass: bool,
    pub failures: Vec<String>,
}

struct Judge(Vec<String>);

impl Judge {
    fn new() -> Self {
        Self(Vec::new())
    }
    fn expect(&mut self, ok: bool, why: impl FnOnce() -> String) {
        if !ok {
            self.0.push(why());
        }
    }
    fn done(self) -> Verdict {
        Verdict { pass: self.0.is_empty(), failures: self.0 }
    }
}

pub fn check(case: &str, snap: &Snapshot, fx: &Fixture) -> Verdict {
    match case {
        "visual-arts" => visual_arts(snap, fx),
        "reschedule-day" => reschedule_day(snap, fx),
        "recurring-calendar" => recurring_calendar(snap, fx),
        "checkin-follow-up" => checkin_follow_up(snap, fx),
        "nightly-plan" => nightly_plan(snap, fx),
        "breakdown-progress" => breakdown_progress(snap, fx),
        other => Verdict { pass: false, failures: vec![format!("no checker for {other}")] },
    }
}

pub const MIN_LESSONS: usize = 35;
const UNIT_KEYS: [&str; 5] = ["unit1", "unit2", "unit3", "unit4", "unit5"];

fn visual_arts(snap: &Snapshot, fx: &Fixture) -> Verdict {
    let mut j = Judge::new();
    let exam = fx.date("exam");
    let goals: Vec<_> = snap.goal_refs().into_iter().filter(|(_, _, due)| *due == exam && exam.is_some()).collect();
    j.expect(goals.len() == 1, || format!("{} goals due on the exam day {exam:?}, want 1", goals.len()));
    let Some((goal, _, _)) = goals.into_iter().max_by_key(|(g, _, _)| snap.under_goal(*g).len()) else {
        return j.done();
    };
    let units = snap.under_goal(goal);
    j.expect(units.len() == 6, || format!("{} tasks under the goal, want 6 (5 units and the exam)", units.len()));
    let mut wanted: Vec<Date> = UNIT_KEYS.iter().filter_map(|k| fx.date(k)).collect();
    wanted.extend(exam);
    let mut dues: Vec<Date> = units.iter().filter_map(|u| u.due).collect();
    for d in &wanted {
        match dues.iter().position(|x| x == d) {
            Some(i) => {
                dues.remove(i);
            }
            None => j.expect(false, || format!("no task under the goal due {d}")),
        }
    }
    let lessons: Vec<(&Node, &Node)> =
        units.iter().flat_map(|u| snap.children(u.id).into_iter().map(move |l| (*u, l))).collect();
    j.expect(lessons.len() >= MIN_LESSONS, || format!("{} lessons under the units, want at least {MIN_LESSONS}", lessons.len()));
    let unsized_lessons: Vec<&str> = lessons
        .iter()
        .filter(|(_, l)| !l.minutes.is_some_and(|m| m > 0 && m % 5 == 0))
        .map(|(_, l)| l.title.as_str())
        .collect();
    j.expect(unsized_lessons.is_empty(), || format!("{} lessons unsized or off the 5-minute grid: {:?}", unsized_lessons.len(), first(&unsized_lessons)));
    for (u, l) in &lessons {
        if let (Some(ld), Some(ud)) = (l.due, u.due) {
            j.expect(ld <= ud, || format!("lesson {:?} due {ld} after its unit {:?} {ud}", l.title, u.title));
        }
    }
    for u in &units {
        let kids = snap.children(u.id);
        if kids.is_empty() {
            continue;
        }
        let sum: u32 = kids.iter().filter_map(|k| k.minutes).sum();
        j.expect(u.minutes == Some(sum), || format!("unit {:?} sized {:?}, its lessons sum to {sum}", u.title, u.minutes));
    }
    let in_tree: Vec<i64> = std::iter::once(goal_node(goal))
        .flatten()
        .chain(units.iter().map(|u| u.id))
        .chain(lessons.iter().map(|(_, l)| l.id))
        .collect();
    let names: std::collections::HashSet<String> =
        units.iter().map(|u| fold(&u.title)).chain(lessons.iter().map(|(_, l)| fold(&l.title))).collect();
    let flat: Vec<&str> = snap
        .nodes
        .iter()
        .filter(|n| !in_tree.contains(&n.id) && !in_tree.contains(&snap.root_of(n.id)) && names.contains(&fold(&n.title)))
        .map(|n| n.title.as_str())
        .collect();
    j.expect(flat.is_empty(), || format!("{} duplicate tasks outside the goal: {:?}", flat.len(), first(&flat)));
    j.done()
}

fn goal_node(goal: GoalRef) -> Option<i64> {
    match goal {
        GoalRef::Node(id) => Some(id),
        GoalRef::Row(_) => None,
    }
}

fn first<'a>(v: &[&'a str]) -> Vec<&'a str> {
    v.iter().take(5).copied().collect()
}

fn t(h: i8, m: i8) -> Time {
    Time::constant(h, m, 0, 0)
}

fn reschedule_day(snap: &Snapshot, fx: &Fixture) -> Verdict {
    let mut j = Judge::new();
    let (Some(today), Some(tomorrow)) = (fx.date("today"), fx.date("tomorrow")) else {
        j.expect(false, || "fixture lacks its dates".into());
        return j.done();
    };
    let on_tomorrow = snap.day_tasks(tomorrow);
    let on_today = snap.day_tasks(today);
    for key in ["physics", "grocery"] {
        let id = fx.id(key);
        j.expect(on_tomorrow.contains(&id), || format!("{key} is not on tomorrow's list"));
        let late_today = snap.blocks_for(today, id).iter().any(|b| b.start.is_some_and(|s| s >= t(13, 0)));
        j.expect(!late_today, || format!("{key} still has a block this afternoon"));
    }
    let essay = fx.id("essay");
    let at_eight = snap.blocks_for(today, essay).iter().any(|b| b.start == Some(t(20, 0)));
    j.expect(at_eight, || "the essay draft is not today at 20:00".into());
    j.expect(!on_tomorrow.contains(&essay), || "the essay draft moved to tomorrow".into());
    for id in fx.list("placed") {
        match snap.node(*id) {
            None => j.expect(false, || format!("task {id} was deleted")),
            Some(n) => {
                j.expect(n.open(), || format!("{:?} is {}, not open", n.title, n.state));
                j.expect(on_today.contains(id) || on_tomorrow.contains(id), || format!("{:?} is on neither day", n.title));
            }
        }
    }
    for date in [today, tomorrow] {
        let fixed: Vec<_> = snap.occurrences.get(&date).into_iter().flatten().filter(|o| o.kind == "fixed").collect();
        for item in snap.days.get(&date).into_iter().flatten() {
            let (Some(_), Some(s), Some(e)) = (item.task_id, item.start, item.end) else { continue };
            if matches!(item.status.as_str(), "dropped" | "done") {
                continue;
            }
            for o in &fixed {
                if let (Some(os), Some(oe)) = (o.start, o.end) {
                    j.expect(!(s < oe && os < e), || format!("a block {s}-{e} on {date} overlaps {:?}", o.title));
                }
            }
        }
    }
    j.done()
}

pub const TUE: i64 = 2;
pub const THU: i64 = 8;

fn recurring_calendar(snap: &Snapshot, fx: &Fixture) -> Verdict {
    let mut j = Judge::new();
    let seeded = fx.list("calendar");
    let new: Vec<_> = snap.calendar.iter().filter(|e| !seeded.contains(&e.id)).collect();
    j.expect(new.len() == 1, || format!("{} new calendar entries, want 1", new.len()));
    if let Some(e) = new.first() {
        j.expect(e.days == TUE | THU, || format!("days mask {}, want tue+thu ({})", e.days, TUE | THU));
        j.expect(e.start == Some(t(17, 0)) && e.end == Some(t(18, 30)), || format!("{:?}-{:?}, want 17:00-18:30", e.start, e.end));
        j.expect(e.until == fx.date("until"), || format!("until {:?}, want {:?}", e.until, fx.date("until")));
        j.expect(e.quiet, || "not quiet".into());
    }
    for id in seeded {
        j.expect(snap.calendar.iter().any(|e| e.id == *id), || format!("seeded entry {id} is gone"));
    }
    j.done()
}

const TERMINAL: [&str; 2] = ["say", "call_user"];

fn checkin_follow_up(snap: &Snapshot, fx: &Fixture) -> Verdict {
    let mut j = Judge::new();
    let sessions: Vec<_> = snap.sessions.iter().filter(|s| s.kind == "Trigger").collect();
    j.expect(!sessions.is_empty(), || "the wake-up never ran".into());
    let last = sessions.iter().flat_map(|s| s.calls.iter()).rfind(|c| TERMINAL.contains(&c.name.as_str()) && !c.is_error);
    let tree = fx.id("lab");
    match last {
        None => j.expect(false, || "no say or call_user ended the wake-up".into()),
        Some(c) => {
            let ids: Vec<i64> = c.args["task_ids"].as_array().into_iter().flatten().filter_map(serde_json::Value::as_i64).collect();
            j.expect(
                ids.is_empty() || ids.iter().any(|id| snap.root_of(*id) == tree),
                || format!("{} names tasks {ids:?}, none in the Lab report tree", c.name),
            );
            j.expect(
                c.name == "call_user" || c.args["await_min"].as_u64().is_some(),
                || "say set no await_min".into(),
            );
        }
    }
    let refused = crate::score::refused(sessions.iter().copied());
    j.expect(refused == 0, || format!("{refused} refused calls"));
    j.done()
}

pub const NOTES_TOOL: &str = "nightly_notes_write";

fn nightly_plan(snap: &Snapshot, fx: &Fixture) -> Verdict {
    let mut j = Judge::new();
    let Some(target) = fx.date("target") else {
        j.expect(false, || "fixture lacks its target date".into());
        return j.done();
    };
    let list = snap.day_tasks(target);
    j.expect(!list.is_empty(), || format!("nothing is on {target}'s list"));
    let parents: Vec<&str> =
        list.iter().filter(|id| !snap.is_leaf(**id)).filter_map(|id| snap.node(*id)).map(|n| n.title.as_str()).collect();
    j.expect(parents.is_empty(), || format!("non-leaves on the list: {parents:?}"));
    let pressing = fx.list("pressing");
    let is_pressing = |id: &i64| pressing.contains(&snap.root_of(*id));
    if let Some(head) = list.first() {
        j.expect(is_pressing(head), || "the list does not open with high or near-due work".into());
    }
    let last_pressing = list.iter().rposition(&is_pressing);
    let out_of_place = last_pressing.map_or(0, |end| list[..end].iter().filter(|id| !is_pressing(id)).count());
    j.expect(out_of_place <= 1, || format!("{out_of_place} other items come before pressing ones"));
    let notes = snap
        .sessions
        .iter()
        .filter(|s| s.kind == "Nightly")
        .flat_map(|s| s.calls.iter())
        .filter(|c| c.name == NOTES_TOOL && !c.is_error)
        .count();
    j.expect(notes == 1, || format!("{NOTES_TOOL} called {notes} times, want 1"));
    j.expect(snap.wakes_laid == 0, || format!("{} wake-ups laid", snap.wakes_laid));
    j.done()
}

const PART_WORDS: [&str; 3] = ["draft", "citation", "proofread"];

fn breakdown_progress(snap: &Snapshot, fx: &Fixture) -> Verdict {
    let mut j = Judge::new();
    let root_id = fx.id("essay");
    let Some(root) = snap.node(root_id) else {
        j.expect(false, || "the History essay is gone".into());
        return j.done();
    };
    let kids = snap.children(root_id);
    j.expect(kids.len() == 4, || format!("{} children, want 4", kids.len()));
    let is_outline = |k: &&Node| {
        let t = fold(&k.title);
        t.contains("outline") && !PART_WORDS.iter().any(|w| t.contains(w))
    };
    let outline: Vec<_> = kids.iter().filter(|k| is_outline(k)).collect();
    j.expect(outline.len() == 1 && outline[0].state == "done", || "no single finished outline child".into());
    let rest: Vec<_> = kids.iter().filter(|k| !is_outline(k)).collect();
    for w in PART_WORDS {
        j.expect(rest.iter().any(|k| fold(&k.title).contains(w)), || format!("no child for {w}"));
    }
    let rest_min: u32 = rest.iter().filter_map(|k| k.minutes).sum();
    j.expect((50..=70).contains(&rest_min), || format!("the new parts sum to {rest_min} min, want about 60"));
    j.expect(root.progress > 0, || "the essay shows no progress".into());
    let all: u32 = kids.iter().filter_map(|k| k.minutes).sum();
    j.expect(
        root.minutes.is_some_and(|m| m.abs_diff(all) <= 5),
        || format!("the essay is sized {:?}, its children sum to {all}", root.minutes),
    );
    j.done()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{Call, DayItem, Entry, Goal, Occurrence, Session};
    use jiff::civil::date;

    fn node(id: i64, parent: Option<i64>, title: &str) -> Node {
        Node { id, parent, title: title.into(), state: "open".into(), urgency: "normal".into(), ..Default::default() }
    }

    fn fixture(dates: &[(&str, Date)]) -> Fixture {
        Fixture { dates: dates.iter().map(|(k, d)| ((*k).to_string(), *d)).collect(), ..Default::default() }
    }

    fn course_dates() -> Fixture {
        fixture(&[
            ("unit1", date(2026, 11, 6)),
            ("unit2", date(2026, 11, 18)),
            ("unit3", date(2026, 12, 3)),
            ("unit4", date(2026, 12, 17)),
            ("unit5", date(2026, 12, 17)),
            ("exam", date(2026, 12, 18)),
        ])
    }

    /// The course as today's model stores it: a goal row, root tasks under it,
    /// lessons as their steps.
    fn course_flat(lessons_per_unit: usize) -> Snapshot {
        let fx = course_dates();
        let mut s = Snapshot {
            goals: vec![Goal { id: 1, title: "Visual arts".into(), due: fx.date("exam"), state: "open".into() }],
            ..Default::default()
        };
        let mut id = 100;
        for (u, key) in UNIT_KEYS.iter().enumerate() {
            let unit_id = (u as i64 + 1) * 10;
            let mut unit = node(unit_id, None, &format!("Unit {}", u + 1));
            unit.goal = Some(1);
            unit.due = fx.date(key);
            unit.minutes = Some(lessons_per_unit as u32 * 30);
            s.nodes.push(unit);
            for l in 0..lessons_per_unit {
                id += 1;
                let mut lesson = node(id, Some(unit_id), &format!("Lesson {u}.{l}"));
                lesson.minutes = Some(30);
                s.nodes.push(lesson);
            }
        }
        let mut exam = node(90, None, "Cumulative exam");
        exam.goal = Some(1);
        exam.due = fx.date("exam");
        s.nodes.push(exam);
        s
    }

    /// The same course as a tree: the goal a root, units its children.
    fn course_tree() -> Snapshot {
        let mut s = course_flat(8);
        s.goals.clear();
        let mut goal = node(1, None, "Visual arts");
        goal.due = course_dates().date("exam");
        for n in &mut s.nodes {
            if n.goal == Some(1) {
                n.goal = None;
                n.parent = Some(1);
            }
        }
        s.nodes.push(goal);
        s
    }

    #[test]
    fn visual_arts_passes_on_today_model_and_on_the_tree() {
        let fx = course_dates();
        let flat = visual_arts(&course_flat(8), &fx);
        assert!(flat.pass, "{:?}", flat.failures);
        let tree = visual_arts(&course_tree(), &fx);
        assert!(tree.pass, "{:?}", tree.failures);
    }

    #[test]
    fn visual_arts_fails_short_unsized_or_duplicated_courses() {
        let fx = course_dates();
        assert!(!visual_arts(&course_flat(6), &fx).pass, "30 lessons are too few");

        let mut s = course_flat(8);
        s.nodes.iter_mut().find(|n| n.id == 101).unwrap().minutes = Some(33);
        let v = visual_arts(&s, &fx);
        assert!(v.failures.iter().any(|f| f.contains("unsized")), "{:?}", v.failures);

        let mut s = course_flat(8);
        s.nodes.push(node(500, None, "Lesson 0.0"));
        let v = visual_arts(&s, &fx);
        assert!(v.failures.iter().any(|f| f.contains("duplicate")), "{:?}", v.failures);

        let mut s = course_flat(8);
        s.nodes.iter_mut().find(|n| n.id == 10).unwrap().minutes = Some(60);
        assert!(!visual_arts(&s, &fx).pass, "a unit's size must be its lessons' sum");

        let mut s = course_flat(8);
        s.nodes.iter_mut().find(|n| n.id == 20).unwrap().due = Some(date(2026, 11, 19));
        assert!(!visual_arts(&s, &fx).pass, "unit 2 must be due on its day");

        let mut s = course_flat(8);
        s.goals.push(Goal { id: 2, title: "Again".into(), due: fx.date("exam"), state: "open".into() });
        assert!(!visual_arts(&s, &fx).pass, "two goals on the exam day");
    }

    fn item(task: i64, start: Option<Time>, end: Option<Time>, position: Option<i64>) -> DayItem {
        DayItem { task_id: Some(task), kind: "x".into(), start, end, status: "pending".into(), position }
    }

    fn sick_day(moved: bool) -> (Snapshot, Fixture) {
        let (today, tomorrow) = (date(2026, 10, 20), date(2026, 10, 21));
        let mut fx = fixture(&[("today", today), ("tomorrow", tomorrow)]);
        for (k, id) in [("email", 1), ("laundry", 2), ("reading", 3), ("physics", 4), ("grocery", 5), ("essay", 6)] {
            fx.ids.insert(k.into(), id);
        }
        fx.lists.insert("placed".into(), (1..=6).collect());
        let mut s = Snapshot { nodes: (1..=6).map(|i| node(i, None, &format!("t{i}"))).collect(), ..Default::default() };
        let mut today_items: Vec<DayItem> = [1, 2, 3, 6].iter().enumerate().map(|(p, id)| item(*id, None, None, Some(p as i64))).collect();
        today_items.push(item(6, Some(t(20, 0)), Some(t(21, 0)), None));
        let mut tomorrow_items = Vec::new();
        if moved {
            tomorrow_items.push(item(4, Some(t(10, 0)), Some(t(11, 0)), None));
            tomorrow_items.push(item(5, None, None, Some(0)));
        } else {
            today_items.push(item(4, Some(t(14, 0)), Some(t(15, 0)), None));
            today_items.push(item(5, Some(t(16, 30)), Some(t(17, 15)), None));
        }
        s.days.insert(today, today_items);
        s.days.insert(tomorrow, tomorrow_items);
        s.occurrences.insert(
            tomorrow,
            vec![Occurrence { title: "Dentist".into(), kind: "fixed".into(), start: Some(t(14, 0)), end: Some(t(15, 0)) }],
        );
        (s, fx)
    }

    #[test]
    fn reschedule_day_wants_the_afternoon_moved_and_the_essay_kept() {
        let (s, fx) = sick_day(true);
        let v = reschedule_day(&s, &fx);
        assert!(v.pass, "{:?}", v.failures);
        let (s, fx) = sick_day(false);
        assert!(!reschedule_day(&s, &fx).pass);
    }

    #[test]
    fn reschedule_day_refuses_a_lost_task_or_a_clash() {
        let (mut s, fx) = sick_day(true);
        s.nodes.retain(|n| n.id != 2);
        assert!(!reschedule_day(&s, &fx).pass, "laundry was deleted");

        let (mut s, fx) = sick_day(true);
        s.days.get_mut(&date(2026, 10, 21)).unwrap()[0] = item(4, Some(t(14, 30)), Some(t(15, 30)), None);
        let v = reschedule_day(&s, &fx);
        assert!(v.failures.iter().any(|f| f.contains("overlaps")), "{:?}", v.failures);
    }

    fn swim(days: i64, quiet: bool) -> (Snapshot, Fixture) {
        let mut fx = fixture(&[("until", date(2026, 12, 12))]);
        fx.lists.insert("calendar".into(), vec![1]);
        let school = Entry { id: 1, title: "School".into(), kind: "fixed".into(), days: 31, ..Default::default() };
        let swim = Entry {
            id: 2,
            title: "Swim practice".into(),
            kind: "busy".into(),
            quiet,
            start: Some(t(17, 0)),
            end: Some(t(18, 30)),
            days,
            until: Some(date(2026, 12, 12)),
            ..Default::default()
        };
        (Snapshot { calendar: vec![school, swim], ..Default::default() }, fx)
    }

    #[test]
    fn recurring_calendar_wants_one_quiet_tue_thu_entry() {
        let (s, fx) = swim(TUE | THU, true);
        assert!(recurring_calendar(&s, &fx).pass);
        let (s, fx) = swim(TUE, true);
        assert!(!recurring_calendar(&s, &fx).pass);
        let (s, fx) = swim(TUE | THU, false);
        assert!(!recurring_calendar(&s, &fx).pass);
        let (mut s, fx) = swim(TUE | THU, true);
        let mut twin = s.calendar[1].clone();
        twin.id = 3;
        s.calendar.push(twin);
        assert!(!recurring_calendar(&s, &fx).pass, "two entries where one was asked");
    }

    fn wake(calls: Vec<Call>) -> (Snapshot, Fixture) {
        let mut fx = Fixture::default();
        fx.ids.insert("lab".into(), 1);
        let nodes = vec![node(1, None, "Lab report"), node(2, Some(1), "Outline"), node(3, Some(1), "Draft"), node(9, None, "Other")];
        let session = Session { kind: "Trigger".into(), outcome: "ok".into(), turns: 2, calls, ..Default::default() };
        (Snapshot { nodes, sessions: vec![session], ..Default::default() }, fx)
    }

    fn call(name: &str, args: serde_json::Value, is_error: bool) -> Call {
        Call { name: name.into(), args, result: serde_json::json!({}), is_error }
    }

    #[test]
    fn checkin_follow_up_wants_a_waiting_say_on_the_tree() {
        let (s, fx) = wake(vec![call("say", serde_json::json!({"text": "hi", "task_ids": [3], "await_min": 30}), false)]);
        assert!(checkin_follow_up(&s, &fx).pass);
        let (s, fx) = wake(vec![call("say", serde_json::json!({"text": "hi", "task_ids": [3]}), false)]);
        assert!(!checkin_follow_up(&s, &fx).pass, "no await_min");
        let (s, fx) = wake(vec![call("say", serde_json::json!({"text": "hi", "task_ids": [9], "await_min": 30}), false)]);
        assert!(!checkin_follow_up(&s, &fx).pass, "names another task");
        let (s, fx) = wake(vec![
            call("task_update", serde_json::json!({"id": 3}), true),
            call("say", serde_json::json!({"text": "hi", "await_min": 30}), false),
        ]);
        assert!(!checkin_follow_up(&s, &fx).pass, "a refused call");
        let (s, fx) = wake(vec![call("stay_quiet", serde_json::json!({"reason": "x"}), false)]);
        assert!(!checkin_follow_up(&s, &fx).pass);
    }

    fn night(order: &[i64], notes: usize, wakes: usize) -> (Snapshot, Fixture) {
        let target = date(2026, 10, 21);
        let mut fx = fixture(&[("target", target)]);
        fx.lists.insert("pressing".into(), vec![1, 2]);
        let nodes = vec![
            node(1, None, "Lab writeup"),
            node(11, Some(1), "Data table"),
            node(2, None, "Problem set"),
            node(3, None, "Notes review"),
            node(4, None, "Study plan"),
            node(41, Some(4), "Topics"),
        ];
        let items = order.iter().enumerate().map(|(p, id)| item(*id, None, None, Some(p as i64))).collect();
        let calls = (0..notes).map(|_| call(NOTES_TOOL, serde_json::json!({}), false)).collect();
        let session = Session { kind: "Nightly".into(), calls, ..Default::default() };
        let mut s = Snapshot { nodes, sessions: vec![session], wakes_laid: wakes, ..Default::default() };
        s.days.insert(target, items);
        (s, fx)
    }

    #[test]
    fn nightly_plan_wants_pressing_leaves_first_one_note_and_no_wakes() {
        let (s, fx) = night(&[11, 2, 41, 3], 1, 0);
        let v = nightly_plan(&s, &fx);
        assert!(v.pass, "{:?}", v.failures);
        assert!(!nightly_plan(&night(&[1, 2, 3], 1, 0).0, &fx).pass, "a parent on the list");
        assert!(!nightly_plan(&night(&[3, 41, 11, 2], 1, 0).0, &fx).pass, "pressing work comes last");
        assert!(!nightly_plan(&night(&[11, 2], 2, 0).0, &fx).pass, "notes written twice");
        assert!(!nightly_plan(&night(&[11, 2], 1, 1).0, &fx).pass, "a wake-up laid");
        assert!(!nightly_plan(&night(&[], 1, 0).0, &fx).pass, "an empty list");
    }

    fn essay(parts: &[(&str, u32, &str)], root_min: u32, progress: u32) -> (Snapshot, Fixture) {
        let mut fx = Fixture::default();
        fx.ids.insert("essay".into(), 1);
        let mut root = node(1, None, "History essay");
        root.minutes = Some(root_min);
        root.progress = progress;
        let mut nodes = vec![root];
        for (i, (title, min, state)) in parts.iter().enumerate() {
            let mut n = node(i as i64 + 2, Some(1), title);
            n.minutes = Some(*min);
            n.state = (*state).into();
            nodes.push(n);
        }
        (Snapshot { nodes, ..Default::default() }, fx)
    }

    #[test]
    fn breakdown_progress_wants_four_parts_and_derived_size() {
        let good = [("Outline", 30, "done"), ("Drafting", 35, "open"), ("Citations", 15, "open"), ("Proofreading", 10, "open")];
        let (s, fx) = essay(&good, 90, 33);
        let v = breakdown_progress(&s, &fx);
        assert!(v.pass, "{:?}", v.failures);
        assert!(!breakdown_progress(&essay(&good, 90, 0).0, &fx).pass, "no progress");
        assert!(!breakdown_progress(&essay(&good, 60, 33).0, &fx).pass, "size not derived");
        let open_outline = [("Outline", 30, "open"), ("Drafting", 35, "open"), ("Citations", 15, "open"), ("Proofreading", 10, "open")];
        assert!(!breakdown_progress(&essay(&open_outline, 90, 33).0, &fx).pass);
        let named = [("Outline", 30, "done"), ("Draft the essay from the outline", 35, "open"), ("Citations", 15, "open"), ("Proofreading", 10, "open")];
        let v = breakdown_progress(&essay(&named, 90, 33).0, &fx);
        assert!(v.pass, "a part that mentions the outline is still a part: {:?}", v.failures);
        let long = [("Outline", 30, "done"), ("Drafting", 90, "open"), ("Citations", 15, "open"), ("Proofreading", 10, "open")];
        assert!(!breakdown_progress(&essay(&long, 145, 20).0, &fx).pass, "parts far over an hour");
    }
}
