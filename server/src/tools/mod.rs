pub mod calendar_ops;
pub mod context_ops;
pub mod harvest_ops;
pub mod inbox_ops;
pub mod memory_ops;
pub mod outreach_ops;
pub mod plan_ops;
pub mod review_ops;
pub mod schedule_ops;
pub mod summary_ops;
pub mod task_ops;
pub mod task_query;
pub mod trigger_ops;

use rusqlite::Connection;
use serde::Serialize;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SessionKind {
    Nightly,
    Checkin,
    Talk,
    /// One imported task, briefed by the agent on its importer's behalf.
    Import,
    /// One item from a learning-management system, judged for what it is worth
    /// remembering.
    Inbox,
    /// One idle conversation, condensed into the summary it carries from then on.
    Summarize,
    /// The day's conversations, read once for the facts worth keeping.
    Harvest,
    /// One trigger point, fired: read the situation, then speak or stay quiet.
    Trigger,
    /// The week just ended, read once for the letter that stands beside Monday's
    /// debrief.
    Review,
}

/// A tool failure returned to the model as a value; `kind` is machine-matchable,
/// `message` is for the model to read.
#[derive(Debug, Serialize)]
pub struct ToolError {
    pub kind: &'static str,
    pub message: String,
}

impl ToolError {
    fn of(kind: &'static str) -> impl Fn(String) -> Self {
        move |message| Self { kind, message }
    }
    pub fn rejected(m: impl Into<String>) -> Self {
        Self::of("rejected")(m.into())
    }
    pub fn invalid_args(m: impl Into<String>) -> Self {
        Self::of("invalid_args")(m.into())
    }
    pub fn not_found(m: impl Into<String>) -> Self {
        Self::of("not_found")(m.into())
    }
    pub fn forbidden(m: impl Into<String>) -> Self {
        Self::of("forbidden")(m.into())
    }
    pub fn unknown_tool(m: impl Into<String>) -> Self {
        Self::of("unknown_tool")(m.into())
    }
    /// The day's own trigger budget is used up; the message names the way out.
    pub fn cap_reached(m: impl Into<String>) -> Self {
        Self::of("cap_reached")(m.into())
    }
    pub fn too_soon(m: impl Into<String>) -> Self {
        Self::of("too_soon")(m.into())
    }
    pub fn past(m: impl Into<String>) -> Self {
        Self::of("past")(m.into())
    }
    pub fn internal(m: impl Into<String>) -> Self {
        Self::of("internal")(m.into())
    }
}

pub struct ToolCtx<'a> {
    pub config_dir: &'a Path,
    pub data_dir: &'a Path,
    pub user_id: i64,
    pub username: &'a str,
    pub vectors: PreparedVectors,
    /// When set, the task tools reach only this task and its steps.
    pub task_scope: Option<i64>,
    /// When set, `inbox_decide` accepts only this source id.
    pub inbox_source: Option<String>,
    /// When set, every fact `memory_write` lands is recorded against this
    /// source id.
    pub memory_source: Option<String>,
}

pub const MAX_ARGS_BYTES: usize = 64 * 1024;

#[derive(Default, Debug)]
pub struct PreparedVectors {
    pub content: Option<Vec<f32>>,
    pub query: Option<Vec<f32>>,
    /// One vector per fact of an `inbox_decide` call, in argument order.
    pub facts: Vec<Option<Vec<f32>>>,
    pub error: Option<String>,
}

/// Embeds any text this tool call will need, so `dispatch` itself never does
/// network I/O. Malformed args embed nothing — dispatch will reject them with
/// a typed error anyway.
pub fn prepare(
    emb: Option<&dyn crate::providers::EmbeddingsProvider>,
    name: &str,
    raw_args: &str,
) -> PreparedVectors {
    let Some(emb) = emb else { return PreparedVectors::default() };
    if raw_args.len() > MAX_ARGS_BYTES {
        return PreparedVectors::default();
    }
    let mut out = PreparedVectors::default();
    match name {
        "memory_query" => {
            #[derive(serde::Deserialize, Default)]
            #[serde(default)]
            struct Q {
                query: String,
            }
            if let Ok(q) = serde_json::from_str::<Q>(raw_args) {
                if !q.query.is_empty() {
                    match emb.embed(&[&q.query]) {
                        Ok(vs) if !vs.is_empty() => out.query = Some(vs[0].clone()),
                        Ok(_) => {}
                        Err(e) => out.error = Some(e.to_string()),
                    }
                }
            }
        }
        "memory_write" => {
            #[derive(serde::Deserialize, Default)]
            #[serde(default)]
            struct W {
                summary: String,
                body: String,
            }
            if let Ok(w) = serde_json::from_str::<W>(raw_args) {
                if !w.summary.is_empty() || !w.body.is_empty() {
                    let text = crate::memory::embed_text(&w.summary, &w.body);
                    match emb.embed(&[&text]) {
                        Ok(vs) if !vs.is_empty() => out.content = Some(vs[0].clone()),
                        Ok(_) => {}
                        Err(e) => out.error = Some(e.to_string()),
                    }
                }
            }
        }
        "inbox_decide" => {
            #[derive(serde::Deserialize, Default)]
            #[serde(default)]
            struct F {
                summary: String,
                body: String,
            }
            #[derive(serde::Deserialize, Default)]
            #[serde(default)]
            struct D {
                facts: Vec<F>,
            }
            if let Ok(d) = serde_json::from_str::<D>(raw_args) {
                let texts: Vec<String> = d
                    .facts
                    .iter()
                    .map(|f| crate::memory::embed_text(&f.summary, &f.body))
                    .collect();
                if !texts.is_empty() {
                    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                    match emb.embed(&refs) {
                        Ok(vs) => {
                            out.facts = texts
                                .iter()
                                .enumerate()
                                .map(|(i, _)| vs.get(i).cloned())
                                .collect()
                        }
                        Err(e) => out.error = Some(e.to_string()),
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// Ceiling for every free-text field a tool accepts, shared so the surfaces
/// cannot drift apart.
pub(crate) const MAX_TEXT_BYTES: usize = 16 * 1024;

pub(crate) fn check_text(field: &str, value: &str) -> Result<(), ToolError> {
    if value.len() > MAX_TEXT_BYTES {
        return Err(ToolError::rejected(format!(
            "{field} must be at most {MAX_TEXT_BYTES} bytes"
        )));
    }
    Ok(())
}

const CHECKIN: &[&str] = &[
    "memory_query",
    "memory_read",
    "memory_write",
    "task_create",
    "task_update",
    "task_split",
    "task_delete",
    "schedule_slide",
    "schedule_snooze",
    "schedule_drop",
    "schedule_reshape",
    "task_list",
    "task_search",
    "task_read",
    "plan_carry",
    "plan_list",
    "calendar_list",
    "trigger_set",
    "wait_until",
    "wait_for",
    "trigger_budget",
];
const TALK: &[&str] = &[
    "memory_query",
    "memory_read",
    "memory_write",
    "task_create",
    "task_update",
    "task_split",
    "task_delete",
    "schedule_slide",
    "schedule_snooze",
    "schedule_drop",
    "schedule_reshape",
    "context_edit",
    "task_list",
    "task_search",
    "task_read",
    "task_bulk_update",
    "plan_tasks",
    "plan_auto",
    "plan_carry",
    "plan_list",
    "calendar_list",
    "calendar_add",
    "calendar_update",
    "calendar_remove",
    "calendar_skip",
    "trigger_set",
    "wait_until",
    "wait_for",
    "trigger_budget",
];
const IMPORT: &[&str] = &["task_brief"];
const SUMMARIZE: &[&str] = &["summary_write"];
const HARVEST: &[&str] =
    &["memory_query", "memory_read", "memory_write", "harvest_done"];
const INBOX: &[&str] = &["memory_query", "memory_read", "inbox_decide"];
const REVIEW: &[&str] =
    &["memory_query", "memory_read", "memory_write", "review_write"];
const NIGHTLY: &[&str] = &[
    "memory_query",
    "memory_read",
    "memory_write",
    "task_create",
    "task_update",
    "task_split",
    "task_delete",
    "schedule_slide",
    "schedule_snooze",
    "schedule_drop",
    "schedule_reshape",
    "context_edit",
    "schedule_insert",
    "notify_send",
    "nightly_notes_write",
    "task_list",
    "task_search",
    "task_read",
    "task_bulk_update",
    "plan_tasks",
    "plan_auto",
    "plan_list",
    "calendar_list",
    "calendar_add",
    "calendar_update",
    "calendar_remove",
    "calendar_skip",
    "trigger_set",
    "wait_until",
    "wait_for",
];
const TRIGGER: &[&str] = &[
    "memory_query",
    "memory_read",
    "task_list",
    "task_read",
    "task_search",
    "plan_carry",
    "plan_list",
    "trigger_set",
    "wait_until",
    "wait_for",
    "say",
    "stay_quiet",
];

/// A tool whose success is the session's whole job: `run_session` returns on
/// it instead of spending another model round on a closing sentence.
pub fn is_terminal(kind: SessionKind, name: &str) -> bool {
    match kind {
        SessionKind::Import => name == "task_brief",
        SessionKind::Inbox => name == "inbox_decide",
        SessionKind::Summarize => name == "summary_write",
        SessionKind::Harvest => name == "harvest_done",
        SessionKind::Review => name == "review_write",
        SessionKind::Trigger => name == "say" || name == "stay_quiet",
        _ => false,
    }
}

pub fn registry(kind: SessionKind) -> &'static [&'static str] {
    match kind {
        SessionKind::Nightly => NIGHTLY,
        SessionKind::Checkin => CHECKIN,
        SessionKind::Talk => TALK,
        SessionKind::Import => IMPORT,
        SessionKind::Inbox => INBOX,
        SessionKind::Summarize => SUMMARIZE,
        SessionKind::Harvest => HARVEST,
        SessionKind::Review => REVIEW,
        SessionKind::Trigger => TRIGGER,
    }
}

fn schema<T: schemars::JsonSchema>() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("schema serializes")
}

fn describe(name: &str) -> (&'static str, serde_json::Value) {
    match name {
        "task_create" => (
            "Create a new task for the current user. Set is_now to put it straight in Now, \
             the user's short list of at most 3 — a fourth pushes the newest one back to Later. \
             due_at is when the work is due, not when to do it. notify says how the block \
             holding this task announces itself when it starts: none is silent, chat writes a \
             line in the day's thread, notify sends a notification (the default).",
            schema::<task_ops::CreateArgs>(),
        ),
        "task_update" => (
            "Update a task's title, description, state, notes, duration (whole 5-minute blocks), \
             due date, how its block announces itself (notify: none, chat or notify), or \
             whether it sits in Now — the short list of at most 3, where a fourth pushes the \
             newest one back to Later. Steps are never in Now, and a step never carries a due \
             date of its own.",
            schema::<task_ops::UpdateArgs>(),
        ),
        "task_split" => (
            "Break the existing task with this id into 2-5 short steps, each with a duration in \
             whole 5-minute blocks. The steps land under that task; this is not the way to create \
             new tasks. Only for a task that has no steps yet.",
            schema::<task_ops::SplitArgs>(),
        ),
        "task_brief" => (
            "Brief this one imported assignment, in a single call that ends the session. \
             With homework false the task is dropped as not homework and reason says why; \
             with homework true, description, duration_min (whole 5-minute blocks) and steps \
             are written — steps only when the task has none yet, otherwise they are kept.",
            schema::<task_ops::BriefArgs>(),
        ),
        "task_delete" => (
            "Delete a task, or one step, for good — with its steps and its place on the day's \
             plan. For a task the user no longer wants at all; to record one they finished or \
             abandoned, set its state with task_update instead.",
            schema::<task_ops::DeleteArgs>(),
        ),
        "inbox_decide" => (
            "Decide what this one learning-management item is worth, in a single call that ends \
             the session. Outcome \"remembered\" writes 1 to 10 durable facts; \"task\" leaves the \
             item to the importer to turn into a task; \"nothing\" records that it held nothing \
             durable. Facts already written for this source are archived and replaced.",
            schema::<inbox_ops::DecideArgs>(),
        ),
        "memory_query" => (
            "Search the user's long-term memory; returns ids and summaries.",
            schema::<memory_ops::QueryArgs>(),
        ),
        "memory_read" => (
            "Read one memory in full by id.",
            schema::<memory_ops::ReadArgs>(),
        ),
        "memory_write" => (
            "Add, update, or supersede a memory. Superseding archives the old fact. \
             On an add, until (YYYY-MM-DD) is the date the fact stops mattering, after \
             which it is archived on its own — set it on anything tied to a stretch of \
             time, and leave it off what stays true.",
            schema::<memory_ops::WriteArgs>(),
        ),
        "summary_write" => (
            "Write this conversation's summary, in a single call that ends the session. \
             1 to 1200 bytes of plain text — no markdown headers.",
            schema::<summary_ops::WriteArgs>(),
        ),
        "harvest_done" => (
            "End the harvest, naming how many facts you wrote and, in note, what you \
             left out and why. Call it once, last.",
            schema::<harvest_ops::DoneArgs>(),
        ),
        "review_write" => (
            "Write the week's letter, in a single call that ends the session. 1 to 4000 bytes \
             of plain text, addressed to the user.",
            schema::<review_ops::WriteArgs>(),
        ),
        "context_edit" => (
            "Edit the standing context document: replace a unique snippet or append a line.",
            schema::<context_ops::EditArgs>(),
        ),
        "schedule_slide" => (
            "Slide a plan event by N minutes (negative = earlier), within its slide window.",
            schema::<schedule_ops::SlideArgs>(),
        ),
        "schedule_snooze" => (
            "Postpone a plan event's delivery by N minutes without changing the schedule intent.",
            schema::<schedule_ops::SnoozeArgs>(),
        ),
        "schedule_drop" => (
            "Drop a droppable plan event for today. If it is moving rather than going away, \
             add the replacement first and name its id, so the user can see where it went.",
            schema::<schedule_ops::DropArgs>(),
        ),
        "schedule_reshape" => (
            "Move or resize a block of time on the day's plan. Blocks are the only entries \
             with a start and an end, and they never notify the user.",
            schema::<schedule_ops::ReshapeArgs>(),
        ),
        "schedule_insert" => (
            "Insert a new event into an existing day plan.",
            schema::<schedule_ops::InsertArgs>(),
        ),
        "notify_send" => (
            "Send the user a nudge with this text (delivered within a minute).",
            schema::<outreach_ops::SendArgs>(),
        ),
        "nightly_notes_write" => (
            "Leave tomorrow's sessions a short brief, replacing last night's: 5-12 plain \
             lines, no markdown headers, nothing that will read as stale. It is injected \
             into every session tomorrow, so write it for yourself, not for the user.",
            schema::<context_ops::NightlyNotesArgs>(),
        ),
        "task_list" => (
            "Survey the user's top-level tasks, newest first, each with its due date, its step \
             count and how many of those are done. Filter by state, by a case-insensitive keyword \
             over title, description and notes, by when the task was added, by when it is due or \
             whether it is overdue, or to the Now list; sort due to put the soonest deadline \
             first and the undated tasks last; total says how many matched, which can be more \
             than one page holds.",
            schema::<task_query::ListArgs>(),
        ),
        "task_search" => (
            "Find the user's tasks by the words in them — the way to turn a task the user names \
             in passing into an id. Every word must match: title matches come first, then tasks \
             whose description or notes carry the words, live ones before finished ones. \
             Top-level tasks only, at most 20, dropped ones left out.",
            schema::<task_query::SearchArgs>(),
        ),
        "task_read" => (
            "Read one task in full: description, notes, state, source, duration, due date, the \
             link and id it was imported under, whether it is in Now, when it was added and last \
             touched, and its steps with their own states and durations. The list tools carry titles only; this is how to see the rest.",
            schema::<task_query::ReadArgs>(),
        ),
        "task_bulk_update" => (
            "Apply one change to a batch of up to 50 tasks: set them all to a state, move them \
             all in or out of Now, or delete them all. Set exactly one of state, is_now or \
             delete. All or nothing — an id that is not the user's rejects the whole call and \
             nothing changes. Now holds at most 3, and the tasks it pushes out come back in \
             demoted_from_now; steps are never in Now.",
            schema::<task_query::BulkUpdateArgs>(),
        ),
        "plan_tasks" => (
            "Lay tasks out as consecutive blocks of time on one day's plan — this is how \
             \"schedule these tasks for tomorrow morning\" is done. Each block runs as long as \
             its task's duration (25 minutes for a task with none), in the order given, from \
             start, with gap_min minutes between them and none ending after end. Blocks are \
             silent: they never ping the user. Not for routines or reminders. The call is \
             refused, changing nothing, when a task is already on that day, when the blocks \
             would overlap something already planned, or when they do not fit before end.",
            schema::<plan_ops::PlanTasksArgs>(),
        ),
        "plan_auto" => (
            "Lay the user's open tasks into a day's free time by itself — the free windows \
             the user has set aside on the calendar, minus what the day already holds. \
             Automatic blocks from an earlier run that have not started are replaced, and \
             blocks the user has already finished or dropped are left alone. Use it after \
             the day's free time changes; to place particular tasks at particular times, \
             use plan_tasks instead.",
            schema::<plan_ops::PlanAutoArgs>(),
        ),
        "plan_carry" => (
            "Carry what is left of a day over to the next: every block still waiting on the \
             user moves to tomorrow's plan with its task, and the day's remaining check-ins \
             are dropped. Defaults to today. This is what the user agreeing to carry the rest \
             to tomorrow at the close of the day means; nothing already done or dropped moves.",
            schema::<plan_ops::PlanCarryArgs>(),
        ),
        "plan_list" => (
            "Read one day's plan: every event with its event_id — the id the schedule tools \
             take — its times, status and flexibility, and for a block laid by plan_tasks the \
             task_id it holds. Defaults to today; the context lists only today's.",
            schema::<plan_ops::PlanListArgs>(),
        ),
        "calendar_list" => (
            "Read the user's calendar of fixed commitments: what each day already \
             belongs to, with the quiet windows marked and the free time the user has set \
             aside for tasks.",
            schema::<calendar_ops::ListArgs>(),
        ),
        "calendar_add" => (
            "Add a standing commitment to the calendar. \"fixed\" is a hard one the day is \
             built around — school, work, a class — and is quiet by default: while it runs, \
             deliveries are held and arrive when it ends. \"busy\" is softer (a commute, a \
             meal), \"note\" is informational and never quiet, and \"free\" is time the user \
             has set aside for tasks — never quiet, and the only time open tasks are laid \
             into. Use this when the user says they cannot be disturbed at certain times, or \
             names something that happens every week: \"school weekdays 08:15-15:30\" is \
             fixed and quiet.",
            schema::<calendar_ops::AddArgs>(),
        ),
        "calendar_update" => (
            "Change a calendar entry: its title, kind, quiet flag, times, weekdays, one-off \
             date or validity range. Only the fields given change.",
            schema::<calendar_ops::UpdateArgs>(),
        ),
        "calendar_remove" => (
            "Delete a calendar entry for good. For a commitment that has ended; to drop a \
             single day of one that continues, use calendar_skip.",
            schema::<calendar_ops::RemoveArgs>(),
        ),
        "calendar_skip" => (
            "Skip one date of a repeating calendar entry — a day off school, a cancelled \
             class. The entry itself stays.",
            schema::<calendar_ops::SkipArgs>(),
        ),
        "trigger_set" => (
            "Lay a trigger point: a moment later today when you will look at the situation \
             again and either say something or stay quiet. prompt is the note to yourself \
             about what you are following up on. This is how you reach out on your own \
             terms rather than waiting to be asked. at least 10 minutes ahead.",
            schema::<trigger_ops::SetArgs>(),
        ),
        "wait_until" => (
            "Lay a trigger point that calls itself off if the user writes back in the \
             thread before it fires — the way to leave a question open without nagging.",
            schema::<trigger_ops::WaitUntilArgs>(),
        ),
        "wait_for" => (
            "Lay a trigger point that calls itself off if the task is finished or dropped, \
             or the plan event is settled, before it fires. Name exactly one of them.",
            schema::<trigger_ops::WaitForArgs>(),
        ),
        "trigger_budget" => (
            "Raise today's check-in budget, after the user has agreed to it — never \
             before. reason is what they agreed to.",
            schema::<trigger_ops::BudgetArgs>(),
        ),
        "say" => (
            "Say this to the user and end the trigger session. One or two warm sentences, \
             no greeting ritual — it lands in the thread and as a notification.",
            schema::<trigger_ops::SayArgs>(),
        ),
        "stay_quiet" => (
            "Say nothing and end the trigger session: the right move when the user is \
             already on it, when what you would say is on their screen, or when the \
             prompt no longer applies.",
            schema::<trigger_ops::QuietArgs>(),
        ),
        _ => unreachable!("describe covers every registered tool"),
    }
}

pub fn schemas(kind: SessionKind) -> Vec<serde_json::Value> {
    registry(kind)
        .iter()
        .map(|name| {
            let (description, input_schema) = describe(name);
            serde_json::json!({ "name": name, "description": description, "input_schema": input_schema })
        })
        .collect()
}

fn parse<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, ToolError> {
    serde_json::from_str(raw).map_err(|e| ToolError::invalid_args(e.to_string()))
}

/// Sole entrypoint for model-originated calls: enforces the payload cap and
/// the per-session registry, then runs the handler inside one transaction so
/// a failed call leaves no trace.
pub fn dispatch(
    conn: &Connection,
    ctx: &ToolCtx,
    kind: SessionKind,
    name: &str,
    raw_args: &str,
) -> Result<serde_json::Value, ToolError> {
    if raw_args.len() > MAX_ARGS_BYTES {
        return Err(ToolError::rejected(format!(
            "arguments exceed {MAX_ARGS_BYTES} bytes"
        )));
    }
    if !registry(kind).contains(&name) {
        return Err(if NIGHTLY.contains(&name) {
            ToolError::forbidden(format!("tool {name} is not available in this session type"))
        } else {
            ToolError::unknown_tool(format!("no such tool: {name}"))
        });
    }
    let tx = conn.unchecked_transaction().map_err(|e| ToolError::internal(e.to_string()))?;
    let out = run(&tx, ctx, kind, name, raw_args)?;
    tx.commit().map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(out)
}

fn run(
    conn: &Connection,
    ctx: &ToolCtx,
    kind: SessionKind,
    name: &str,
    raw: &str,
) -> Result<serde_json::Value, ToolError> {
    match name {
        "task_create" => task_ops::create(conn, ctx, parse(raw)?),
        "task_update" => task_ops::update(conn, ctx, parse(raw)?),
        "task_split" => task_ops::split(conn, ctx, parse(raw)?),
        "task_brief" => task_ops::brief(conn, ctx, parse(raw)?),
        "task_delete" => task_ops::delete(conn, ctx, parse(raw)?),
        "inbox_decide" => inbox_ops::decide(conn, ctx, parse(raw)?),
        "memory_query" => memory_ops::query(conn, ctx, parse(raw)?),
        "memory_read" => memory_ops::read(conn, ctx, parse(raw)?),
        "memory_write" => memory_ops::write(conn, ctx, parse(raw)?),
        "summary_write" => summary_ops::write(conn, ctx, parse(raw)?),
        "harvest_done" => harvest_ops::done(conn, ctx, parse(raw)?),
        "review_write" => review_ops::write(conn, ctx, parse(raw)?),
        "context_edit" => context_ops::edit(conn, ctx, parse(raw)?),
        "schedule_slide" => schedule_ops::slide(conn, ctx, parse(raw)?),
        "schedule_snooze" => schedule_ops::snooze(conn, ctx, parse(raw)?),
        "schedule_drop" => schedule_ops::drop_event(conn, ctx, parse(raw)?),
        "schedule_reshape" => schedule_ops::reshape(conn, ctx, parse(raw)?),
        "schedule_insert" => schedule_ops::insert(conn, ctx, parse(raw)?),
        "notify_send" => outreach_ops::send(conn, ctx, parse(raw)?),
        "nightly_notes_write" => context_ops::nightly_notes_write(conn, ctx, parse(raw)?),
        "task_list" => task_query::list(conn, ctx, parse(raw)?),
        "task_search" => task_query::search(conn, ctx, parse(raw)?),
        "task_read" => task_query::read(conn, ctx, parse(raw)?),
        "task_bulk_update" => task_query::bulk_update(conn, ctx, parse(raw)?),
        "plan_tasks" => plan_ops::plan_tasks(conn, ctx, parse(raw)?),
        "plan_auto" => plan_ops::plan_auto(conn, ctx, parse(raw)?),
        "plan_carry" => plan_ops::plan_carry(conn, ctx, parse(raw)?),
        "plan_list" => plan_ops::plan_list(conn, ctx, parse(raw)?),
        "calendar_list" => calendar_ops::list(conn, ctx, parse(raw)?),
        "calendar_add" => calendar_ops::add(conn, ctx, parse(raw)?),
        "calendar_update" => calendar_ops::update(conn, ctx, parse(raw)?),
        "calendar_remove" => calendar_ops::remove(conn, ctx, parse(raw)?),
        "calendar_skip" => calendar_ops::skip(conn, ctx, parse(raw)?),
        "trigger_set" => trigger_ops::set(conn, ctx, kind, parse(raw)?),
        "wait_until" => trigger_ops::wait_until(conn, ctx, kind, parse(raw)?),
        "wait_for" => trigger_ops::wait_for(conn, ctx, kind, parse(raw)?),
        "trigger_budget" => trigger_ops::budget(conn, ctx, parse(raw)?),
        "say" => trigger_ops::say(conn, ctx, parse(raw)?),
        "stay_quiet" => trigger_ops::stay_quiet(conn, ctx, parse(raw)?),
        _ => unreachable!("registry guarantees a known name"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        // argon2 is slow and irrelevant here; insert the user row directly
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir) -> ToolCtx<'a> {
        ToolCtx { config_dir: tmp.path(), data_dir: tmp.path(), user_id: 1, username: "aki", vectors: PreparedVectors::default(), task_scope: None, inbox_source: None, memory_source: None }
    }

    #[test]
    fn task_create_and_update_roundtrip() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"call dentist","description":"about the molar"}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{id},"state":"done"}}"#)).unwrap();
        assert_eq!(out["state"], "done");
    }

    #[test]
    fn agent_sets_durations_in_five_minute_steps() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"email landlord","duration_min":20}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        let (dur, src): (i64, String) = conn
            .query_row("SELECT duration_min, duration_source FROM tasks WHERE id = ?1", [id],
                |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((dur, src.as_str()), (20, "agent"));

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{id},"duration_min":23}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");

        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{id},"duration_min":25}}"#)).unwrap();
        let dur: i64 = conn
            .query_row("SELECT duration_min FROM tasks WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(dur, 25);
    }

    #[test]
    fn agent_moves_tasks_in_and_out_of_now() {
        let (conn, tmp) = env();
        let flag = |id: i64| -> i64 {
            conn.query_row("SELECT is_now FROM tasks WHERE id = ?1", [id], |r| r.get(0)).unwrap()
        };
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"email landlord","is_now":true}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        assert_eq!(flag(id), 1);

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{id},"is_now":false}}"#)).unwrap();
        assert_eq!(out["is_now"], false);
        assert_eq!(flag(id), 0);

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{id},"is_now":true}}"#)).unwrap();
        assert_eq!(out["is_now"], true);
        assert_eq!(out["demoted_from_now"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_fourth_agent_write_pushes_the_newest_task_out_of_now() {
        let (conn, tmp) = env();
        let mut ids = Vec::new();
        for title in ["a", "b", "c", "d"] {
            let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
                &format!(r#"{{"title":"{title}","is_now":true}}"#)).unwrap();
            ids.push(out["task_id"].as_i64().unwrap());
        }
        let live: Vec<i64> = {
            let mut stmt = conn.prepare(
                "SELECT id FROM tasks WHERE is_now = 1 AND state IN ('open','in_progress') ORDER BY id",
            ).unwrap();
            let rows = stmt.query_map([], |r| r.get(0)).unwrap();
            rows.collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(live, vec![ids[0], ids[1], ids[3]], "the newest already in Now stepped aside");

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{},"is_now":true}}"#, ids[2])).unwrap();
        assert_eq!(out["demoted_from_now"][0], ids[3]);
    }

    #[test]
    fn the_agent_cannot_put_a_step_in_now() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"email landlord"}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            &format!(r#"{{"task_id":{id},"steps":[
                {{"title":"find the thread","duration_min":5}},
                {{"title":"write and send","duration_min":10}}]}}"#)).unwrap();
        let step = out["step_ids"][0].as_i64().unwrap();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{step},"is_now":true}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");
    }

    #[test]
    fn task_delete_removes_the_task_and_its_steps() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"junk from a test run"}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            &format!(r#"{{"task_id":{id},"steps":[
                {{"title":"a","duration_min":5}},{{"title":"b","duration_min":5}}]}}"#)).unwrap();

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_delete",
            &format!(r#"{{"task_id":{id}}}"#)).unwrap();
        assert_eq!(out["deleted"], true);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_delete",
            &format!(r#"{{"task_id":{id}}}"#)).unwrap_err();
        assert_eq!(e.kind, "not_found");
    }

    #[test]
    fn task_delete_leaves_another_users_task_alone() {
        let (conn, tmp) = env();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('rin', 'x', 'member')",
            [],
        )
        .unwrap();
        let mut theirs = ctx(&tmp);
        theirs.user_id = 2;
        theirs.username = "rin";
        let out =
            dispatch(&conn, &theirs, SessionKind::Talk, "task_create", r#"{"title":"theirs"}"#)
                .unwrap();
        let id = out["task_id"].as_i64().unwrap();

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_delete",
            &format!(r#"{{"task_id":{id}}}"#)).unwrap_err();
        assert_eq!(e.kind, "not_found");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn task_delete_drops_a_step_like_the_http_route() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"email landlord"}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            &format!(r#"{{"task_id":{id},"steps":[
                {{"title":"a","duration_min":5}},{{"title":"b","duration_min":5}}]}}"#)).unwrap();
        let step = out["step_ids"][0].as_i64().unwrap();

        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_delete",
            &format!(r#"{{"task_id":{step}}}"#)).unwrap();
        let left: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT id FROM tasks ORDER BY id").unwrap();
            let rows = stmt.query_map([], |r| r.get(0)).unwrap();
            rows.collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(left, vec![id, out["step_ids"][1].as_i64().unwrap()]);
    }

    #[test]
    fn task_update_schema_lists_the_valid_states() {
        let schema = schemas(SessionKind::Talk)
            .into_iter()
            .find(|s| s["name"] == "task_update")
            .unwrap();
        let text = schema["input_schema"]["properties"]["state"].to_string();
        for state in ["open", "in_progress", "done", "dropped"] {
            assert!(text.contains(state), "state schema does not mention {state}: {text}");
        }
    }

    #[test]
    fn task_split_makes_one_level_of_steps() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"email landlord"}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            &format!(r#"{{"task_id":{id},"steps":[
                {{"title":"find the last email thread","duration_min":5}},
                {{"title":"write and send","duration_min":10}}]}}"#)).unwrap();
        assert_eq!(out["duration_min"], 15);
        assert_eq!(out["step_ids"].as_array().unwrap().len(), 2);
        let child = out["step_ids"][0].as_i64().unwrap();

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            &format!(r#"{{"task_id":{child},"steps":[
                {{"title":"a","duration_min":5}},{{"title":"b","duration_min":5}}]}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            &format!(r#"{{"task_id":{id},"steps":[{{"title":"only","duration_min":5}}]}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            r#"{"task_id":999,"steps":[{"title":"a","duration_min":5},{"title":"b","duration_min":5}]}"#).unwrap_err();
        assert_eq!(e.kind, "not_found");
    }

    #[test]
    fn unknown_tool_unknown_field_and_bad_json_are_typed() {
        let (conn, tmp) = env();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "rm_rf", "{}").unwrap_err();
        assert_eq!(e.kind, "unknown_tool");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"x","surprise":1}"#).unwrap_err();
        assert_eq!(e.kind, "invalid_args");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create", "not json").unwrap_err();
        assert_eq!(e.kind, "invalid_args");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            r#"{"task_id":999,"state":"done"}"#).unwrap_err();
        assert_eq!(e.kind, "not_found");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            r#"{"task_id":1,"state":"exploded"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
    }

    #[test]
    fn oversized_payload_is_rejected_before_parsing() {
        let (conn, tmp) = env();
        let big = format!(r#"{{"title":"{}"}}"#, "x".repeat(MAX_ARGS_BYTES));
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create", &big).unwrap_err();
        assert_eq!(e.kind, "rejected");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn oversized_text_fields_are_rejected_and_leave_the_row_unchanged() {
        let (conn, tmp) = env();
        let out = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Talk,
            "task_create",
            r#"{"title":"call dentist"}"#,
        )
        .unwrap();
        let id = out["task_id"].as_i64().unwrap();

        let e = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Talk,
            "task_update",
            &format!(r#"{{"task_id":{id},"title":"{}"}}"#, "x".repeat(501)),
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");

        let e = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Talk,
            "task_update",
            &format!(
                r#"{{"task_id":{id},"title":"renamed","notes":"{}"}}"#,
                "x".repeat(16 * 1024 + 1)
            ),
        )
        .unwrap_err();
        assert_eq!(e.kind, "rejected");

        let (title, notes): (String, String) = conn
            .query_row("SELECT title, notes FROM tasks WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(title, "call dentist");
        assert_eq!(notes, "");
    }

    #[test]
    fn prepare_embeds_only_memory_tools_and_reports_failures() {
        use crate::providers::mock::MockEmbeddings;
        let emb = MockEmbeddings;

        let v = prepare(Some(&emb), "memory_query", r#"{"query":"abba"}"#);
        assert!(v.query.is_some());
        assert!(v.content.is_none() && v.error.is_none());

        let v = prepare(Some(&emb), "memory_write", r#"{"op":"add","category":"semantic","summary":"s","body":"b"}"#);
        assert!(v.content.is_some());
        assert!(v.query.is_none());

        // non-memory tools and malformed args cost nothing
        let v = prepare(Some(&emb), "task_create", r#"{"title":"x"}"#);
        assert!(v.content.is_none() && v.query.is_none() && v.error.is_none());
        let v = prepare(Some(&emb), "memory_query", "not json");
        assert!(v.query.is_none() && v.error.is_none());

        // no provider → all None
        let v = prepare(None, "memory_query", r#"{"query":"abba"}"#);
        assert!(v.query.is_none());

        struct FailingEmb;
        impl crate::providers::EmbeddingsProvider for FailingEmb {
            fn embed(&self, _: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
                anyhow::bail!("endpoint down")
            }
        }
        let v = prepare(Some(&FailingEmb), "memory_query", r#"{"query":"abba"}"#);
        assert!(v.query.is_none());
        assert!(v.error.as_deref().unwrap_or("").contains("endpoint down"));
    }

    #[test]
    fn prepared_vector_text_matches_index_text() {
        use crate::providers::{mock::MockEmbeddings, EmbeddingsProvider};
        let emb = MockEmbeddings;
        let v = prepare(Some(&emb), "memory_write",
            r#"{"op":"add","category":"semantic","summary":"line one\nline two","body":"  padded  "}"#);
        let direct = emb
            .embed(&[&crate::memory::embed_text("line one\nline two", "  padded  ")])
            .unwrap();
        assert_eq!(v.content.unwrap(), direct[0]);
    }

    /// Checkin ⊆ Talk ⊆ Nightly but for `trigger_budget` and `plan_carry`,
    /// which live where the user is there to agree to them; a harvest reads
    /// memory with the nightly's own tools plus the one that ends it; Import,
    /// Inbox, Summarize and Harvest share nothing with an import session.
    #[test]
    fn session_surfaces_nest_and_import_stands_apart() {
        let is_subset = |a: &[&str], b: &[&str]| a.iter().all(|t| b.contains(t));
        assert!(is_subset(registry(SessionKind::Checkin), registry(SessionKind::Talk)));
        let asks_the_user = |t: &str| t == "trigger_budget" || t == "plan_carry";
        assert!(registry(SessionKind::Talk)
            .iter()
            .filter(|t| !asks_the_user(t))
            .all(|t| registry(SessionKind::Nightly).contains(t)));
        assert!(
            !registry(SessionKind::Nightly).iter().any(|t| asks_the_user(t)),
            "the nightly run has nobody to ask"
        );
        assert!(registry(SessionKind::Trigger)
            .iter()
            .filter(|t| !is_terminal(SessionKind::Trigger, t) && !asks_the_user(t))
            .all(|t| registry(SessionKind::Nightly).contains(t)));
        assert!(registry(SessionKind::Harvest)
            .iter()
            .filter(|t| !is_terminal(SessionKind::Harvest, t))
            .all(|t| registry(SessionKind::Nightly).contains(t)));
        assert!(registry(SessionKind::Review)
            .iter()
            .filter(|t| !is_terminal(SessionKind::Review, t))
            .all(|t| registry(SessionKind::Nightly).contains(t)));
        assert!(registry(SessionKind::Import)
            .iter()
            .all(|t| !registry(SessionKind::Nightly).contains(t)));
        for kind in [SessionKind::Summarize, SessionKind::Harvest, SessionKind::Review] {
            assert!(
                registry(kind).iter().all(|t| !registry(SessionKind::Import).contains(t)),
                "{kind:?} shares a tool with an import session"
            );
        }
    }

    #[test]
    fn a_summary_writes_nothing_but_its_own_line() {
        assert_eq!(registry(SessionKind::Summarize), &["summary_write"]);
        assert!(is_terminal(SessionKind::Summarize, "summary_write"));
        assert!(is_terminal(SessionKind::Harvest, "harvest_done"));
        assert!(is_terminal(SessionKind::Review, "review_write"));
        let (conn, tmp) = env();
        for kind in [SessionKind::Talk, SessionKind::Nightly, SessionKind::Harvest] {
            let e = dispatch(&conn, &ctx(&tmp), kind, "summary_write", r#"{"summary":"x"}"#)
                .unwrap_err();
            assert_eq!(e.kind, "unknown_tool", "{kind:?}");
        }
        let e = dispatch(
            &conn,
            &ctx(&tmp),
            SessionKind::Summarize,
            "memory_write",
            r#"{"op":"add","category":"semantic","summary":"s","body":"b"}"#,
        )
        .unwrap_err();
        assert_eq!(e.kind, "forbidden");
    }

    #[test]
    fn nightly_notes_write_is_reachable_only_from_the_nightly_run() {
        assert!(registry(SessionKind::Nightly).contains(&"nightly_notes_write"));
        for kind in [
            SessionKind::Talk,
            SessionKind::Checkin,
            SessionKind::Import,
            SessionKind::Inbox,
            SessionKind::Summarize,
            SessionKind::Harvest,
            SessionKind::Review,
            SessionKind::Trigger,
        ] {
            assert!(
                !registry(kind).contains(&"nightly_notes_write"),
                "{kind:?} can write the nightly notes"
            );
        }
        let (conn, tmp) = env();
        for kind in [SessionKind::Talk, SessionKind::Checkin] {
            let e = dispatch(&conn, &ctx(&tmp), kind, "nightly_notes_write", r#"{"text":"x"}"#)
                .unwrap_err();
            assert_eq!(e.kind, "forbidden");
        }
    }

    #[test]
    fn the_import_surface_is_one_tool() {
        assert_eq!(registry(SessionKind::Import), &["task_brief"]);
        let (conn, tmp) = env();
        for name in ["task_update", "task_split"] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Import, name, r#"{"task_id":1}"#)
                .unwrap_err();
            assert_eq!(e.kind, "forbidden", "{name} is still reachable from an import session");
        }
    }

    #[test]
    fn task_brief_is_the_terminal_tool_of_an_import_session() {
        assert!(is_terminal(SessionKind::Import, "task_brief"));
        assert!(!is_terminal(SessionKind::Talk, "task_update"));
    }

    fn scoped<'a>(tmp: &'a tempfile::TempDir, task_id: i64) -> ToolCtx<'a> {
        ToolCtx { task_scope: Some(task_id), ..ctx(tmp) }
    }

    /// Creates a scoped task with two steps, plus an unrelated task, and
    /// returns (scoped id, first step id, foreign id).
    fn scope_fixture(conn: &Connection, tmp: &tempfile::TempDir) -> (i64, i64, i64) {
        let mine = dispatch(conn, &ctx(tmp), SessionKind::Talk, "task_create",
            r#"{"title":"biology ch.4"}"#).unwrap()["task_id"].as_i64().unwrap();
        let split = dispatch(conn, &ctx(tmp), SessionKind::Talk, "task_split",
            &format!(r#"{{"task_id":{mine},"steps":[
                {{"title":"read","duration_min":25}},
                {{"title":"answer","duration_min":20}}]}}"#)).unwrap();
        let step = split["step_ids"][0].as_i64().unwrap();
        let other = dispatch(conn, &ctx(tmp), SessionKind::Talk, "task_create",
            r#"{"title":"someone else's"}"#).unwrap()["task_id"].as_i64().unwrap();
        (mine, step, other)
    }

    #[test]
    fn a_scoped_session_reaches_only_its_own_task_and_steps() {
        let (conn, tmp) = env();
        let (mine, step, other) = scope_fixture(&conn, &tmp);

        dispatch(&conn, &scoped(&tmp, mine), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{mine},"description":"a brief"}}"#)).unwrap();
        dispatch(&conn, &scoped(&tmp, mine), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{step},"title":"read carefully"}}"#)).unwrap();

        let e = dispatch(&conn, &scoped(&tmp, mine), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{other},"description":"not yours"}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");
        let e = dispatch(&conn, &scoped(&tmp, mine), SessionKind::Talk, "task_update",
            r#"{"task_id":9999,"description":"nowhere"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
    }

    #[test]
    fn a_scoped_split_refuses_a_step_and_a_foreign_task() {
        let (conn, tmp) = env();
        let (mine, step, other) = scope_fixture(&conn, &tmp);
        let two_steps = r#""steps":[{"title":"a","duration_min":5},{"title":"b","duration_min":5}]"#;
        for target in [step, other] {
            let e = dispatch(&conn, &scoped(&tmp, mine), SessionKind::Talk, "task_split",
                &format!(r#"{{"task_id":{target},{two_steps}}}"#)).unwrap_err();
            assert_eq!(e.kind, "rejected", "task_split reached {target}");
        }
    }

    #[test]
    fn a_scoped_session_cannot_touch_now() {
        let (conn, tmp) = env();
        let (mine, _, _) = scope_fixture(&conn, &tmp);
        for value in ["true", "false"] {
            let e = dispatch(&conn, &scoped(&tmp, mine), SessionKind::Talk, "task_update",
                &format!(r#"{{"task_id":{mine},"is_now":{value}}}"#)).unwrap_err();
            assert_eq!(e.kind, "rejected");
        }
        let is_now: i64 = conn
            .query_row("SELECT is_now FROM tasks WHERE id = ?1", [mine], |r| r.get(0))
            .unwrap();
        assert_eq!(is_now, 0);
    }

    #[test]
    fn a_scoped_session_may_only_drop_never_restate() {
        let (conn, tmp) = env();
        let (mine, step, _) = scope_fixture(&conn, &tmp);
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{mine},"state":"in_progress"}}"#)).unwrap();
        for target in [mine, step] {
            for state in ["done", "in_progress", "open"] {
                let e = dispatch(&conn, &scoped(&tmp, mine), SessionKind::Talk, "task_update",
                    &format!(r#"{{"task_id":{target},"state":"{state}"}}"#)).unwrap_err();
                assert_eq!(e.kind, "rejected", "task {target} was set {state}");
            }
        }
        let state: String = conn
            .query_row("SELECT state FROM tasks WHERE id = ?1", [mine], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "in_progress");
    }

    #[test]
    fn a_scoped_session_drops_only_an_open_task() {
        let (conn, tmp) = env();
        let (mine, _, _) = scope_fixture(&conn, &tmp);
        let drop = format!(r#"{{"task_id":{mine},"state":"dropped"}}"#);
        for state in ["in_progress", "done"] {
            dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
                &format!(r#"{{"task_id":{mine},"state":"{state}"}}"#)).unwrap();
            let e = dispatch(&conn, &scoped(&tmp, mine), SessionKind::Talk, "task_update", &drop)
                .unwrap_err();
            assert_eq!(e.kind, "rejected", "dropped a task that was {state}");
        }
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{mine},"state":"open"}}"#)).unwrap();
        let out =
            dispatch(&conn, &scoped(&tmp, mine), SessionKind::Talk, "task_update", &drop).unwrap();
        assert_eq!(out["state"], "dropped");
    }

    #[test]
    fn schemas_cover_the_registry_and_are_objects() {
        for kind in [
            SessionKind::Nightly,
            SessionKind::Checkin,
            SessionKind::Talk,
            SessionKind::Import,
            SessionKind::Inbox,
            SessionKind::Summarize,
            SessionKind::Harvest,
            SessionKind::Review,
            SessionKind::Trigger,
        ] {
            let schemas = schemas(kind);
            assert_eq!(schemas.len(), registry(kind).len());
            for s in schemas {
                assert!(s["name"].is_string());
                assert!(!s["description"].as_str().unwrap().is_empty());
                assert!(s["input_schema"].is_object());
            }
        }
    }
}
