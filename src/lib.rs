#[macro_use]
extern crate tracing;
#[macro_use]
extern crate anyhow;
#[macro_use]
extern crate serde;
#[macro_use]
extern crate async_trait;

use actix::clock::sleep;
use adapters::matrix::MatrixHandle;
use primitives::ChainName;
use std::fs;
use std::time::Duration;

pub type Result<T> = std::result::Result<T, anyhow::Error>;

use adapters::run_adapters;
use api::run_rest_api_server;
use connector::run_connector;
use database::Database;
use notifier::run_session_notifier;

mod adapters;
mod api;
mod connector;
mod database;
mod display_name;
mod notifier;
mod primitives;
#[cfg(test)]
mod tests;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct Config {
    pub log_level: LogLevel,
    pub db: DatabaseConfig,
    pub adapter: Option<AdapterConfig>,
    pub notifier: Option<NotifierConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
struct DatabaseConfig {
    pub uri: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct NotifierConfig {
    pub api_address: String,
    pub cors_allow_origin: Vec<String>,
    pub display_name: DisplayNameConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AdapterConfig {
    pub watcher: Vec<WatcherConfig>,
    pub matrix: MatrixConfig,
    pub display_name: DisplayNameConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WatcherConfig {
    pub network: ChainName,
    pub endpoint: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DisplayNameConfig {
    pub enabled: bool,
    pub limit: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MatrixConfig {
    pub enabled: bool,
    pub homeserver: String,
    pub username: String,
    pub password: String,
    pub db_path: String,
    pub admins: Option<Vec<MatrixHandle>>,
}

fn open_config() -> Result<Config> {
    // Open config file.
    let content = fs::read_to_string("config.yaml")
        .map_err(|_| {
            anyhow!("Failed to open config at 'config.yaml'.")
        })?;

    // Parse config file as JSON.
    let config = serde_yaml::from_str::<Config>(&content)
        .map_err(|err| anyhow!("Failed to parse config: {:?}", err))?;

    Ok(config)
}

async fn config_adapter_listener(db: Database, config: AdapterConfig) -> Result<()> {
    let watchers = config.watcher.clone();
    let dn_config = config.display_name.clone();
    run_adapters(config.clone(), db.clone()).await?;
    run_connector(db, watchers, dn_config).await
}

async fn config_session_notifier(db: Database, not_config: NotifierConfig) -> Result<()> {
    let lookup = run_rest_api_server(not_config, db.clone()).await?;

    actix::spawn(async move { run_session_notifier(db, lookup).await });

    Ok(())
}

pub async fn run() -> Result<()> {
    let config = open_config()?;
    let db_config = config.db;

    tracing_subscriber::fmt()
        .with_env_filter(format!("system={}", config.log_level.as_str()))
        .init();

    info!("Starting registrar service");

    info!("Initializing connection to database");
    let db = Database::new(&db_config.uri, &db_config.name).await?;
    db.connectivity_check().await?;

    if let Some(adapter_config) = config.adapter {
        info!("Starting adapter listener instance");
        config_adapter_listener(db.clone(), adapter_config).await?;
    }
    if let Some(notifier_config) = config.notifier {
        info!("Starting session notifier instance");
        config_session_notifier(db, notifier_config).await?;
    }

    info!("Setup completed");

    loop {
        sleep(Duration::from_secs(u64::MAX)).await;
    }
}
