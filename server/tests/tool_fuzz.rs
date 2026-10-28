use note_server::tools::{dispatch, PreparedVectors, SessionKind, ToolCtx, MAX_ARGS_BYTES};
use proptest::prelude::*;

fn setup() -> (rusqlite::Connection, tempfile::TempDir) {
    let conn = note_server::db::open_memory().unwrap();
    conn.execute(
        "INSERT INTO users (username, pass_hash, role) VALUES ('aitest', 'x', 'member')",
        [],
    )
    .unwrap();
    let tmpl = note_server::templates::Template {
        events: vec![
            note_server::templates::TemplateEvent {
                kind: "nudge".into(), time: "09:00".into(),
                days: vec!["mon".into()], flexibility: Some("drop".into()),
                slide_window_min: Some(30), channel: "push".into(), ..Default::default()
            },
            note_server::templates::TemplateEvent {
                kind: "checkin_call".into(), time: "10:00".into(),
                days: vec!["mon".into()], flexibility: Some("slide".into()),
                slide_window_min: Some(30), channel: "push".into(), ..Default::default()
            },
            note_server::templates::TemplateEvent {
                kind: "Work time".into(), time: "13:30".into(),
                days: vec!["mon".into()], entry: note_server::templates::Entry::Block,
                end_time: Some("16:30".into()), channel: "push".into(), ..Default::default()
            },
        ],
    };
    let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
    note_server::plan::generate(&conn, 1, &tmpl, date).unwrap();
    (conn, tempfile::tempdir().unwrap())
}

fn snapshot(conn: &rusqlite::Connection) -> Vec<i64> {
    ["users", "tasks", "plans", "events", "memory_index"]
        .iter()
        .map(|t| {
            conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap()
        })
        .collect()
}

fn event_state(conn: &rusqlite::Connection) -> Vec<(i64, String, String)> {
    let mut stmt = conn
        .prepare("SELECT id, wall_time, status FROM events ORDER BY id")
        .unwrap();
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    rows.collect::<rusqlite::Result<_>>().unwrap()
}

fn memory_files(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let root = dir.join("memory");
    let Ok(walk) = std::fs::read_dir(&root) else { return out };
    for user in walk.flatten() {
        for cat in std::fs::read_dir(user.path()).into_iter().flatten().flatten() {
            for f in std::fs::read_dir(cat.path()).into_iter().flatten().flatten() {
                out.push(f.path().to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

fn top_level_names(dir: &std::path::Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

fn arb_name() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => prop_oneof![
            Just("task_find".to_string()), Just("task_save".to_string()),
            Just("task_remove".to_string()),
            Just("memory_find".to_string()), Just("memory_save".to_string()),
            Just("memory_remove".to_string()), Just("context_edit".to_string()),
            Just("day_find".to_string()), Just("day_save".to_string()), Just("wake_save".to_string()),
            Just("day_remove".to_string()),
            Just("calendar_save".to_string()), Just("goal_save".to_string()),
            Just("say".to_string()), Just("stay_quiet".to_string()),
            Just("task_update".to_string()), Just("trigger_set".to_string()),
        ],
        1 => "[a-z_]{1,20}",
        1 => ".*",
    ]
}

fn arb_args() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("{}".to_string()),
        Just("[]".to_string()),
        Just("null".to_string()),
        Just("not json at all".to_string()),
        ".*",
        Just(r#"{"tasks": 42}"#.to_string()),
        Just(r#"{"tasks": [{"title": null}]}"#.to_string()),
        Just(r#"{"tasks": [{"title": 42, "minutes": "lots"}]}"#.to_string()),
        Just(r#"{"items": [{"id": "e1", "at": "+9223372036854775807min"}]}"#.to_string()),
        Just(r#"{"items": [{"id": "e-1", "snooze_min": -9223372036854775808}]}"#.to_string()),
        Just(r#"{"items": [{"id": "e2", "minutes": 9223372036854775807}]}"#.to_string()),
        Just(r#"{"items":[{"kind":"semantic","title":"s","body":"b","extra":1}]}"#.to_string()),
        Just(r#"{"items":[{"kind":"../../../etc","title":"s","body":"b"}]}"#.to_string()),
        Just(r#"{"items":[{"id":"../../../../etc/passwd","title":"s","body":"b"}]}"#.to_string()),
        Just(r#"{"items":[{"id":"../../../../../etc/passwd","kind":"note","title":"s"}]}"#.to_string()),
        Just(r#"{"id":"..%2f..%2f..%2fetc%2fpasswd"}"#.to_string()),
        Just(r#"{"ids":["../../../../etc/passwd"]}"#.to_string()),
        Just(r#"{"query":"'; DROP TABLE tasks; --"}"#.to_string()),
        Just(r#"{"find":"a","replace":"b","append":"c"}"#.to_string()),
        Just(r#"{"date":"2026-08-31","items":[{"routine":"x","at":"25:99","flexibility":"drop"}]}"#.to_string()),
        Just(r#"{"items":[{"wake":"look again","at":"+90min"}]}"#.to_string()),
        Just(r#"{"note":"look again","at":"+90min"}"#.to_string()),
        Just(r#"{"note":"look again","at":"+90min","id":"e1"}"#.to_string()),
        Just(r#"{"date":"2026-08-31","carry":true,"items":[]}"#.to_string()),
        Just(r#"{"items":[{"wake":"look again","at":"25:99"}]}"#.to_string()),
        Just(r#"{"items":[{"wake":"look again","at":"+9223372036854775807min"}]}"#.to_string()),
        Just(r#"{"items":[{"wake":"look again","at":"+90min","thread":999}]}"#.to_string()),
        Just(r#"{"items":[{"wake":"x","task":1,"at":"+90min","cancel_on":"task"}]}"#.to_string()),
        Just(r#"{"items":[],"extra_wakes":{"n":9223372036854775807,"reason":"x"}}"#.to_string()),
        Just(r#"{"ids":["e1","t1","c1",null,42]}"#.to_string()),
        Just(r#"{"text":"anything at all"}"#.to_string()),
        Just(r#"{"items":[{"id":"00000000-0000-4000-8000-000000000001"}]}"#.to_string()),
        Just(format!(r#"{{"text":"{}"}}"#, "あ".repeat(201))),
        Just(format!(r#"{{"tasks":[{{"title":"{}"}}]}}"#, "x".repeat(MAX_ARGS_BYTES))),
        Just(r#"{"tasks":[{"title":"a real task"}]}"#.to_string()),
        Just(r#"{"tasks":[{"title":"a real task","notify":"chat"}]}"#.to_string()),
        Just(r#"{"tasks":[{"id":1,"notify":"whenever"}]}"#.to_string()),
        Just(r#"{"tasks":[{"title":"deep","children":[{"title":"a","children":[{"title":"b","children":[{"title":"c","children":[{"title":"d","children":["e"]}]}]}]}]}]}"#.to_string()),
        Just(r#"{"items":[{"id":"e1","snooze_min":10}]}"#.to_string()),
        Just(r#"{"items":[{"kind":"semantic","title":"s","body":"b"}]}"#.to_string()),
    ]
}

// weight 1: random pairings; weight 1: known-good calls that reach a handler
// and succeed against the fixture, so the no-partial-write property is
// exercised on real write paths, not just rejections
fn arb_call() -> impl Strategy<Value = (String, String)> {
    let good = |n: &str, a: &str| Just((n.to_string(), a.to_string()));
    prop_oneof![
        1 => (arb_name(), arb_args()),
        1 => prop_oneof![
            good("task_save", r#"{"tasks":[{"title":"a real task"}]}"#),
            good("task_save", r#"{"tasks":[{"title":"a quiet task","notify":"none","children":["part one","part two"]}]}"#),
            good("task_find", r#"{"query":"task"}"#),
            good("memory_save", r#"{"items":[{"kind":"semantic","title":"s","body":"b"}]}"#),
            good("memory_find", r#"{"query":"s"}"#),
            good("note_save", r#"{"title":"buy stamps"}"#),
            good("day_save", r#"{"date":"2026-08-31","items":[{"id":"e1","snooze_min":10}]}"#),
            good("day_remove", r#"{"date":"2026-08-31","ids":["e1"]}"#),
            good("day_save", r#"{"date":"2026-08-31","items":[{"id":"e2","at":"10:10"}]}"#),
            good("day_save", r#"{"date":"2026-08-31","items":[{"id":"e3","at":"13:00","minutes":120}]}"#),
            good("context_edit", r#"{"append":"a standing note"}"#),
            good("day_save", r#"{"date":"2026-08-31","carry":true,"items":[]}"#),
            good("wake_save", r#"{"note":"look again","at":"+90min"}"#),
            good("wake_save", r#"{"items":[{"note":"did they answer?","at":"+120min","cancel_on":"reply"}]}"#),
            good("wake_save", r#"{"extra_wakes":{"n":1,"reason":"they agreed"}}"#),
            good("day_save", r#"{"date":"2026-08-31","items":[{"routine":"extra","at":"11:30","flexibility":"slide"}]}"#),
        ],
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn dispatch_is_total_and_never_partially_writes(
        (name, raw) in arb_call(),
        kind_idx in 0..3usize,
    ) {
        let (conn, tmp) = setup();
        // data_dir is a dedicated subdirectory so any path escaping it lands
        // in tmp, which this test owns exclusively and watches for changes
        let data = tmp.path().join("data");
        std::fs::create_dir(&data).unwrap();
        let ctx = ToolCtx {
            config_dir: &data, data_dir: &data,
            user_id: 1, username: "aitest", vectors: PreparedVectors::default(),
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            share: None,
            share_thread: None,
        };
        let kind = [SessionKind::Nightly, SessionKind::Trigger, SessionKind::Talk][kind_idx];
        let before_counts = snapshot(&conn);
        let before_events = event_state(&conn);
        let before_files = memory_files(&data);
        let before_parent = top_level_names(tmp.path());

        let result = dispatch(&conn, &ctx, kind, &name, &raw);

        if result.is_err() {
            prop_assert_eq!(before_counts, snapshot(&conn), "failed call changed row counts");
            prop_assert_eq!(before_events, event_state(&conn), "failed call changed events");
            prop_assert_eq!(before_files, memory_files(&data), "failed call changed memory files");
        }
        // path-shaped ids/categories must never escape the data dir
        prop_assert_eq!(before_parent, top_level_names(tmp.path()), "write escaped the data dir");
        prop_assert!(!tmp.path().parent().unwrap().join("etc").exists());
        // every event still carries a well-formed wall time
        for (_, wall, status) in event_state(&conn) {
            prop_assert!(wall.len() == 5 && wall.as_bytes()[2] == b':', "bad wall_time {wall}");
            prop_assert!(
                ["pending", "fired", "snoozed", "dropped", "done"].contains(&status.as_str())
            );
        }
    }
}
