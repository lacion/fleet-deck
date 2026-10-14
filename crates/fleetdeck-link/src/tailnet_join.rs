//! The flow of `fleetdeck station join --tailnet` on the link layer
//! (DL-178, tailnet design 5.2 and 5.4): the retry file, the choice
//! between `PairRequest` and `PairResume`, and the dial order of the
//! addresses of the deck.
//!
//! - A new join makes the request, and the retry file is written after the
//!   handshake and before `PairRequest` leaves.
//! - An existing retry file for the same deck is resumed: the station asks
//!   for the stored approval with `PairResume`. A deck that holds none
//!   (`unknown_request`) never approved that request, so the station sends
//!   the same request again.
//! - Every answer of the deck that ends the request removes the file. A
//!   failure of the network, a timeout and `busy` keep it.
//!
//! The function stores no deck pin. The caller stores the pin, its grants
//! and the dial target in one local transaction, and removes the file after
//! it.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use fleetdeck_core::identity::Fingerprint;
use fleetdeck_proto::link::Scope;

use crate::net::NetError;
use crate::node::NodeConfig;
use crate::pair_dial::{PairApproved, PairMode, request_pairing};
use crate::pair_request::{CanonicalRequest, RequestError, reason};
use crate::retry_file::{self, RetryError, RetryRecord};
use crate::ws::LinkError;

/// What the person asked for.
#[derive(Debug, Clone)]
pub struct JoinPlan {
    /// The deck as the person typed it (a name or an address), kept in the
    /// retry file.
    pub host: String,
    pub port: u16,
    /// The deck link key fingerprint that the person confirmed.
    pub deck: Fingerprint,
    /// The requested name of this station.
    pub name: String,
    /// The scopes that this station gives to the deck.
    pub scopes: BTreeSet<Scope>,
    /// The addresses of the deck, in dial order.
    pub addrs: Vec<SocketAddr>,
    /// The station data directory (holds the retry file).
    pub dir: PathBuf,
    /// The longest wait for the owner.
    pub wait: Duration,
}

/// A request that the owner approved.
#[derive(Debug, Clone)]
pub struct Joined {
    pub approved: PairApproved,
    /// The address that answered.
    pub addr: SocketAddr,
    /// The scopes that this station gives to the deck.
    pub scopes: BTreeSet<Scope>,
}

/// Why the join did not finish.
#[derive(Debug, thiserror::Error)]
pub enum JoinError {
    /// A retry file of another deck or fingerprint exists.
    #[error(
        "a pairing request is pending; run `fleetdeck station join --tailnet --cancel` first"
    )]
    Pending,
    /// The deck (or this station) ended the request with a stable code.
    #[error("{code}: {detail}")]
    Refused { code: String, detail: String },
    /// The network or the protocol failed; the retry file stays.
    #[error("{0}")]
    Link(LinkError),
    #[error(transparent)]
    File(#[from] RetryError),
    #[error(transparent)]
    Request(#[from] RequestError),
    #[error("no address of the deck was given")]
    NoAddress,
}

impl JoinError {
    /// The stable code of the error.
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::Refused { code, .. } => code,
            Self::Pending => "pending",
            Self::Link(e) => e.reason().unwrap_or("failed"),
            Self::File(_) => "retry_file",
            Self::Request(_) => "invalid_request",
            Self::NoAddress => "no_address",
        }
    }
}

/// The codes that end a request: the file is removed.
const ENDS_REQUEST: [&str; 9] = [
    reason::DENIED,
    reason::EXPIRED,
    reason::CANCELLED,
    reason::WINDOW_CLOSED,
    reason::ALREADY_PAIRED,
    reason::REQUEST_ID_CONFLICT,
    reason::REVOKED,
    reason::FINGERPRINT_MISMATCH,
    reason::UNSUPPORTED,
];

fn refused(e: &LinkError) -> Option<JoinError> {
    let code = e.reason()?;
    (ENDS_REQUEST.contains(&code) || code == reason::UNKNOWN_REQUEST).then(|| JoinError::Refused {
        code: code.to_owned(),
        detail: match e {
            LinkError::Closed { detail, .. } => detail.clone(),
            LinkError::Handshake(crate::hello::HandshakeError::PeerClosed { detail, .. }) => {
                detail.clone()
            }
            other => other.to_string(),
        },
    })
}

fn same_deck(record: &RetryRecord, plan: &JoinPlan) -> bool {
    record.host.trim_end_matches('.').eq_ignore_ascii_case(plan.host.trim_end_matches('.'))
        && record.port == plan.port
        && record.deck_fingerprint == plan.deck.to_string()
}

/// Runs the join. See the module text.
///
/// # Errors
/// [`JoinError`].
pub async fn drive(node: &NodeConfig, plan: &JoinPlan) -> Result<Joined, JoinError> {
    if plan.addrs.is_empty() {
        return Err(JoinError::NoAddress);
    }
    let (request, mut mode, scopes) = match retry_file::read(&plan.dir)? {
        Some(record) if same_deck(&record, plan) => {
            let request = record.request().ok_or(RetryError::Invalid(plan.dir.clone()))?;
            let scopes = record.offered();
            (request, PairMode::Resume, scopes)
        }
        Some(_) => return Err(JoinError::Pending),
        None => (
            CanonicalRequest::generate(&plan.name, plan.scopes.clone())?,
            PairMode::Request,
            plan.scopes.clone(),
        ),
    };
    let record = RetryRecord::new(&plan.host, plan.port, &plan.deck.to_string(), &request);
    let mut last = None;
    let mut index = 0;
    while index < plan.addrs.len() {
        let addr = plan.addrs[index];
        let write = || {
            // The file exists before the frame leaves; a resumed file stays.
            if retry_file::read(&plan.dir).is_ok_and(|r| r.is_some()) {
                return Ok(());
            }
            retry_file::write(&plan.dir, &record).map_err(|e| e.to_string())
        };
        match request_pairing(node, addr, plan.deck, &request, mode, plan.wait, write).await {
            Ok(approved) => {
                return Ok(Joined {
                    approved,
                    addr,
                    scopes,
                });
            }
            // The deck never approved that request: send it again.
            Err(e) if mode == PairMode::Resume && e.reason() == Some(reason::UNKNOWN_REQUEST) => {
                mode = PairMode::Request;
            }
            // This address did not connect: try the next one.
            Err(LinkError::Net(NetError::Io(io))) if index + 1 < plan.addrs.len() => {
                last = Some(LinkError::Net(NetError::Io(io)));
                index += 1;
            }
            Err(e) => {
                return Err(match refused(&e) {
                    Some(answer) => {
                        retry_file::remove(&plan.dir)?;
                        answer
                    }
                    None => JoinError::Link(e),
                });
            }
        }
    }
    Err(JoinError::Link(
        last.unwrap_or_else(|| LinkError::Protocol("no address answered".into())),
    ))
}
