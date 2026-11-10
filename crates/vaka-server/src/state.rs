use crate::{ch::ClickHouse, config::Config, db::Pool, processor::Work};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

#[derive(Clone, Debug)]
pub struct ProjectInfo {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub platform: String,
    pub retention_days: i64,
    pub scrub_enabled: bool,
    pub scrub_fields: String,
}

pub struct AppStateInner {
    pub config: Config,
    pub db: Pool,
    pub ch: ClickHouse,
    pub work_tx: mpsc::Sender<Work>,
    pub key_cache: RwLock<HashMap<String, (ProjectInfo, i64)>>,
    pub geoip: Option<maxminddb::Reader<Vec<u8>>>,
    pub symbols: crate::symbolicate::SymbolCache,
    pub http: reqwest::Client,
    /// (day, salt) used for anonymous visitor hashing; rotated daily.
    pub salt: RwLock<(String, String)>,
}

pub type AppState = Arc<AppStateInner>;
