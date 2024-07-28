#[macro_use]
extern crate tracing;
#[macro_use]
extern crate anyhow;
#[macro_use]
extern crate serde;

mod matrix;
use matrix::Nickname;

use tracing::Level;
use std::fs;

#[tokio::main]
pub async fn main() -> Result<(), anyhow::Error> {
    let config = open_config()?;

    tracing_subscriber::fmt()
        .with_max_level(config.log_level
            .parse::<Level>()
            .expect("Failed to parse log level"))
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

    Ok(())
}

fn open_config() -> Result<Config, anyhow::Error> {
    let content = fs::read_to_string("config.yaml")
        .map_err(|_| {
            anyhow!("Failed to open config at 'config.yaml'.")
        })?;

    let config = serde_yaml::from_str::<Config>(&content)
        .map_err(|err| anyhow!("Failed to parse config: {:?}", err))?;

    Ok(config)
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct Config {
    pub log_level: String,
    pub matrix: MatrixConfig,
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
