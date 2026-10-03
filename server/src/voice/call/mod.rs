pub mod brief;
pub mod clauses;
pub mod driver;
pub mod jobs;
pub mod queue;
pub mod render;
pub mod turn;

use super::Conversation;
use crate::agent::{self, SessionDeps};
use crate::channels::OutboundMessage;
use crate::providers::{ChatRequest, EmbeddingsProvider, LLMProvider, Message, StreamOpts, StreamSink, ToolCall};
use crate::search::SearchProvider;
use crate::tools::SessionKind;
use driver::{CallConfig, DriverDeps, DriverIn};
use note_voice_proto::{CallBody, Outcome};
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

const RESUMED_HISTORY: usize = 40;
const RESTARTED: &str = "Note restarted; the call is still on";

/// Sends a frame of the named call to the voice side.
pub type CallSender = Arc<dyn Fn(&str, CallBody) + Send + Sync>;

/// Runs a call's tools as `user_id` in a `Call` session.
pub struct CallRunner {
    pub db: Arc<Mutex<Connection>>,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub llm: Arc<dyn LLMProvider>,
    pub embeddings: Option<Arc<dyn EmbeddingsProvider>>,
    pub search: Option<Arc<dyn SearchProvider>>,
    pub user_id: i64,
    pub username: String,
}

impl jobs::ToolRunner for CallRunner {
    fn run(&self, name: &str, args: &str, once: (&str, &str)) -> (String, bool) {
        let deps = SessionDeps {
            db: &self.db,
            config_dir: &self.config_dir,
            data_dir: &self.data_dir,
            llm: &*self.llm,
            embeddings: self.embeddings.as_deref(),
            search: self.search.as_deref(),
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            token_id: None,
            thread_note: None,
            share: None,
        };
        agent::run_tool(&deps, self.user_id, &self.username, SessionKind::Call, name, args, Some(once))
    }

    fn is_network(&self, name: &str) -> bool {
        name == "web_search"
    }
}

/// `[voice]`'s pacing for a call.
#[derive(Debug, Clone)]
pub struct CallSettings {
    pub wake_settle: Duration,
    pub max_wakes: u32,
    pub job_timeout: Duration,
    pub max_jobs: usize,
    pub first_token: Duration,
    /// No `[voice] model`: each call speaks on the main model of the moment.
    pub follows_main_model: bool,
}

impl From<&crate::config::VoiceConfig> for CallSettings {
    fn from(v: &crate::config::VoiceConfig) -> Self {
        Self {
            wake_settle: Duration::from_millis(v.wake_settle_ms),
            max_wakes: v.max_wakes,
            job_timeout: Duration::from_secs(v.job_timeout_secs),
            max_jobs: v.max_jobs,
            first_token: Duration::from_millis(v.first_token_ms),
            follows_main_model: v.model.is_none(),
        }
    }
}

impl Default for CallSettings {
    fn default() -> Self {
        use crate::config as c;
        Self {
            wake_settle: Duration::from_millis(c::default_wake_settle_ms()),
            max_wakes: c::default_max_wakes(),
            job_timeout: Duration::from_secs(c::default_job_timeout_secs()),
            max_jobs: c::default_max_jobs(),
            first_token: Duration::from_millis(c::default_first_token_ms()),
            follows_main_model: true,
        }
    }
}

pub struct CallDeps {
    pub db: Arc<Mutex<Connection>>,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    /// Runs the call's tools.
    pub llm: Arc<dyn LLMProvider>,
    /// Speaks on the call.
    pub voice_llm: Arc<dyn LLMProvider>,
    pub embeddings: Option<Arc<dyn EmbeddingsProvider>>,
    pub search: Option<Arc<dyn SearchProvider>>,
    pub settings: CallSettings,
}

impl CallDeps {
    fn session(&self) -> SessionDeps<'_> {
        SessionDeps {
            db: &self.db,
            config_dir: &self.config_dir,
            data_dir: &self.data_dir,
            llm: &*self.llm,
            embeddings: self.embeddings.as_deref(),
            search: self.search.as_deref(),
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            token_id: None,
            thread_note: None,
            share: None,
        }
    }

    /// Points the voice model at the main model's current one, unless
    /// `[voice] model` names its own.
    fn follow_main_model(&self) {
        if !self.settings.follows_main_model {
            return;
        }
        if let Some(model) = self.llm.model() {
            self.voice_llm.set_model(&model);
        }
    }

    /// The call's system prompt and tool schemas.
    fn prompt(
        &self,
        user_id: i64,
        username: &str,
        reason: &brief::Reason,
        conversation_id: Option<i64>,
    ) -> anyhow::Result<(String, Vec<serde_json::Value>)> {
        let system = brief::build(&self.session(), user_id, username, reason, conversation_id, jiff::Timestamp::now())?;
        let mut tools = crate::tools::schemas(SessionKind::Call);
        if self.search.is_none() {
            tools.retain(|s| s["name"] != "web_search");
        }
        Ok((system, tools))
    }
}

struct Live {
    call_id: String,
    user_id: i64,
    username: String,
    conversation_id: i64,
    system: String,
    tools: Vec<serde_json::Value>,
    opening: Option<String>,
    history: Vec<Message>,
    initial: Vec<render::Item>,
}

/// Holds the conversation of every live call, one driver thread each. Does
/// nothing until `set_deps`.
pub struct CallManager {
    deps: OnceLock<CallDeps>,
    send: CallSender,
    calls: Mutex<HashMap<String, mpsc::Sender<DriverIn>>>,
    resuming: Mutex<()>,
}

impl CallManager {
    pub fn new(send: CallSender) -> Self {
        Self { deps: OnceLock::new(), send, calls: Mutex::default(), resuming: Mutex::new(()) }
    }

    pub fn set_deps(&self, deps: CallDeps) {
        let _ = self.deps.set(deps);
    }

    pub fn is_ready(&self) -> bool {
        self.deps.get().is_some()
    }

    pub fn is_live(&self, call_id: &str) -> bool {
        self.calls().contains_key(call_id)
    }

    fn calls(&self) -> std::sync::MutexGuard<'_, HashMap<String, mpsc::Sender<DriverIn>>> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes up every answered call an earlier run of Note held; returns how many.
    pub fn resume(&self) -> usize {
        let Some(d) = self.deps.get() else { return 0 };
        let ids: Vec<String> = {
            let conn = crate::db_guard(&d.db);
            conn.prepare("SELECT id FROM voice_calls WHERE state = 'answered' AND conversation_id IS NOT NULL")
                .and_then(|mut stmt| stmt.query_map([], |r| r.get(0))?.collect())
                .unwrap_or_default()
        };
        ids.iter().filter(|id| self.resume_call(d, id)).count()
    }

    /// Respawns the driver of an answered call that has none, telling the
    /// model how its running jobs ended; true when it did.
    fn resume_call(&self, d: &CallDeps, call_id: &str) -> bool {
        let _once = self.resuming.lock().unwrap_or_else(PoisonError::into_inner);
        if self.is_live(call_id) {
            return false;
        }
        let row: Option<(i64, String, Option<String>, i64)> = crate::db_guard(&d.db)
            .query_row(
                "SELECT c.user_id, u.username, c.message, c.conversation_id
                 FROM voice_calls c JOIN users u ON u.id = c.user_id
                 WHERE c.id = ?1 AND c.state = 'answered' AND c.conversation_id IS NOT NULL",
                [call_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .ok()
            .flatten();
        let Some((user_id, username, message, conversation_id)) = row else { return false };
        let msg = message.and_then(|m| serde_json::from_str::<OutboundMessage>(&m).ok());
        let started = d.prompt(user_id, &username, &reason(msg.as_ref()), Some(conversation_id)).and_then(|(system, tools)| {
            let history = crate::talk::history(&crate::db_guard(&d.db), conversation_id, RESUMED_HISTORY)?;
            Ok((system, tools, history))
        });
        match started {
            Ok((system, tools, history)) => {
                let mut initial = jobs::JobTable::recover(&d.db, call_id);
                initial.push(render::Item::System(RESTARTED.into()));
                let call_id = call_id.to_string();
                self.spawn(d, Live { call_id, user_id, username, conversation_id, system, tools, opening: None, history, initial });
                true
            }
            Err(e) => {
                self.give_up(d, call_id, user_id, &e);
                false
            }
        }
    }

    /// Primes the reply model's connection and prompt cache while the phone
    /// rings: the call's own prompt, stopped at the first delta.
    pub fn warm_up(self: &Arc<Self>, user_id: i64, msg: &OutboundMessage) {
        if !self.is_ready() {
            return;
        }
        let (me, msg) = (self.clone(), msg.clone());
        spawn_blocking(move || {
            let Some(d) = me.deps.get() else { return };
            d.follow_main_model();
            let username: Option<String> = crate::db_guard(&d.db)
                .query_row("SELECT username FROM users WHERE id = ?1", [user_id], |r| r.get(0))
                .ok();
            let Some(username) = username else { return };
            let conversation_id = owned_conversation(&d.db, user_id, msg.conversation_id);
            let Ok((system, tools)) = d.prompt(user_id, &username, &reason(Some(&msg)), conversation_id) else {
                return;
            };
            let messages = [Message::Assistant { text: msg.body.clone(), tool_calls: vec![] }, Message::User("[note] The phone is ringing.".into())];
            let req = ChatRequest { system: &system, messages: &messages, tools: &tools, background: false };
            let opts = StreamOpts { first_token: d.settings.first_token };
            let _ = d.voice_llm.chat_stream(&req, &opts, &mut FirstDelta);
        });
    }

    /// Starts the conversation of a call just answered, in the thread its
    /// message belongs to or a new one.
    fn answer(&self, d: &CallDeps, call_id: &str) -> anyhow::Result<()> {
        if self.is_live(call_id) {
            return Ok(());
        }
        let row: Option<(i64, String, Option<String>)> = crate::db_guard(&d.db)
            .query_row(
                "SELECT c.user_id, u.username, c.message FROM voice_calls c JOIN users u ON u.id = c.user_id
                 WHERE c.id = ?1 AND c.state = 'answered' AND c.conversation_id IS NULL",
                [call_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((user_id, username, message)) = row else { return Ok(()) };
        let msg = message.map(|m| serde_json::from_str::<OutboundMessage>(&m)).transpose()?;
        let existing = owned_conversation(&d.db, user_id, msg.as_ref().and_then(|m| m.conversation_id));
        let (system, tools) = d.prompt(user_id, &username, &reason(msg.as_ref()), existing)?;
        let conversation_id = {
            let conn = crate::db_guard(&d.db);
            let now = jiff::Timestamp::now();
            let id = match existing {
                Some(id) => id,
                None => crate::talk::create(&conn, user_id, "Call", now)?,
            };
            conn.execute("UPDATE voice_calls SET conversation_id = ?2 WHERE id = ?1", (call_id, id))?;
            crate::talk::mark_via(&conn, id, crate::talk::Via::Voice, now)?;
            id
        };
        self.spawn(
            d,
            Live {
                call_id: call_id.to_string(),
                user_id,
                username,
                conversation_id,
                system,
                tools,
                opening: msg.map(|m| m.body),
                history: Vec::new(),
                initial: Vec::new(),
            },
        );
        Ok(())
    }

    fn spawn(&self, d: &CallDeps, live: Live) {
        d.follow_main_model();
        let (tx, rx) = mpsc::channel();
        self.calls().insert(live.call_id.clone(), tx.clone());
        let send = self.send.clone();
        let id = live.call_id.clone();
        let started = Instant::now();
        let s = &d.settings;
        let deps = DriverDeps {
            call_id: live.call_id,
            user_id: live.user_id,
            username: live.username.clone(),
            conversation_id: live.conversation_id,
            db: d.db.clone(),
            llm: d.voice_llm.clone(),
            system: Arc::new(live.system),
            tools: Arc::new(live.tools),
            runner: Arc::new(CallRunner {
                db: d.db.clone(),
                config_dir: d.config_dir.clone(),
                data_dir: d.data_dir.clone(),
                llm: d.llm.clone(),
                embeddings: d.embeddings.clone(),
                search: d.search.clone(),
                user_id: live.user_id,
                username: live.username,
            }),
            cfg: CallConfig {
                wake: queue::WakeConfig { settle: s.wake_settle, max_wakes: s.max_wakes },
                jobs: jobs::JobLimits { timeout: s.job_timeout, max_running: s.max_jobs },
                first_token: s.first_token,
                ticker: true,
            },
            send: Arc::new(move |body| send(&id, body)),
            clock: Arc::new(move || started.elapsed()),
        };
        let (opening, history, initial) = (live.opening, live.history, live.initial);
        std::thread::spawn(move || driver::run(deps, opening, history, initial, rx, tx));
    }

    /// Hangs up a call whose conversation could not start.
    fn give_up(&self, d: &CallDeps, call_id: &str, user_id: i64, e: &anyhow::Error) {
        eprintln!("voice: starting the conversation of {call_id} failed: {e:#}");
        {
            let conn = crate::db_guard(&d.db);
            let _ = crate::log::record(&conn, Some(user_id), "voice_error", &format!("a call's conversation did not start: {e:#}"));
        }
        (self.send)(call_id, CallBody::HangUp);
    }
}

impl Conversation for CallManager {
    fn on_frame(&self, call_id: &str, body: &CallBody) {
        match body {
            CallBody::Outcome { outcome: Outcome::Answered } => {
                let Some(d) = self.deps.get() else { return };
                if let Err(e) = self.answer(d, call_id) {
                    let user_id = crate::db_guard(&d.db)
                        .query_row("SELECT user_id FROM voice_calls WHERE id = ?1", [call_id], |r| r.get(0))
                        .ok();
                    match user_id {
                        Some(user_id) => self.give_up(d, call_id, user_id, &e),
                        None => (self.send)(call_id, CallBody::HangUp),
                    }
                }
            }
            CallBody::Outcome { .. } => {}
            CallBody::Ended => {
                if let Some(tx) = self.calls().remove(call_id) {
                    let _ = tx.send(DriverIn::Frame(CallBody::Ended));
                }
            }
            other => {
                if let Some(d) = self.deps.get().filter(|_| !self.is_live(call_id)) {
                    self.resume_call(d, call_id);
                }
                if let Some(tx) = self.calls().get(call_id) {
                    let _ = tx.send(DriverIn::Frame(other.clone()));
                }
            }
        }
    }
}

fn reason(msg: Option<&OutboundMessage>) -> brief::Reason {
    match msg {
        Some(m) => brief::Reason::CheckIn { title: m.title.clone(), body: m.body.clone() },
        None => brief::Reason::UserCalled,
    }
}

/// `id` when it names a thread of `user_id`.
fn owned_conversation(db: &Mutex<Connection>, user_id: i64, id: Option<i64>) -> Option<i64> {
    let id = id?;
    crate::db_guard(db)
        .query_row("SELECT id FROM conversations WHERE id = ?1 AND user_id = ?2", (id, user_id), |r| r.get(0))
        .optional()
        .ok()
        .flatten()
}

struct FirstDelta;

impl StreamSink for FirstDelta {
    fn text(&mut self, _delta: &str) -> bool {
        false
    }

    fn tool_call(&mut self, _call: &ToolCall) -> bool {
        false
    }
}

/// On tokio's blocking pool when a runtime is running, else on a thread of its own.
pub(crate) fn spawn_blocking(f: impl FnOnce() + Send + 'static) {
    match tokio::runtime::Handle::try_current() {
        Ok(rt) => {
            rt.spawn_blocking(f);
        }
        Err(_) => {
            std::thread::spawn(f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jobs::ToolRunner;

    struct Named(Mutex<String>);

    impl LLMProvider for Named {
        fn chat(&self, _req: &ChatRequest) -> anyhow::Result<crate::providers::ChatResponse> {
            Ok(crate::providers::ChatResponse::default())
        }

        fn model(&self) -> Option<String> {
            Some(self.0.lock().unwrap().clone())
        }

        fn set_model(&self, model: &str) -> bool {
            *self.0.lock().unwrap() = model.to_string();
            true
        }
    }

    fn deps_on(main: &Arc<Named>, voice: &Arc<Named>, follows_main_model: bool) -> CallDeps {
        CallDeps {
            db: Arc::new(Mutex::new(crate::db::open_memory().unwrap())),
            config_dir: PathBuf::new(),
            data_dir: PathBuf::new(),
            llm: main.clone(),
            voice_llm: voice.clone(),
            embeddings: None,
            search: None,
            settings: CallSettings { follows_main_model, ..Default::default() },
        }
    }

    #[test]
    fn a_call_starts_on_the_main_model_of_the_moment_unless_it_has_its_own() {
        let main = Arc::new(Named(Mutex::new("a".into())));
        let voice = Arc::new(Named(Mutex::new("startup".into())));
        let d = deps_on(&main, &voice, true);
        d.follow_main_model();
        assert_eq!(voice.model().as_deref(), Some("a"));
        main.set_model("b");
        d.follow_main_model();
        assert_eq!(voice.model().as_deref(), Some("b"), "an admin switch reaches the next call");

        let own = Arc::new(Named(Mutex::new("fast".into())));
        deps_on(&main, &own, false).follow_main_model();
        assert_eq!(own.model().as_deref(), Some("fast"));
    }

    #[test]
    fn the_runner_lands_a_write_once_per_op_key() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = crate::db::open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", []).unwrap();
        conn.execute(
            "INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at)
             VALUES ('c1', 1, 'outbound', 'answered', 'x', 'x')",
            [],
        )
        .unwrap();
        let db = Arc::new(Mutex::new(conn));
        let runner = CallRunner {
            db: db.clone(),
            config_dir: tmp.path().to_path_buf(),
            data_dir: tmp.path().to_path_buf(),
            llm: Arc::new(crate::providers::mock::MockLLM::scripted(vec![])),
            embeddings: None,
            search: None,
            user_id: 1,
            username: "aki".into(),
        };
        let first = runner.run("task_create", r#"{"title":"essay"}"#, ("c1", "2:0"));
        assert!(!first.1, "{}", first.0);
        assert_eq!(runner.run("task_create", r#"{"title":"essay"}"#, ("c1", "2:0")), first);
        let tasks: i64 = crate::db_guard(&db).query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(tasks, 1);
        assert!(!runner.is_network("task_create"));
        assert!(runner.is_network("web_search"));
    }
}
