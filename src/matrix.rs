#![allow(dead_code)]

use crate::{Database, Result};
use crate::watcher::{ChainAddress, ChainName, IdentityContext, RawFieldName, Response};

use matrix_sdk::room::Room;
use matrix_sdk::{Client};
use matrix_sdk::ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent};
use matrix_sdk::config::SyncSettings;

use std::str::FromStr;
use matrix_sdk::event_handler::Ctx;
use url::Url;

const REJOIN_DELAY: u64 = 10;
const REJOIN_MAX_ATTEMPTS: usize = 5;

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
    let client = Client::new(Url::parse(cfg.homeserver)?).await?;

    info!("Logging in as {}", cfg.username);
    let res = client
        .matrix_auth()
        .login_username(cfg.username, cfg.password)
        .await?;

    info!(
        "Logged in with device ID {} and access token {}",
        res.device_id, res.access_token
    );

    // Perform an initial sync to set up state.
    let response = client.sync_once(SyncSettings::default()).await.unwrap();
    // Add an event handler to be notified of incoming messages.
    // We do this after the initial sync to avoid responding to messages before
    // the bot was running.
    client.add_event_handler(on_room_message);
    client.add_event_handler_context(BotContext { db });

    // Since we called `sync_once` before we entered our sync loop we must pass
    // that sync token to `sync`.
    info!("Entering sync loop");
    let settings = SyncSettings::default().token(response.next_batch);
    client.sync(settings).await?;

    Ok(())
}

async fn on_room_message(e: OriginalSyncRoomMessageEvent, room: Room, ctx: Ctx<BotContext>) {
    info!("Received {:#?}", e);

    let MessageType::Text(text) = e.content.msgtype else {
        return;
    };

    if let Ok(cmd) = Command::from_str(&text.body) {
        info!("Executing command {:#?}", cmd);

        let res = execute_command(&ctx.db, cmd).await;
        room
            .send(RoomMessageEventContent::text_plain(res.to_string()))
            .await
            .unwrap();
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
