use crate::Result;
use crate::registrar::{ChainAddress, ChainName, Database, ExternalMessage, ExternalMessageType, IdentityContext, RawFieldName, Response, Timestamp};

use matrix_sdk::ruma::events::room::message::MessageType;
use matrix_sdk::ruma::events::room::message::OriginalSyncRoomMessageEvent;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::Client;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::room::Room;
use matrix_sdk::event_handler::Ctx;
use matrix_sdk::ruma::events::AnySyncMessageLikeEvent;
use matrix_sdk::encryption::{BackupDownloadStrategy, EncryptionSettings};
use matrix_sdk::matrix_auth::MatrixSession;

use std::path::Path;
use std::str::FromStr;

const STATE_DIR: &str = "/tmp/matrix";

#[derive(Debug, Clone, PartialEq)]
pub struct BotConfig<'a> {
    pub homeserver: &'a str,
    pub username: &'a str,
    pub password: &'a str,
    pub admins: Vec<Nickname>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Nickname(String);

#[derive(Debug, Clone)]
struct BotContext {
    db: Database,
}

pub async fn start_bot<'a>(db: Database, cfg: BotConfig<'a>) -> Result<()> {
    let state_dir = Path::new(STATE_DIR);
    let session_path = state_dir.join("session.json");

    info!("Creating client");
    let client = Client::builder()
        .homeserver_url(cfg.homeserver)
        .sqlite_store(STATE_DIR, None)
        .with_encryption_settings(EncryptionSettings {
            auto_enable_cross_signing: true,
            auto_enable_backups: true,
            backup_download_strategy: BackupDownloadStrategy::AfterDecryptionFailure,
        })
        .build().await.unwrap();

    if session_path.exists() {
        info!("Restoring session in {}", session_path.display());
        let session = tokio::fs::read_to_string(session_path).await?;
        let session: MatrixSession = serde_json::from_str(&session)?;
        client.restore_session(session).await?;
    } else {
        info!("Logging in as {}", cfg.username);
        client
            .matrix_auth()
            .login_username(cfg.username, cfg.password)
            .initial_device_display_name("w3-reg-bot")
            .await?;

        info!("Writing session to {}", session_path.display());
        let session = client.matrix_auth().session().expect("Session missing");
        let session = serde_json::to_string(&session)?;
        tokio::fs::write(session_path, session).await?;
    }

    if let Some(device_id) = client.device_id() {
        info!("Logged in with device ID {}", device_id);
    }

    // Perform an initial sync to set up state.
    info!("Performing initial sync");
    let response = client.sync_once(SyncSettings::default()).await.unwrap();
    // Add event handlers to be notified of incoming messages.
    // We do this after the initial sync to avoid responding to messages before
    // the bot was running.
    client.add_event_handler(on_any_message_like_event);
    client.add_event_handler(on_room_message);
    client.add_event_handler_context(BotContext { db });

    // Since we called `sync_once` before we entered our sync loop we must pass
    // that sync token to `sync`.
    info!("Listening for messages...");
    let settings = SyncSettings::default().token(response.next_batch);
    actix::spawn(async move {
        client.sync(settings).await.unwrap();
    });

    Ok(())
}

async fn on_any_message_like_event(e: AnySyncMessageLikeEvent) {
    info!("Received {:#?}", e);
}

async fn on_room_message(e: OriginalSyncRoomMessageEvent, room: Room, ctx: Ctx<BotContext>) {
    let MessageType::Text(text) = e.content.msgtype else {
        return;
    };

    info!("Received message from {}:\n\t\n\t{}\n", e.sender, text.body);

    if let Ok(cmd) = Command::from_str(&text.body) {
        info!("Executing command {:#?}", cmd);

        let res = execute_command(&ctx.db, cmd).await;
        room
            .send(RoomMessageEventContent::text_plain(res.to_string()))
            .await
            .unwrap();
   } else {
        info!("Verifying message");

        let msg = ExternalMessage {
            origin: ExternalMessageType::Matrix(e.sender.to_string()),
            // A message UID is not relevant regarding a live
            // message listener. The Matrix SDK handles
            // synchronization.
            id: 0u32.into(),
            timestamp: Timestamp::now(),
            values: vec![text.body.into()],
        };

        match ctx.db.verify_message(&msg).await {
            Err(e) => {
                info!("Message verification failed: {:}", e);
            }
            _ => {}
        }
    }
}

async fn execute_command<'a>(db: &'a Database, command: Command) -> Response {
    let local = |db: &'a Database, command: Command| async move {
        match command {
            Command::Status(addr) => {
                let context = create_identity_context(addr);
                let state = db.fetch_judgement_state(&context).await?;

                // Determine response based on database lookup.
                match state {
                    Some(state) => Ok(Response::Status(state.into())),
                    None => Ok(Response::IdentityNotFound),
                }
            }
            Command::Verify(addr, fields) => {
                let context = create_identity_context(addr.clone());

                // Check if _all_ should be verified (respectively the full identity)
                if fields.iter().any(|f| matches!(f, RawFieldName::All)) {
                    return if db.full_manual_verification(&context).await? {
                        Ok(Response::FullyVerified(addr))
                    } else {
                        Ok(Response::IdentityNotFound)
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
fn create_identity_context(address: ChainAddress) -> IdentityContext {
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
    type Err = &'static str;

    // TODO: Refactor parsing.
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let s = s.trim().replace("  ", " ");

        if s.starts_with("status") {
            let parts: Vec<&str> = s.split(' ').skip(1).collect();
            if parts.len() != 1 {
                return Err("Invalid `status` command");
            }

            Ok(Command::Status(ChainAddress::from(parts[0].to_string())))
        } else if s.starts_with("verify") {
            let parts: Vec<&str> = s.split(' ').skip(1).collect();
            if parts.len() < 2 {
                return Err("Invalid `verify` command");
            }

            Ok(Command::Verify(
                ChainAddress::from(parts[0].to_string()),
                parts[1..]
                    .iter()
                    .map(|s| RawFieldName::from_str(s))
                    .collect::<std::result::Result<Vec<RawFieldName>, &'static str>>()?,
            ))
        } else if s.starts_with("help") {
            let count = s.split(' ').count();

            if count > 1 {
                return Err("Invalid `help` command");
            }

            Ok(Command::Help)
        } else {
            Err("Unknown command")
        }
    }
}
