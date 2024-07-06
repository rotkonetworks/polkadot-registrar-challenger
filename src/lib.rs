#[macro_use]
extern crate tracing;
#[macro_use]
extern crate anyhow;
#[macro_use]
extern crate serde;
extern crate async_trait;

mod matrix;
mod watcher;
mod database;
mod display_name;

use database::Database;
use watcher::ChainName;

use actix::clock::sleep;
use matrix::Nickname;
use std::fs;
use std::time::Duration;

pub type Result<T> = std::result::Result<T, anyhow::Error>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct Config {
    pub log_level: LogLevel,
    pub db: DatabaseConfig,
    pub listener: Option<ListenerConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LogLevel {
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

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ListenerConfig {
    pub watchers: Vec<WatcherConfig>,
    pub matrix: MatrixConfig,
    pub display_name: DisplayNameConfig,
}

#[derive(Debug, Clone, Deserialize)]
struct WatcherConfig {
    pub network: ChainName,
    pub endpoint: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct MatrixConfig {
    pub enabled: bool,
    pub homeserver: String,
    pub username: String,
    pub password: String,
    pub admins: Option<Vec<Nickname>>,
}

#[derive(Debug, Clone, Deserialize)]
struct DisplayNameConfig {
    pub enabled: bool,
    pub limit: f64,
}

fn open_config() -> Result<Config> {
    let content = fs::read_to_string("config.yaml")
        .map_err(|_| {
            anyhow!("Failed to open config at 'config.yaml'.")
        })?;

    let config = serde_yaml::from_str::<Config>(&content)
        .map_err(|err| anyhow!("Failed to parse config: {:?}", err))?;

    Ok(config)
}

pub async fn run() -> Result<()> {
    let config = open_config()?;
    let db_config = config.db;

    tracing_subscriber::fmt()
        .with_env_filter(format!("system={}", config.log_level.as_str()))
        .init();

    info!("Connecting to the database");
    let db = Database::new(&db_config.uri, &db_config.name).await?;
    db.connectivity_check().await?;

    if let Some(config) = config.listener {
        if config.matrix.enabled {
            info!("Starting Matrix bot");
            let config = config.matrix;
            matrix::start_bot(
                db.clone(),
                matrix::BotConfig {
                    homeserver: &config.homeserver,
                    username: &config.username,
                    password: &config.password,
                    admins: config.admins.unwrap_or_default(),
                }
            ).await?;
        }

        // info!("Connecting to watchers");
        // let watchers = config.watchers.clone();
        // let dn_config = config.display_name.clone();
        // watcher::open_connections(db, watchers, dn_config).await?;
    }

    info!("Setup completed");

    loop {
        sleep(Duration::from_secs(u64::MAX)).await;
    }
}
