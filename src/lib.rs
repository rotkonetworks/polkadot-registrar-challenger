#[macro_use]
extern crate tracing;
#[macro_use]
extern crate anyhow;
#[macro_use]
extern crate serde;

mod matrix;
use matrix::Nickname;

use actix::clock::sleep;
use std::fs;
use std::time::Duration;

pub type Result<T> = std::result::Result<T, anyhow::Error>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct Config {
    pub log_level: LogLevel,
    pub matrix: MatrixConfig,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct MatrixConfig {
    pub enabled: bool,
    pub homeserver: String,
    pub username: String,
    pub password: String,
    pub security_key: String,
    pub admins: Option<Vec<Nickname>>,
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

    tracing_subscriber::fmt()
        .with_env_filter(format!("system={}", config.log_level.as_str()))
        .init();

    if config.matrix.enabled {
        info!("Starting Matrix bot");
        let config = config.matrix;
        matrix::start_bot(matrix::BotConfig {
            homeserver: &config.homeserver,
            username: &config.username,
            password: &config.password,
            security_key: &config.security_key,
            admins: config.admins.unwrap_or_default(),
        }).await?;
    }

    loop {
        sleep(Duration::from_secs(u64::MAX)).await;
    }
}
