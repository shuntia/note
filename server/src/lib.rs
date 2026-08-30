pub mod agent;
pub mod api;
pub mod auth;
pub mod config;
pub mod context;
pub mod db;
pub mod log;
pub mod memory;
pub mod nightly;
pub mod plan;
pub mod prompts;
pub mod providers;
pub mod push_subs;
pub mod runner;
pub mod tasks;
pub mod templates;
pub mod tools;

use crate::providers::{EmbeddingsProvider, LLMProvider};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub llm: Arc<dyn LLMProvider>,
    pub embeddings: Option<Arc<dyn EmbeddingsProvider>>,
    pub vapid_public_key: Option<String>,
}

impl AppState {
    /// Providers default to an unscripted mock and no embeddings, so a state
    /// built without `with_providers` is still fully runnable.
    pub fn new(conn: Connection, config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            db: Arc::new(Mutex::new(conn)),
            config_dir,
            data_dir,
            llm: Arc::new(crate::providers::mock::MockLLM::empty()),
            embeddings: None,
            vapid_public_key: None,
        }
    }

    pub fn with_providers(
        mut self,
        llm: Arc<dyn LLMProvider>,
        embeddings: Option<Arc<dyn EmbeddingsProvider>>,
    ) -> Self {
        self.llm = llm;
        self.embeddings = embeddings;
        self
    }
}
