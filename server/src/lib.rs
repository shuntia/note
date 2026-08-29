pub mod api;
pub mod auth;
pub mod config;
pub mod context;
pub mod db;
pub mod log;
pub mod memory;
pub mod plan;
pub mod prompts;
pub mod providers;
pub mod runner;
pub mod tasks;
pub mod templates;
pub mod tools;

use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub config_dir: PathBuf,
}

impl AppState {
    pub fn new(conn: Connection, config_dir: PathBuf) -> Self {
        Self { db: Arc::new(Mutex::new(conn)), config_dir }
    }
}
