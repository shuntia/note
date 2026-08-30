use note_server::tools::{dispatch, PreparedVectors, SessionKind, ToolCtx, MAX_ARGS_BYTES};
use proptest::prelude::*;

fn setup() -> (rusqlite::Connection, tempfile::TempDir) {
    let conn = note_server::db::open_memory().unwrap();
    conn.execute(
        "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
        [],
    )
    .unwrap();
    let tmpl = note_server::templates::Template {
        events: vec![
            note_server::templates::TemplateEvent {
                kind: "nudge".into(), time: "09:00".into(),
                days: vec!["mon".into()], flexibility: "drop".into(),
                slide_window_min: 30, channel: "push".into(),
            },
            note_server::templates::TemplateEvent {
                kind: "checkin_call".into(), time: "10:00".into(),
                days: vec!["mon".into()], flexibility: "slide".into(),
                slide_window_min: 30, channel: "voice".into(),
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
            Just("task_create".to_string()), Just("task_update".to_string()),
            Just("memory_query".to_string()), Just("memory_read".to_string()),
            Just("memory_write".to_string()), Just("context_edit".to_string()),
            Just("schedule_slide".to_string()), Just("schedule_snooze".to_string()),
            Just("schedule_drop".to_string()), Just("schedule_insert".to_string()),
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
        Just(r#"{"title": 42}"#.to_string()),
        Just(r#"{"title": null}"#.to_string()),
        Just(r#"{"event_id": 1, "minutes": 9223372036854775807}"#.to_string()),
        Just(r#"{"event_id": -1, "minutes": -9223372036854775808}"#.to_string()),
        Just(r#"{"op":"add","category":"semantic","summary":"s","body":"b","extra":1}"#.to_string()),
        Just(r#"{"op":"add","category":"../../../etc","summary":"s","body":"b"}"#.to_string()),
        Just(r#"{"op":"update","id":"../../../../etc/passwd","summary":"s","body":"b"}"#.to_string()),
        Just(r#"{"op":"update","id":"../../../../../etc/passwd","summary":"s","body":"b"}"#.to_string()),
        Just(r#"{"id":"..%2f..%2f..%2fetc%2fpasswd"}"#.to_string()),
        Just(r#"{"query":"'; DROP TABLE tasks; --"}"#.to_string()),
        Just(r#"{"find":"a","replace":"b","append":"c"}"#.to_string()),
        Just(r#"{"date":"2026-08-31","kind":"x","time":"25:99","flexibility":"drop","channel":"push"}"#.to_string()),
        Just(format!(r#"{{"title":"{}"}}"#, "x".repeat(MAX_ARGS_BYTES))),
        Just(r#"{"title":"a real task"}"#.to_string()),
        Just(r#"{"event_id":1,"minutes":10}"#.to_string()),
        Just(r#"{"op":"add","category":"semantic","summary":"s","body":"b"}"#.to_string()),
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
            good("task_create", r#"{"title":"a real task"}"#),
            good("memory_write", r#"{"op":"add","category":"semantic","summary":"s","body":"b"}"#),
            good("memory_query", r#"{"query":"s"}"#),
            good("schedule_snooze", r#"{"event_id":1,"minutes":10}"#),
            good("schedule_drop", r#"{"event_id":1}"#),
            good("schedule_slide", r#"{"event_id":2,"minutes":10}"#),
            good("context_edit", r#"{"append":"a standing note"}"#),
            good("schedule_insert", r#"{"date":"2026-08-31","kind":"extra","time":"11:30","flexibility":"slide","slide_window_min":15,"channel":"push"}"#),
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
            user_id: 1, username: "aki", vectors: PreparedVectors::default(),
        };
        let kind = [SessionKind::Nightly, SessionKind::Checkin, SessionKind::Talk][kind_idx];
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
