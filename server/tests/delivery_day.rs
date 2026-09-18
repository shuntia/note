mod common;

use note_server::channels::voice::VoiceChannel;
use note_server::channels::{self, mock::MockChannel, ws::ClientHub, ws::WsChannel, Channel};
use note_server::providers::{mock::MockLLM, ChatResponse};
use std::sync::{Arc, Mutex};

/// One simulated evening-to-day for a JST user: nightly debrief at 03:30,
/// morning debrief delivery over WS, a daytime nudge falling back to the mock
/// push channel, a voice check-in that rings the phone, and a refused call
/// falling back down the push ladder.
#[test]
fn delivery_reaches_the_user_through_the_ladder() {
    let conn = note_server::db::open_memory().unwrap();
    let uid = {
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    };
    let tmp = tempfile::tempdir().unwrap();
    let write = |rel: &str, c: &str| {
        let p = tmp.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, c).unwrap();
    };
    write(
        "defaults/user.toml",
        "display_name = \"Aki\"\ntimezone = \"Asia/Tokyo\"\ntemplate = \"default\"\nnightly_time = \"03:00\"\nphone_number = \"+819012345678\"\n",
    );
    write("defaults/prompts/persona.md", "you are note");
    write("defaults/prompts/planning.md", "plan the day");
    write(
        "defaults/templates/default.toml",
        concat!(
            "[[events]]\nkind = \"debrief\"\ntime = \"07:30\"\n",
            "days = [\"mon\",\"tue\",\"wed\",\"thu\",\"fri\",\"sat\",\"sun\"]\n",
            "flexibility = \"fixed\"\nchannel = \"push\"\n",
            "[[events]]\nkind = \"nudge\"\ntime = \"10:00\"\n",
            "days = [\"mon\",\"tue\",\"wed\",\"thu\",\"fri\",\"sat\",\"sun\"]\n",
            "flexibility = \"drop\"\nchannel = \"push\"\n",
            "[[events]]\nkind = \"checkin_call\"\ntime = \"16:00\"\n",
            "days = [\"mon\",\"tue\",\"wed\",\"thu\",\"fri\",\"sat\",\"sun\"]\n",
            "flexibility = \"fixed\"\nchannel = \"voice\"\n",
            "[[events]]\nentry = \"block\"\nkind = \"Work time\"\ntime = \"09:30\"\nend_time = \"12:30\"\n",
            "days = [\"mon\",\"tue\",\"wed\",\"thu\",\"fri\",\"sat\",\"sun\"]\n",
            "[[events]]\nkind = \"stretch\"\ntime = \"10:30\"\n",
            "days = [\"mon\",\"tue\",\"wed\",\"thu\",\"fri\",\"sat\",\"sun\"]\n",
            "flexibility = \"drop\"\nchannel = \"push\"\nalert = false\n",
        ),
    );

    let db = Arc::new(Mutex::new(conn));
    let hub = Arc::new(ClientHub::new());
    let push = Arc::new(MockChannel::new("mockpush"));
    let ladder: Vec<Arc<dyn Channel>> = vec![Arc::new(WsChannel::new(hub.clone())), push.clone()];

    // --- 03:30 JST 2026-08-31 (= 18:30Z 08-30): nightly writes plan + debrief.
    let llm = MockLLM::scripted(vec![ChatResponse {
        text: "quiet day; dentist rolled forward".into(),
        tool_calls: vec![],
    }]);
    let deps = note_server::agent::SessionDeps {
        db: &db,
        config_dir: tmp.path(),
        data_dir: tmp.path(),
        llm: &llm,
        embeddings: None,
        task_scope: None,
        inbox_source: None,
        memory_source: None,
        token_id: None,
        thread_note: None,
    };
    let nightly_now: jiff::Timestamp = "2026-08-30T18:30:00Z".parse().unwrap();
    note_server::nightly::run_for_user(&deps, uid, "aki", nightly_now).unwrap();

    // --- 07:31 JST: debrief event fires and reaches the connected client.
    let (conn_id, mut rx) = hub.register(uid).unwrap();
    let morning: jiff::Timestamp = "2026-08-30T22:31:00Z".parse().unwrap(); // 07:31 JST 08-31
    let fired = {
        let conn = db.lock().unwrap();
        note_server::runner::fire_due(&conn, tmp.path(), morning).unwrap()
    };
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].kind, "debrief");
    channels::deliver_event(&db, &ladder, None, &fired[0]);
    let text = rx.try_recv().unwrap();
    assert!(text.contains("dentist rolled forward"), "ws frame: {text}");
    assert!(push.seen().is_empty());

    // --- 10:01 JST, client gone: the nudge falls through to the mock channel.
    drop(rx);
    hub.unregister(uid, conn_id);
    let midmorning: jiff::Timestamp = "2026-08-31T01:01:00Z".parse().unwrap(); // 10:01 JST
    let fired = {
        let conn = db.lock().unwrap();
        note_server::runner::fire_due(&conn, tmp.path(), midmorning).unwrap()
    };
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].kind, "nudge");
    channels::deliver_event(&db, &ladder, None, &fired[0]);
    assert_eq!(push.seen().len(), 1);
    assert_eq!(push.seen()[0].0, uid);

    // --- 13:00 JST: the block and the silent routine are both past due and
    // neither reaches a channel.
    let afternoon: jiff::Timestamp = "2026-08-31T04:00:00Z".parse().unwrap(); // 13:00 JST
    {
        let conn = db.lock().unwrap();
        assert!(note_server::runner::fire_due(&conn, tmp.path(), afternoon).unwrap().is_empty());
        let quiet: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE kind IN ('Work time','stretch') AND status='pending'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(quiet, 2);
    }
    assert_eq!(push.seen().len(), 1);

    // --- 16:01 JST: the voice check-in rings the phone and never touches the ladder.
    let (twilio, rx) = common::one_shot("201 Created", r#"{"sid":"CA1"}"#);
    let voice = VoiceChannel::new(
        tmp.path().to_path_buf(),
        db.clone(),
        &common::voice_settings(tmp.path(), &twilio),
        common::VOICE_PUBLIC_BASE,
    )
    .unwrap();
    let afternoon: jiff::Timestamp = "2026-08-31T07:01:00Z".parse().unwrap(); // 16:01 JST
    let fired = {
        let conn = db.lock().unwrap();
        note_server::runner::fire_due(&conn, tmp.path(), afternoon).unwrap()
    };
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].channel, "voice");
    channels::deliver_event(&db, &ladder, Some(&voice), &fired[0]);
    assert_eq!(push.seen().len(), 1, "a placed call does not also push");
    let raw = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(raw.contains("To=%2B819012345678"), "call request: {raw}");
    {
        let conn = db.lock().unwrap();
        let ok: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='delivery_ok'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ok, 3);
        let status: String = conn
            .query_row("SELECT status FROM voice_calls WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "placed");
    }

    // --- 16:15 JST: Twilio refuses the next one, so the nudge takes the ladder.
    let (dead, _dead_rx) = common::one_shot("500 Internal Server Error", "{}");
    let refusing = VoiceChannel::new(
        tmp.path().to_path_buf(),
        db.clone(),
        &common::voice_settings(tmp.path(), &dead),
        common::VOICE_PUBLIC_BASE,
    )
    .unwrap();
    {
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, flexibility, slide_window_min, channel)
             SELECT plan_id, 'checkin_call', '16:15', '16:15', 'drop', 0, 'voice' FROM events LIMIT 1",
            [],
        )
        .unwrap();
    }
    let quarter_past: jiff::Timestamp = "2026-08-31T07:16:00Z".parse().unwrap(); // 16:16 JST
    let fired = {
        let conn = db.lock().unwrap();
        note_server::runner::fire_due(&conn, tmp.path(), quarter_past).unwrap()
    };
    assert_eq!(fired.len(), 1);
    channels::deliver_event(&db, &ladder, Some(&refusing), &fired[0]);
    assert_eq!(push.seen().len(), 2);
    {
        let conn = db.lock().unwrap();
        let detail: String = conn
            .query_row("SELECT detail FROM event_log WHERE kind='voice_fallback'", [], |r| r.get(0))
            .unwrap();
        assert!(detail.contains("500"), "unexpected detail: {detail}");
        let status: String = conn
            .query_row("SELECT status FROM voice_calls WHERE id = 2", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "failed");
    }

    // --- total failure degrades, never errors: fail the mock and re-nudge.
    push.set_fail(true);
    {
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, flexibility, slide_window_min, channel)
             SELECT plan_id, 'nudge', '16:30', '16:30', 'drop', 0, 'push' FROM events LIMIT 1",
            [],
        )
        .unwrap();
    }
    let late: jiff::Timestamp = "2026-08-31T07:31:00Z".parse().unwrap(); // 16:31 JST
    let fired = {
        let conn = db.lock().unwrap();
        note_server::runner::fire_due(&conn, tmp.path(), late).unwrap()
    };
    assert_eq!(fired.len(), 1);
    channels::deliver_event(&db, &ladder, None, &fired[0]);
    {
        let conn = db.lock().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='delivery_degraded'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1);
    }
}
