use super::{Address, AddressType, Challenge, PubKey, Result, Signature};
use crate::db::{Database, ScopedDatabase};
use crossbeam::channel::{unbounded, Receiver, Sender};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Serialize, Deserialize)]
pub struct OnChainIdentity {
    pub pub_key: PubKey,
    // TODO: Should this just be a String?
    pub display_name: Option<AddressState>,
    // TODO: Should this just be a String?
    pub legal_name: Option<AddressState>,
    pub email: Option<AddressState>,
    pub web: Option<AddressState>,
    pub twitter: Option<AddressState>,
    pub matrix: Option<AddressState>,
}

impl OnChainIdentity {
    // Get the address state based on the address type (Email, Matrix, etc.).
    fn address_state(&self, addr_type: &AddressType) -> Option<&AddressState> {
        use AddressType::*;

        match addr_type {
            Email => self.email.as_ref(),
            Web => self.web.as_ref(),
            Twitter => self.twitter.as_ref(),
            Matrix => self.matrix.as_ref(),
        }
    }
    // Get the address state based on the addresses type. If the addresses
    // themselves match (`me@email.com == me@email.com`), it returns the state
    // wrapped in `Some(_)`, or `None` if the match is invalid.
    fn address_state_match(
        &self,
        addr_type: &AddressType,
        addr: &Address,
    ) -> Option<&AddressState> {
        if let Some(addr_state) = self.address_state(addr_type) {
            if &addr_state.addr == addr {
                return Some(addr_state);
            }
        }
        None
    }
    fn from_json(val: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(&val)?)
    }
    fn to_json(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }
}

#[derive(Serialize, Deserialize)]
pub struct AddressState {
    addr: Address,
    addr_type: AddressType,
    addr_validity: AddressValidity,
    pub challenge: Challenge,
    pub attempt_contact: AtomicBool,
    confirmed: AtomicBool,
}

impl AddressState {
    pub fn new(addr: Address, addr_type: AddressType) -> Self {
        AddressState {
            addr: addr,
            addr_type: addr_type,
            addr_validity: AddressValidity::Unknown,
            challenge: Challenge::gen_random(),
            attempt_contact: AtomicBool::new(false),
            confirmed: AtomicBool::new(false),
        }
    }
    pub fn attempt_contact(&self) {
        self.attempt_contact.store(true, Ordering::Relaxed);
    }
}

#[derive(Eq, PartialEq, Serialize, Deserialize)]
enum AddressValidity {
    #[serde(rename = "unknown")]
    Unknown,
    #[serde(rename = "valid")]
    Valid,
    #[serde(rename = "invalid")]
    Invalid,
}

pub struct IdentityScope<'a> {
    pub identity: &'a OnChainIdentity,
    pub addr_state: &'a AddressState,
    db: &'a ScopedDatabase<'a>,
}

impl<'a> IdentityScope<'a> {
    pub fn address(&self) -> &Address {
        &self.addr_state.addr
    }
    pub fn verify_challenge(&self, sig: Signature) -> Result<bool> {
        if let Ok(_) = self
            .identity
            .pub_key
            .0
            // TODO: Check context in substrate.
            .verify_simple(b"", self.addr_state.challenge.0.as_bytes(), &sig.0)
        {
            self.addr_state.confirmed.store(true, Ordering::Relaxed);

            // Keep track of the current progress on disk.
            self.db
                .put(self.identity.pub_key.0.to_bytes(), self.identity.to_json()?)?;

            Ok(true)
        } else {
            Ok(false)
        }
    }
}

enum CommsMessage {
    NewOnChainIdentity(OnChainIdentity),
    Inform {
        pub_key: PubKey,
        address: Address,
        address_ty: AddressType,
        challenge: Challenge,
    },
    ValidAddress {
        address: Address,
        address_ty: AddressType,
    },
    InvalidAddress {
        address: Address,
        address_ty: AddressType,
    },
}

pub struct CommsMain(Sender<CommsMessage>);

pub struct CommsVerifier {
    tx: Sender<CommsMessage>,
    recv: Receiver<CommsMessage>,
}

pub struct IdentityManager<'a> {
    pub idents: Vec<OnChainIdentity>,
    pub db: ScopedDatabase<'a>,
    comms: CommsTable,
}

struct CommsTable {
    to_main: Sender<CommsMessage>,
    listener: Receiver<CommsMessage>,
    email: Option<CommsMain>,
    web: Option<CommsMain>,
    twitter: Option<CommsMain>,
    matrix: Option<CommsMain>,
}

impl<'a> IdentityManager<'a> {
    pub fn new(db: &'a Database) -> Result<Self> {
        let db = db.scope("pending_identities");

        let mut idents = vec![];

        // Read pending on-chain identities from storage. Ideally, there are none.
        for (_, value) in db.all()? {
            idents.push(OnChainIdentity::from_json(&*value)?);
        }

        let (tx, recv) = unbounded();

        Ok(IdentityManager {
            idents: idents,
            db: db,
            comms: CommsTable {
                to_main: tx,
                listener: recv,
                email: None,
                web: None,
                twitter: None,
                matrix: None,
            },
        })
    }
    fn get_comms(&self) -> (CommsMain, CommsVerifier) {
        let (tx, recv) = unbounded();

        (
            CommsMain(tx),
            CommsVerifier {
                tx: self.comms.to_main.clone(),
                recv: recv,
            },
        )
    }
    pub fn register_comms(&'static mut self, addr_type: &AddressType) -> CommsVerifier {
        use AddressType::*;

        let (cm, cv) = self.get_comms();

        match addr_type {
            Email => {
                self.comms.email = Some(cm);
            }
            Web => {
                self.comms.web = Some(cm);
            }
            Twitter => {
                self.comms.twitter = Some(cm);
            }
            Matrix => {
                self.comms.matrix = Some(cm);
            }
            _ => panic!("TODO"),
        }

        cv
    }
    pub async fn run(&mut self) -> Result<()> {
        use CommsMessage::*;

        if let Ok(msg) = self.comms.listener.recv() {
            match msg {
                CommsMessage::NewOnChainIdentity(ident) => {
                    self.register_request(ident)?;
                }
                CommsMessage::Inform { .. } => {
                    // INVALID
                    // TODO: log
                }
                ValidAddress {
                    address,
                    address_ty,
                } => {}
                InvalidAddress {
                    address,
                    address_ty,
                } => {}
            }
        }

        Ok(())
    }
    pub fn register_request(&mut self, ident: OnChainIdentity) -> Result<()> {
        // Only add the identity to the list if it doesn't exists yet.
        if self
            .idents
            .iter()
            .find(|ident| ident.pub_key == ident.pub_key)
            .is_none()
        {
            // Save the pending on-chain identity to disk.
            self.db.put(ident.pub_key.0.to_bytes(), ident.to_json()?)?;
            self.idents.push(ident);
        }

        Ok(())
    }
    pub fn get_identity_scope(
        &'a self,
        addr_type: &AddressType,
        addr: &Address,
    ) -> Option<IdentityScope<'a>> {
        self.idents
            .iter()
            .find(|ident| ident.address_state_match(addr_type, addr).is_some())
            .map(|ident| IdentityScope {
                identity: &ident,
                // Unwrapping is fine here, since `Some` is verified in the
                // previous `find()` combinator.
                addr_state: &ident.address_state(&addr_type).unwrap(),
                db: &self.db,
            })
    }
    pub fn get_uninitialized_channel(&'a self, addr_type: AddressType) -> Vec<IdentityScope<'a>> {
        self.idents
            .iter()
            .filter(|ident| {
                if let Some(addr_state) = ident.address_state(&addr_type) {
                    addr_state.addr_validity == AddressValidity::Unknown
                        && !addr_state.attempt_contact.load(Ordering::Relaxed)
                } else {
                    false
                }
            })
            .map(|ident| IdentityScope {
                identity: &ident,
                // Unwrapping is fine here, since `None` values are filtered out
                // in the previous `filter` combinator.
                addr_state: &ident.address_state(&addr_type).unwrap(),
                db: &self.db,
            })
            .collect()
    }
}
