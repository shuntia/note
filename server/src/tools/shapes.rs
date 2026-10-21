//! The input schemas of the `_find`, `_save` and `_remove` tools, written out
//! by hand: canonical names, plain types, and only what a model needs.

use serde_json::{json, Value};

const DAY: &str = "A day: YYYY-MM-DD, M/D, today, tomorrow or a weekday.";
const DUE: &str = "A day (YYYY-MM-DD, M/D, today, friday) or a moment (YYYY-MM-DDTHH:MM, HH:MM). Empty clears it.";

/// An id, or the exact title the forgiving reader resolves.
fn id_or_title() -> Value {
    json!(["integer", "string"])
}

fn ids(what: &str) -> Value {
    json!({ "type": "array", "items": { "type": id_or_title() }, "description": what })
}

fn node() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": id_or_title(), "description": "Updates this task; an exact title also works." },
            "parent": { "type": id_or_title(), "description": "Attach or move under this task (id or exact title); 0 makes it top-level." },
            "before": { "type": id_or_title(), "description": "Place it before this sibling." },
            "title": { "type": "string" },
            "description": { "type": "string" },
            "notes": { "type": "string" },
            "minutes": { "type": "integer", "description": "Its size; \"1h30\" works too." },
            "due": { "type": "string", "description": DUE },
            "progress": { "type": "integer", "description": "0-100." },
            "state": { "type": "string", "enum": ["open", "in_progress", "done", "dropped"] },
            "urgency": { "type": "string", "enum": ["low", "normal", "high"] },
            "notify": { "type": "string", "enum": ["none", "chat", "notify"], "description": "How its block announces itself." },
            "now": { "type": "boolean", "description": "In Now, the short list of at most 3." },
            "category": { "type": "string", "description": "Top-level tasks only: a course, project or part of life." },
            "goal_id": { "type": "integer", "description": "Top-level tasks only." },
            "url": { "type": "string" },
            "new": { "type": "boolean", "description": "Create it even though an open task under the same parent has this title." },
            "children": { "type": "array", "items": { "type": ["object", "string"] }, "description": "Child tasks of this same shape, or plain titles. A part already finished goes in with state done, so progress counts." }
        }
    })
}

pub fn task_find() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": ["integer", "string", "array"], "items": { "type": id_or_title() }, "description": "Read it in full with its whole subtree; a list works too." },
            "query": { "type": "string", "description": "Words that must all match, at any depth." },
            "state": { "type": "string", "enum": ["open", "in_progress", "done", "dropped", "any"], "description": "Default: open and in_progress." },
            "now": { "type": "boolean" },
            "category": { "type": "string" },
            "goal_id": { "type": "integer" },
            "due": { "type": "string", "description": DAY },
            "due_before": { "type": "string", "description": "A day, left out itself." },
            "due_after": { "type": "string", "description": "A day, kept itself." },
            "overdue": { "type": "boolean" },
            "urgency": { "type": "string", "enum": ["low", "normal", "high"] },
            "sort": { "type": "string", "enum": ["added", "due", "urgency"] },
            "depth": { "type": "integer", "description": "Levels of children a survey shows, 0-4; default 1." },
            "limit": { "type": "integer", "description": "1-200; default 50." }
        }
    })
}

pub fn task_save() -> Value {
    json!({
        "type": "object",
        "properties": { "tasks": { "type": "array", "items": node() } },
        "required": ["tasks"]
    })
}

pub fn task_remove() -> Value {
    json!({
        "type": "object",
        "properties": {
            "ids": ids("Tasks to delete, each with its whole subtree."),
            "all": { "type": "boolean", "description": "Needed when more than 20 tasks would go." }
        },
        "required": ["ids"]
    })
}

pub fn goal_find() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "integer", "description": "Read this goal with its tasks." },
            "state": { "type": "string", "enum": ["open", "done", "dropped", "any"], "description": "Default: open." }
        }
    })
}

pub fn goal_save() -> Value {
    json!({
        "type": "object",
        "properties": {
            "goals": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": id_or_title(), "description": "Updates this goal; an exact title also works." },
                        "title": { "type": "string" },
                        "description": { "type": "string" },
                        "due": { "type": "string", "description": DUE },
                        "state": { "type": "string", "enum": ["open", "done", "dropped"] },
                        "tasks": { "type": "array", "items": { "type": ["object", "string"] }, "description": "Task nodes, as task_save takes them, that hang from this goal." }
                    }
                }
            }
        },
        "required": ["goals"]
    })
}

pub fn goal_remove() -> Value {
    json!({ "type": "object", "properties": { "ids": ids("Goals to delete; their tasks stay.") }, "required": ["ids"] })
}

pub fn calendar_find() -> Value {
    json!({
        "type": "object",
        "properties": {
            "date": { "type": "string", "description": "First day to show; default today." },
            "days": { "type": "integer", "description": "How many days, 1-14; default 1." }
        }
    })
}

pub fn calendar_save() -> Value {
    json!({
        "type": "object",
        "properties": {
            "entries": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": id_or_title(), "description": "Changes this entry." },
                        "title": { "type": "string" },
                        "kind": { "type": "string", "enum": ["fixed", "busy", "note", "free"] },
                        "start": { "type": "string", "description": "HH:MM on the user's clock." },
                        "end": { "type": "string", "description": "HH:MM." },
                        "days": { "type": "array", "items": { "type": "string", "enum": ["mon", "tue", "wed", "thu", "fri", "sat", "sun"] }, "description": "Makes it repeat." },
                        "on": { "type": "string", "description": "The one day of a one-off." },
                        "from": { "type": "string", "description": "First day a repeating entry holds." },
                        "until": { "type": "string", "description": "Last day a repeating entry holds." },
                        "quiet": { "type": "boolean", "description": "Hold deliveries until it ends; default true, never for note or free." },
                        "tz": { "type": "string", "description": "IANA zone a one-off's day and times are given in." },
                        "skip": { "type": "array", "items": { "type": "string" }, "description": "Days a repeating entry does not happen." },
                        "unskip": { "type": "array", "items": { "type": "string" }, "description": "Skipped days to restore." },
                        "new": { "type": "boolean", "description": "Add it even though one of the same title overlaps." }
                    }
                }
            }
        },
        "required": ["entries"]
    })
}

pub fn calendar_remove() -> Value {
    json!({ "type": "object", "properties": { "ids": ids("Entries to delete for good.") }, "required": ["ids"] })
}

pub fn day_find() -> Value {
    json!({
        "type": "object",
        "properties": {
            "date": { "type": "string", "description": DAY },
            "days": { "type": "integer", "description": "How many days, 1-7; default 1." }
        }
    })
}

pub fn day_save() -> Value {
    json!({
        "type": "object",
        "properties": {
            "date": { "type": "string", "description": "The day; default today, and for the nightly run the day it plans." },
            "items": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "An item of the day as day_find lists it (t412, e88), to change it." },
                        "task": { "type": id_or_title(), "description": "A task to place (id or exact title)." },
                        "at": { "type": "string", "description": "HH:MM, 9am or +20min. On a task it makes a block; empty makes it untimed." },
                        "minutes": { "type": "integer", "description": "A block's length; default the task's size." },
                        "before": { "type": id_or_title(), "description": "Place the task in front of this item of the list; \"end\" puts it last." },
                        "wake": { "type": "string", "description": "A wake-up: your note on what to look at when at comes." },
                        "ring": { "type": "boolean", "description": "The wake-up calls the user." },
                        "cancel_on": { "type": "string", "enum": ["reply", "task"], "description": "What calls the wake-up off first: the user writing back in thread, or task finished." },
                        "session_check": { "type": "boolean", "description": "A check on the open work session, dropped when it ends." },
                        "thread": { "type": "integer", "description": "The conversation the wake-up speaks in; default the day's check-in thread." },
                        "routine": { "type": "string", "description": "Add a routine of this name at at." },
                        "flexibility": { "type": "string", "enum": ["fixed", "slide", "drop"], "description": "A new routine's; default slide." },
                        "snooze_min": { "type": "integer", "description": "Hold back a routine's delivery this long." }
                    }
                }
            },
            "replace": { "type": "boolean", "description": "The task items given become the day's whole task list, in order." },
            "carry": { "type": "boolean", "description": "Move what is left of the day to the next: untimed items to its front, blocks at their time." },
            "extra_wakes": {
                "type": "object",
                "properties": { "n": { "type": "integer" }, "reason": { "type": "string" } },
                "description": "Raise the day's wake-up budget by n, only once the user agreed; reason is what they agreed to."
            },
            "tz": { "type": "string", "description": "The IANA zone the times are given in, when not the user's." }
        },
        "required": ["items"]
    })
}

pub fn day_remove() -> Value {
    json!({
        "type": "object",
        "properties": {
            "date": { "type": "string", "description": DAY },
            "ids": ids("Items to take off the day: t412, e88, or a task's exact title.")
        },
        "required": ["ids"]
    })
}

pub fn task_brief() -> Value {
    json!({
        "type": "object",
        "properties": {
            "task_id": { "type": "integer" },
            "homework": { "type": "boolean", "description": "False drops it as not work; true writes the brief." },
            "reason": { "type": "string", "description": "Why it is not work, under 100 characters." },
            "description": { "type": "string", "description": "At most 6 short plain lines." },
            "minutes": { "type": "integer", "description": "Focused minutes." },
            "category": { "type": "string", "description": "The course or source it came from." },
            "children": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "title": { "type": "string" }, "minutes": { "type": "integer" } },
                    "required": ["title"]
                },
                "description": "Its parts in order, while it has none yet."
            }
        },
        "required": ["task_id", "homework"]
    })
}

/// Any object; an old name's arguments are read by the tool that replaced it.
pub fn any() -> Value {
    json!({ "type": "object" })
}

pub fn memory_find() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string" },
            "id": { "type": ["string", "array"], "items": { "type": "string" }, "description": "Read in full; a list works too." },
            "kind": { "type": "string", "enum": ["semantic", "episodic", "procedural", "note"], "description": "Notes show only with kind note." },
            "limit": { "type": "integer", "description": "1-50; default 8." }
        }
    })
}

pub fn memory_save() -> Value {
    json!({
        "type": "object",
        "properties": {
            "items": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Changes this memory or note; a note's id alone keeps it." },
                        "kind": { "type": "string", "enum": ["semantic", "episodic", "procedural", "note"], "description": "Default semantic. A note given another kind becomes that memory." },
                        "title": { "type": "string", "description": "One line." },
                        "body": { "type": "string" },
                        "from": { "type": "string", "description": "A note's start, YYYY-MM-DDTHH:MM." },
                        "until": { "type": "string", "description": "When it stops mattering, for what is tied to a stretch of time: a day, or a note's moment. Empty clears it." },
                        "supersedes": { "type": "string", "description": "A memory this replaces, archived." }
                    }
                }
            }
        },
        "required": ["items"]
    })
}

pub fn memory_remove() -> Value {
    json!({
        "type": "object",
        "properties": { "ids": { "type": "array", "items": { "type": "string" }, "description": "An exact title works too." } },
        "required": ["ids"]
    })
}
