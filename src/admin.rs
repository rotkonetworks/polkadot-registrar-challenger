use crate::base::{ChainAddress, ChainName, IdentityContext, RawFieldName, Response};
use crate::{base, Database};
use std::str::FromStr;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Command {
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

#[allow(clippy::needless_lifetimes)]
pub async fn process_admin<'a>(db: &'a Database, command: Command) -> Response {
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
pub fn create_context(address: ChainAddress) -> IdentityContext {
    let chain = if address.as_str().starts_with('1') {
        ChainName::Polkadot
    } else {
        ChainName::Kusama
    };

    IdentityContext { address, chain }
}
