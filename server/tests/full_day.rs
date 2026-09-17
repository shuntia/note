use note_server::agent::SessionDeps;
use note_server::providers::{mock::MockLLM, ChatResponse, ToolCall};
use note_server::tools::SessionKind;
use std::sync::Mutex;

fn write(dir: &std::path::Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

#[test]
fn a_full_simulated_day() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "defaults/user.toml",
        "display_name = \"Aki\"\ntimezone = \"Asia/Tokyo\"\ntemplate = \"default\"\n",
    );
    write(
        tmp.path(),
        "defaults/templates/default.toml",
        "[[events]]\nkind='checkin_call'\ntime='09:00'\ndays=['mon','tue','wed','thu','fri','sat','sun']\nflexibility='slide'\nslide_window_min=60\nchannel='voice'\n",
    );
    write(tmp.path(), "defaults/prompts/persona.md", "you are note");
    write(tmp.path(), "defaults/prompts/planning.md", "plan the day, then debrief");
    let conn = note_server::db::open_memory().unwrap();
    conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", [])
        .unwrap();
    let db = Mutex::new(conn);

    // --- 03:30 JST, 2026-08-31 (Monday): nightly run.
    // The agent inserts an afternoon nudge, then debriefs.
    let nightly_llm = MockLLM::scripted(vec![
        ChatResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "n1".into(),
                name: "schedule_insert".into(),
                args: r#"{"date":"2026-08-31","kind":"nudge","time":"16:00","flexibility":"drop","channel":"push"}"#.into(),
            }],
        },
        ChatResponse {
            text: "yesterday was quiet. today: checkin at nine, nudge at four.".into(),
            tool_calls: vec![],
        },
    ]);
    let now: jiff::Timestamp = "2026-08-30T18:30:00Z".parse().unwrap(); // 03:30 JST on the 31st
    {
        let deps = SessionDeps {
            db: &db,
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            llm: &nightly_llm,
            embeddings: None,
            task_scope: None,
            token_id: None,
        };
        assert_eq!(note_server::nightly::due(&db.lock().unwrap(), tmp.path(), now).unwrap().len(), 1);
        note_server::nightly::run_for_user(&deps, 1, "aki", now).unwrap();
    }
    {
        let conn = db.lock().unwrap();
        let kinds: Vec<String> = conn
            .prepare(
                "SELECT e.kind FROM events e JOIN plans p ON p.id = e.plan_id WHERE p.date='2026-08-31' ORDER BY e.wall_time",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(kinds, vec!["checkin_call".to_string(), "nudge".to_string()]);
        let debrief: String = conn
            .query_row("SELECT content FROM debriefs WHERE user_id=1 AND date='2026-08-31'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(debrief.contains("checkin at nine"));
    }
    // the agent planned against the simulated day, not the wall clock
    let nightly_system = &nightly_llm.seen()[0].system;
    assert!(nightly_system.contains("2026-08-31 03:30 (Asia/Tokyo)"), "{nightly_system}");
    let plan_section = nightly_system.split("# Today's plan").nth(1).unwrap();
    assert!(plan_section.contains("09:00 checkin_call [pending] via voice"), "{plan_section}");

    // --- 10:00 JST: the user talks; the agent captures a task and remembers a fact.
    let talk_llm = MockLLM::scripted(vec![
        ChatResponse {
            text: String::new(),
            tool_calls: vec![
                ToolCall {
                    id: "t1".into(),
                    name: "task_create".into(),
                    args: r#"{"title":"submit report"}"#.into(),
                },
                ToolCall {
                    id: "t2".into(),
                    name: "memory_write".into(),
                    args: r#"{"op":"add","category":"episodic","summary":"report due friday","body":"mentioned during monday talk"}"#.into(),
                },
            ],
        },
        ChatResponse { text: "got it — report's on the list.".into(), tool_calls: vec![] },
    ]);
    {
        let deps = SessionDeps {
            db: &db,
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            llm: &talk_llm,
            embeddings: None,
            task_scope: None,
            token_id: None,
        };
        let out = note_server::agent::run_session(
            &deps,
            1,
            "aki",
            SessionKind::Talk,
            "2026-08-31T01:00:00Z".parse().unwrap(), // 10:00 JST
            &[],
            "the report is due friday, remind me",
        )
        .unwrap();
        assert!(out.reply.contains("on the list"));
    }
    {
        let conn = db.lock().unwrap();
        let title: String = conn
            .query_row("SELECT title FROM tasks WHERE user_id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "submit report");
        let hits = note_server::memory::query(&conn, "aki", "report", 5, None).unwrap();
        assert_eq!(hits.len(), 1);
    }

    // --- 11:00 JST: only the 09:00 checkin is due; the 16:00 nudge must not fire yet.
    let midday: jiff::Timestamp = "2026-08-31T02:00:00Z".parse().unwrap(); // 11:00 JST
    let fired_midday = {
        let conn = db.lock().unwrap();
        note_server::runner::fire_due(&conn, tmp.path(), midday).unwrap()
    };
    {
        let kinds: Vec<String> = fired_midday.iter().map(|f| f.kind.clone()).collect();
        assert_eq!(kinds, vec!["checkin_call".to_string()]);
        let conn = db.lock().unwrap();
        let pending: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE status='pending'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pending, 1);
    }

    // --- 16:05 JST: the nudge comes due; the already-fired checkin is not re-fired.
    let fire_at: jiff::Timestamp = "2026-08-31T07:05:00Z".parse().unwrap(); // 16:05 JST
    let fired = {
        let conn = db.lock().unwrap();
        note_server::runner::fire_due(&conn, tmp.path(), fire_at).unwrap()
    };
    {
        let kinds: Vec<String> = fired.iter().map(|f| f.kind.clone()).collect();
        assert_eq!(kinds, vec!["nudge".to_string()]);
    }
    {
        let conn = db.lock().unwrap();
        let logged: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='event_fired'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(logged, 2);
    }
}
