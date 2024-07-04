#![allow(dead_code)]

use crate::display_name::DisplayNameVerifier;
use crate::{Database, DisplayNameConfig, Result, WatcherConfig};
use actix::io::SinkWrite;
use actix::io::WriteHandler;
use actix::prelude::*;
use actix_codec::Framed;
use awc::{
    BoxedSocket,
    Client,
    error::WsProtocolError, ws::{Codec, Frame, Message},
};
use futures::stream::{SplitSink, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::Instrument;
use std::str::FromStr;

// In seconds
const HEARTBEAT_INTERVAL: u64 = 60;
const RECONNECTION_TIMEOUT: u64 = 10;

#[cfg(not(test))]
const PENDING_JUDGEMENTS_INTERVAL: u64 = 120;
#[cfg(not(test))]
const DISPLAY_NAMES_INTERVAL: u64 = 60;
#[cfg(not(test))]
const JUDGEMENT_CANDIDATES_INTERVAL: u64 = 10;

#[cfg(test)]
const PENDING_JUDGEMENTS_INTERVAL: u64 = 1;
#[cfg(test)]
const DISPLAY_NAMES_INTERVAL: u64 = 1;
#[cfg(test)]
const JUDGEMENT_CANDIDATES_INTERVAL: u64 = 1;

pub async fn run_connector(
    db: Database,
    watchers: Vec<WatcherConfig>,
    dn_config: DisplayNameConfig,
) -> Result<()> {
    if watchers.is_empty() {
        warn!("No watcher is configured. Cannot process any requests or issue judgments");
        return Ok(());
    }

    for config in watchers {
        let span = info_span!("connector_initialization");
        span.in_scope(|| {
            debug!(
                network = config.network.as_str(),
                endpoint = config.endpoint.as_str()
            );
        });

        async {
            // Start Connector.
            let dn_verifier = DisplayNameVerifier::new(db.clone(), dn_config.clone());
            let conn =
                Connector::start(config.endpoint, config.network, db.clone(), dn_verifier).await?;

            info!("Connection initiated");
            info!("Sending pending judgements request to Watcher");
            let _ = conn.send(ClientCommand::RequestPendingJudgements).await?;

            Result::Ok(())
        }
        .instrument(span)
        .await?;
    }

    Ok(())
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResponseMessage<T> {
    pub event: EventType,
    pub data: T,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum EventType {
    #[serde(rename = "ack")]
    Ack,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "newJudgementRequest")]
    NewJudgementRequest,
    #[serde(rename = "judgementResult")]
    JudgementResult,
    #[serde(rename = "pendingJudgementsRequest")]
    PendingJudgementsRequest,
    #[serde(rename = "pendingJudgementsResponse")]
    PendingJudgementsResponse,
    #[serde(rename = "displayNamesRequest")]
    DisplayNamesRequest,
    #[serde(rename = "displayNamesResponse")]
    DisplayNamesResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgementResponse {
    pub address: ChainAddress,
    pub judgement: Judgement,
    pub verified: Vec<VerifiedEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedEntry {
    #[serde(rename = "accountTy")]
    pub account_ty: AccountType,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AckResponse {
    result: String,
    address: Option<ChainAddress>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum Judgement {
    #[serde(rename = "reasonable")]
    Reasonable,
    #[serde(rename = "erroneous")]
    Erroneous,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgementRequest {
    pub address: ChainAddress,
    pub accounts: HashMap<AccountType, String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct DisplayNameEntry {
    pub context: IdentityContext,
    pub display_name: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
/// The entry as sent by the Watcher. Then converted into `DisplayNameEntry`.
pub struct DisplayNameEntryRaw {
    pub address: ChainAddress,
    #[serde(alias = "displayName")]
    pub display_name: String,
}

impl DisplayNameEntryRaw {
    /// Display names with emojis are represented in HEX form. Decode the
    /// display name, assuming it can be decoded...
    pub fn try_decode_hex(&mut self) {
        try_decode_hex(&mut self.display_name);
    }
}

fn try_decode_hex(display_name: &mut String) {
    if display_name.starts_with("0x") {
        // Might be a false positive. Leave it as is if it cannot be decoded.
        if let Ok(name) = hex::decode(&display_name[2..]) {
            if let Ok(name) = String::from_utf8(name) {
                *display_name = name;
            }
        }
    }
}

#[derive(Debug, Clone, Message)]
#[rtype(result = "crate::Result<()>")]
pub enum WatcherMessage {
    Ack(AckResponse),
    NewJudgementRequest(JudgementRequest),
    PendingJudgementsRequests(Vec<JudgementRequest>),
    ActiveDisplayNames(Vec<DisplayNameEntryRaw>),
}

#[derive(Debug, Clone, Message)]
#[rtype(result = "crate::Result<()>")]
pub enum ClientCommand {
    ProvideJudgement(JudgementState),
    RequestPendingJudgements,
    RequestDisplayNames,
}

/// Handles incoming and outgoing websocket messages to and from the Watcher.
struct Connector {
    #[allow(clippy::type_complexity)]
    sink: Option<SinkWrite<Message, SplitSink<Framed<BoxedSocket, Codec>, Message>>>,
    db: Database,
    dn_verifier: DisplayNameVerifier,
    endpoint: String,
    network: ChainName,
    outgoing: UnboundedSender<ClientCommand>,
    inserted_states: Arc<RwLock<Vec<JudgementState>>>,
    // Tracks the last message received from the Watcher. If a certain treshold
    // was exceeded, the Connector attempts to reconnect.
    last_watcher_msg: Timestamp,
}

impl Connector {
    async fn start(
        endpoint: String,
        network: ChainName,
        db: Database,
        dn_verifier: DisplayNameVerifier,
    ) -> Result<Addr<Connector>> {
        let (_, framed) = Client::new()
            .ws(&endpoint)
            .max_frame_size(5_000_000)
            .connect()
            .await
            .map_err(|err| {
                anyhow!(
                    "failed to initiate client connector to {}: {:?}",
                    endpoint,
                    err
                )
            })?;

        // Create throw-away channels (`outgoing` in `Connector` is only used in tests.)
        let (outgoing, _recv) = mpsc::unbounded_channel();

        // Start the Connector actor with the attached websocket stream.
        let (sink, stream) = framed.split();
        let actor = Connector::create(|ctx| {
            Connector::add_stream(stream, ctx);
            Connector {
                sink: Some(SinkWrite::new(sink, ctx)),
                db,
                dn_verifier,
                endpoint,
                network,
                outgoing,
                inserted_states: Default::default(),
                last_watcher_msg: Timestamp::now(),
            }
        });

        Ok(actor)
    }

    // Request pending judgements every couple of seconds.
    fn start_pending_judgements_task(&self, ctx: &mut Context<Self>) {
        info!("Starting pending judgement requester background task");

        ctx.run_interval(
            Duration::new(PENDING_JUDGEMENTS_INTERVAL, 0),
            |_act, ctx| {
                ctx.address()
                    .do_send(ClientCommand::RequestPendingJudgements)
            },
        );
    }

    // Request actively used display names every couple of seconds.
    fn start_active_display_names_task(&self, ctx: &mut Context<Self>) {
        info!("Starting display name requester background task");

        ctx.run_interval(Duration::new(DISPLAY_NAMES_INTERVAL, 0), |_act, ctx| {
            ctx.address().do_send(ClientCommand::RequestDisplayNames)
        });
    }

    // Look for verified identities and submit those to the Watcher.
    fn start_judgement_candidates_task(&self, ctx: &mut Context<Self>) {
        info!("Starting judgement candidate submitter background task");

        let db = self.db.clone();
        let addr = ctx.address();
        let network = self.network;

        ctx.run_interval(
            Duration::new(JUDGEMENT_CANDIDATES_INTERVAL, 0),
            move |_act, _ctx| {
                let db = db.clone();
                let addr = addr.clone();

                actix::spawn(async move {
                    // Provide judgments for the specific network.
                    match db.fetch_judgement_candidates(network).await {
                        Ok(completed) => {
                            for state in completed {
                                info!("Notifying Watcher about judgement: {:?}", state.context);
                                addr.do_send(ClientCommand::ProvideJudgement(state));
                            }
                        }
                        Err(err) => {
                            error!("Failed to fetch judgement candidates: {:?}", err);
                        }
                    }
                });
            },
        );
    }
}

impl Actor for Connector {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Context<Self>) {
        let span = info_span!("connector_background_tasks");

        span.in_scope(|| {
            debug!(
                network = self.network.as_str(),
                endpoint = self.endpoint.as_str()
            );

            self.start_pending_judgements_task(ctx);
            self.start_active_display_names_task(ctx);
            self.start_judgement_candidates_task(ctx);
        });
    }

    fn stopped(&mut self, _ctx: &mut Context<Self>) {
        let span = warn_span!("watcher_connection_drop");
        span.in_scope(|| {
            debug!(
                network = self.network.as_str(),
                endpoint = self.endpoint.as_str()
            );
        });

        let endpoint = self.endpoint.clone();
        let network = self.network;
        let db = self.db.clone();
        let dn_verifier = self.dn_verifier.clone();

        actix::spawn(
            async move {
                warn!("Watcher disconnected, trying to reconnect...");

                let mut counter = 0;
                loop {
                    if Connector::start(endpoint.clone(), network, db.clone(), dn_verifier.clone())
                        .await
                        .is_err()
                    {
                        warn!("Reconnection failed, retrying...");

                        counter += 1;
                        if counter >= 10 {
                            error!("Cannot reconnect to Watcher after {} attempts", counter);
                        }

                        sleep(Duration::from_secs(RECONNECTION_TIMEOUT)).await;
                    } else {
                        info!("Reconnected to Watcher!");
                        break;
                    }
                }
            }
            .instrument(span),
        );
    }
}

impl WriteHandler<WsProtocolError> for Connector {}

// Handle messages that should be sent to the Watcher.
impl Handler<ClientCommand> for Connector {
    type Result = crate::Result<()>;

    fn handle(&mut self, msg: ClientCommand, ctx: &mut Context<Self>) -> Self::Result {
        let span = debug_span!("handling_client_message");

        // NOTE: make sure no async code comes after this.
        let _guard = span.enter();
        debug!(
            network = self.network.as_str(),
            endpoint = self.endpoint.as_str()
        );

        // If the sink (outgoing WS stream) is not configured (i.e. when
        // testing), send the client command to the channel.
        if self.sink.is_none() {
            warn!("Skipping message to Watcher, not configured (only occurs when testing)");
            self.outgoing.send(msg).unwrap();
            return Ok(());
        }

        let sink = self.sink.as_mut().unwrap();

        // Do a connection check and reconnect if necessary.
        if sink.closed() {
            ctx.stop();
            return Ok(());
        }

        // Do a timestamp check and reconnect if necessary.
        if Timestamp::now().raw() - self.last_watcher_msg.raw() > (HEARTBEAT_INTERVAL * 2) {
            warn!("Last received message from the Watcher was a while ago, resetting connection");
            ctx.stop();
            return Ok(());
        }

        match msg {
            ClientCommand::ProvideJudgement(state) => {
                debug!(
                    "Providing judgement over websocket stream: {:?}",
                    state.context
                );
                let verified = state.as_verified_entries();

                sink.write(Message::Text(
                    serde_json::to_string(&ResponseMessage {
                        event: EventType::JudgementResult,
                        data: JudgementResponse {
                            address: state.context.address,
                            judgement: Judgement::Reasonable,
                            verified,
                        },
                    })
                    .unwrap()
                    .into(),
                ))
                .map_err(|err| anyhow!("failed to provide judgement: {:?}", err))?;
            }
            ClientCommand::RequestPendingJudgements => {
                debug!("Requesting pending judgements over websocket stream");

                sink.write(Message::Text(
                    serde_json::to_string(&ResponseMessage {
                        event: EventType::PendingJudgementsRequest,
                        data: (),
                    })
                    .unwrap()
                    .into(),
                ))
                .map_err(|err| anyhow!("failed to request pending judgements: {:?}", err))?;
            }
            ClientCommand::RequestDisplayNames => {
                debug!("Requesting display names over websocket stream");

                sink.write(Message::Text(
                    serde_json::to_string(&ResponseMessage {
                        event: EventType::DisplayNamesRequest,
                        data: (),
                    })
                    .unwrap()
                    .into(),
                ))
                .map_err(|err| anyhow!("failed to request display names: {:?}", err))?;
            }
        }

        Ok(())
    }
}

// Handle messages that were received from the Watcher.
impl Handler<WatcherMessage> for Connector {
    type Result = ResponseActFuture<Self, crate::Result<()>>;

    fn handle(&mut self, msg: WatcherMessage, _ctx: &mut Context<Self>) -> Self::Result {
        /// Handle a judgement request.
        async fn process_request(
            db: &Database,
            id: IdentityContext,
            mut accounts: HashMap<AccountType, String>,
            dn_verifier: &DisplayNameVerifier,
            // Only used in testing.
            inserted_states: &Arc<RwLock<Vec<JudgementState>>>,
        ) -> Result<()> {
            // Decode display name if appropriate.
            if let Some((_, val)) = accounts
                .iter_mut()
                .find(|(ty, _)| *ty == &AccountType::DisplayName)
            {
                try_decode_hex(val);
            }

            // If the fields of the request are the same as the current state, return.
            if let Some(current_state) = db.fetch_judgement_state(&id).await? {
                if current_state.has_same_fields_as(&accounts) {
                    return Ok(());
                }
            }

            // Create judgement state and prepare to insert into database.
            let state = JudgementState::new(id, accounts.into_iter().map(|a| a.into()).collect());

            // Add the judgement state that's about to get inserted into the
            // local queue which is then fetched from the unit tests.
            #[cfg(not(test))]
            let _ = inserted_states;
            #[cfg(test)]
            {
                let mut l = inserted_states.write().await;
                (*l).push(state.clone());
            }

            // Insert identity into the database and verify display name if the
            // database entry was modified (or newly inserted).
            if db.add_judgement_request(&state).await? {
                dn_verifier.verify_display_name(&state).await?;
            }

            Ok(())
        }

        // Update timestamp
        self.last_watcher_msg = Timestamp::now();

        let network = self.network;
        let db = self.db.clone();
        let dn_verifier = self.dn_verifier.clone();
        let inserted_states = Arc::clone(&self.inserted_states);

        Box::pin(
            async move {
                match msg {
                    WatcherMessage::Ack(data) => {
                        if data.result.to_lowercase().contains("judgement given") {
                            // Create identity context.
                            let address =
                                data.address
                                .ok_or_else(|| {
                                anyhow!(
                                    "no address specified in 'judgement given' response from Watcher"
                                )
                            })?;

                            let context = IdentityContext::new(address, network);

                            info!("Marking {:?} as judged", context);
                            db.set_judged(&context).await?;
                        }
                    }
                    WatcherMessage::NewJudgementRequest(data) => {
                        let id = IdentityContext::new(data.address, network);
                        process_request(&db, id, data.accounts, &dn_verifier, &inserted_states).await?;
                    }
                    WatcherMessage::PendingJudgementsRequests(data) => {
                        // Convert data.
                        let data: Vec<(IdentityContext, HashMap<AccountType, String>)> = data
                            .into_iter()
                            .map(|req| (
                                IdentityContext::new(req.address, network),
                                req.accounts
                            ))
                            .collect();

                        for (context, accounts) in data {
                            process_request(&db, context, accounts, &dn_verifier, &inserted_states).await?;
                        }
                    }
                    WatcherMessage::ActiveDisplayNames(data) => {
                        for mut name in data {
                            name.try_decode_hex();

                            let context = IdentityContext::new(name.address, network);
                            let entry = DisplayNameEntry {
                                context,
                                display_name: name.display_name,
                            };

                            db.insert_display_name(&entry).await?;
                        }
                    }
                }

                Ok(())
            }.into_actor(self)
        )
    }
}

/// Handle websocket messages received from the Watcher. Those messages will be
/// forwarded to the `Handler<WatcherMessage>` implementation.
impl StreamHandler<std::result::Result<Frame, WsProtocolError>> for Connector {
    fn handle(
        &mut self,
        msg: std::result::Result<Frame, WsProtocolError>,
        ctx: &mut Context<Self>,
    ) {
        async fn local(
            conn: Addr<Connector>,
            msg: std::result::Result<Frame, WsProtocolError>,
        ) -> Result<()> {
            let parsed: ResponseMessage<serde_json::Value> = match msg {
                Ok(Frame::Text(txt)) => serde_json::from_slice(&txt)?,
                Ok(other) => {
                    debug!("Received unexpected message: {:?}", other);
                    return Ok(());
                }
                Err(err) => return Err(anyhow!("error message: {:?}", err)),
            };

            match parsed.event {
                EventType::Ack => {
                    debug!("Received acknowledgement from Watcher: {:?}", parsed.data);

                    let data: AckResponse = serde_json::from_value(parsed.data)?;
                    conn.send(WatcherMessage::Ack(data)).await??;
                }
                EventType::Error => {
                    error!("Received error from Watcher: {:?}", parsed.data);
                }
                EventType::NewJudgementRequest => {
                    info!(
                        "Received new judgement request from Watcher: {:?}",
                        parsed.data
                    );

                    let data: JudgementRequest = serde_json::from_value(parsed.data)?;
                    conn.send(WatcherMessage::NewJudgementRequest(data))
                        .await??;
                }
                EventType::PendingJudgementsResponse => {
                    let data: Vec<JudgementRequest> = serde_json::from_value(parsed.data)?;
                    debug!("Received {} pending judgments from Watcher", data.len());
                    conn.send(WatcherMessage::PendingJudgementsRequests(data))
                        .await??;
                }
                EventType::DisplayNamesResponse => {
                    let data: Vec<DisplayNameEntryRaw> = serde_json::from_value(parsed.data)?;
                    debug!("Received {} display names from the Watcher", data.len());
                    conn.send(WatcherMessage::ActiveDisplayNames(data))
                        .await??;
                }
                _ => {
                    warn!("Received unrecognized message from Watcher: {:?}", parsed);
                }
            }

            Ok(())
        }

        let span = debug_span!("handling_websocket_message");
        span.in_scope(|| {
            debug!(
                network = self.network.as_str(),
                endpoint = self.endpoint.as_str()
            );

            let addr = ctx.address();
            actix::spawn(
                async move {
                    if let Err(err) = local(addr, msg).await {
                        error!("Failed to process message in websocket stream: {:?}", err);
                    }
                }
                .in_current_span(),
            );
        });
    }
}

#[derive(Eq, PartialEq, Hash, Clone, Debug, Serialize, Deserialize)]
pub enum AccountType {
    #[serde(rename = "legal_name")]
    LegalName,
    #[serde(rename = "display_name")]
    DisplayName,
    #[serde(rename = "email")]
    Email,
    #[serde(rename = "web")]
    Web,
    #[serde(rename = "twitter")]
    Twitter,
    #[serde(rename = "matrix")]
    Matrix,
    #[serde(rename = "pgpFingerprint")]
    PGPFingerprint,
    #[serde(rename = "image")]
    Image,
    #[serde(rename = "additional")]
    Additional,
}

impl From<(AccountType, String)> for IdentityFieldValue {
    fn from(val: (AccountType, String)) -> Self {
        let (ty, value) = val;

        match ty {
            AccountType::LegalName => IdentityFieldValue::LegalName(value),
            AccountType::DisplayName => IdentityFieldValue::DisplayName(value),
            AccountType::Email => IdentityFieldValue::Email(value),
            AccountType::Web => IdentityFieldValue::Web(value),
            AccountType::Twitter => IdentityFieldValue::Twitter(value.to_lowercase()),
            AccountType::Matrix => IdentityFieldValue::Matrix(value),
            AccountType::PGPFingerprint => IdentityFieldValue::PGPFingerprint(()),
            AccountType::Image => IdentityFieldValue::Image(()),
            AccountType::Additional => IdentityFieldValue::Additional(()),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct IdentityContext {
    pub address: ChainAddress,
    pub chain: ChainName,
}

impl IdentityContext {
    pub fn new(addr: ChainAddress, network: ChainName) -> Self {
        IdentityContext {
            address: addr,
            chain: network,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ChainAddress(String);

impl ChainAddress {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl From<String> for ChainAddress {
    fn from(v: String) -> Self {
        ChainAddress(v)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainName {
    Polkadot,
    Kusama,
}

impl ChainName {
    pub fn as_str(&self) -> &str {
        match self {
            ChainName::Polkadot => "polkadot",
            ChainName::Kusama => "kusama",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct IdentityField {
    pub value: IdentityFieldValue,
    pub challenge: ChallengeType,
    pub failed_attempts: usize,
}

impl IdentityField {
    pub fn new(val: IdentityFieldValue) -> Self {
        use IdentityFieldValue::*;

        let challenge = {
            match val {
                LegalName(_) => ChallengeType::Unsupported { is_verified: None },
                Web(_) => ChallengeType::Unsupported { is_verified: None },
                PGPFingerprint(_) => ChallengeType::Unsupported { is_verified: None },
                Image(_) => ChallengeType::Unsupported { is_verified: None },
                Additional(_) => ChallengeType::Unsupported { is_verified: None },
                DisplayName(_) => ChallengeType::DisplayNameCheck {
                    passed: false,
                    violations: vec![],
                },
                Email(_) => ChallengeType::ExpectedMessage {
                    expected: ExpectedMessage::random(),
                    second: Some(ExpectedMessage::random()),
                },
                Twitter(_) => ChallengeType::ExpectedMessage {
                    expected: ExpectedMessage::random(),
                    second: None,
                },
                Matrix(_) => ChallengeType::ExpectedMessage {
                    expected: ExpectedMessage::random(),
                    second: None,
                },
            }
        };

        IdentityField {
            value: val,
            challenge,
            failed_attempts: 0,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "content")]
pub enum ChallengeType {
    ExpectedMessage {
        expected: ExpectedMessage,
        second: Option<ExpectedMessage>,
    },
    DisplayNameCheck {
        passed: bool,
        violations: Vec<DisplayNameEntry>,
    },
    Unsupported {
        // For manual judgements via the admin interface.
        is_verified: Option<bool>,
    },
}

impl ChallengeType {
    pub fn is_verified(&self) -> bool {
        match self {
            ChallengeType::ExpectedMessage { expected, second } => {
                if let Some(second) = second {
                    expected.is_verified && second.is_verified
                } else {
                    expected.is_verified
                }
            }
            ChallengeType::DisplayNameCheck {
                passed,
                violations: _,
            } => *passed,
            ChallengeType::Unsupported { is_verified } => is_verified.unwrap_or(false),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ExpectedMessage {
    pub value: String,
    pub is_verified: bool,
}

impl ExpectedMessage {
    pub fn random() -> Self {
        use rand::{thread_rng, Rng};

        let random: [u8; 16] = thread_rng().gen();
        ExpectedMessage {
            value: hex::encode(random),
            is_verified: false,
        }
    }

    pub fn is_message_valid(&self, message: &ExternalMessage) -> bool {
        for value in &message.values {
            if value.0.contains(&self.value) {
                return true;
            }
        }

        false
    }

    #[cfg(test)]
    pub fn set_verified(&mut self) {
        self.is_verified = true;
    }

    #[cfg(test)]
    pub fn to_message_parts(&self) -> Vec<MessagePart> {
        vec![self.value.clone().into()]
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum IdentityFieldValue {
    LegalName(String),
    DisplayName(String),
    Email(String),
    Web(String),
    Twitter(String),
    Matrix(String),
    PGPFingerprint(()),
    Image(()),
    Additional(()),
}

impl IdentityFieldValue {
    pub fn as_account_type(&self) -> (AccountType, String) {
        match self {
            IdentityFieldValue::LegalName(val) => (AccountType::LegalName, val.to_string()),
            IdentityFieldValue::DisplayName(val) => (AccountType::DisplayName, val.to_string()),
            IdentityFieldValue::Email(val) => (AccountType::Email, val.to_string()),
            IdentityFieldValue::Web(val) => (AccountType::Web, val.to_string()),
            IdentityFieldValue::Twitter(val) => (AccountType::Twitter, val.to_string()),
            IdentityFieldValue::Matrix(val) => (AccountType::Matrix, val.to_string()),
            IdentityFieldValue::PGPFingerprint(_) => (AccountType::PGPFingerprint, String::new()),
            IdentityFieldValue::Image(_) => (AccountType::Image, String::new()),
            IdentityFieldValue::Additional(_) => (AccountType::Additional, String::new()),
        }
    }

    pub fn matches_type(&self, ty: &AccountType, value: &str) -> bool {
        match (self, ty) {
            (IdentityFieldValue::LegalName(val), AccountType::LegalName) => val == value,
            (IdentityFieldValue::DisplayName(val), AccountType::DisplayName) => val == value,
            (IdentityFieldValue::Email(val), AccountType::Email) => val == value,
            (IdentityFieldValue::Web(val), AccountType::Web) => val == value,
            (IdentityFieldValue::Twitter(val), AccountType::Twitter) => val == value,
            (IdentityFieldValue::Matrix(val), AccountType::Matrix) => val == value,
            (IdentityFieldValue::PGPFingerprint(_), AccountType::PGPFingerprint) => true,
            (IdentityFieldValue::Image(_), AccountType::Image) => true,
            (IdentityFieldValue::Additional(_), AccountType::Additional) => true,
            _ => false,
        }
    }

    pub fn matches_origin(&self, message: &ExternalMessage) -> bool {
        match self {
            IdentityFieldValue::Email(n1) => match &message.origin {
                ExternalMessageType::Email(n2) => n1 == n2,
                _ => false,
            },
            IdentityFieldValue::Twitter(n1) => match &message.origin {
                ExternalMessageType::Twitter(n2) => n1 == n2,
                _ => false,
            },
            IdentityFieldValue::Matrix(n1) => match &message.origin {
                ExternalMessageType::Matrix(n2) => n1 == n2,
                _ => false,
            },
            _ => false,
        }
    }
}

// The blanked judgement state sent to the frontend UI. Does not include the
// secondary challenge. NOTE: `JudgementState` could be converted to take a
// generic and `JudgementStateBlanked` could just be a type alias.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct JudgementStateBlanked {
    pub context: IdentityContext,
    pub is_fully_verified: bool,
    pub inserted_timestamp: Timestamp,
    pub completion_timestamp: Option<Timestamp>,
    pub judgement_submitted: bool,
    pub fields: Vec<IdentityFieldBlanked>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct IdentityFieldBlanked {
    pub value: IdentityFieldValue,
    pub challenge: ChallengeTypeBlanked,
    failed_attempts: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "content")]
pub enum ChallengeTypeBlanked {
    ExpectedMessage {
        expected: ExpectedMessage,
        second: Option<ExpectedMessageBlanked>,
    },
    DisplayNameCheck {
        passed: bool,
        violations: Vec<DisplayNameEntry>,
    },
    Unsupported {
        // For manual judgements via the admin interface.
        is_verified: Option<bool>,
    },
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExpectedMessageBlanked {
    // IMPORTANT: This value is blanked.
    // pub value: String,
    pub is_verified: bool,
}

impl From<JudgementState> for JudgementStateBlanked {
    fn from(s: JudgementState) -> Self {
        JudgementStateBlanked {
            context: s.context,
            is_fully_verified: s.is_fully_verified,
            inserted_timestamp: s.inserted_timestamp,
            completion_timestamp: s.completion_timestamp,
            judgement_submitted: s.judgement_submitted,
            fields: s
                .fields
                .into_iter()
                .map(|f| IdentityFieldBlanked {
                    value: f.value,
                    challenge: {
                        match f.challenge {
                            ChallengeType::ExpectedMessage { expected, second } => {
                                ChallengeTypeBlanked::ExpectedMessage {
                                    expected,
                                    second: second.map(|s| ExpectedMessageBlanked {
                                        is_verified: s.is_verified,
                                    }),
                                }
                            }
                            ChallengeType::DisplayNameCheck { passed, violations } => {
                                ChallengeTypeBlanked::DisplayNameCheck { passed, violations }
                            }
                            ChallengeType::Unsupported { is_verified } => {
                                ChallengeTypeBlanked::Unsupported { is_verified }
                            }
                        }
                    },
                    failed_attempts: f.failed_attempts,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct JudgementState {
    pub context: IdentityContext,
    pub is_fully_verified: bool,
    pub inserted_timestamp: Timestamp,
    pub completion_timestamp: Option<Timestamp>,
    pub judgement_submitted: bool,
    pub issue_judgement_at: Option<Timestamp>,
    pub fields: Vec<IdentityField>,
}

impl JudgementState {
    pub fn new(context: IdentityContext, fields: Vec<IdentityFieldValue>) -> Self {
        JudgementState {
            context,
            is_fully_verified: false,
            inserted_timestamp: Timestamp::now(),
            completion_timestamp: None,
            judgement_submitted: false,
            issue_judgement_at: None,
            fields: fields.into_iter().map(IdentityField::new).collect(),
        }
    }

    pub fn check_full_verification(&self) -> bool {
        self.fields
            .iter()
            .all(|field| field.challenge.is_verified())
    }

    pub fn display_name(&self) -> Option<&str> {
        self.fields
            .iter()
            .find(|field| matches!(field.value, IdentityFieldValue::DisplayName(_)))
            .map(|field| match &field.value {
                IdentityFieldValue::DisplayName(name) => name.as_str(),
                _ => panic!("Failed to get display name. This is a bug."),
            })
    }

    pub fn has_same_fields_as(&self, other: &HashMap<AccountType, String>) -> bool {
        if other.len() != self.fields.len() {
            return false;
        }

        for (account, value) in other {
            let matches = self
                .fields
                .iter()
                .any(|field| field.value.matches_type(account, value));
            if !matches {
                return false;
            }
        }

        true
    }

    pub fn as_verified_entries(&self) -> Vec<VerifiedEntry> {
        let mut list = vec![];

        for field in &self.fields {
            let (account_ty, value) = field.value.as_account_type();
            list.push(VerifiedEntry { account_ty, value });
        }

        list
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize, Message)]
#[serde(rename_all = "snake_case")]
#[rtype(result = "()")]
pub struct ExternalMessage {
    pub origin: ExternalMessageType,
    pub id: MessageId,
    pub timestamp: Timestamp,
    pub values: Vec<MessagePart>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum ExternalMessageType {
    Email(String),
    Twitter(String),
    Matrix(String),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessageId(u64);

impl From<u64> for MessageId {
    fn from(val: u64) -> Self {
        MessageId(val)
    }
}

impl From<u32> for MessageId {
    fn from(val: u32) -> Self {
        MessageId::from(val as u64)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Timestamp(u64);

impl Timestamp {
    pub fn now() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        let start = SystemTime::now();
        let time = start
            .duration_since(UNIX_EPOCH)
            .expect("Failed to calculate UNIX time")
            .as_secs();

        Timestamp(time)
    }

    pub fn with_offset(offset: u64) -> Self {
        let now = Self::now();
        Timestamp(now.0 + offset)
    }

    pub fn max(self, other: Timestamp) -> Self {
        if self.0 >= other.0 {
            self
        } else {
            other
        }
    }

    pub fn raw(&self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessagePart(String);

impl From<String> for MessagePart {
    fn from(val: String) -> Self {
        MessagePart(val)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Event {
    pub timestamp: Timestamp,
    pub message: NotificationMessage,
}

impl Event {
    pub fn new(message: NotificationMessage) -> Self {
        Event {
            timestamp: Timestamp::now(),
            message,
        }
    }
}

impl From<NotificationMessage> for Event {
    fn from(val: NotificationMessage) -> Self {
        Event::new(val)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize, Message)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
#[rtype(result = "()")]
pub enum NotificationMessage {
    IdentityInserted {
        context: IdentityContext,
    },
    IdentityUpdated {
        context: IdentityContext,
    },
    FieldVerified {
        context: IdentityContext,
        field: IdentityFieldValue,
    },
    FieldVerificationFailed {
        context: IdentityContext,
        field: IdentityFieldValue,
    },
    SecondFieldVerified {
        context: IdentityContext,
        field: IdentityFieldValue,
    },
    SecondFieldVerificationFailed {
        context: IdentityContext,
        field: IdentityFieldValue,
    },
    AwaitingSecondChallenge {
        context: IdentityContext,
        field: IdentityFieldValue,
    },
    IdentityFullyVerified {
        context: IdentityContext,
    },
    JudgementProvided {
        context: IdentityContext,
    },
    ManuallyVerified {
        context: IdentityContext,
        field: RawFieldName,
    },
    FullManualVerification {
        context: IdentityContext,
    },
}

impl NotificationMessage {
    pub fn context(&self) -> &IdentityContext {
        use NotificationMessage::*;

        match self {
            IdentityInserted { context } => context,
            IdentityUpdated { context } => context,
            FieldVerified { context, field: _ } => context,
            FieldVerificationFailed { context, field: _ } => context,
            SecondFieldVerified { context, field: _ } => context,
            SecondFieldVerificationFailed { context, field: _ } => context,
            AwaitingSecondChallenge { context, field: _ } => context,
            IdentityFullyVerified { context } => context,
            JudgementProvided { context } => context,
            ManuallyVerified { context, field: _ } => context,
            FullManualVerification { context } => context,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct IdentityJudged {
    context: IdentityContext,
    timestamp: Timestamp,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum RawFieldName {
    LegalName,
    DisplayName,
    Email,
    Web,
    Twitter,
    Matrix,
    // Represents the full identity
    All,
}

impl std::fmt::Display for RawFieldName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", {
            match self {
                RawFieldName::LegalName => "legal_name",
                RawFieldName::DisplayName => "display_name",
                RawFieldName::Email => "email",
                RawFieldName::Web => "web",
                RawFieldName::Twitter => "twitter",
                RawFieldName::Matrix => "matrix",
                RawFieldName::All => "all",
            }
        })
    }
}

impl FromStr for RawFieldName {
    type Err = Response;

    fn from_str(s: &str) -> std::result::Result<RawFieldName, Response> {
        // Convenience handler.
        let s = s.trim().replace('-', "").replace('_', "").to_lowercase();

        let f = match s.as_str() {
            "legalname" => RawFieldName::LegalName,
            "displayname" => RawFieldName::DisplayName,
            "email" => RawFieldName::Email,
            "web" => RawFieldName::Web,
            "twitter" => RawFieldName::Twitter,
            "matrix" => RawFieldName::Matrix,
            "all" => RawFieldName::All,
            _ => return Err(Response::InvalidSyntax(Some(s.to_string()))),
        };

        Ok(f)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Response {
    Status(JudgementStateBlanked),
    Verified(ChainAddress, Vec<RawFieldName>),
    UnknownCommand,
    IdentityNotFound,
    InvalidSyntax(Option<String>),
    FullyVerified(ChainAddress),
    InternalError,
    Help,
}

impl std::fmt::Display for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Response::Status(state) => serde_json::to_string_pretty(state).unwrap(),
            Response::Verified(_, fields) => {
                format!("Verified the following fields: {}", {
                    let mut all = String::new();
                    for field in fields {
                        all.push_str(&format!("{}, ", field));
                    }

                    // Remove `, ` suffix.
                    all.pop();
                    all.pop();

                    all
                })
            }
            Response::UnknownCommand => "The provided command is unknown".to_string(),
            Response::IdentityNotFound => {
                "Identity was not found or invalid query executed".to_string()
            }
            Response::InvalidSyntax(input) => {
                format!(
                    "Invalid input{}",
                    match input {
                        Some(input) => format!(" '{}'", input),
                        None => "".to_string(),
                    }
                )
            }
            Response::InternalError => {
                "An internal error occured. Please contact the architects.".to_string()
            }
            Response::Help => "\
                status <ADDR>\t\t\tShow the current verification status of the specified address.\n\
                verify <ADDR> <FIELD>...\tVerify one or multiple fields of the specified address.\n\
                "
            .to_string(),
            Response::FullyVerified(_) => {
                "Identity has been fully verified. The extrinsic will be submitted in a couple of minutes".to_string()
            },
        };

        write!(f, "{}", msg)
    }
}