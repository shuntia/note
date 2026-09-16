use note_server::tools::{dispatch, PreparedVectors, SessionKind, ToolCtx};
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Op {
    TaskCreate(String),
    TaskSetState(i64, String),
    MemAdd(String, String),
    MemUpdate(usize, String),
    MemSupersede(usize, String),
    MemQuery(String),
    Slide(i64, i64),
    Snooze(i64, i64),
    Drop(i64),
    CtxAppend(String),
    CtxReplace(String, String),
    Insert(String, String),
    TaskSplit(i64),
    TaskDuration(i64, u32),
    TaskSetNow(i64, bool),
    Reshape(i64, String, String),
}

fn arb_op() -> impl Strategy<Value = Op> {
    let word = "[a-z]{1,12}";
    prop_oneof![
        word.prop_map(Op::TaskCreate),
        (1..6i64, prop_oneof![
            Just("open".to_string()), Just("done".to_string()),
            Just("dropped".to_string()), Just("bogus".to_string()),
        ]).prop_map(|(id, s)| Op::TaskSetState(id, s)),
        (word, word).prop_map(|(s, b)| Op::MemAdd(s, b)),
        (0..8usize, word).prop_map(|(i, s)| Op::MemUpdate(i, s)),
        (0..8usize, word).prop_map(|(i, s)| Op::MemSupersede(i, s)),
        word.prop_map(Op::MemQuery),
        (1..5i64, -200..200i64).prop_map(|(e, m)| Op::Slide(e, m)),
        (1..5i64, -10..100i64).prop_map(|(e, m)| Op::Snooze(e, m)),
        (1..5i64).prop_map(Op::Drop),
        word.prop_map(Op::CtxAppend),
        (word, word).prop_map(|(f, r)| Op::CtxReplace(f, r)),
        (1..6i64).prop_map(Op::TaskSplit),
        (1..6i64, prop_oneof![Just(5u32), Just(10), Just(23), Just(0)])
            .prop_map(|(id, d)| Op::TaskDuration(id, d)),
        (1..6i64, any::<bool>()).prop_map(|(id, f)| Op::TaskSetNow(id, f)),
        (1..5i64, "([01][0-9]|2[0-3]):[0-5][0-9]", "([01][0-9]|2[0-3]):[0-5][0-9]")
            .prop_map(|(e, s, t)| Op::Reshape(e, s, t)),
        (prop_oneof![Just("2026-08-31".to_string()), Just("2026-09-01".to_string())], "([01][0-9]|2[0-3]):[0-5][0-9]")
            .prop_map(|(d, t)| Op::Insert(d, t)),
    ]
}

fn apply(conn: &rusqlite::Connection, ctx: &ToolCtx, op: &Op, mem_ids: &mut Vec<String>) {
    let pick = |ids: &Vec<String>, i: usize| -> String {
        if ids.is_empty() { "00000000-0000-4000-8000-000000000000".into() }
        else { ids[i % ids.len()].clone() }
    };
    let (name, raw) = match op {
        Op::TaskCreate(t) => ("task_create", format!(r#"{{"title":"{t}"}}"#)),
        Op::TaskSetState(id, s) => ("task_update", format!(r#"{{"task_id":{id},"state":"{s}"}}"#)),
        Op::MemAdd(s, b) => ("memory_write",
            format!(r#"{{"op":"add","category":"semantic","summary":"{s}","body":"{b}"}}"#)),
        Op::MemUpdate(i, s) => ("memory_write",
            format!(r#"{{"op":"update","id":"{}","summary":"{s}","body":"{s}"}}"#, pick(mem_ids, *i))),
        Op::MemSupersede(i, s) => ("memory_write",
            format!(r#"{{"op":"supersede","id":"{}","summary":"{s}","body":"{s}"}}"#, pick(mem_ids, *i))),
        Op::MemQuery(q) => ("memory_query", format!(r#"{{"query":"{q}"}}"#)),
        Op::Slide(e, m) => ("schedule_slide", format!(r#"{{"event_id":{e},"minutes":{m}}}"#)),
        Op::Snooze(e, m) => ("schedule_snooze", format!(r#"{{"event_id":{e},"minutes":{m}}}"#)),
        Op::Drop(e) => ("schedule_drop", format!(r#"{{"event_id":{e}}}"#)),
        Op::CtxAppend(t) => ("context_edit", format!(r#"{{"append":"{t}"}}"#)),
        Op::CtxReplace(f, r) => ("context_edit", format!(r#"{{"find":"{f}","replace":"{r}"}}"#)),
        Op::TaskSplit(id) => ("task_split", format!(
            r#"{{"task_id":{id},"steps":[{{"title":"a","duration_min":5}},{{"title":"b","duration_min":10}}]}}"#)),
        Op::TaskDuration(id, d) => ("task_update", format!(r#"{{"task_id":{id},"duration_min":{d}}}"#)),
        Op::TaskSetNow(id, f) => ("task_update", format!(r#"{{"task_id":{id},"is_now":{f}}}"#)),
        Op::Reshape(e, s, t) => ("schedule_reshape",
            format!(r#"{{"event_id":{e},"start":"{s}","end":"{t}"}}"#)),
        Op::Insert(d, t) => ("schedule_insert",
            format!(r#"{{"date":"{d}","kind":"extra","time":"{t}","flexibility":"slide","slide_window_min":15,"channel":"push"}}"#)),
    };
    if let Ok(out) = dispatch(conn, ctx, SessionKind::Nightly, name, &raw) {
        if name == "memory_write" {
            if let Some(id) = out["id"].as_str() {
                mem_ids.push(id.to_string());
            }
        }
    }
}

fn assert_invariants(conn: &rusqlite::Connection, data_dir: &std::path::Path) {
    // 1. Every event's wall_time is well-formed and its status/flexibility valid.
    let mut stmt = conn
        .prepare("SELECT wall_time, orig_wall_time, status, flexibility, slide_window_min FROM events")
        .unwrap();
    let rows: Vec<(String, String, String, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let minutes = |w: &str| -> i64 {
        let (h, m) = w.split_once(':').unwrap();
        h.parse::<i64>().unwrap() * 60 + m.parse::<i64>().unwrap()
    };
    for (wall, orig, status, flex, window) in rows {
        assert!(wall.len() == 5 && minutes(&wall) < 24 * 60, "bad wall_time {wall}");
        assert!(["pending", "fired", "snoozed", "dropped", "done"].contains(&status.as_str()));
        assert!(["fixed", "slide", "drop"].contains(&flex.as_str()));
        // window bound: a never-snoozed event within a positive window stays inside it;
        // snoozed events are exempt by design, so only check non-snoozed ones.
        if window > 0 && status != "snoozed" {
            assert!(
                (minutes(&wall) - minutes(&orig)).abs() <= window,
                "event slid outside window: {orig} -> {wall} (window {window})"
            );
        }
    }

    // 2. A block keeps a well-ordered range and never regains its bell; a
    //    routine never grows one.
    let mut stmt = conn.prepare("SELECT id, wall_time, end_wall_time, alert FROM events").unwrap();
    let shapes: Vec<(i64, String, Option<String>, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for (id, start, end, alert) in shapes {
        match end {
            Some(end) => {
                assert!(minutes(&end) > minutes(&start), "block {id} ends at or before {start}");
                assert_eq!(alert, 0, "block {id} regained its bell");
            }
            None => assert_eq!(alert, 1, "routine {id} lost its bell"),
        }
    }

    // 3. Task steps stay exactly one level deep and every duration is a whole
    //    number of 5-minute blocks.
    let mut stmt = conn
        .prepare(
            "SELECT c.id FROM tasks c JOIN tasks p ON c.parent_id = p.id
             WHERE p.parent_id IS NOT NULL",
        )
        .unwrap();
    let deep: Vec<i64> =
        stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert!(deep.is_empty(), "tasks nested more than one level deep: {deep:?}");
    let mut stmt = conn
        .prepare("SELECT id, duration_min, duration_source FROM tasks WHERE duration_min IS NOT NULL")
        .unwrap();
    let durs: Vec<(i64, i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for (id, d, source) in durs {
        assert!(d > 0 && d % 5 == 0, "task {id} has an unusable duration {d}");
        assert_eq!(source, "agent", "task {id} duration came from nowhere");
    }

    // 4. Now holds at most three live top-level tasks, and no step is ever in it.
    let live: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks
             WHERE user_id = 1 AND is_now = 1 AND parent_id IS NULL
               AND state IN ('open','in_progress')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(live <= 3, "Now overflowed with {live} tasks");
    let flagged_steps: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE is_now = 1 AND parent_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(flagged_steps, 0, "a step was flagged into Now");

    // 5. Memory: index rows and files agree; ids unique; supersede chains resolve.
    let mut stmt = conn
        .prepare("SELECT id, archived, path FROM memory_index WHERE user='aki'")
        .unwrap();
    let index: Vec<(String, i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mut seen = std::collections::HashSet::new();
    for (id, archived, path) in &index {
        assert!(note_server::memory::valid_id(id), "index holds bad id {id}");
        assert!(seen.insert(id.clone()), "duplicate memory id {id}");
        assert!(std::path::Path::new(path).exists(), "index points at missing file {path}");
        let in_archive = path.contains("/archive/");
        assert_eq!(in_archive, *archived == 1, "archived flag disagrees with location: {path}");
    }
    for (id, _, _) in &index {
        let f = note_server::memory::read(data_dir, "aki", id).unwrap().unwrap();
        if let Some(target) = f.supersedes {
            let t = note_server::memory::read(data_dir, "aki", &target).unwrap()
                .unwrap_or_else(|| panic!("supersede target {target} vanished"));
            assert!(t.archived, "superseded fact {target} was not archived");
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn interleaved_tool_calls_preserve_invariants(ops in proptest::collection::vec(arb_op(), 1..25)) {
        let conn = note_server::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        let tmpl = note_server::templates::Template {
            events: vec![
                note_server::templates::TemplateEvent {
                    kind: "checkin_call".into(), time: "09:00".into(),
                    days: vec!["mon".into()], flexibility: Some("slide".into()),
                    slide_window_min: Some(60), channel: "voice".into(), ..Default::default()
                },
                note_server::templates::TemplateEvent {
                    kind: "nudge".into(), time: "14:00".into(),
                    days: vec!["mon".into()], flexibility: Some("drop".into()),
                    slide_window_min: Some(0), channel: "push".into(), ..Default::default()
                },
                note_server::templates::TemplateEvent {
                    kind: "meds".into(), time: "20:00".into(),
                    days: vec!["mon".into()], flexibility: Some("fixed".into()),
                    slide_window_min: Some(0), channel: "push".into(), ..Default::default()
                },
                note_server::templates::TemplateEvent {
                    kind: "Work time".into(), time: "09:30".into(),
                    days: vec!["mon".into()], entry: note_server::templates::Entry::Block,
                    end_time: Some("12:30".into()), channel: "push".into(), ..Default::default()
                },
            ],
        };
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        note_server::plan::generate(&conn, 1, &tmpl, date).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            config_dir: tmp.path(), data_dir: tmp.path(),
            user_id: 1, username: "aki", vectors: PreparedVectors::default(),
            task_scope: None,
        };

        let mut mem_ids = Vec::new();
        for op in &ops {
            apply(&conn, &ctx, op, &mut mem_ids);
            assert_invariants(&conn, tmp.path());
        }
    }
}
