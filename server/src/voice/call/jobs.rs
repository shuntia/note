use super::render::{Item, JobOutcome, Running};
use crate::tools::ToolError;
use rusqlite::{Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

pub trait ToolRunner: Send + Sync + 'static {
    /// Runs one tool; `once` is `(call_id, op_key)` for exactly-once writes.
    fn run(&self, name: &str, args: &str, once: (&str, &str)) -> (String, bool);
    /// Whether the tool reaches the network (cancellable, runs without the DB).
    fn is_network(&self, name: &str) -> bool;
}

pub struct JobLimits {
    pub timeout: Duration,
    pub max_running: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobDone {
    pub job: u32,
    pub tool: String,
    pub outcome: JobOutcome,
}

const CANCEL_WAIT: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Progress {
    /// The first of the tool's result, the deadline and a cancel to end the job.
    ended_as: Option<&'static str>,
    result: Option<JobOutcome>,
}

#[derive(Default)]
struct Slot {
    progress: Mutex<Progress>,
    changed: Condvar,
}

impl Slot {
    fn lock(&self) -> MutexGuard<'_, Progress> {
        self.progress.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// True when this ends the job; false when something already had.
    fn end_as(&self, state: &'static str) -> bool {
        let mut progress = self.lock();
        let first = progress.ended_as.is_none();
        if first {
            progress.ended_as = Some(state);
            self.changed.notify_all();
        }
        first
    }
}

struct RunningJob {
    tool: String,
    network: bool,
    started: Instant,
    slot: Arc<Slot>,
}

pub struct JobTable {
    call_id: String,
    db: Arc<Mutex<Connection>>,
    runner: Arc<dyn ToolRunner>,
    limits: JobLimits,
    next: u32,
    running: HashMap<u32, RunningJob>,
    /// Jobs already reported to the model, with the state they were reported in.
    reported: HashMap<u32, &'static str>,
    /// Writes reported as timed out whose real result is still owed to the model.
    owed: HashSet<u32>,
    done_tx: mpsc::Sender<JobDone>,
}

impl JobTable {
    pub fn new(
        call_id: String,
        db: Arc<Mutex<Connection>>,
        runner: Arc<dyn ToolRunner>,
        limits: JobLimits,
        done_tx: mpsc::Sender<JobDone>,
    ) -> Self {
        let last: u32 = crate::db_guard(&db)
            .query_row("SELECT COALESCE(MAX(job), 0) FROM voice_jobs WHERE call_id = ?1", [&call_id], |r| {
                r.get(0)
            })
            .unwrap_or(0);
        Self {
            call_id,
            db,
            runner,
            limits,
            next: last + 1,
            running: HashMap::new(),
            reported: HashMap::new(),
            owed: HashSet::new(),
            done_tx,
        }
    }

    /// Starts `call` as a job of reply `reply`; returns the ack JSON for the model, or a tool error JSON when capped.
    pub fn start(&mut self, reply: u64, call_index: usize, name: &str, args: &str) -> (String, bool) {
        if self.running.len() >= self.limits.max_running {
            return error(&ToolError::cap_reached(format!(
                "{} jobs are running; wait for a result",
                self.limits.max_running
            )));
        }
        let job = self.next;
        let inserted = crate::db_guard(&self.db).execute(
            "INSERT INTO voice_jobs (call_id, job, reply, call_index, tool, args, state, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running', ?7)",
            (&self.call_id, job, reply as i64, call_index as i64, name, args, now()),
        );
        if let Err(e) = inserted {
            return error(&ToolError::internal(e.to_string()));
        }
        self.next += 1;
        let network = self.runner.is_network(name);
        let slot = Arc::new(Slot::default());
        self.spawn_worker(job, name, args, format!("{reply}:{call_index}"), network, slot.clone());
        self.spawn_watchdog(job, name, slot.clone());
        self.running.insert(job, RunningJob { tool: name.to_string(), network, started: Instant::now(), slot });
        (serde_json::json!({"job": job, "started": true}).to_string(), false)
    }

    fn spawn_worker(&self, job: u32, name: &str, args: &str, op_key: String, network: bool, slot: Arc<Slot>) {
        let (runner, db, call_id, tx) = (self.runner.clone(), self.db.clone(), self.call_id.clone(), self.done_tx.clone());
        let (name, args) = (name.to_string(), args.to_string());
        std::thread::spawn(move || {
            let (text, is_error) = runner.run(&name, &args, (&call_id, &op_key));
            let (outcome, state) =
                if is_error { (JobOutcome::Error(text.clone()), "error") } else { (JobOutcome::Done(text.clone()), "done") };
            let first = {
                let mut progress = slot.lock();
                progress.result = Some(outcome.clone());
                let first = progress.ended_as.is_none();
                if first {
                    progress.ended_as = Some(state);
                }
                slot.changed.notify_all();
                first
            };
            if first || !network {
                finish_row(&db, &call_id, job, state, Some(&text), false);
            }
            let _ = tx.send(JobDone { job, tool: name, outcome });
        });
    }

    fn spawn_watchdog(&self, job: u32, name: &str, slot: Arc<Slot>) {
        let (db, call_id, tx, timeout) = (self.db.clone(), self.call_id.clone(), self.done_tx.clone(), self.limits.timeout);
        let tool = name.to_string();
        std::thread::spawn(move || {
            let first = {
                let progress = slot.lock();
                let (mut progress, _) = slot
                    .changed
                    .wait_timeout_while(progress, timeout, |p| p.ended_as.is_none())
                    .unwrap_or_else(PoisonError::into_inner);
                let first = progress.ended_as.is_none();
                if first {
                    progress.ended_as = Some("timed_out");
                }
                first
            };
            if first {
                finish_row(&db, &call_id, job, "timed_out", None, true);
                let _ = tx.send(JobDone { job, tool, outcome: JobOutcome::TimedOut });
            }
        });
    }

    /// `cancel_job`: the tool result for the model.
    pub fn cancel(&mut self, job: u32) -> (String, bool) {
        let Some(running) = self.running.get(&job) else {
            return match self.reported.get(&job) {
                Some(state) => already(job, state),
                None => error(&ToolError::not_found(format!("no job {job} in this call"))),
            };
        };
        if let Some(state) = running.slot.lock().ended_as {
            return already(job, state);
        }
        if running.network {
            if !self.cancel_network(job) {
                return already(job, running.slot.lock().ended_as.unwrap_or("done"));
            }
            return (serde_json::json!({"job": job, "cancelled": true}).to_string(), false);
        }
        let slot = running.slot.clone();
        let landed = {
            let progress = slot.lock();
            let (progress, _) = slot
                .changed
                .wait_timeout_while(progress, CANCEL_WAIT, |p| p.result.is_none())
                .unwrap_or_else(PoisonError::into_inner);
            progress.result.clone()
        };
        let reply = match &landed {
            Some(JobOutcome::Done(text)) => serde_json::json!({"job": job, "done": true, "result": value(text)}),
            Some(JobOutcome::Error(text)) => serde_json::json!({"job": job, "error": value(text)}),
            _ => return (serde_json::json!({"job": job, "cancelling": true}).to_string(), false),
        };
        self.running.remove(&job);
        self.reported.insert(job, if matches!(landed, Some(JobOutcome::Done(_))) { "done" } else { "error" });
        (reply.to_string(), false)
    }

    fn cancel_network(&self, job: u32) -> bool {
        let running = &self.running[&job];
        if !running.slot.end_as("cancelled") {
            return false;
        }
        finish_row(&self.db, &self.call_id, job, "cancelled", None, true);
        let _ = self.done_tx.send(JobDone { job, tool: running.tool.clone(), outcome: JobOutcome::Cancelled });
        true
    }

    /// Called by the driver when a `JobDone` arrives, before rendering it; false means discard it (already reported).
    /// A write reported as timed out is reported once more when its real result lands.
    pub fn settle(&mut self, done: &JobDone) -> bool {
        let real = matches!(done.outcome, JobOutcome::Done(_) | JobOutcome::Error(_));
        match self.running.remove(&done.job) {
            Some(job) => {
                if done.outcome == JobOutcome::TimedOut && !job.network {
                    self.owed.insert(done.job);
                }
            }
            None if real && self.owed.remove(&done.job) => {}
            None => return false,
        }
        self.reported.insert(done.job, state_of(&done.outcome));
        true
    }

    pub fn running(&self) -> Vec<Running> {
        let mut out: Vec<Running> = self
            .running
            .iter()
            .filter(|(_, r)| r.slot.lock().ended_as.is_none())
            .map(|(job, r)| Running { job: *job, tool: r.tool.clone(), elapsed: r.started.elapsed() })
            .collect();
        out.sort_by_key(|r| r.job);
        out
    }

    /// The call ended: cancel network jobs, let the rest finish (their `JobDone` still arrive).
    pub fn end(&mut self) {
        let network: Vec<u32> = self.running.iter().filter(|(_, r)| r.network).map(|(job, _)| *job).collect();
        for job in network {
            self.cancel_network(job);
        }
    }

    /// After a Note restart: every `running` row of `call_id` becomes `done` (from `voice_ops`) or `interrupted`,
    /// and a `timed_out` row whose write landed becomes `done`; returns them as items.
    pub fn recover(db: &Mutex<Connection>, call_id: &str) -> Vec<Item> {
        let conn = crate::db_guard(db);
        let rows: Vec<(u32, i64, i64, String, bool)> = conn
            .prepare(
                "SELECT job, reply, call_index, tool, state = 'timed_out' FROM voice_jobs
                 WHERE call_id = ?1 AND state IN ('running', 'timed_out') ORDER BY job",
            )
            .and_then(|mut stmt| {
                stmt.query_map([call_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect()
            })
            .unwrap_or_default();
        let finished = now();
        rows.into_iter()
            .filter_map(|(job, reply, call_index, tool, timed_out)| {
                let stored: Option<String> = conn
                    .query_row(
                        "SELECT result FROM voice_ops WHERE call_id = ?1 AND op_key = ?2",
                        (call_id, format!("{reply}:{call_index}")),
                        |r| r.get(0),
                    )
                    .optional()
                    .unwrap_or(None);
                let (state, outcome) = match &stored {
                    Some(result) => ("done", JobOutcome::Done(result.clone())),
                    None if timed_out => return None,
                    None => ("interrupted", JobOutcome::Interrupted),
                };
                let _ = conn.execute(
                    "UPDATE voice_jobs SET state = ?3, result = ?4, finished_at = ?5 WHERE call_id = ?1 AND job = ?2",
                    (call_id, job, state, &stored, &finished),
                );
                Some(Item::Job { job, tool, outcome })
            })
            .collect()
    }
}

/// `only_running` leaves a row alone once something else has ended it.
fn finish_row(db: &Mutex<Connection>, call_id: &str, job: u32, state: &str, result: Option<&str>, only_running: bool) {
    let _ = crate::db_guard(db).execute(
        "UPDATE voice_jobs SET state = ?3, result = ?4, finished_at = ?5
         WHERE call_id = ?1 AND job = ?2 AND (state = 'running' OR NOT ?6)",
        (call_id, job, state, result, now(), only_running),
    );
}

fn state_of(outcome: &JobOutcome) -> &'static str {
    match outcome {
        JobOutcome::Done(_) => "done",
        JobOutcome::Error(_) => "error",
        JobOutcome::Cancelled => "cancelled",
        JobOutcome::TimedOut => "timed_out",
        JobOutcome::Interrupted => "interrupted",
    }
}

fn already(job: u32, state: &str) -> (String, bool) {
    (serde_json::json!({"job": job, "already": state}).to_string(), false)
}

fn error(e: &ToolError) -> (String, bool) {
    (serde_json::to_string(e).unwrap_or_else(|_| r#"{"kind":"internal"}"#.into()), true)
}

fn value(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|_| serde_json::Value::String(text.to_string()))
}

fn now() -> String {
    jiff::Timestamp::now().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Instant;

    struct Fake {
        delay: Duration,
        result: &'static str,
        network: bool,
        fails: bool,
    }

    struct FakeRunner(HashMap<&'static str, Fake>);

    impl ToolRunner for FakeRunner {
        fn run(&self, name: &str, _args: &str, _once: (&str, &str)) -> (String, bool) {
            let fake = &self.0[name];
            std::thread::sleep(fake.delay);
            (fake.result.to_string(), fake.fails)
        }
        fn is_network(&self, name: &str) -> bool {
            self.0[name].network
        }
    }

    fn fake(name: &'static str, ms: u64, result: &'static str, network: bool) -> (&'static str, Fake) {
        (name, Fake { delay: Duration::from_millis(ms), result, network, fails: false })
    }

    fn failing(name: &'static str, ms: u64, result: &'static str) -> (&'static str, Fake) {
        (name, Fake { delay: Duration::from_millis(ms), result, network: false, fails: true })
    }

    fn db() -> Arc<Mutex<Connection>> {
        let conn = crate::db::open_memory().unwrap();
        conn.execute_batch(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member');
             INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at)
                 VALUES ('c1', 1, 'outbound', 'answered', 'x', 'x');",
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn table(
        db: &Arc<Mutex<Connection>>,
        fakes: Vec<(&'static str, Fake)>,
        timeout_ms: u64,
    ) -> (JobTable, mpsc::Receiver<JobDone>) {
        let (tx, rx) = mpsc::channel();
        let limits = JobLimits { timeout: Duration::from_millis(timeout_ms), max_running: 8 };
        let runner = Arc::new(FakeRunner(fakes.into_iter().collect()));
        (JobTable::new("c1".into(), db.clone(), runner, limits, tx), rx)
    }

    fn state(db: &Mutex<Connection>, job: u32) -> (String, Option<String>) {
        crate::db_guard(db)
            .query_row(
                "SELECT state, result FROM voice_jobs WHERE call_id = 'c1' AND job = ?1",
                [job],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    fn json(text: &str) -> serde_json::Value {
        serde_json::from_str(text).unwrap()
    }

    fn next(rx: &mpsc::Receiver<JobDone>) -> JobDone {
        rx.recv_timeout(Duration::from_secs(2)).expect("a JobDone arrives")
    }

    #[test]
    fn start_acks_at_once_and_reports_when_done() {
        let db = db();
        let (mut jobs, rx) = table(&db, vec![fake("calendar_add", 40, r#"{"id":3}"#, false)], 5000);
        let at = Instant::now();
        let ack = jobs.start(1, 0, "calendar_add", "{}");
        assert!(at.elapsed() < Duration::from_millis(10));
        assert_eq!(ack, (r#"{"job":1,"started":true}"#.to_string(), false));
        assert_eq!(state(&db, 1).0, "running");
        assert_eq!(jobs.running().len(), 1);
        let done = next(&rx);
        assert!(at.elapsed() >= Duration::from_millis(40));
        assert_eq!(
            done,
            JobDone { job: 1, tool: "calendar_add".into(), outcome: JobOutcome::Done(r#"{"id":3}"#.into()) }
        );
        assert!(jobs.settle(&done));
        assert!(jobs.running().is_empty());
        assert_eq!(state(&db, 1), ("done".into(), Some(r#"{"id":3}"#.into())));
    }

    #[test]
    fn the_ninth_concurrent_job_is_refused() {
        let db = db();
        let (mut jobs, _rx) = table(&db, vec![fake("slow", 300, "{}", false)], 5000);
        for i in 0..8 {
            assert!(!jobs.start(1, i, "slow", "{}").1);
        }
        let (text, is_error) = jobs.start(1, 8, "slow", "{}");
        assert!(is_error);
        assert_eq!(
            json(&text),
            serde_json::json!({"kind": "cap_reached", "message": "8 jobs are running; wait for a result"})
        );
        let rows: i64 = crate::db_guard(&db)
            .query_row("SELECT COUNT(*) FROM voice_jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 8);
    }

    #[test]
    fn a_write_past_its_timeout_reports_timed_out_then_its_result() {
        let db = db();
        let (mut jobs, rx) = table(&db, vec![fake("task_add", 200, r#"{"ok":1}"#, false)], 50);
        jobs.start(1, 0, "task_add", "{}");
        let first = next(&rx);
        assert_eq!(first.outcome, JobOutcome::TimedOut);
        assert!(jobs.settle(&first));
        let late = next(&rx);
        assert_eq!(late.outcome, JobOutcome::Done(r#"{"ok":1}"#.into()));
        assert!(jobs.settle(&late), "a landed write is reported after its timeout");
        assert!(!jobs.settle(&late), "but only once");
        assert_eq!(state(&db, 1), ("done".into(), Some(r#"{"ok":1}"#.into())), "the write landed");
        let (text, is_error) = jobs.cancel(1);
        assert!(!is_error);
        assert_eq!(json(&text), serde_json::json!({"job": 1, "already": "done"}));
    }

    #[test]
    fn a_network_job_past_its_timeout_drops_its_late_result() {
        let db = db();
        let (mut jobs, rx) = table(&db, vec![fake("web_search", 200, "{}", true)], 50);
        jobs.start(1, 0, "web_search", "{}");
        let first = next(&rx);
        assert_eq!(first.outcome, JobOutcome::TimedOut);
        assert!(jobs.settle(&first));
        assert!(!jobs.settle(&next(&rx)));
        assert_eq!(state(&db, 1).0, "timed_out");
    }

    #[test]
    fn cancelling_a_write_that_fails_in_the_wait_reports_the_error() {
        let db = db();
        let (mut jobs, rx) = table(&db, vec![failing("task_done", 20, r#"{"kind":"not_found"}"#)], 5000);
        jobs.start(1, 0, "task_done", "{}");
        let (text, is_error) = jobs.cancel(1);
        assert!(!is_error);
        assert_eq!(json(&text), serde_json::json!({"job": 1, "error": {"kind": "not_found"}}));
        assert!(!jobs.settle(&next(&rx)));
        assert_eq!(state(&db, 1).0, "error");
    }

    #[test]
    fn a_cancelled_job_never_reports_again() {
        let db = db();
        let (mut jobs, rx) = table(&db, vec![fake("web_search", 100, "{}", true)], 5000);
        jobs.start(1, 0, "web_search", "{}");
        let (text, is_error) = jobs.cancel(1);
        assert!(!is_error);
        assert_eq!(json(&text), serde_json::json!({"job": 1, "cancelled": true}));
        assert!(jobs.running().is_empty());
        let cancelled = next(&rx);
        assert_eq!(cancelled.outcome, JobOutcome::Cancelled);
        assert!(jobs.settle(&cancelled));
        let late = next(&rx);
        assert!(!jobs.settle(&late));
        assert_eq!(state(&db, 1).0, "cancelled");
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "no deadline report after a cancel");
    }

    #[test]
    fn cancelling_a_landed_write_reports_it_done() {
        let db = db();
        let (mut jobs, rx) = table(&db, vec![fake("task_done", 20, r#"{"done":4}"#, false)], 5000);
        jobs.start(1, 0, "task_done", "{}");
        let (text, is_error) = jobs.cancel(1);
        assert!(!is_error);
        assert_eq!(json(&text), serde_json::json!({"job": 1, "done": true, "result": {"done": 4}}));
        assert!(!jobs.settle(&next(&rx)), "the cancel already reported it");
        assert_eq!(state(&db, 1).0, "done");
    }

    #[test]
    fn cancelling_an_unknown_job_is_not_found() {
        let db = db();
        let (mut jobs, _rx) = table(&db, vec![], 5000);
        let (text, is_error) = jobs.cancel(7);
        assert!(is_error);
        assert_eq!(json(&text)["kind"], "not_found");
    }

    #[test]
    fn ending_the_call_cancels_network_jobs_and_lets_writes_finish() {
        let db = db();
        let (mut jobs, rx) = table(
            &db,
            vec![fake("web_search", 200, "{}", true), fake("task_add", 40, "{}", false)],
            5000,
        );
        jobs.start(1, 0, "web_search", "{}");
        jobs.start(1, 1, "task_add", "{}");
        jobs.end();
        let mut seen: Vec<(u32, JobOutcome)> = (0..2).map(|_| next(&rx)).map(|d| (d.job, d.outcome)).collect();
        seen.sort_by_key(|(job, _)| *job);
        assert_eq!(seen, vec![(1, JobOutcome::Cancelled), (2, JobOutcome::Done("{}".into()))]);
        assert_eq!(state(&db, 1).0, "cancelled");
        assert_eq!(state(&db, 2).0, "done");
    }

    #[test]
    fn restart_reports_landed_writes_done_and_others_interrupted() {
        let db = db();
        crate::db_guard(&db)
            .execute_batch(
                "INSERT INTO voice_jobs (call_id, job, reply, call_index, tool, args, state, started_at)
                     VALUES ('c1', 1, 3, 0, 'task_add', '{}', 'running', 'x'),
                            ('c1', 2, 3, 1, 'web_search', '{}', 'running', 'x'),
                            ('c1', 3, 2, 0, 'task_add', '{}', 'done', 'x'),
                            ('c1', 4, 3, 2, 'task_add', '{}', 'timed_out', 'x'),
                            ('c1', 5, 3, 3, 'web_search', '{}', 'timed_out', 'x');
                 INSERT INTO voice_ops (call_id, op_key, result, created_at)
                     VALUES ('c1', '3:0', '{\"id\":9}', 'x'), ('c1', '3:2', '{\"id\":10}', 'x');",
            )
            .unwrap();
        let items = JobTable::recover(&db, "c1");
        assert_eq!(
            items,
            vec![
                Item::Job { job: 1, tool: "task_add".into(), outcome: JobOutcome::Done(r#"{"id":9}"#.into()) },
                Item::Job { job: 2, tool: "web_search".into(), outcome: JobOutcome::Interrupted },
                Item::Job { job: 4, tool: "task_add".into(), outcome: JobOutcome::Done(r#"{"id":10}"#.into()) },
            ]
        );
        assert_eq!(state(&db, 1), ("done".into(), Some(r#"{"id":9}"#.into())));
        assert_eq!(state(&db, 2).0, "interrupted");
        assert_eq!(state(&db, 3).0, "done");
        assert_eq!(state(&db, 4), ("done".into(), Some(r#"{"id":10}"#.into())));
        assert_eq!(state(&db, 5).0, "timed_out");
        let (mut jobs, _rx) = table(&db, vec![fake("task_add", 0, "{}", false)], 5000);
        assert_eq!(jobs.start(4, 0, "task_add", "{}").0, r#"{"job":6,"started":true}"#, "numbering continues");
    }
}
