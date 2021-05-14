use crate::actors::api::NotifyAccountState;
use crate::database::{Database, VerificationOutcome};
use crate::primitives::ExternalMessage;
use crate::primitives::{JudgementState, NotificationMessage};
use crate::{EmailConfig, MatrixConfig, Result, TwitterConfig};
use actix::prelude::*;
use actix_broker::{Broker, SystemBroker};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::time::{interval, Duration};

pub struct AdapterListener {
    db: Database,
    tx: UnboundedSender<ExternalMessage>,
    rx: UnboundedReceiver<ExternalMessage>,
    verifier: Addr<Verifier>,
}

pub trait Adapter {
    fn name(&self) -> &'static str;
    fn fetch_messages(&mut self) -> Result<Vec<ExternalMessage>>;
}

struct Verifier {
    db: Database,
}

impl Actor for Verifier {
    type Context = Context<Self>;
}

impl Handler<ExternalMessage> for Verifier {
    type Result = ResponseActFuture<Self, ()>;

    fn handle(&mut self, msg: ExternalMessage, ctx: &mut Self::Context) -> Self::Result {
        fn notify_session(state: JudgementState, notifications: Vec<NotificationMessage>) {
            Broker::<SystemBroker>::issue_async(NotifyAccountState {
                state: state,
                notifications: notifications,
            });
        }

        let db = self.db.clone();
        Box::pin(
            async move {
                debug!("Verifying message: {:?}", msg);
                let outcome = match db.verify_message(&msg).await {
                    Ok(outcome) => outcome,
                    Err(err) => {
                        error!("Failed to verify message: {:?}", err);
                        return ()
                    }
                };

                match outcome {
                    VerificationOutcome::AlreadyVerified => {
                        debug!("The account field has already been verified: {:?}", msg)
                        // Don't inform user.
                    }
                    VerificationOutcome::Valid {
                        state,
                        notifications,
                    } => {
                        info!("Message verification succeeded : {:?}", msg);
                        notify_session(state, notifications);
                    }
                    VerificationOutcome::Invalid {
                        state,
                        notifications,
                    } => {
                        info!("Message verification failed: {:?}", msg);
                        notify_session(state, notifications);
                    }
                    VerificationOutcome::SecondChallengeExpected {
                        state,
                        notifications,
                    } => {
                        info!(
                            "Message verification succeeded: {:?}, second verification expected",
                            msg
                        );
                        notify_session(state, notifications);

                        // TODO: Notify client
                    }
                    VerificationOutcome::NotFound => {
                        debug!(
                            "No judgement state could be found based on the external message: {:?}",
                            msg
                        );
                    }
                }
            }
            .into_actor(self),
        )
    }
}

impl AdapterListener {
    pub async fn new(db: Database) -> Self {
        let (tx, mut rx) = unbounded_channel();

        AdapterListener {
            db: db.clone(),
            tx: tx,
            rx: rx,
            verifier: Verifier { db: db }.start(),
        }
    }
    pub async fn start_message_adapter<T>(&self, mut adapter: T, timeout: u64)
    where
        T: 'static + Adapter + Send,
    {
        let mut interval = interval(Duration::from_secs(timeout));

        let tx = self.tx.clone();
        tokio::spawn(async move {
            loop {
                // Timeout (skipped the first time);
                interval.tick().await;

                // Fetch message and send it to the listener, if any.
                match adapter.fetch_messages() {
                    Ok(messages) => {
                        for message in messages {
                            debug!("Received message: {:?}", message);
                            // TODO: Is unwrapping fine here?
                            tx.send(message).unwrap();
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
