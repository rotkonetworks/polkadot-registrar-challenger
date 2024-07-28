#[macro_use]
extern crate tracing;
#[macro_use]
extern crate anyhow;
#[macro_use]
extern crate serde;

mod matrix;

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

    info!("Starting Matrix bot");
    matrix::start_bot(config.matrix).await?;

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
    pub matrix: matrix::BotConfig,
}
