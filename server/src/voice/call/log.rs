use super::jobs::JobDone;
use super::render::JobOutcome;
use crate::failure::Failure;
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Reply numbers reserved in `voice_calls.last_reply` per write.
const REPLY_BLOCK: u64 = 16;

struct JobRow {
    args: String,
    started: Duration,
    written: bool,
    row: Option<i64>,
}

/// What a call writes to the thread and to `voice_calls`.
pub struct CallLog {
    db: Arc<Mutex<Connection>>,
    call_id: String,
    user_id: i64,
    conversation_id: i64,
    /// Heard turns not yet written; written before any row that follows them.
    heard: Vec<(String, jiff::Timestamp)>,
    jobs: HashMap<u32, JobRow>,
    /// The highest reply number handed out.
    taken: u64,
    /// The highest reply number persisted as reserved.
    reserved: u64,
}

impl CallLog {
    pub fn new(db: Arc<Mutex<Connection>>, call_id: String, user_id: i64, conversation_id: i64) -> Self {
        Self {
            db,
            call_id,
            user_id,
            conversation_id,
            heard: Vec::new(),
            jobs: HashMap::new(),
            taken: 1,
            reserved: 1,
        }
    }

    /// The next reply number; a resumed call continues above every number reserved before.
    pub fn next_reply(&mut self) -> u64 {
        if self.taken < self.reserved {
            self.taken += 1;
            return self.taken;
        }
        let reserved = crate::db_guard(&self.db).query_row(
            "UPDATE voice_calls SET last_reply = MAX(last_reply, ?2) + ?3 WHERE id = ?1 RETURNING last_reply",
            (&self.call_id, self.taken as i64, REPLY_BLOCK as i64),
            |r| r.get::<_, i64>(0),
        );
        match reserved {
            Ok(n) => {
                self.reserved = n.max(0) as u64;
                self.taken = self.reserved.saturating_sub(REPLY_BLOCK - 1);
            }
            Err(e) => {
                eprintln!("voice: taking a reply number for {} failed: {e}", self.call_id);
                self.taken += 1;
            }
        }
        self.taken
    }

    pub fn append_user(&mut self, text: &str) {
        self.heard.push((text.to_string(), jiff::Timestamp::now()));
    }

    pub fn flush_user(&mut self) {
        if self.heard.is_empty() {
            return;
        }
        let conn = crate::db_guard(&self.db);
        for (text, at) in self.heard.drain(..) {
            if let Err(e) = crate::talk::append_text(&conn, self.conversation_id, "user", &text, at) {
                eprintln!("voice: recording a heard turn for {} failed: {e:#}", self.call_id);
            }
        }
    }

    /// The row id of the new assistant row.
    pub fn append_assistant(&mut self, text: &str) -> Option<i64> {
        self.flush_user();
        let conn = crate::db_guard(&self.db);
        match crate::talk::append_assistant(&conn, self.conversation_id, text, "", 0, jiff::Timestamp::now()) {
            Ok(()) => Some(conn.last_insert_rowid()),
            Err(e) => {
                eprintln!("voice: recording a reply for {} failed: {e:#}", self.call_id);
                None
            }
        }
    }

    /// Rewrites assistant row `row` to what the caller heard of it.
    pub fn trim(&self, row: i64, heard: &str) {
        let updated =
            crate::db_guard(&self.db).execute("UPDATE talk_messages SET content = ?1 WHERE id = ?2", (heard, row));
        if let Err(e) = updated {
            eprintln!("voice: trimming a cut-off reply for {} failed: {e}", self.call_id);
        }
    }

    pub fn job_started(&mut self, job: u32, args: &str, started: Duration) {
        self.jobs.insert(job, JobRow { args: args.to_string(), started, written: false, row: None });
    }

    pub fn job_unwritten(&self, job: u32) -> bool {
        self.jobs.get(&job).is_some_and(|j| !j.written)
    }

    pub fn any_job_unwritten(&self) -> bool {
        self.jobs.values().any(|j| !j.written)
    }

    /// Marks `job` written; returns its args and how long it ran by `now`.
    pub fn job_landed(&mut self, job: u32, now: Duration) -> (String, u64) {
        match self.jobs.get_mut(&job) {
            Some(j) => {
                j.written = true;
                (j.args.clone(), now.saturating_sub(j.started).as_millis() as u64)
            }
            None => (String::new(), 0),
        }
    }

    /// Writes the job's tool row, or updates the one already written for it.
    pub fn record_job(&mut self, job: u32, tool: &str, args: &str, result: &str, is_error: bool) {
        self.flush_user();
        let row = self.jobs.get(&job).and_then(|j| j.row);
        let conn = crate::db_guard(&self.db);
        let written = if let Some(id) = row {
            conn.execute("UPDATE talk_messages SET content = ?1, is_error = ?2 WHERE id = ?3", (result, is_error, id))
                .map(|_| ())
                .map_err(anyhow::Error::from)
        } else {
            crate::talk::append_tool(
                &conn,
                self.conversation_id,
                tool,
                args,
                result,
                is_error,
                None,
                jiff::Timestamp::now(),
            )
            .map(|()| {
                if let Some(j) = self.jobs.get_mut(&job) {
                    j.row = Some(conn.last_insert_rowid());
                }
            })
        };
        if let Err(e) = written {
            eprintln!("voice: recording job {job} of {} failed: {e:#}", self.call_id);
        }
    }

    pub fn bowed_out(&self) {
        if let Err(e) = super::mark_bowed_out(&crate::db_guard(&self.db), &self.call_id) {
            eprintln!("voice: recording the bow-out of {} failed: {e:#}", self.call_id);
        }
    }

    pub fn turn_failed(&self, e: &Failure) {
        let conn = crate::db_guard(&self.db);
        let _ = crate::log::record(&conn, Some(self.user_id), "voice_error", &format!("a call's turn failed: {e}"));
    }

    pub fn save_trace(&self, trace: &crate::trace::Builder) {
        if let Err(e) = trace.insert(&crate::db_guard(&self.db), self.user_id) {
            eprintln!("voice: saving the trace of {} failed: {e:#}", self.call_id);
        }
    }
}

/// The tool row's content for a job's outcome, and whether it is an error.
pub fn job_result(done: &JobDone) -> (String, bool) {
    match &done.outcome {
        JobOutcome::Done(text) => (text.clone(), false),
        JobOutcome::Error(text) => (text.clone(), true),
        JobOutcome::Cancelled => (serde_json::json!({"job": done.job, "cancelled": true}).to_string(), false),
        JobOutcome::TimedOut => (serde_json::json!({"job": done.job, "timed_out": true}).to_string(), true),
        JobOutcome::Interrupted => (serde_json::json!({"job": done.job, "interrupted": true}).to_string(), true),
    }
}
