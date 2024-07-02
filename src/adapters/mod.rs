use crate::database::{Database, EventCursor};
use crate::primitives::{
    ExpectedMessage, ExternalMessage, IdentityFieldValue, NotificationMessage,
};
use crate::{AdapterConfig, Result};
use tokio::time::{interval, Duration};
use tracing::Instrument;

pub mod admin;
pub mod matrix;

pub async fn run_adapters(config: AdapterConfig, db: Database) -> Result<()> {
    let listener = AdapterListener::new(db.clone()).await;
    // Convenience flat for logging
    let mut started = false;

    // Deconstruct struct to get around borrowing violations.
    let AdapterConfig {
        watchers: _,
        matrix: matrix_config,
        display_name: _,
    } = config;

    // Matrix client configuration and execution.
    if matrix_config.enabled {
        let config = matrix_config;

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
                db,
                config.admins.unwrap_or_default(),
            )
            .await?;

            info!("Starting message adapter");
            listener.start_message_adapter(matrix_client, 1).await;
            Result::Ok(())
        }
        .instrument(span)
        .await?;

        started = true;
    }

    if !started {
        warn!("No adapters are enabled");
    }

    Ok(())
}

#[async_trait]
pub trait Adapter {
    type MessageType;

    fn name(&self) -> &'static str;
    async fn fetch_messages(&mut self) -> Result<Vec<ExternalMessage>>;
    async fn send_message(&mut self, to: &str, content: Self::MessageType) -> Result<()>;
}

// Filler for adapters that do not send messages.
impl From<ExpectedMessage> for () {
    fn from(_: ExpectedMessage) -> Self {}
}

pub struct AdapterListener {
    db: Database,
}

impl AdapterListener {
    pub async fn new(db: Database) -> Self {
        AdapterListener { db }
    }

    pub async fn start_message_adapter<T>(&self, mut adapter: T, timeout: u64)
    where
        T: 'static + Adapter + Send,
        <T as Adapter>::MessageType: From<ExpectedMessage>,
    {
        let mut interval = interval(Duration::from_secs(timeout));

        let mut db = self.db.clone();
        let mut cursor = EventCursor::new();
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

                // Check if a second challenge must be sent to the user directly.
                match db.fetch_events(&mut cursor).await {
                    Ok(events) => {
                        for event in &events {
                            if let NotificationMessage::AwaitingSecondChallenge { context, field } =
                                event
                            {
                                if let IdentityFieldValue::Email(to) = field {
                                    if adapter.name() == "email" {
                                        info!("Sending second challenge to {}", to);
                                        if let Ok(challenge) = db
                                            .fetch_second_challenge(context, field)
                                            .await
                                            .map_err(|err| error!("Failed to fetch second challenge from database: {:?}", err)) {
                                                let _ = adapter
                                                    .send_message(to.as_str(), challenge.into())
                                                    .await
                                                    .map_err(|err| error!("Failed to send second challenge to {} ({} adapter): {:?}", to, adapter.name(), err));
                                                }
                                    }
                                }
                            }
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
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[derive(Clone)]
    pub struct MessageInjector {
        messages: Arc<Mutex<Vec<ExternalMessage>>>,
    }

    impl MessageInjector {
        pub fn new() -> Self {
            MessageInjector {
                messages: Arc::new(Mutex::new(vec![])),
            }
        }

        pub async fn send(&self, msg: ExternalMessage) {
            let mut lock = self.messages.lock().await;
            (*lock).push(msg);
        }
    }

    #[async_trait]
    impl Adapter for MessageInjector {
        type MessageType = ();

        fn name(&self) -> &'static str {
            "test_state_injector"
        }

        async fn fetch_messages(&mut self) -> Result<Vec<ExternalMessage>> {
            let mut lock = self.messages.lock().await;
            Ok(std::mem::take(&mut *lock))
        }

        async fn send_message(&mut self, _to: &str, _content: Self::MessageType) -> Result<()> {
            unimplemented!()
        }
    }
}
