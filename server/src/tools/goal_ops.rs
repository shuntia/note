use super::task_ops::{checked_due, task_error};
use super::{check_text, ToolCtx, ToolError};
use crate::goals::{GoalPatch, NewGoal};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateArgs {
    /// What reaching the goal looks like, in a few words.
    pub title: String,
    /// What it involves and why it matters, for your own later reference.
    #[serde(default)]
    pub description: Option<String>,
    /// When the goal has to be reached: an RFC 3339 instant, or a bare
    /// YYYY-MM-DD day, which means the end of that day where the user lives.
    #[serde(default)]
    pub due_at: Option<String>,
}

pub fn create(
    conn: &Connection,
    ctx: &ToolCtx,
    args: CreateArgs,
) -> Result<serde_json::Value, ToolError> {
    super::task_query::unscoped(ctx)?;
    if let Some(d) = &args.description {
        check_text("description", d)?;
    }
    let due_at = args.due_at.as_deref().map(|raw| checked_due(ctx, raw)).transpose()?.flatten();
    let goal = crate::goals::create(
        conn,
        ctx.user_id,
        NewGoal { title: args.title, description: args.description, due_at: due_at.map(Some) },
    )
    .map_err(task_error)?;
    Ok(serde_json::json!({ "goal_id": goal.id, "title": goal.title, "due_at": goal.due_at }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateArgs {
    pub goal_id: i64,
    pub title: Option<String>,
    pub description: Option<String>,
    /// One of open, done, dropped.
    pub state: Option<String>,
    /// An RFC 3339 instant, or a bare YYYY-MM-DD day, which means the end of
    /// that day where the user lives. An empty string clears it.
    pub due_at: Option<String>,
}

pub fn update(
    conn: &Connection,
    ctx: &ToolCtx,
    args: UpdateArgs,
) -> Result<serde_json::Value, ToolError> {
    super::task_query::unscoped(ctx)?;
    if let Some(d) = &args.description {
        check_text("description", d)?;
    }
    let due_at = match &args.due_at {
        Some(raw) => Some(checked_due(ctx, raw)?),
        None => None,
    };
    let patch = GoalPatch {
        title: args.title,
        description: args.description,
        state: args.state,
        due_at,
    };
    match crate::goals::update(conn, ctx.user_id, args.goal_id, patch) {
        Ok(Some(g)) => Ok(serde_json::json!({
            "goal_id": g.id,
            "title": g.title,
            "state": g.state,
            "due_at": g.due_at,
        })),
        Ok(None) => Err(ToolError::not_found(format!("no goal {}", args.goal_id))),
        Err(e) => Err(task_error(e)),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {
    /// open (default), done, dropped, or any.
    #[serde(default)]
    pub state: Option<String>,
}

pub fn list(
    conn: &Connection,
    ctx: &ToolCtx,
    args: ListArgs,
) -> Result<serde_json::Value, ToolError> {
    super::task_query::unscoped(ctx)?;
    let goals = crate::goals::list(conn, ctx.user_id, args.state.as_deref()).map_err(task_error)?;
    let goals: Vec<serde_json::Value> = goals
        .into_iter()
        .map(|g| {
            serde_json::json!({
                "goal_id": g.id,
                "title": g.title,
                "description": g.description,
                "state": g.state,
                "due_at": g.due_at,
                "tasks": g.tasks,
                "done_tasks": g.done_tasks,
                "next_task_id": g.next_task_id,
                "next_task_title": g.next_task_title,
                "next_due_at": g.next_due_at,
            })
        })
        .collect();
    Ok(serde_json::json!({ "goals": goals }))
}

#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, registry, PreparedVectors, SessionKind, ToolCtx};
    use rusqlite::Connection;
    use serde_json::Value;

    fn env() -> (Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("defaults")).unwrap();
        std::fs::write(
            tmp.path().join("defaults/user.toml"),
            "display_name = \"X\"\ntimezone = \"Asia/Tokyo\"\ntemplate = \"default\"\n",
        )
        .unwrap();
        (conn, tmp)
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir, scope: Option<i64>) -> ToolCtx<'a> {
        ToolCtx {
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            user_id: 1,
            username: "aki",
            vectors: PreparedVectors::default(),
            task_scope: scope,
            inbox_source: None,
            memory_source: None,
        }
    }

    fn call(conn: &Connection, tmp: &tempfile::TempDir, name: &str, args: &str) -> Value {
        dispatch(conn, &ctx(tmp, None), SessionKind::Talk, name, args).unwrap()
    }

    #[test]
    fn a_goal_is_made_read_back_and_closed() {
        let (conn, tmp) = env();
        let out = call(
            &conn,
            &tmp,
            "goal_create",
            r#"{"title":"get into the programme","description":"the whole application","due_at":"2026-11-01"}"#,
        );
        let id = out["goal_id"].as_i64().unwrap();
        assert_eq!(out["due_at"], "2026-11-01T14:59:00Z", "a bare day is the end of it in Tokyo");

        let out = call(&conn, &tmp, "goal_list", "{}");
        assert_eq!(out["goals"].as_array().unwrap().len(), 1);
        assert_eq!(out["goals"][0]["goal_id"], id);
        assert_eq!(out["goals"][0]["description"], "the whole application");
        assert_eq!(out["goals"][0]["tasks"], 0);

        let out = call(&conn, &tmp, "goal_update", &format!(r#"{{"goal_id":{id},"state":"done"}}"#));
        assert_eq!(out["state"], "done");
        assert!(call(&conn, &tmp, "goal_list", "{}")["goals"].as_array().unwrap().is_empty());
        assert_eq!(
            call(&conn, &tmp, "goal_list", r#"{"state":"any"}"#)["goals"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn a_goal_counts_the_tasks_hung_from_it_and_names_the_next_one_due() {
        let (conn, tmp) = env();
        let goal = call(&conn, &tmp, "goal_create", r#"{"title":"apply"}"#)["goal_id"]
            .as_i64()
            .unwrap();
        let task = |args: &str| call(&conn, &tmp, "task_create", args)["task_id"].as_i64().unwrap();
        let essay = task(&format!(r#"{{"title":"essay","goal_id":{goal},"due_at":"2026-10-25"}}"#));
        let form = task(&format!(r#"{{"title":"form","goal_id":{goal},"due_at":"2026-10-02"}}"#));
        task(r#"{"title":"unrelated"}"#);
        call(&conn, &tmp, "task_update", &format!(r#"{{"task_id":{essay},"state":"done"}}"#));

        let out = call(&conn, &tmp, "goal_list", "{}");
        assert_eq!(out["goals"][0]["tasks"], 2);
        assert_eq!(out["goals"][0]["done_tasks"], 1);
        assert_eq!(out["goals"][0]["next_task_id"], form);
        assert_eq!(out["goals"][0]["next_task_title"], "form");
    }

    #[test]
    fn a_goal_tool_refuses_what_is_not_a_goal() {
        let (conn, tmp) = env();
        let bad = |name: &str, args: &str| {
            dispatch(&conn, &ctx(&tmp, None), SessionKind::Talk, name, args).unwrap_err()
        };
        assert_eq!(bad("goal_create", r#"{"title":"   "}"#).kind, "rejected");
        assert_eq!(bad("goal_create", r#"{"title":"x","due_at":"friday"}"#).kind, "rejected");
        assert_eq!(bad("goal_update", r#"{"goal_id":99,"title":"x"}"#).kind, "not_found");
        let id = call(&conn, &tmp, "goal_create", r#"{"title":"apply"}"#)["goal_id"]
            .as_i64()
            .unwrap();
        assert_eq!(
            bad("goal_update", &format!(r#"{{"goal_id":{id},"state":"someday"}}"#)).kind,
            "rejected"
        );
        assert_eq!(bad("goal_list", r#"{"state":"maybe"}"#).kind, "rejected");
    }

    #[test]
    fn the_goal_tools_sit_where_the_user_can_be_asked_and_nowhere_else() {
        for name in ["goal_create", "goal_update", "goal_list"] {
            for kind in [SessionKind::Talk, SessionKind::Checkin, SessionKind::Nightly] {
                assert!(registry(kind).contains(&name), "{name} missing from {kind:?}");
            }
            for kind in [SessionKind::Import, SessionKind::Inbox, SessionKind::Trigger] {
                assert!(!registry(kind).contains(&name), "{name} reached {kind:?}");
            }
        }
        let (conn, tmp) = env();
        let e = dispatch(&conn, &ctx(&tmp, Some(1)), SessionKind::Talk, "goal_list", "{}")
            .unwrap_err();
        assert_eq!(e.kind, "rejected", "a scoped session surveys nothing");
    }
}
