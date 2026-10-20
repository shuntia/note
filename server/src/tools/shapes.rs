//! The input schemas of the `_find`, `_save` and `_remove` tools, written out
//! by hand: canonical names, plain types, and only what a model needs.

use serde_json::{json, Value};

const DAY: &str = "A day: YYYY-MM-DD, M/D, today, tomorrow or a weekday.";
const DUE: &str = "A day (YYYY-MM-DD, M/D, today, friday) or a moment (YYYY-MM-DDTHH:MM, HH:MM). Empty clears it.";

fn ids(what: &str) -> Value {
    json!({ "type": "array", "items": { "type": "integer" }, "description": what })
}

fn node() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "integer", "description": "Updates this task; an exact title also works." },
            "parent": { "type": "integer", "description": "Attach or move under this task (id or exact title); 0 makes it top-level." },
            "before": { "type": "integer", "description": "Place it before this sibling." },
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
            "children": { "type": "array", "items": { "type": "object" }, "description": "Child tasks of this same shape, or plain titles." }
        }
    })
}

pub fn task_find() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "integer", "description": "Read this task in full with its whole subtree; a list of ids works too." },
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
                        "id": { "type": "integer", "description": "Updates this goal; an exact title also works." },
                        "title": { "type": "string" },
                        "description": { "type": "string" },
                        "due": { "type": "string", "description": DUE },
                        "state": { "type": "string", "enum": ["open", "done", "dropped"] },
                        "tasks": { "type": "array", "items": { "type": "object" }, "description": "Task nodes, as task_save takes them, that hang from this goal." }
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
                        "id": { "type": "integer", "description": "Changes this entry." },
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
