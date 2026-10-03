use super::clauses::{speakable, Clauses};
use super::jobs::{JobDone, JobLimits, JobTable, ToolRunner};
use super::queue::{Queue, WakeConfig};
use super::render::{block, Item, JobOutcome};
use super::turn::{self, TurnEvent, TurnSpec};
use crate::providers::{LLMProvider, Message, StreamOpts, ToolCall};
use crate::tools::{CancelJobArgs, SessionKind, ToolError};
use note_voice_proto::CallBody;
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub enum DriverIn {
    Frame(CallBody),
    Job(JobDone),
    Turn(TurnEvent),
    Tick,
    Stop,
}

pub struct CallConfig {
    pub wake: WakeConfig,
    pub jobs: JobLimits,
    pub first_token: Duration,
    /// Sends a `Tick` every 50 ms; tests drive ticks by hand.
    pub ticker: bool,
}

pub struct DriverDeps {
    pub call_id: String,
    pub user_id: i64,
    pub username: String,
    pub conversation_id: i64,
    pub db: Arc<Mutex<Connection>>,
    pub llm: Arc<dyn LLMProvider>,
    pub system: Arc<String>,
    pub tools: Arc<Vec<serde_json::Value>>,
    pub runner: Arc<dyn ToolRunner>,
    pub cfg: CallConfig,
    pub send: Arc<dyn Fn(CallBody) + Send + Sync>,
    /// Monotonic time since the call started.
    pub clock: Arc<dyn Fn() -> Duration + Send + Sync>,
}

const APOLOGY: &str = "Sorry, I lost my train of thought. Could you say that again?";
const TICK: Duration = Duration::from_millis(50);
const END_WAIT: Duration = Duration::from_secs(2);
const DRAFT_WAIT: Duration = Duration::from_secs(3);

/// Runs until Stop or Ended; `opening` is spoken as reply 1 before anything
/// else, and `initial` is queued before the first event.
pub fn run(
    deps: DriverDeps,
    opening: Option<String>,
    history: Vec<Message>,
    initial: Vec<Item>,
    rx: mpsc::Receiver<DriverIn>,
    tx: mpsc::Sender<DriverIn>,
) {
    if deps.cfg.ticker {
        let ticks = tx.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(TICK);
            if ticks.send(DriverIn::Tick).is_err() {
                break;
            }
        });
    }
    let mut driver = Driver::new(deps, opening.as_deref().unwrap_or(""), history, tx);
    for item in initial {
        driver.queue.push(item, driver.now());
    }
    if let Some(text) = &opening {
        driver.open(text);
    }
    while let Ok(msg) = rx.recv() {
        match msg {
            DriverIn::Stop | DriverIn::Frame(CallBody::Ended) => break,
            DriverIn::Frame(body) => driver.on_frame(body),
            DriverIn::Job(done) => driver.on_job(&done),
            DriverIn::Turn(event) => driver.on_turn(event),
            DriverIn::Tick => driver.on_tick(),
        }
    }
    driver.finish(&rx);
}

#[derive(Default)]
struct ReplyRecord {
    /// The clauses sent as `Speak`, joined with spaces.
    full_text: String,
    heard_chars: Option<u32>,
    message: Option<usize>,
    row: Option<i64>,
}

impl ReplyRecord {
    fn heard_text(&self) -> String {
        let cut = self
            .heard_chars
            .and_then(|n| self.full_text.char_indices().nth(n as usize))
            .map(|(at, _)| at);
        match cut {
            Some(at) => format!("{}…", &self.full_text[..at]),
            None => self.full_text.clone(),
        }
    }
}

struct TurnEnd {
    calls: Vec<ToolCall>,
    stopped: bool,
    error: Option<String>,
}

struct InFlight {
    reply: u64,
    draft_of: Option<u64>,
    draft_text: String,
    snapshot: Vec<Item>,
    input: String,
    stop: Arc<AtomicBool>,
    /// A draft's calls, held until its commit.
    calls: Vec<(usize, ToolCall)>,
    emitted: Vec<ToolCall>,
    results: HashMap<usize, (String, bool)>,
    ended: Option<TurnEnd>,
    promoted: bool,
    played: bool,
    since: Duration,
    first_clause: Option<Duration>,
    handle: JoinHandle<()>,
    end_sent: Arc<AtomicBool>,
}

impl InFlight {
    fn held(&self) -> bool {
        self.draft_of.is_some() && !self.promoted
    }
}

struct JobMeta {
    args: String,
    started: Duration,
    written: bool,
    row: Option<i64>,
}

struct Driver {
    deps: DriverDeps,
    tx: mpsc::Sender<DriverIn>,
    messages: Vec<Message>,
    queue: Queue,
    jobs: JobTable,
    job_meta: HashMap<u32, JobMeta>,
    next_reply: u64,
    in_flight: Option<InFlight>,
    replies: HashMap<u64, ReplyRecord>,
    ending: bool,
    trace: crate::trace::Builder,
    last_reply: String,
}

impl Driver {
    fn new(
        mut deps: DriverDeps,
        opening: &str,
        history: Vec<Message>,
        tx: mpsc::Sender<DriverIn>,
    ) -> Self {
        let (done_tx, done_rx) = mpsc::channel::<JobDone>();
        let forward = tx.clone();
        std::thread::spawn(move || {
            for done in done_rx {
                if forward.send(DriverIn::Job(done)).is_err() {
                    break;
                }
            }
        });
        let wake = std::mem::replace(
            &mut deps.cfg.wake,
            WakeConfig {
                settle: Duration::ZERO,
                max_wakes: 0,
            },
        );
        let limits = std::mem::replace(
            &mut deps.cfg.jobs,
            JobLimits {
                timeout: Duration::ZERO,
                max_running: 0,
            },
        );
        let queue = Queue::new(wake, (deps.clock)());
        let next_reply = first_reply(&deps.db, &deps.call_id);
        let jobs = JobTable::new(
            deps.call_id.clone(),
            deps.db.clone(),
            deps.runner.clone(),
            limits,
            done_tx,
        );
        Self {
            deps,
            tx,
            messages: history,
            queue,
            jobs,
            job_meta: HashMap::new(),
            next_reply,
            in_flight: None,
            replies: HashMap::new(),
            ending: false,
            trace: crate::trace::Builder::new(SessionKind::Call, opening),
            last_reply: String::new(),
        }
    }

    fn now(&self) -> Duration {
        (self.deps.clock)()
    }

    fn send(&self, body: CallBody) {
        (self.deps.send)(body);
    }

    fn open(&mut self, text: &str) {
        let mut clauses = Clauses::default();
        let mut spoken: Vec<String> = clauses.push(text);
        spoken.extend(clauses.finish());
        let spoken: Vec<String> = spoken
            .iter()
            .map(|c| speakable(c))
            .filter(|c| !c.is_empty())
            .collect();
        self.say(1, &spoken, text);
    }

    /// Speaks a reply Note already holds whole, and commits it as `text`.
    fn say(&mut self, reply: u64, clauses: &[String], text: &str) {
        for (idx, clause) in clauses.iter().enumerate() {
            self.send(CallBody::Speak {
                reply,
                idx: idx as u32,
                text: clause.clone(),
            });
        }
        self.send(CallBody::SpeakDone { reply });
        self.send(CallBody::Play { reply });
        self.queue.note_playing(true, self.now());
        self.messages.push(Message::Assistant {
            text: text.to_string(),
            tool_calls: vec![],
        });
        let row = self.append_assistant(text);
        self.last_reply = text.to_string();
        self.replies.insert(
            reply,
            ReplyRecord {
                full_text: clauses.join(" "),
                heard_chars: None,
                message: Some(self.messages.len() - 1),
                row,
            },
        );
    }

    fn on_frame(&mut self, body: CallBody) {
        match body {
            CallBody::Commit { turn, text } => self.on_commit(turn, text),
            CallBody::Draft { turn, text } => self.on_draft(turn, text),
            CallBody::Retract { turn } => {
                if self
                    .in_flight
                    .as_ref()
                    .is_some_and(|f| f.held() && f.draft_of == Some(turn))
                {
                    self.drop_draft();
                }
            }
            CallBody::Floor { floor } => self.queue.floor(floor, self.now()),
            CallBody::BargeIn { reply, heard_chars } => self.on_barge_in(reply, heard_chars),
            _ => {}
        }
    }

    fn on_commit(&mut self, turn: u64, text: String) {
        let conv = self.deps.conversation_id;
        if let Err(e) = crate::talk::append_text(
            &crate::db_guard(&self.deps.db),
            conv,
            "user",
            &text,
            jiff::Timestamp::now(),
        ) {
            eprintln!(
                "voice: recording a heard turn for {} failed: {e:#}",
                self.deps.call_id
            );
        }
        if let Some(f) = self.in_flight.as_ref().filter(|f| f.held()) {
            if f.draft_of == Some(turn) && normalize(&text) == normalize(&f.draft_text) {
                self.promote();
                return;
            }
            self.drop_draft();
        }
        self.queue.push(Item::Heard(text), self.now());
        self.try_start();
    }

    fn on_draft(&mut self, turn: u64, text: String) {
        if self.in_flight.is_some() || self.ending {
            return;
        }
        if self.queue.has_heard() {
            self.try_start();
            return;
        }
        let snapshot = self.queue.snapshot();
        let mut items = snapshot.clone();
        items.push(Item::Heard(text.clone()));
        let input = block(&items, &self.jobs.running());
        self.spawn_turn(input, Some((turn, text, snapshot)));
    }

    fn promote(&mut self) {
        let now = self.now();
        let Some(f) = self.in_flight.as_mut() else {
            return;
        };
        f.promoted = true;
        f.played = true;
        f.since = now;
        let reply = f.reply;
        let held = std::mem::take(&mut f.calls);
        self.send(CallBody::Play { reply });
        self.queue.heard();
        if self
            .replies
            .get(&reply)
            .is_some_and(|r| !r.full_text.is_empty())
        {
            self.queue.note_playing(true, now);
        }
        for (index, call) in held {
            let result = self.run_call(reply, index, &call);
            if let Some(f) = self.in_flight.as_mut() {
                f.results.insert(index, result);
            }
        }
        if self.in_flight.as_ref().is_some_and(|f| f.ended.is_some()) {
            self.commit_turn();
        }
    }

    fn drop_draft(&mut self) {
        let Some(f) = self.in_flight.take() else {
            return;
        };
        f.stop.store(true, Ordering::SeqCst);
        self.send(CallBody::Drop { reply: f.reply });
        self.queue.give_back(f.snapshot);
        self.replies.remove(&f.reply);
    }

    fn on_barge_in(&mut self, reply: u64, heard_chars: u32) {
        let Some(record) = self.replies.get_mut(&reply) else {
            return;
        };
        record.heard_chars = Some(heard_chars);
        let heard = record.heard_text();
        let (message, row) = (record.message, record.row);
        self.queue.note_playing(false, self.now());
        if let Some(f) = self.in_flight.as_ref().filter(|f| f.reply == reply) {
            f.stop.store(true, Ordering::SeqCst);
        }
        if let Some(Message::Assistant { text, .. }) =
            message.and_then(|i| self.messages.get_mut(i))
        {
            text.clone_from(&heard);
        }
        if let Some(id) = row {
            let updated = crate::db_guard(&self.deps.db).execute(
                "UPDATE talk_messages SET content = ?1 WHERE id = ?2",
                (&heard, id),
            );
            if let Err(e) = updated {
                eprintln!(
                    "voice: trimming a cut-off reply for {} failed: {e}",
                    self.deps.call_id
                );
            }
        }
    }

    fn on_tick(&mut self) {
        let now = self.now();
        if self
            .in_flight
            .as_ref()
            .is_some_and(|f| f.held() && now.saturating_sub(f.since) >= DRAFT_WAIT)
        {
            self.drop_draft();
        }
        let dead = self.in_flight.as_ref().is_some_and(|f| {
            f.ended.is_none() && f.handle.is_finished() && !f.end_sent.load(Ordering::SeqCst)
        });
        if dead {
            let calls = self
                .in_flight
                .as_ref()
                .map(|f| f.emitted.clone())
                .unwrap_or_default();
            self.on_end(calls, false, Some("the turn stopped".into()));
        }
        self.try_start();
    }

    fn try_start(&mut self) {
        if self.in_flight.is_some() || self.ending {
            return;
        }
        if let Some((_, items)) = self.queue.take_turn(self.now()) {
            let input = block(&items, &self.jobs.running());
            self.spawn_turn(input, None);
        }
    }

    fn spawn_turn(&mut self, input: String, draft: Option<(u64, String, Vec<Item>)>) {
        let reply = self.next_reply;
        self.next_reply += 1;
        let stop = Arc::new(AtomicBool::new(false));
        let end_sent = Arc::new(AtomicBool::new(false));
        let mut request = self.messages.clone();
        push_user(&mut request, input.clone());
        let (tx, flag) = (self.tx.clone(), end_sent.clone());
        let handle = turn::spawn(
            self.deps.llm.clone(),
            self.deps.system.clone(),
            request,
            self.deps.tools.clone(),
            TurnSpec {
                reply,
                draft: draft.is_some(),
            },
            StreamOpts {
                first_token: self.deps.cfg.first_token,
            },
            stop.clone(),
            move |event| {
                if matches!(event, TurnEvent::End { .. }) {
                    flag.store(true, Ordering::SeqCst);
                }
                let _ = tx.send(DriverIn::Turn(event));
            },
        );
        let (draft_of, draft_text, snapshot) = match draft {
            Some((turn, text, snapshot)) => (Some(turn), text, snapshot),
            None => (None, String::new(), Vec::new()),
        };
        self.replies.insert(reply, ReplyRecord::default());
        self.in_flight = Some(InFlight {
            reply,
            draft_of,
            draft_text,
            snapshot,
            input,
            stop,
            calls: Vec::new(),
            emitted: Vec::new(),
            results: HashMap::new(),
            ended: None,
            promoted: false,
            played: false,
            since: self.now(),
            first_clause: None,
            handle,
            end_sent,
        });
    }

    fn on_turn(&mut self, event: TurnEvent) {
        let reply = match &event {
            TurnEvent::Clause { reply, .. }
            | TurnEvent::Call { reply, .. }
            | TurnEvent::End { reply, .. } => *reply,
        };
        if self.in_flight.as_ref().is_none_or(|f| f.reply != reply) {
            return;
        }
        match event {
            TurnEvent::Clause { idx, text, .. } => self.on_clause(reply, idx, text),
            TurnEvent::Call { index, call, .. } => self.on_call(reply, index, call),
            TurnEvent::End {
                calls,
                stopped,
                error,
                ..
            } => self.on_end(calls, stopped, error),
        }
    }

    fn on_clause(&mut self, reply: u64, idx: u32, text: String) {
        let now = self.now();
        let Some(record) = self
            .replies
            .get_mut(&reply)
            .filter(|r| r.heard_chars.is_none())
        else {
            return;
        };
        if !record.full_text.is_empty() {
            record.full_text.push(' ');
        }
        record.full_text.push_str(&text);
        (self.deps.send)(CallBody::Speak { reply, idx, text });
        let Some(f) = self.in_flight.as_mut() else {
            return;
        };
        f.first_clause.get_or_insert(now);
        if f.held() {
            return;
        }
        if !f.played {
            f.played = true;
            (self.deps.send)(CallBody::Play { reply });
        }
        self.queue.note_playing(true, now);
    }

    fn on_call(&mut self, reply: u64, index: usize, call: ToolCall) {
        let Some(f) = self.in_flight.as_mut() else {
            return;
        };
        f.emitted.push(call.clone());
        if f.held() {
            f.calls.push((index, call));
            return;
        }
        let result = self.run_call(reply, index, &call);
        if let Some(f) = self.in_flight.as_mut() {
            f.results.insert(index, result);
        }
    }

    /// The tool result the model sees for `call`: an ack for a job, or the inline result of a call-only tool.
    fn run_call(&mut self, reply: u64, index: usize, call: &ToolCall) -> (String, bool) {
        match call.name.as_str() {
            "hang_up" => {
                self.ending = true;
                (serde_json::json!({"ok": true}).to_string(), false)
            }
            "cancel_job" => match serde_json::from_str::<CancelJobArgs>(&call.args) {
                Ok(args) => match u32::try_from(args.job) {
                    Ok(job) => self.jobs.cancel(job),
                    Err(_) => tool_error(ToolError::not_found(format!(
                        "no job {} in this call",
                        args.job
                    ))),
                },
                Err(e) => tool_error(ToolError::invalid_args(e.to_string())),
            },
            name => {
                let started = self.now();
                let (ack, is_error) = self.jobs.start(reply, index, name, &call.args);
                let job = serde_json::from_str::<serde_json::Value>(&ack)
                    .ok()
                    .and_then(|v| v["job"].as_u64())
                    .and_then(|job| u32::try_from(job).ok());
                if let (false, Some(job)) = (is_error, job) {
                    self.job_meta.insert(
                        job,
                        JobMeta {
                            args: call.args.clone(),
                            started,
                            written: false,
                            row: None,
                        },
                    );
                }
                (ack, is_error)
            }
        }
    }

    fn on_end(&mut self, calls: Vec<ToolCall>, stopped: bool, error: Option<String>) {
        let Some(f) = self.in_flight.as_mut() else {
            return;
        };
        let reply = f.reply;
        f.ended = Some(TurnEnd {
            calls,
            stopped,
            error,
        });
        let held = f.held();
        self.send(CallBody::SpeakDone { reply });
        if !held {
            self.commit_turn();
        }
    }

    fn commit_turn(&mut self) {
        let Some(f) = self.in_flight.take() else {
            return;
        };
        let Some(end) = f.ended else { return };
        if let Some(first) = f.first_clause {
            self.trace
                .round(first.saturating_sub(f.since).as_millis() as u64);
        }
        push_user(&mut self.messages, f.input);
        let (spoken_any, heard) = match self.replies.get(&f.reply) {
            Some(record) => (!record.full_text.is_empty(), record.heard_text()),
            None => (false, String::new()),
        };
        let mut message = None;
        if spoken_any || !end.calls.is_empty() {
            self.messages.push(Message::Assistant {
                text: heard.clone(),
                tool_calls: end.calls.clone(),
            });
            message = Some(self.messages.len() - 1);
            for (index, call) in end.calls.iter().enumerate() {
                let (content, is_error) = f
                    .results
                    .get(&index)
                    .cloned()
                    .unwrap_or_else(|| tool_error(ToolError::internal("the call never ran")));
                self.messages.push(Message::ToolResult {
                    call_id: call.id.clone(),
                    content,
                    is_error,
                });
            }
        }
        let row = if heard.is_empty() {
            None
        } else {
            self.append_assistant(&heard)
        };
        if !heard.is_empty() {
            self.last_reply = heard;
        }
        if let Some(record) = self.replies.get_mut(&f.reply) {
            record.message = message;
            record.row = row;
        }
        if end.error.is_some() && !spoken_any && end.calls.is_empty() && !end.stopped {
            let reply = self.next_reply;
            self.next_reply += 1;
            self.say(reply, &[APOLOGY.to_string()], APOLOGY);
        }
        if self.ending {
            self.send(CallBody::HangUp);
        }
        self.try_start();
    }

    fn on_job(&mut self, done: &JobDone) {
        let settled = self.jobs.settle(done);
        let landed = matches!(done.outcome, JobOutcome::Done(_) | JobOutcome::Error(_));
        let unwritten = self
            .job_meta
            .get(&done.job)
            .is_some_and(|meta| !meta.written);
        if settled {
            self.queue.push(
                Item::Job {
                    job: done.job,
                    tool: done.tool.clone(),
                    outcome: done.outcome.clone(),
                },
                self.now(),
            );
        }
        if settled || (landed && unwritten) {
            self.record_job(done);
        }
    }

    fn record_job(&mut self, done: &JobDone) {
        let now = self.now();
        let (args, ms, row) = match self.job_meta.get_mut(&done.job) {
            Some(meta) => {
                meta.written = true;
                (
                    meta.args.clone(),
                    now.saturating_sub(meta.started).as_millis() as u64,
                    meta.row,
                )
            }
            None => (String::new(), 0, None),
        };
        let (result, is_error) = match &done.outcome {
            JobOutcome::Done(text) => (text.clone(), false),
            JobOutcome::Error(text) => (text.clone(), true),
            JobOutcome::Cancelled => (
                serde_json::json!({"job": done.job, "cancelled": true}).to_string(),
                false,
            ),
            JobOutcome::TimedOut => (
                serde_json::json!({"job": done.job, "timed_out": true}).to_string(),
                true,
            ),
            JobOutcome::Interrupted => (
                serde_json::json!({"job": done.job, "interrupted": true}).to_string(),
                true,
            ),
        };
        self.trace.call(&done.tool, &args, &result, is_error, ms);
        let written = if let Some(id) = row {
            crate::db_guard(&self.deps.db)
                .execute(
                    "UPDATE talk_messages SET content = ?1, is_error = ?2 WHERE id = ?3",
                    (&result, is_error, id),
                )
                .map(|_| ())
                .map_err(anyhow::Error::from)
        } else {
            let conn = crate::db_guard(&self.deps.db);
            crate::talk::append_tool(
                &conn,
                self.deps.conversation_id,
                &done.tool,
                &args,
                &result,
                is_error,
                None,
                jiff::Timestamp::now(),
            )
            .map(|()| {
                if let Some(meta) = self.job_meta.get_mut(&done.job) {
                    meta.row = Some(conn.last_insert_rowid());
                }
            })
        };
        if let Err(e) = written {
            eprintln!(
                "voice: recording job {} of {} failed: {e:#}",
                done.job, self.deps.call_id
            );
        }
    }

    /// The row id of the new assistant row.
    fn append_assistant(&self, text: &str) -> Option<i64> {
        let conn = crate::db_guard(&self.deps.db);
        match crate::talk::append_assistant(
            &conn,
            self.deps.conversation_id,
            text,
            "",
            0,
            jiff::Timestamp::now(),
        ) {
            Ok(()) => Some(conn.last_insert_rowid()),
            Err(e) => {
                eprintln!(
                    "voice: recording a reply for {} failed: {e:#}",
                    self.deps.call_id
                );
                None
            }
        }
    }

    /// Cancels network jobs, records the reply cut short, and waits up to 2 s for the rest of the jobs.
    fn finish(mut self, rx: &mpsc::Receiver<DriverIn>) {
        self.jobs.end();
        if let Some(f) = self.in_flight.take() {
            f.stop.store(true, Ordering::SeqCst);
            let heard = self
                .replies
                .get(&f.reply)
                .map(ReplyRecord::heard_text)
                .unwrap_or_default();
            if !f.held() && !heard.is_empty() {
                self.append_assistant(&heard);
                self.last_reply = heard;
            }
        }
        let deadline = Instant::now() + END_WAIT;
        while self.job_meta.values().any(|meta| !meta.written) {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(DriverIn::Job(done)) => self.on_job(&done),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        self.trace.ok(&self.last_reply);
        if let Err(e) = self
            .trace
            .insert(&crate::db_guard(&self.deps.db), self.deps.user_id)
        {
            eprintln!(
                "voice: saving the trace of {} failed: {e:#}",
                self.deps.call_id
            );
        }
    }
}

/// The first reply number no earlier run of this call has used; 1 is the opening.
fn first_reply(db: &Mutex<Connection>, call_id: &str) -> u64 {
    let last: i64 = crate::db_guard(db)
        .query_row(
            "SELECT MAX(COALESCE((SELECT MAX(reply) FROM voice_jobs WHERE call_id = ?1), 0),
                        COALESCE((SELECT MAX(CAST(substr(op_key, 1, instr(op_key, ':') - 1) AS INTEGER))
                                  FROM voice_ops WHERE call_id = ?1), 0))",
            [call_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    (last.max(0) as u64 + 1).max(2)
}

/// Appends `input` as a user message, merged into the last message when that is one too.
fn push_user(messages: &mut Vec<Message>, input: String) {
    match messages.last_mut() {
        Some(Message::User(text)) => {
            text.push('\n');
            text.push_str(&input);
        }
        _ => messages.push(Message::User(input)),
    }
}

/// Lowercased, whitespace collapsed, trailing punctuation stripped.
fn normalize(text: &str) -> String {
    let collapsed = text
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    collapsed
        .trim_end_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace())
        .to_string()
}

fn tool_error(e: ToolError) -> (String, bool) {
    (
        serde_json::to_string(&e).unwrap_or_else(|_| r#"{"kind":"internal"}"#.into()),
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::mock::{MockLLM, StreamPiece};
    use crate::providers::ToolCall;
    use note_voice_proto::Floor;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread::JoinHandle;
    use std::time::Instant;
    use StreamPiece::{Call as Calls, Fail, Text};

    const WAIT: Duration = Duration::from_secs(5);

    struct FakeRunner {
        tools: HashMap<&'static str, (u64, &'static str)>,
        ran: Mutex<Vec<String>>,
        keys: Mutex<Vec<String>>,
    }

    impl ToolRunner for FakeRunner {
        fn run(&self, name: &str, _args: &str, once: (&str, &str)) -> (String, bool) {
            self.ran.lock().unwrap().push(name.to_string());
            self.keys.lock().unwrap().push(once.1.to_string());
            let (ms, result) = self.tools[name];
            std::thread::sleep(Duration::from_millis(ms));
            (result.to_string(), false)
        }

        fn is_network(&self, name: &str) -> bool {
            name == "web_search"
        }
    }

    struct Harness {
        tx: mpsc::Sender<DriverIn>,
        frames: mpsc::Receiver<CallBody>,
        sent: Vec<CallBody>,
        now_ms: Arc<AtomicU64>,
        llm: Arc<MockLLM>,
        runner: Arc<FakeRunner>,
        db: Arc<Mutex<Connection>>,
        driver: Option<JoinHandle<()>>,
    }

    fn call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            args: args.into(),
        }
    }

    fn speak(reply: u64, idx: u32, text: &str) -> CallBody {
        CallBody::Speak {
            reply,
            idx,
            text: text.into(),
        }
    }

    fn start(
        rounds: Vec<Vec<StreamPiece>>,
        tools: &[(&'static str, u64, &'static str)],
        opening: Option<&str>,
        seed: &str,
    ) -> Harness {
        start_with(rounds, tools, opening, seed, Duration::from_secs(5))
    }

    fn start_with(
        rounds: Vec<Vec<StreamPiece>>,
        tools: &[(&'static str, u64, &'static str)],
        opening: Option<&str>,
        seed: &str,
        job_timeout: Duration,
    ) -> Harness {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        let conv = crate::talk::create(&conn, 1, "call", jiff::Timestamp::now()).unwrap();
        conn.execute(
            "INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at, conversation_id)
             VALUES ('c1', 1, 'outbound', 'answered', 'x', 'x', ?1)",
            [conv],
        )
        .unwrap();
        conn.execute_batch(seed).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let llm = Arc::new(MockLLM::streamed(rounds));
        let runner = Arc::new(FakeRunner {
            tools: tools
                .iter()
                .map(|&(name, ms, result)| (name, (ms, result)))
                .collect(),
            ran: Mutex::new(Vec::new()),
            keys: Mutex::new(Vec::new()),
        });
        let now_ms = Arc::new(AtomicU64::new(0));
        let (frame_tx, frames) = mpsc::channel();
        let clock_ms = now_ms.clone();
        let deps = DriverDeps {
            call_id: "c1".into(),
            user_id: 1,
            username: "aki".into(),
            conversation_id: conv,
            db: db.clone(),
            llm: llm.clone(),
            system: Arc::new("sys".into()),
            tools: Arc::new(vec![]),
            runner: runner.clone(),
            cfg: CallConfig {
                wake: WakeConfig {
                    settle: Duration::from_millis(600),
                    max_wakes: 4,
                },
                jobs: JobLimits {
                    timeout: job_timeout,
                    max_running: 8,
                },
                first_token: Duration::from_secs(1),
                ticker: false,
            },
            send: Arc::new(move |body| {
                let _ = frame_tx.send(body);
            }),
            clock: Arc::new(move || Duration::from_millis(clock_ms.load(Ordering::SeqCst))),
        };
        let (tx, rx) = mpsc::channel();
        let driver_tx = tx.clone();
        let opening = opening.map(String::from);
        let driver = std::thread::spawn(move || run(deps, opening, vec![], vec![], rx, driver_tx));
        Harness {
            tx,
            frames,
            sent: Vec::new(),
            now_ms,
            llm,
            runner,
            db,
            driver: Some(driver),
        }
    }

    impl Harness {
        fn frame(&self, body: CallBody) {
            self.tx.send(DriverIn::Frame(body)).unwrap();
        }

        fn at(&self, ms: u64) {
            self.now_ms.store(ms, Ordering::SeqCst);
        }

        fn next(&mut self, within: Duration) -> Option<CallBody> {
            let body = self.frames.recv_timeout(within).ok()?;
            self.sent.push(body.clone());
            Some(body)
        }

        /// The next frames are exactly `want`, in order.
        fn expect(&mut self, want: &[CallBody]) {
            for body in want {
                let got = self.next(WAIT);
                assert_eq!(got.as_ref(), Some(body), "sent so far: {:?}", self.sent);
            }
        }

        fn quiet(&mut self, ms: u64) -> Vec<CallBody> {
            let mut out = Vec::new();
            while let Some(body) = self.next(Duration::from_millis(ms)) {
                out.push(body);
            }
            out
        }

        /// Ticks, moving the clock 50 ms per tick, until `want` is sent.
        fn tick_until(&mut self, want: &CallBody) {
            let deadline = Instant::now() + WAIT;
            while Instant::now() < deadline {
                self.now_ms.fetch_add(50, Ordering::SeqCst);
                self.tx.send(DriverIn::Tick).unwrap();
                while let Some(body) = self.next(Duration::from_millis(20)) {
                    if &body == want {
                        return;
                    }
                }
            }
            panic!("never sent {want:?}; sent {:?}", self.sent);
        }

        fn wait_for(&self, what: &str, done: impl Fn(&Self) -> bool) {
            let deadline = Instant::now() + WAIT;
            while !done(self) {
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn ran(&self) -> Vec<String> {
            self.runner.ran.lock().unwrap().clone()
        }

        fn rows(&self) -> Vec<(String, String, Option<String>)> {
            let conn = crate::db_guard(&self.db);
            let mut stmt = conn
                .prepare("SELECT role, content, tool_name FROM talk_messages ORDER BY id")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        }

        fn last_user(&self, request: usize) -> String {
            match self.llm.seen()[request].messages.last() {
                Some(Message::User(text)) => text.clone(),
                other => panic!("request {request} ends with {other:?}"),
            }
        }

        fn stop(&mut self) {
            let _ = self.tx.send(DriverIn::Stop);
            self.driver.take().unwrap().join().unwrap();
        }
    }

    fn row(role: &str, content: &str, tool: Option<&str>) -> (String, String, Option<String>) {
        (role.into(), content.into(), tool.map(String::from))
    }

    #[test]
    fn a_committed_turn_speaks_and_its_tool_runs_in_the_background() {
        let mut h = start(
            vec![
                vec![
                    Text("Let me look that up."),
                    Calls(call("t1", "web_search", r#"{"q":"train"}"#)),
                ],
                vec![Text("The next train is at 7:40.")],
            ],
            &[("web_search", 100, r#"{"next":"7:40"}"#)],
            None,
            "",
        );
        h.frame(CallBody::Commit {
            turn: 1,
            text: "when is the next train".into(),
        });
        h.tx.send(DriverIn::Tick).unwrap();
        h.expect(&[
            speak(2, 0, "Let me look that up."),
            CallBody::Play { reply: 2 },
            CallBody::SpeakDone { reply: 2 },
        ]);
        h.at(200);
        h.frame(CallBody::Floor {
            floor: Floor::Drained,
        });
        h.tick_until(&speak(3, 0, "The next train is at 7:40."));
        h.expect(&[
            CallBody::Play { reply: 3 },
            CallBody::SpeakDone { reply: 3 },
        ]);

        let seen = h.llm.seen();
        assert_eq!(seen.len(), 2);
        let second = &seen[1].messages;
        assert!(
            matches!(&second[1], Message::Assistant { text, tool_calls } if text == "Let me look that up." && tool_calls.len() == 1),
            "{second:?}"
        );
        assert!(
            matches!(&second[2], Message::ToolResult { call_id, content, is_error: false }
                if call_id == "t1" && content == r#"{"job":1,"started":true}"#),
            "{second:?}"
        );
        assert!(h
            .last_user(1)
            .contains(r#"[job 1 · web_search · done] {"next":"7:40"}"#));

        h.stop();
        assert_eq!(
            h.rows(),
            vec![
                row("user", "when is the next train", None),
                row("assistant", "Let me look that up.", None),
                row("tool", r#"{"next":"7:40"}"#, Some("web_search")),
                row("assistant", "The next train is at 7:40.", None),
            ]
        );
        let kind: String = crate::db_guard(&h.db)
            .query_row("SELECT kind FROM agent_traces WHERE user_id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kind, "Call");
    }

    #[test]
    fn two_jobs_one_round_report_as_they_land() {
        let mut h = start(
            vec![
                vec![
                    Calls(call("t1", "web_search", r#"{"q":"venue"}"#)),
                    Calls(call("t2", "calendar_update", r#"{"id":4}"#)),
                ],
                vec![Text("")],
                vec![Text("Found it.")],
            ],
            &[
                ("web_search", 300, r#"{"venue":"hall"}"#),
                ("calendar_update", 10, r#"{"ok":true}"#),
            ],
            None,
            "",
        );
        h.frame(CallBody::Commit {
            turn: 1,
            text: "find the venue and move my run".into(),
        });
        h.expect(&[CallBody::SpeakDone { reply: 2 }]);
        h.at(1000);
        h.tick_until(&speak(4, 0, "Found it."));

        assert!(
            h.sent.contains(&CallBody::SpeakDone { reply: 3 }),
            "{:?}",
            h.sent
        );
        assert!(
            !h.sent.iter().any(|b| matches!(
                b,
                CallBody::Speak { reply: 3, .. } | CallBody::Play { reply: 3 }
            )),
            "the silent wake says nothing: {:?}",
            h.sent
        );
        let seen = h.llm.seen();
        assert_eq!(seen.len(), 3);
        let second = h.last_user(1);
        assert!(
            second.contains("[job 2 · calendar_update · done]"),
            "{second}"
        );
        assert!(second.contains("[running] job 1 · web_search"), "{second}");
        assert_eq!(
            seen[2].messages.len(),
            5,
            "the silent wake's input merges into the next: {:?}",
            seen[2].messages
        );
        let third = h.last_user(2);
        let block = &third[third
            .find("[job 1 · web_search · done]")
            .expect("search result")..];
        assert!(!block.contains("[running]"), "{block}");
        h.stop();
    }

    #[test]
    fn a_draft_that_matches_its_commit_plays_and_starts_its_jobs() {
        let mut h = start(
            vec![vec![
                Text("Moving it."),
                Calls(call("t1", "calendar_update", r#"{"id":3}"#)),
            ]],
            &[("calendar_update", 10, r#"{"ok":true}"#)],
            None,
            "",
        );
        h.frame(CallBody::Draft {
            turn: 1,
            text: "move my run".into(),
        });
        h.expect(&[speak(2, 0, "Moving it."), CallBody::SpeakDone { reply: 2 }]);
        assert_eq!(h.quiet(100), vec![], "a draft does not play");
        assert!(h.ran().is_empty(), "a draft's calls are held");

        h.frame(CallBody::Commit {
            turn: 1,
            text: "Move my run.".into(),
        });
        h.expect(&[CallBody::Play { reply: 2 }]);
        h.wait_for("the held call to run", |h| !h.ran().is_empty());
        h.stop();
        assert_eq!(h.ran(), vec!["calendar_update"]);
        assert_eq!(
            h.rows(),
            vec![
                row("user", "Move my run.", None),
                row("assistant", "Moving it.", None),
                row("tool", r#"{"ok":true}"#, Some("calendar_update")),
            ]
        );
    }

    #[test]
    fn a_draft_that_differs_is_dropped_and_redone() {
        let mut h = start(
            vec![
                vec![
                    Text("Sure."),
                    Calls(call("t1", "calendar_update", r#"{"id":3}"#)),
                ],
                vec![Text("Moved to Friday.")],
            ],
            &[("calendar_update", 10, r#"{"ok":true}"#)],
            None,
            "",
        );
        h.frame(CallBody::Draft {
            turn: 1,
            text: "move my".into(),
        });
        h.expect(&[speak(2, 0, "Sure."), CallBody::SpeakDone { reply: 2 }]);
        h.frame(CallBody::Commit {
            turn: 1,
            text: "move my run to Friday".into(),
        });
        h.expect(&[
            CallBody::Drop { reply: 2 },
            speak(3, 0, "Moved to Friday."),
            CallBody::Play { reply: 3 },
            CallBody::SpeakDone { reply: 3 },
        ]);
        let seen = h.llm.seen();
        assert_eq!(seen[1].messages.len(), 1, "{:?}", seen[1].messages);
        assert_eq!(h.last_user(1), "[you] move my run to Friday");
        assert_eq!(h.quiet(100), vec![]);
        h.stop();
        assert!(h.ran().is_empty(), "the dropped draft's calls never run");
    }

    #[test]
    fn a_retracted_draft_returns_its_completions() {
        let mut h = start(
            vec![
                vec![Text("On it."), Calls(call("t1", "task_add", r#"{"title":"stretch"}"#))],
                vec![Text("Okay.")],
                vec![Text("Noted.")],
            ],
            &[("task_add", 10, r#"{"id":5}"#)],
            None,
            "INSERT INTO voice_jobs (call_id, job, reply, call_index, tool, args, state, started_at)
             VALUES ('c1', 1, 1, 0, 't', '{}', 'done', 'x'), ('c1', 2, 1, 1, 't', '{}', 'done', 'x'),
                    ('c1', 3, 1, 2, 't', '{}', 'done', 'x'), ('c1', 4, 1, 3, 't', '{}', 'done', 'x'),
                    ('c1', 5, 1, 4, 't', '{}', 'done', 'x'), ('c1', 6, 1, 5, 't', '{}', 'done', 'x');",
        );
        h.frame(CallBody::Commit {
            turn: 1,
            text: "remind me to stretch".into(),
        });
        h.expect(&[
            speak(2, 0, "On it."),
            CallBody::Play { reply: 2 },
            CallBody::SpeakDone { reply: 2 },
        ]);
        h.wait_for("job 7 to land", |h| {
            h.rows().iter().any(|(role, ..)| role == "tool")
        });

        h.frame(CallBody::Draft {
            turn: 2,
            text: "ok".into(),
        });
        h.expect(&[speak(3, 0, "Okay."), CallBody::SpeakDone { reply: 3 }]);
        assert!(
            h.last_user(1).contains("[job 7 · task_add · done]"),
            "the draft took the snapshot"
        );
        h.frame(CallBody::Retract { turn: 2 });
        h.expect(&[CallBody::Drop { reply: 3 }]);

        h.frame(CallBody::Commit {
            turn: 2,
            text: "ok".into(),
        });
        h.expect(&[
            speak(4, 0, "Noted."),
            CallBody::Play { reply: 4 },
            CallBody::SpeakDone { reply: 4 },
        ]);
        let block = h.last_user(2);
        assert!(block.contains("[job 7 · task_add · done]"), "{block}");
        assert!(block.contains("[you] ok"), "{block}");
        h.stop();
    }

    #[test]
    fn barge_in_trims_the_reply_and_keeps_its_jobs() {
        let mut h = start(
            vec![
                vec![
                    Text("I'll move it to Friday and check the weather."),
                    Calls(call("t1", "calendar_update", r#"{"id":3}"#)),
                ],
                vec![Text("Okay.")],
            ],
            &[("calendar_update", 10, r#"{"ok":true}"#)],
            None,
            "",
        );
        h.frame(CallBody::Commit {
            turn: 1,
            text: "move my run".into(),
        });
        h.expect(&[
            speak(2, 0, "I'll move it to Friday and check the weather."),
            CallBody::Play { reply: 2 },
            CallBody::SpeakDone { reply: 2 },
        ]);
        h.frame(CallBody::BargeIn {
            reply: 2,
            heard_chars: 10,
        });
        h.frame(CallBody::Commit {
            turn: 2,
            text: "wait".into(),
        });
        h.expect(&[
            speak(3, 0, "Okay."),
            CallBody::Play { reply: 3 },
            CallBody::SpeakDone { reply: 3 },
        ]);

        let seen = h.llm.seen();
        assert!(
            matches!(&seen[1].messages[1], Message::Assistant { text, tool_calls } if text == "I'll move …" && tool_calls.len() == 1),
            "{:?}",
            seen[1].messages
        );
        h.wait_for("the job to run", |h| !h.ran().is_empty());
        h.stop();
        assert_eq!(h.ran(), vec!["calendar_update"]);
        let rows = h.rows();
        assert_eq!(rows[1], row("assistant", "I'll move …", None), "{rows:?}");
        assert!(
            rows.contains(&row("tool", r#"{"ok":true}"#, Some("calendar_update"))),
            "{rows:?}"
        );
    }

    #[test]
    fn hang_up_sends_hangup_after_the_turn() {
        let mut h = start(
            vec![vec![Text("Bye!"), Calls(call("t1", "hang_up", "{}"))]],
            &[],
            Some("Hi, it's Note."),
            "",
        );
        h.expect(&[
            speak(1, 0, "Hi, it's Note."),
            CallBody::SpeakDone { reply: 1 },
            CallBody::Play { reply: 1 },
        ]);
        h.frame(CallBody::Commit {
            turn: 1,
            text: "that's all, thanks".into(),
        });
        h.expect(&[
            speak(2, 0, "Bye!"),
            CallBody::Play { reply: 2 },
            CallBody::SpeakDone { reply: 2 },
            CallBody::HangUp,
        ]);
        assert!(
            matches!(&h.llm.seen()[0].messages[0], Message::Assistant { text, .. } if text == "Hi, it's Note."),
            "the opening is in the transcript"
        );
        h.frame(CallBody::Ended);
        h.driver.take().unwrap().join().unwrap();
        assert!(h.ran().is_empty());
    }

    #[test]
    fn a_stalled_model_retries_once_then_apologises() {
        let mut h = start(
            vec![vec![Fail("stalled")], vec![Fail("stalled")]],
            &[],
            None,
            "",
        );
        h.frame(CallBody::Commit {
            turn: 1,
            text: "hello?".into(),
        });
        h.expect(&[
            CallBody::SpeakDone { reply: 2 },
            speak(3, 0, APOLOGY),
            CallBody::SpeakDone { reply: 3 },
            CallBody::Play { reply: 3 },
        ]);
        assert_eq!(h.llm.seen().len(), 2);
        h.stop();
        assert_eq!(
            h.rows(),
            vec![row("user", "hello?", None), row("assistant", APOLOGY, None)]
        );
        assert_eq!(traces(&h), vec![0], "a turn with no clause has no round");
    }

    #[test]
    fn a_resumed_call_never_reuses_a_reply_number() {
        let mut h = start(
            vec![vec![Text("Adding it."), Calls(call("t1", "task_add", r#"{"title":"x"}"#))]],
            &[("task_add", 10, r#"{"id":9}"#)],
            None,
            "INSERT INTO voice_jobs (call_id, job, reply, call_index, tool, args, state, started_at)
             VALUES ('c1', 1, 5, 0, 'task_add', '{}', 'done', 'x');
             INSERT INTO voice_ops (call_id, op_key, result, created_at)
             VALUES ('c1', '7:0', '{\"id\":1}', 'x'), ('c1', '5:0', '{\"id\":2}', 'x');",
        );
        h.frame(CallBody::Commit {
            turn: 1,
            text: "add x".into(),
        });
        h.expect(&[
            speak(8, 0, "Adding it."),
            CallBody::Play { reply: 8 },
            CallBody::SpeakDone { reply: 8 },
        ]);
        h.wait_for("the tool to run", |h| !h.ran().is_empty());
        h.stop();
        assert_eq!(*h.runner.keys.lock().unwrap(), vec!["8:0"]);
        assert!(h
            .rows()
            .contains(&row("tool", r#"{"id":9}"#, Some("task_add"))));
    }

    #[test]
    fn a_draft_left_unresolved_is_dropped_after_three_seconds() {
        let mut h = start(
            vec![
                vec![
                    Text("On it."),
                    Calls(call("t1", "task_add", r#"{"title":"stretch"}"#)),
                ],
                vec![Text("Hm.")],
                vec![Text("Sure.")],
            ],
            &[("task_add", 10, r#"{"id":5}"#)],
            None,
            "",
        );
        h.frame(CallBody::Commit {
            turn: 1,
            text: "remind me to stretch".into(),
        });
        h.expect(&[
            speak(2, 0, "On it."),
            CallBody::Play { reply: 2 },
            CallBody::SpeakDone { reply: 2 },
        ]);
        h.wait_for("job 1 to land", |h| {
            h.rows().iter().any(|(role, ..)| role == "tool")
        });

        h.at(1000);
        h.frame(CallBody::Draft {
            turn: 2,
            text: "um".into(),
        });
        h.expect(&[speak(3, 0, "Hm."), CallBody::SpeakDone { reply: 3 }]);
        h.at(3999);
        h.tx.send(DriverIn::Tick).unwrap();
        assert_eq!(h.quiet(100), vec![], "still inside the wait");
        h.at(4000);
        h.tx.send(DriverIn::Tick).unwrap();
        h.expect(&[CallBody::Drop { reply: 3 }]);

        h.frame(CallBody::Commit {
            turn: 3,
            text: "so".into(),
        });
        h.expect(&[
            speak(4, 0, "Sure."),
            CallBody::Play { reply: 4 },
            CallBody::SpeakDone { reply: 4 },
        ]);
        let block = h.last_user(2);
        assert!(
            block.contains("[job 1 · task_add · done]"),
            "the snapshot came back: {block}"
        );
        assert!(block.contains("[you] so"), "{block}");
        h.stop();
    }

    fn traces(h: &Harness) -> Vec<i64> {
        let conn = crate::db_guard(&h.db);
        let mut stmt = conn.prepare("SELECT turns FROM agent_traces").unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn a_promoted_draft_resets_the_wake_cap() {
        let mut h = start(
            vec![
                vec![
                    Calls(call("t1", "task_add", r#"{"n":1}"#)),
                    Calls(call("t2", "task_add2", r#"{"n":2}"#)),
                    Calls(call("t3", "task_add3", r#"{"n":3}"#)),
                    Calls(call("t4", "task_add4", r#"{"n":4}"#)),
                ],
                vec![Text("")],
                vec![Text("")],
                vec![Text("")],
                vec![Text("")],
                vec![Calls(call("t5", "task_add", r#"{"n":5}"#))],
                vec![Text("Done.")],
            ],
            &[
                ("task_add", 10, "{}"),
                ("task_add2", 250, "{}"),
                ("task_add3", 500, "{}"),
                ("task_add4", 750, "{}"),
            ],
            None,
            "",
        );
        h.frame(CallBody::Commit {
            turn: 1,
            text: "add four things".into(),
        });
        h.expect(&[CallBody::SpeakDone { reply: 2 }]);
        h.at(1000);
        h.tick_until(&CallBody::SpeakDone { reply: 6 });
        assert_eq!(h.llm.seen().len(), 5, "four separate wakes");

        h.frame(CallBody::Draft {
            turn: 2,
            text: "and one more".into(),
        });
        h.expect(&[CallBody::SpeakDone { reply: 7 }]);
        h.frame(CallBody::Commit {
            turn: 2,
            text: "And one more.".into(),
        });
        h.expect(&[CallBody::Play { reply: 7 }]);
        h.tick_until(&speak(8, 0, "Done."));
        h.stop();
    }

    #[test]
    fn a_barge_in_frees_the_floor_for_a_wake() {
        let mut h = start(
            vec![
                vec![Text("Let me check."), Calls(call("t1", "task_add", "{}"))],
                vec![Text("Anything else?")],
            ],
            &[("task_add", 10, "{}")],
            None,
            "",
        );
        h.frame(CallBody::Commit {
            turn: 1,
            text: "add it".into(),
        });
        h.expect(&[
            speak(2, 0, "Let me check."),
            CallBody::Play { reply: 2 },
            CallBody::SpeakDone { reply: 2 },
        ]);
        h.frame(CallBody::BargeIn {
            reply: 2,
            heard_chars: 3,
        });
        h.tick_until(&speak(3, 0, "Anything else?"));
        h.stop();
    }

    #[test]
    fn a_heard_queued_during_a_turn_starts_when_it_ends() {
        let mut h = start(
            vec![
                vec![
                    Text("One sec,"),
                    StreamPiece::Wait(Duration::from_millis(200)),
                    Text(" here."),
                ],
                vec![Text("And that too.")],
            ],
            &[],
            None,
            "",
        );
        h.frame(CallBody::Commit {
            turn: 1,
            text: "first".into(),
        });
        h.expect(&[speak(2, 0, "One sec,"), CallBody::Play { reply: 2 }]);
        h.frame(CallBody::Commit {
            turn: 2,
            text: "second".into(),
        });
        h.expect(&[
            speak(2, 1, "here."),
            CallBody::SpeakDone { reply: 2 },
            speak(3, 0, "And that too."),
            CallBody::Play { reply: 3 },
            CallBody::SpeakDone { reply: 3 },
        ]);
        assert_eq!(h.last_user(1), "[you] second");
        h.stop();
    }

    #[test]
    fn a_timed_out_write_updates_its_row_when_it_lands() {
        let mut h = start_with(
            vec![vec![Text("Adding."), Calls(call("t1", "task_add", "{}"))]],
            &[("task_add", 300, r#"{"id":5}"#)],
            None,
            "",
            Duration::from_millis(100),
        );
        h.frame(CallBody::Commit {
            turn: 1,
            text: "add it".into(),
        });
        h.expect(&[
            speak(2, 0, "Adding."),
            CallBody::Play { reply: 2 },
            CallBody::SpeakDone { reply: 2 },
        ]);
        h.wait_for("the late result", |h| {
            h.rows()
                .contains(&row("tool", r#"{"id":5}"#, Some("task_add")))
        });
        h.stop();
        let tools = h
            .rows()
            .into_iter()
            .filter(|(role, ..)| role == "tool")
            .count();
        assert_eq!(tools, 1);
    }

    #[test]
    fn ending_with_jobs_running_records_them_and_one_trace() {
        let mut h = start(
            vec![vec![
                Text("Working on it."),
                Calls(call("t1", "task_add", "{}")),
                Calls(call("t2", "web_search", r#"{"q":"x"}"#)),
            ]],
            &[("task_add", 200, r#"{"id":5}"#), ("web_search", 3000, "{}")],
            None,
            "",
        );
        h.frame(CallBody::Commit {
            turn: 1,
            text: "do both".into(),
        });
        h.expect(&[
            speak(2, 0, "Working on it."),
            CallBody::Play { reply: 2 },
            CallBody::SpeakDone { reply: 2 },
        ]);
        h.frame(CallBody::Ended);
        h.driver.take().unwrap().join().unwrap();
        let rows = h.rows();
        assert!(
            rows.contains(&row("tool", r#"{"id":5}"#, Some("task_add"))),
            "{rows:?}"
        );
        assert!(
            rows.contains(&row(
                "tool",
                r#"{"cancelled":true,"job":2}"#,
                Some("web_search")
            )),
            "{rows:?}"
        );
        assert_eq!(traces(&h), vec![1]);
    }

    #[test]
    fn a_hang_up_held_in_a_draft_ends_the_call_once_committed() {
        let mut h = start(
            vec![vec![Text("Bye!"), Calls(call("t1", "hang_up", "{}"))]],
            &[],
            None,
            "",
        );
        h.frame(CallBody::Draft {
            turn: 1,
            text: "bye".into(),
        });
        h.expect(&[speak(2, 0, "Bye!"), CallBody::SpeakDone { reply: 2 }]);
        assert_eq!(h.quiet(100), vec![], "a held hang_up does nothing yet");
        h.frame(CallBody::Commit {
            turn: 1,
            text: "Bye.".into(),
        });
        h.expect(&[CallBody::Play { reply: 2 }, CallBody::HangUp]);
        h.stop();
    }

    #[test]
    fn a_hang_up_held_in_a_retracted_draft_never_runs() {
        let mut h = start(
            vec![
                vec![Text("Bye!"), Calls(call("t1", "hang_up", "{}"))],
                vec![Text("Go on.")],
            ],
            &[],
            None,
            "",
        );
        h.frame(CallBody::Draft {
            turn: 1,
            text: "bye".into(),
        });
        h.expect(&[speak(2, 0, "Bye!"), CallBody::SpeakDone { reply: 2 }]);
        h.frame(CallBody::Retract { turn: 1 });
        h.expect(&[CallBody::Drop { reply: 2 }]);
        h.frame(CallBody::Commit {
            turn: 1,
            text: "bye the way, one more thing".into(),
        });
        h.expect(&[
            speak(3, 0, "Go on."),
            CallBody::Play { reply: 3 },
            CallBody::SpeakDone { reply: 3 },
        ]);
        assert_eq!(h.quiet(100), vec![]);
        h.stop();
    }

    #[test]
    fn a_draft_during_a_cut_off_wake_waits_for_the_commit() {
        let mut h = start(
            vec![
                vec![Text("On it."), Calls(call("t1", "task_add", "{}"))],
                vec![
                    Text("Done with that. "),
                    StreamPiece::Wait(Duration::from_millis(300)),
                    Text("Anything else?"),
                ],
                vec![Text("Okay.")],
            ],
            &[("task_add", 10, "{}")],
            None,
            "",
        );
        h.frame(CallBody::Commit {
            turn: 1,
            text: "add it".into(),
        });
        h.expect(&[
            speak(2, 0, "On it."),
            CallBody::Play { reply: 2 },
            CallBody::SpeakDone { reply: 2 },
        ]);
        h.frame(CallBody::Floor {
            floor: Floor::Drained,
        });
        h.tick_until(&speak(3, 0, "Done with that."));
        h.frame(CallBody::BargeIn {
            reply: 3,
            heard_chars: 4,
        });
        h.frame(CallBody::Draft {
            turn: 2,
            text: "wait".into(),
        });
        h.expect(&[
            CallBody::Play { reply: 3 },
            CallBody::SpeakDone { reply: 3 },
        ]);
        assert_eq!(
            h.quiet(100),
            vec![],
            "the draft came while the wake was in flight"
        );
        h.frame(CallBody::Commit {
            turn: 2,
            text: "wait".into(),
        });
        h.expect(&[
            speak(4, 0, "Okay."),
            CallBody::Play { reply: 4 },
            CallBody::SpeakDone { reply: 4 },
        ]);
        let messages = &h.llm.seen()[2].messages;
        assert!(
            matches!(&messages[4], Message::Assistant { text, .. } if text == "Done…"),
            "{messages:?}"
        );
        assert_eq!(h.last_user(2), "[you] wait");
        h.stop();
        assert!(h.rows().contains(&row("assistant", "Done…", None)));
    }
}
