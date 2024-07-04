use crate::base::{ChainAddress, ChainName, ExternalMessage, ExternalMessageType, IdentityContext, RawFieldName, Response, Timestamp};
use crate::{base, Database, Result};
use matrix_sdk::events::room::member::MemberEventContent;
use matrix_sdk::events::room::message::{MessageEventContent, MessageType, TextMessageEventContent};
use matrix_sdk::events::{AnyMessageEventContent, StrippedStateEvent, SyncMessageEvent};
use matrix_sdk::room::Room;
use matrix_sdk::{Client, ClientConfig, EventHandler, SyncSettings};
use std::str::FromStr;
use tokio::time::{self, Duration};
use url::Url;

const REJOIN_DELAY: u64 = 10;
const REJOIN_MAX_ATTEMPTS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MatrixHandle(String);

pub async fn start_listener(
    db: Database,
    homeserver: &str,
    username: &str,
    password: &str,
    db_path: &str,
    admins: Vec<MatrixHandle>,
) -> Result<()> {
    info!("Setting up client");
    let client_config = ClientConfig::new().store_path(db_path);
    let homeserver = Url::parse(homeserver)?;
    let client = Client::new_with_config(homeserver, client_config)?;

    info!("Login with credentials");
    client
        .login(username, password, None, Some("w3f-registrar-bot"))
        .await?;

    info!("Syncing client");
    client.sync_once(SyncSettings::default()).await?;

    client.set_event_handler(Box::new(Listener::new(
        client.clone(),
        db,
        admins,
    ))).await;

    // Start backend syncing service
    info!("Executing background sync");
    let settings = SyncSettings::default().token(
        client
            .sync_token()
            .await
            .ok_or_else(|| anyhow!("Failed to acquire sync token"))?,
    );

    actix::spawn(async move {
        client.clone().sync(settings).await;
    });

    Ok(())
}

struct Listener {
    client: Client,
    db: Database,
    admins: Vec<MatrixHandle>,
}

impl Listener {
    fn new(
        client: Client,
        db: Database,
        admins: Vec<MatrixHandle>,
    ) -> Self {
        Self { client, db, admins }
    }
}

#[async_trait]
impl EventHandler for Listener {
    async fn on_room_message(&self, room: Room, event: &SyncMessageEvent<MessageEventContent>) {
        info!("Received message from {}", event.sender);

        if let Room::Joined(room) = room {
            let msg_body = if let SyncMessageEvent {
                content:
                MessageEventContent {
                    msgtype: MessageType::Text(TextMessageEventContent { body: msg_body, .. }),
                    ..
                },
                ..
            } = event
            {
                msg_body
            } else {
                info!("Received unacceptable message type from {}", event.sender);
                return;
            };

            // Check for admin message
            let sender = event.sender.to_string();
            if self.admins.contains(&MatrixHandle(sender)) {
                let resp = match Command::from_str(msg_body) {
                    // If a valid admin command was found, execute it.
                    Ok(cmd) => Some(process_admin(&self.db, cmd).await),
                    Err(err @ Response::InvalidSyntax(_)) => Some(err),
                    // Ignore, allow noise (catches `UnknownCommand`).
                    Err(_) => None,
                };

                // If response should be sent, then do so.
                if let Some(resp) = resp {
                    if let Err(err) = room
                        .send(
                            AnyMessageEventContent::RoomMessage(MessageEventContent::text_plain(
                                resp.to_string(),
                            )),
                            None,
                        )
                        .await
                    {
                        error!("Failed to send message: {:?}", err);
                    }

                    return;
                }
            }

            let message = ExternalMessage {
                origin: ExternalMessageType::Matrix(event.sender.to_string()),
                // A message UID is not relevant regarding a live
                // message listener. The Matrix SDK handles
                // synchronization.
                id: 0u32.into(),
                timestamp: Timestamp::now(),
                values: vec![msg_body.to_string().into()],
            };

            info!("Received {:#?}", message);
            let _ = self.db
                .verify_message(&message)
                .await
                .map_err(|err| error!("Error when verifying message: {:?}", err));
        }
    }

    async fn on_stripped_state_member(
        &self,
        room: Room,
        _: &StrippedStateEvent<MemberEventContent>,
        _: Option<MemberEventContent>,
    ) {
        if let Room::Invited(room) = room {
            let mut rejoin_attempts = 0;

            while let Err(err) = self.client.join_room_by_id(room.room_id()).await {
                warn!(
                    "Failed to join room {} ({:?}), retrying in {}s",
                    room.room_id(),
                    err,
                    REJOIN_DELAY,
                );

                time::sleep(Duration::from_secs(REJOIN_DELAY)).await;

                if rejoin_attempts == REJOIN_MAX_ATTEMPTS {
                    error!("Can't join room {}, exiting ({:?})", room.room_id(), err);
                    return;
                }

                rejoin_attempts += 1;
            }

            debug!("Joined room {}", room.room_id());
        }
    }
}

//------------------------------------------------------------------------------

#[allow(clippy::needless_lifetimes)]
async fn process_admin<'a>(db: &'a Database, command: Command) -> Response {
    let local = |db: &'a Database, command: Command| async move {
        match command {
            Command::Status(addr) => {
                let context = create_context(addr);
                let state = db.fetch_judgement_state(&context).await?;

                // Determine response based on database lookup.
                match state {
                    Some(state) => Ok(Response::Status(state.into())),
                    None => Ok(Response::IdentityNotFound),
                }
            }
            Command::Verify(addr, fields) => {
                let context = create_context(addr.clone());

                // Check if _all_ should be verified (respectively the full identity)
                #[allow(clippy::collapsible_if)]
                if fields.iter().any(|f| matches!(f, RawFieldName::All)) {
                    if db.full_manual_verification(&context).await? {
                        return Ok(Response::FullyVerified(addr));
                    } else {
                        return Ok(Response::IdentityNotFound);
                    }
                }

                // Verify each passed on field.
                for field in &fields {
                    if db
                        .verify_manually(&context, field, true, None)
                        .await?
                        .is_none()
                    {
                        return Ok(Response::IdentityNotFound);
                    }
                }

                Ok(Response::Verified(addr, fields))
            }
            Command::Help => Ok(Response::Help),
        }
    };

    let res: crate::Result<Response> = local(db, command).await;
    match res {
        Ok(resp) => resp,
        Err(err) => {
            error!("Admin tool: {:?}", err);
            dbg!(err);
            Response::InternalError
        }
    }
}

/// Convenience function for creating a full identity context when only the
/// address itself is present. Only supports Kusama and Polkadot for now.
fn create_context(address: ChainAddress) -> IdentityContext {
    let chain = if address.as_str().starts_with('1') {
        ChainName::Polkadot
    } else {
        ChainName::Kusama
    };

    IdentityContext { address, chain }
}

//------------------------------------------------------------------------------

#[derive(Debug, Clone, Eq, PartialEq)]
enum Command {
    Status(ChainAddress),
    Verify(ChainAddress, Vec<RawFieldName>),
    Help,
}

impl FromStr for Command {
    type Err = Response;

    fn from_str(s: &str) -> base::Result<Self> {
        // Convenience handler.
        let s = s.trim().replace("  ", " ");

        if s.starts_with("status") {
            let parts: Vec<&str> = s.split(' ').skip(1).collect();
            if parts.len() != 1 {
                return Err(Response::UnknownCommand);
            }

            Ok(Command::Status(ChainAddress::from(parts[0].to_string())))
        } else if s.starts_with("verify") {
            let parts: Vec<&str> = s.split(' ').skip(1).collect();
            if parts.len() < 2 {
                return Err(Response::UnknownCommand);
            }

            Ok(Command::Verify(
                ChainAddress::from(parts[0].to_string()),
                parts[1..]
                    .iter()
                    .map(|s| RawFieldName::from_str(s))
                    .collect::<base::Result<Vec<RawFieldName>>>()?,
            ))
        } else if s.starts_with("help") {
            let count = s.split(' ').count();

            if count > 1 {
                return Err(Response::UnknownCommand);
            }

            Ok(Command::Help)
        } else {
            Err(Response::UnknownCommand)
        }
    }
}

