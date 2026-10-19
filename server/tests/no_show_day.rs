use rusqlite::Connection;

fn write(dir: &std::path::Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

/// 2026-10-19 is a Monday. The user was last seen the evening before and never
/// opens the app on the day itself.
fn at(wall: &str) -> jiff::Timestamp {
    let tokyo = jiff::tz::TimeZone::get("Asia/Tokyo").unwrap();
    let (h, m) = wall.split_once(':').unwrap();
    jiff::civil::date(2026, 10, 19)
        .at(h.parse().unwrap(), m.parse().unwrap(), 0, 0)
        .to_zoned(tokyo)
        .unwrap()
        .timestamp()
}

fn wall(t: jiff::Timestamp) -> String {
    t.to_zoned(jiff::tz::TimeZone::get("Asia/Tokyo").unwrap()).strftime("%H:%M").to_string()
}

/// What a fired idle check's session did: it said something, and ended.
fn settle_as_said(conn: &Connection, uid: i64, id: i64, now: jiff::Timestamp) {
    let done = now + jiff::Span::new().minutes(1);
    conn.execute(
        "UPDATE events SET status = 'done', fired_at = ?1, decided_at = ?2 WHERE id = ?3",
        (now.to_string(), done.to_string(), id),
    )
    .unwrap();
    note_server::log::record(conn, Some(uid), "trigger_said", &format!("event {id}: still nothing today")).unwrap();
}

#[test]
fn a_day_the_user_never_shows_up_to_is_still_nudged_within_its_gates() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "defaults/user.toml",
        "display_name = \"T\"\ntimezone = \"Asia/Tokyo\"\ntemplate = \"default\"\n\
         day_start = \"08:00\"\nclose_day_time = \"21:30\"\ntriggers_per_day = 20\n",
    );
    write(tmp.path(), "defaults/templates/default.toml", "events = []\n");
    let conn = note_server::db::open_memory().unwrap();
    let uid = note_server::auth::create_user(&conn, "aitest", "p", false).unwrap();
    conn.execute(
        "UPDATE users SET last_active_at = ?1 WHERE id = ?2",
        (note_server::presence::stamp(at("00:00") - jiff::Span::new().hours(2)), uid),
    )
    .unwrap();
    note_server::calendar::create(
        &conn,
        uid,
        note_server::calendar::Fields {
            title: "school".into(),
            kind: "fixed".into(),
            quiet: Some(true),
            start_time: "12:00".into(),
            end_time: "15:00".into(),
            days: Some(note_server::calendar::day_mask(&["mon"]).unwrap()),
            ..Default::default()
        },
    )
    .unwrap();
    let plans: i64 = conn.query_row("SELECT COUNT(*) FROM plans", [], |r| r.get(0)).unwrap();
    assert_eq!(plans, 0, "nothing exists for the day before the server reaches it");

    let mut nudges = Vec::new();
    let mut now = at("00:00");
    let end = at("23:59");
    while now <= end {
        for id in note_server::idle::check(&conn, tmp.path(), now).unwrap() {
            nudges.push(now);
            settle_as_said(&conn, uid, id, now);
        }
        now += jiff::Span::new().minutes(1);
    }

    let walls: Vec<String> = nudges.iter().map(|t| wall(*t)).collect();
    assert_eq!(walls.first().map(String::as_str), Some("08:50"), "{walls:?}");
    assert!(walls.len() >= 6, "the whole day goes by unnudged: {walls:?}");
    assert!(
        walls.iter().all(|w| !("12:00".."15:00").contains(&w.as_str())),
        "school is quiet: {walls:?}"
    );
    assert!(walls.iter().any(|w| ("15:00".."16:00").contains(&w.as_str())), "school's end reopens it: {walls:?}");
    assert!(walls.iter().all(|w| w.as_str() < "21:30"), "the day closes: {walls:?}");
    for pair in nudges.windows(2) {
        let gap = (pair[1] - pair[0]).total(jiff::Unit::Minute).unwrap();
        assert!(gap >= 60.0, "a nudge that spoke holds the next an hour: {walls:?}");
    }
    let origins: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare("SELECT p.date, e.origin FROM events e JOIN plans p ON p.id = e.plan_id WHERE e.kind = 'trigger'")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    };
    assert!(origins.iter().all(|(d, o)| d == "2026-10-19" && o == "idle"), "{origins:?}");
}
