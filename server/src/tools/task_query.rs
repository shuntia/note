#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, registry, PreparedVectors, SessionKind, ToolCtx, ToolError};
    use rusqlite::Connection;
    use serde_json::Value;

    fn env() -> (Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir, scope: Option<i64>) -> ToolCtx<'a> {
        ToolCtx {
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            user_id: 1,
            username: "aki",
            vectors: PreparedVectors::default(),
            task_scope: scope,
        }
    }

    fn call(
        conn: &Connection,
        tmp: &tempfile::TempDir,
        name: &str,
        args: &str,
    ) -> Result<Value, ToolError> {
        dispatch(conn, &ctx(tmp, None), SessionKind::Talk, name, args)
    }

    fn task(conn: &Connection, tmp: &tempfile::TempDir, args: &str) -> i64 {
        call(conn, tmp, "task_create", args).unwrap()["task_id"].as_i64().unwrap()
    }

    fn patch(conn: &Connection, tmp: &tempfile::TempDir, id: i64, fields: &str) {
        call(conn, tmp, "task_update", &format!(r#"{{"task_id":{id},{fields}}}"#)).unwrap();
    }

    fn ids(out: &Value, key: &str) -> Vec<i64> {
        out[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_i64().or_else(|| t.as_i64()).unwrap())
            .collect()
    }

    fn split_two(conn: &Connection, tmp: &tempfile::TempDir, id: i64) -> Vec<i64> {
        let out = call(
            conn,
            tmp,
            "task_split",
            &format!(
                r#"{{"task_id":{id},"steps":[{{"title":"find the thread","duration_min":5}},
                     {{"title":"write and send","duration_min":10}}]}}"#
            ),
        )
        .unwrap();
        out["step_ids"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect()
    }

    #[test]
    fn task_list_shows_the_live_tasks_newest_first_with_their_steps() {
        let (conn, tmp) = env();
        let dentist = task(&conn, &tmp, r#"{"title":"call dentist"}"#);
        let landlord = task(&conn, &tmp, r#"{"title":"email landlord"}"#);
        let steps = split_two(&conn, &tmp, landlord);
        patch(&conn, &tmp, steps[0], r#""state":"done""#);
        let gone = task(&conn, &tmp, r#"{"title":"an abandoned idea"}"#);
        patch(&conn, &tmp, gone, r#""state":"dropped""#);

        let out = call(&conn, &tmp, "task_list", "{}").unwrap();
        assert_eq!(ids(&out, "tasks"), vec![landlord, dentist]);
        assert_eq!(out["total"], 2);
        assert_eq!(out["tasks"][0]["steps"], 2);
        assert_eq!(out["tasks"][0]["done_steps"], 1);
        assert_eq!(out["tasks"][0]["duration_min"], 15);
        assert_eq!(out["tasks"][0]["state"], "open");
        assert_eq!(out["tasks"][0]["is_now"], false);
        assert_eq!(out["tasks"][1]["steps"], 0);
        assert!(out["tasks"][0]["created_at"].is_string());
        assert!(out["tasks"][0]["updated_at"].is_string());
    }

    #[test]
    fn task_list_filters_by_state_keyword_age_and_now() {
        let (conn, tmp) = env();
        let dentist =
            task(&conn, &tmp, r#"{"title":"call dentist","description":"about the MOLAR","is_now":true}"#);
        let landlord = task(&conn, &tmp, r#"{"title":"email landlord"}"#);
        patch(&conn, &tmp, landlord, r#""notes":"the broken window again""#);
        let taxes = task(&conn, &tmp, r#"{"title":"tax return"}"#);
        conn.execute("UPDATE tasks SET created_at = '2026-01-01T00:00:00Z' WHERE id = ?1", [taxes])
            .unwrap();
        patch(&conn, &tmp, taxes, r#""state":"done""#);

        let list = |args: &str| ids(&call(&conn, &tmp, "task_list", args).unwrap(), "tasks");
        assert_eq!(list(r#"{"state":"done"}"#), vec![taxes]);
        assert_eq!(list(r#"{"state":"any"}"#), vec![landlord, dentist, taxes]);
        assert_eq!(list(r#"{"keyword":"molar"}"#), vec![dentist]);
        assert_eq!(list(r#"{"keyword":"BROKEN window"}"#), vec![landlord]);
        assert_eq!(list(r#"{"older_than_days":30,"state":"any"}"#), vec![taxes]);
        assert_eq!(list(r#"{"added_before":"2026-06-01","state":"any"}"#), vec![taxes]);
        assert_eq!(list(r#"{"added_after":"2026-06-01","state":"any"}"#), vec![landlord, dentist]);
        assert_eq!(list(r#"{"is_now":true}"#), vec![dentist]);
        assert_eq!(list(r#"{"is_now":false}"#), vec![landlord]);
    }

    #[test]
    fn task_list_bounds_its_page_and_reports_the_whole_count() {
        let (conn, tmp) = env();
        for title in ["a", "b", "c", "d"] {
            task(&conn, &tmp, &format!(r#"{{"title":"{title}"}}"#));
        }
        let out = call(&conn, &tmp, "task_list", r#"{"limit":2}"#).unwrap();
        assert_eq!(out["tasks"].as_array().unwrap().len(), 2);
        assert_eq!(out["total"], 4);

        for (args, why) in [
            (r#"{"limit":0}"#, "no page at all"),
            (r#"{"limit":201}"#, "past the ceiling"),
            (r#"{"state":"maybe"}"#, "not a state"),
            (r#"{"keyword":"   "}"#, "blank keyword"),
            (r#"{"added_after":"last tuesday"}"#, "not a date"),
            (r#"{"added_before":"2026-13-01"}"#, "not a date"),
        ] {
            assert_eq!(
                call(&conn, &tmp, "task_list", args).unwrap_err().kind,
                "rejected",
                "{why}"
            );
        }
        let e = call(&conn, &tmp, "task_list", r#"{"surprise":1}"#).unwrap_err();
        assert_eq!(e.kind, "invalid_args");
    }

    #[test]
    fn task_list_leaves_steps_out() {
        let (conn, tmp) = env();
        let landlord = task(&conn, &tmp, r#"{"title":"email landlord"}"#);
        split_two(&conn, &tmp, landlord);
        let out = call(&conn, &tmp, "task_list", r#"{"state":"any"}"#).unwrap();
        assert_eq!(ids(&out, "tasks"), vec![landlord]);
    }

    #[test]
    fn task_search_puts_title_matches_first_and_finished_ones_last() {
        let (conn, tmp) = env();
        let by_title = task(&conn, &tmp, r#"{"title":"email the landlord"}"#);
        let by_body = task(
            &conn,
            &tmp,
            r#"{"title":"tuesday admin","description":"email the landlord about the window"}"#,
        );
        let finished = task(&conn, &tmp, r#"{"title":"email the landlord again"}"#);
        patch(&conn, &tmp, finished, r#""state":"done""#);
        let abandoned = task(&conn, &tmp, r#"{"title":"email the landlord once"}"#);
        patch(&conn, &tmp, abandoned, r#""state":"dropped""#);
        task(&conn, &tmp, r#"{"title":"call dentist"}"#);

        let out = call(&conn, &tmp, "task_search", r#"{"query":"LANDLORD  email"}"#).unwrap();
        assert_eq!(ids(&out, "tasks"), vec![by_title, finished, by_body]);
        assert_eq!(out["tasks"][0]["title"], "email the landlord");
        assert_eq!(out["tasks"][0]["state"], "open");
        assert_eq!(out["tasks"][0]["is_now"], false);
        assert!(out["tasks"][0].get("duration_min").is_some());

        let out = call(&conn, &tmp, "task_search", r#"{"query":"landlord dentist"}"#).unwrap();
        assert!(out["tasks"].as_array().unwrap().is_empty(), "every word must match");
    }

    #[test]
    fn task_search_needs_a_query_of_its_own() {
        let (conn, tmp) = env();
        let long = format!(r#"{{"query":"{}"}}"#, "x".repeat(201));
        for args in [r#"{"query":"   "}"#, &long] {
            assert_eq!(call(&conn, &tmp, "task_search", args).unwrap_err().kind, "rejected");
        }
    }

    #[test]
    fn task_read_returns_the_whole_task_with_its_steps() {
        let (conn, tmp) = env();
        let landlord =
            task(&conn, &tmp, r#"{"title":"email landlord","description":"about the window"}"#);
        patch(&conn, &tmp, landlord, r#""notes":"his number is on the fridge","is_now":true"#);
        let steps = split_two(&conn, &tmp, landlord);
        patch(&conn, &tmp, steps[0], r#""state":"done""#);

        let out = call(&conn, &tmp, "task_read", &format!(r#"{{"task_id":{landlord}}}"#)).unwrap();
        assert_eq!(out["id"], landlord);
        assert_eq!(out["title"], "email landlord");
        assert_eq!(out["description"], "about the window");
        assert_eq!(out["notes"], "his number is on the fridge");
        assert_eq!(out["state"], "open");
        assert_eq!(out["source"], "agent");
        assert_eq!(out["duration_min"], 15);
        assert_eq!(out["is_now"], true);
        assert!(out["parent_id"].is_null());
        assert!(out["created_at"].is_string());
        assert!(out["updated_at"].is_string());
        assert_eq!(out["children"].as_array().unwrap().len(), 2);
        assert_eq!(out["children"][0]["state"], "done");
        assert_eq!(out["children"][0]["duration_min"], 5);
        assert_eq!(out["children"][0]["parent_id"], landlord);
    }

    #[test]
    fn task_read_does_not_reach_another_users_task() {
        let (conn, tmp) = env();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('rin', 'x', 'member')",
            [],
        )
        .unwrap();
        let mut theirs = ctx(&tmp, None);
        theirs.user_id = 2;
        theirs.username = "rin";
        let id = dispatch(&conn, &theirs, SessionKind::Talk, "task_create", r#"{"title":"theirs"}"#)
            .unwrap()["task_id"]
            .as_i64()
            .unwrap();

        for target in [id, 9999] {
            let e = call(&conn, &tmp, "task_read", &format!(r#"{{"task_id":{target}}}"#))
                .unwrap_err();
            assert_eq!(e.kind, "not_found", "reached {target}");
        }
    }

    #[test]
    fn task_bulk_update_sets_one_state_on_many_tasks() {
        let (conn, tmp) = env();
        let a = task(&conn, &tmp, r#"{"title":"a"}"#);
        let b = task(&conn, &tmp, r#"{"title":"b"}"#);
        let out = call(
            &conn,
            &tmp,
            "task_bulk_update",
            &format!(r#"{{"task_ids":[{a},{b}],"state":"done"}}"#),
        )
        .unwrap();
        assert_eq!(ids(&out, "updated"), vec![a, b]);
        assert!(out["demoted_from_now"].as_array().unwrap().is_empty());
        for id in [a, b] {
            assert_eq!(crate::tasks::get(&conn, 1, id).unwrap().unwrap().state, "done");
        }
    }

    #[test]
    fn task_bulk_update_fills_now_and_reports_who_stepped_aside() {
        let (conn, tmp) = env();
        let all: Vec<i64> = ["a", "b", "c", "d"]
            .iter()
            .map(|t| task(&conn, &tmp, &format!(r#"{{"title":"{t}"}}"#)))
            .collect();
        let list = all.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        let out = call(
            &conn,
            &tmp,
            "task_bulk_update",
            &format!(r#"{{"task_ids":[{list}],"is_now":true}}"#),
        )
        .unwrap();
        assert_eq!(ids(&out, "updated"), all);
        assert_eq!(ids(&out, "demoted_from_now"), vec![all[2]]);
        let now: Vec<i64> = {
            let mut stmt = conn
                .prepare("SELECT id FROM tasks WHERE is_now = 1 ORDER BY id")
                .unwrap();
            stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(now, vec![all[0], all[1], all[3]]);
    }

    #[test]
    fn task_bulk_update_deletes_the_whole_batch() {
        let (conn, tmp) = env();
        let a = task(&conn, &tmp, r#"{"title":"a"}"#);
        let b = task(&conn, &tmp, r#"{"title":"b"}"#);
        split_two(&conn, &tmp, b);
        let keep = task(&conn, &tmp, r#"{"title":"keep me"}"#);

        let out = call(
            &conn,
            &tmp,
            "task_bulk_update",
            &format!(r#"{{"task_ids":[{a},{b}],"delete":true}}"#),
        )
        .unwrap();
        assert_eq!(ids(&out, "deleted"), vec![a, b]);
        let left: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 1);
        assert!(crate::tasks::get(&conn, 1, keep).unwrap().is_some());
    }

    #[test]
    fn task_bulk_update_needs_exactly_one_change_and_a_sane_batch() {
        let (conn, tmp) = env();
        let a = task(&conn, &tmp, r#"{"title":"a"}"#);
        let fifty_one = (0..51).map(|_| a.to_string()).collect::<Vec<_>>().join(",");
        for (args, why) in [
            (format!(r#"{{"task_ids":[{a}]}}"#), "nothing to change"),
            (format!(r#"{{"task_ids":[{a}],"state":"done","is_now":true}}"#), "two changes"),
            (format!(r#"{{"task_ids":[{a}],"delete":false}}"#), "delete only takes true"),
            (r#"{"task_ids":[],"state":"done"}"#.into(), "no tasks"),
            (format!(r#"{{"task_ids":[{fifty_one}],"state":"done"}}"#), "past the batch cap"),
            (format!(r#"{{"task_ids":[{a},{a}],"state":"done"}}"#), "the same id twice"),
            (format!(r#"{{"task_ids":[{a}],"state":"exploded"}}"#), "not a state"),
        ] {
            assert_eq!(
                call(&conn, &tmp, "task_bulk_update", &args).unwrap_err().kind,
                "rejected",
                "{why}"
            );
        }
        assert_eq!(crate::tasks::get(&conn, 1, a).unwrap().unwrap().state, "open");
    }

    #[test]
    fn one_bad_id_rejects_the_whole_batch() {
        let (conn, tmp) = env();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('rin', 'x', 'member')",
            [],
        )
        .unwrap();
        let mut theirs = ctx(&tmp, None);
        theirs.user_id = 2;
        theirs.username = "rin";
        let foreign =
            dispatch(&conn, &theirs, SessionKind::Talk, "task_create", r#"{"title":"theirs"}"#)
                .unwrap()["task_id"]
                .as_i64()
                .unwrap();
        let mine = task(&conn, &tmp, r#"{"title":"mine"}"#);

        for other in [foreign, 9999] {
            for change in [r#""state":"done""#, r#""delete":true"#] {
                let e = call(
                    &conn,
                    &tmp,
                    "task_bulk_update",
                    &format!(r#"{{"task_ids":[{mine},{other}],{change}}}"#),
                )
                .unwrap_err();
                assert_eq!(e.kind, "not_found", "{other} {change}");
                assert!(e.message.contains(&other.to_string()), "the id is named: {}", e.message);
            }
        }
        let mine_now = crate::tasks::get(&conn, 1, mine).unwrap().unwrap();
        assert_eq!(mine_now.state, "open", "a rejected batch changes nothing");
        assert!(crate::tasks::get(&conn, 2, foreign).unwrap().is_some());
    }

    #[test]
    fn task_bulk_update_never_puts_a_step_in_now() {
        let (conn, tmp) = env();
        let landlord = task(&conn, &tmp, r#"{"title":"email landlord"}"#);
        let steps = split_two(&conn, &tmp, landlord);
        let e = call(
            &conn,
            &tmp,
            "task_bulk_update",
            &format!(r#"{{"task_ids":[{landlord},{}],"is_now":true}}"#, steps[0]),
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");
        let flag: i64 = conn
            .query_row("SELECT is_now FROM tasks WHERE id = ?1", [landlord], |r| r.get(0))
            .unwrap();
        assert_eq!(flag, 0, "a rejected batch changes nothing");
    }

    #[test]
    fn the_survey_tools_are_closed_to_a_scoped_session() {
        let (conn, tmp) = env();
        let id = task(&conn, &tmp, r#"{"title":"biology ch.4"}"#);
        for (name, args) in [
            ("task_list", "{}".to_string()),
            ("task_search", r#"{"query":"biology"}"#.to_string()),
            ("task_read", format!(r#"{{"task_id":{id}}}"#)),
            ("task_bulk_update", format!(r#"{{"task_ids":[{id}],"state":"done"}}"#)),
        ] {
            let e = dispatch(&conn, &ctx(&tmp, Some(id)), SessionKind::Talk, name, &args)
                .unwrap_err();
            assert_eq!(e.kind, "rejected", "{name} answered a scoped session");
        }
    }

    #[test]
    fn the_survey_tools_sit_in_the_right_registries() {
        for name in ["task_list", "task_search", "task_read"] {
            for kind in [SessionKind::Checkin, SessionKind::Talk, SessionKind::Nightly] {
                assert!(registry(kind).contains(&name), "{name} missing from {kind:?}");
            }
        }
        assert!(!registry(SessionKind::Checkin).contains(&"task_bulk_update"));
        for kind in [SessionKind::Talk, SessionKind::Nightly] {
            assert!(registry(kind).contains(&"task_bulk_update"));
        }
        for name in ["task_list", "task_search", "task_read", "task_bulk_update"] {
            assert!(!registry(SessionKind::Import).contains(&name), "{name} reached import");
        }

        let (conn, tmp) = env();
        let e = dispatch(
            &conn,
            &ctx(&tmp, None),
            SessionKind::Checkin,
            "task_bulk_update",
            r#"{"task_ids":[1],"state":"done"}"#,
        )
        .unwrap_err();
        assert_eq!(e.kind, "forbidden");
    }
}
