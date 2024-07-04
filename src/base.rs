use crate::blockchain::{AccountType, DisplayNameEntry, VerifiedEntry};

use actix::Message;
use std::collections::HashMap;
use std::str::FromStr;

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