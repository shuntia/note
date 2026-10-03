pub mod brief;
pub mod clauses;
pub mod jobs;
pub mod queue;
pub mod render;
pub mod turn;

use crate::agent::{self, SessionDeps};
use crate::providers::{EmbeddingsProvider, LLMProvider};
use crate::search::SearchProvider;
use crate::tools::SessionKind;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
