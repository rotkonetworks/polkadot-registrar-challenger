use crate::database::{Database};
use crate::base::{
    ExpectedMessage, ExternalMessage,
};
use crate::{MatrixConfig, Result};

use tokio::time::{interval, Duration};
use tracing::Instrument;

pub mod admin;
pub mod matrix;

pub async fn run_matrix_adapter(config: MatrixConfig, db: Database) -> Result<()> {
    let span = info_span!("matrix_adapter");
    info!(
        homeserver = config.homeserver.as_str(),
        username = config.username.as_str()
    );

    async {
        info!("Configuring client");
        let matrix_client = matrix::MatrixClient::new(
            &config.homeserver,
            &config.username,
            &config.password,
            &config.db_path,
            db.clone(),
            config.admins.unwrap_or_default(),
        ).await?;

        info!("Starting message adapter");
        start_message_adapter(db.clone(), matrix_client, 1).await;
        Result::Ok(())
    }
        .instrument(span)
        .await?;

    Ok(())
}

async fn start_message_adapter<T>(db: Database, mut adapter: T, timeout: u64)
where
    T: 'static + Adapter + Send,
    <T as Adapter>::MessageType: From<ExpectedMessage>,
{
    let mut interval = interval(Duration::from_secs(timeout));

    let db = db.clone();
    actix::spawn(async move {
        loop {
            // Timeout (skipped the first time);
            interval.tick().await;

            // Fetch message and send it to the listener, if any.
            match adapter.fetch_messages().await {
                Ok(messages) => {
                    for message in messages {
                        info!("Received {:#?}", message);
                        let _ = db
                            .verify_message(&message)
                            .await
                            .map_err(|err| error!("Error when verifying message: {:?}", err));
                    }
                }
                Err(err) => {
                    error!(
                            "Error fetching messages in {} adapter: {:?}",
                            adapter.name(),
                            err
                        );
                }
            }
        }
    });
}

#[async_trait]
pub trait Adapter {
    type MessageType;
    
    fn name(&self) -> &'static str;

    async fn fetch_messages(&mut self) -> Result<Vec<ExternalMessage>>;
}

impl From<ExpectedMessage> for () {
    fn from(_: ExpectedMessage) -> Self {}
}
