//! The board sign-out on the target machine (R13 P7 §7.6).
//!
//! An agent that types on a station can type into a browser on the same
//! machine, so the deck signs out every owner and member browser session
//! that runs on a machine where an agent types. The parts:
//!
//! - [`addr`]: the canonical form of an address.
//! - [`exposure`]: the exposure windows and the cache of the exposed set.
//! - [`registry`]: the board sockets and the shell-attach sockets, so a
//!   sign-out ends every relay of a deleted session.
//! - This module: the guard rule ([`guard_rule`], "Sign-out" rule 2), the
//!   sign-in refusal ([`sign_in_check`]), the socket admission
//!   ([`admit_socket`]) and the interface for the input dispatch (task K1):
//!   [`machine_addresses`], [`crate::store::board_fence::sign_out_in_tx`],
//!   [`audit_signed_out`], [`revoke_before_dispatch`] and
//!   [`crate::store::DeckStore::invalidate_exposure`].
//!
//! The order of the input dispatch (K1): [`machine_addresses`] gives the
//! snapshot; [`Registry::sessions_at`] names the sessions of the sockets at
//! the snapshot addresses; one transaction marks the operation `attempted`,
//! writes its `exposure_addrs` and calls `sign_out_in_tx` (the sessions of
//! the snapshot addresses and the named sessions); after the commit K1 calls
//! `invalidate_exposure`, [`audit_signed_out`] and [`revoke_before_dispatch`],
//! and fails the operation with `input_interrupted` before any input when
//! the last one returns false. A session creation reads the exposure in the
//! transaction that inserts the session (`exposed_in_tx`), so it is
//! serialized with that transaction.

pub mod addr;
pub mod exposure;
pub mod registry;

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::extract::Extension;
use axum::http::StatusCode;
use fleetdeck_core::local_addrs::{LocalAddrs, local_addresses};
use fleetdeck_proto::StationId;
use fleetdeck_proto::coordinator_desktop_text::{code, text};

pub use self::registry::{Registry, SessionKind, SocketKind, Ticket, Waiter, wait_all};
use crate::http::admission::PeerAddr;
use crate::http::guard::ApiError;
pub use crate::store::board_fence::SignedOut;
use crate::store::{AuditResult, DeckStore, NewAudit, Principal};

/// The most addresses that a station reports (`host_addresses`), and so the
/// most that [`machine_addresses`] keeps of them.
const MAX_HOST_ADDRESSES: usize = 16;
/// The most addresses of one snapshot (`exposure_addrs`).
const MAX_SNAPSHOT_ADDRESSES: usize = 40;
/// How long [`revoke_before_dispatch`] waits for the relays to end.
const REVOKE_WAIT: Duration = Duration::from_secs(2);
/// The audit action of one deleted session.
const AUDIT_ACTION: &str = "board.signed_out_for_input";
/// The characters of a session hash in an audit target.
const AUDIT_HASH_CHARS: usize = 8;

/// The state of the fence in the deck store.
#[derive(Debug, Default)]
pub(crate) struct FenceState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) exposure: exposure::Cache,
    /// The local station of `fleetdeck up`: the cell that `Board.local_station`
    /// shares once `serve` wires it ([`DeckStore::share_local_station`]).
    pub(crate) local_station: Mutex<Arc<OnceLock<StationId>>>,
}

/// The canonical machine addresses of a target station (§7.6 "Machine
/// addresses of a target"), for the snapshot `exposure_addrs`: the union of
///
/// - the peer address of the current link of the station (`link_peer`);
/// - the valid `host_addresses` that the station reports: each text is
///   parsed as an address, an invalid text, a loopback address and a
///   link-local address are skipped, and at most 16 are kept;
/// - when `is_local_station`: `127.0.0.1`, `::1` and every address of the
///   interfaces of this machine ([`local_addresses`]).
///
/// The result is sorted, has no duplicate and has at most 40 entries. When
/// the union has more, the link peer, the reported addresses and the
/// loopback addresses stay, and the interface addresses (in sorted order)
/// fill the rest.
#[must_use]
pub fn machine_addresses(
    _store: &DeckStore,
    link_peer: Option<IpAddr>,
    host_addresses: &[String],
    is_local_station: bool,
) -> Vec<String> {
    let local = is_local_station.then(local_addresses);
    machine_addresses_with(link_peer, host_addresses, local.as_ref())
}

/// [`machine_addresses`] with the interface addresses given (`None`: not the
/// local station).
fn machine_addresses_with(
    link_peer: Option<IpAddr>,
    host_addresses: &[String],
    local: Option<&LocalAddrs>,
) -> Vec<String> {
    let mut kept: BTreeSet<String> = BTreeSet::new();
    if let Some(peer) = link_peer {
        kept.insert(addr::canonical(peer));
    }
    let mut hosts: BTreeSet<String> = BTreeSet::new();
    for text in host_addresses {
        if hosts.len() >= MAX_HOST_ADDRESSES {
            break;
        }
        let Ok(ip) = text.parse::<IpAddr>() else {
            continue;
        };
        if addr::loopback_or_link_local(ip) {
            continue;
        }
        hosts.insert(addr::canonical(ip));
    }
    kept.extend(hosts);
    let Some(local) = local else {
        return kept.into_iter().take(MAX_SNAPSHOT_ADDRESSES).collect();
    };
    kept.insert("127.0.0.1".to_owned());
    kept.insert("::1".to_owned());
    let mut interfaces: BTreeSet<String> = BTreeSet::new();
    for ip in local.v4.iter().flatten() {
        interfaces.insert(addr::canonical(IpAddr::V4(*ip)));
    }
    for ip in local.v6.iter().flatten() {
        interfaces.insert(addr::canonical(IpAddr::V6(*ip)));
    }
    for ip in interfaces {
        if kept.len() >= MAX_SNAPSHOT_ADDRESSES {
            break;
        }
        kept.insert(ip);
    }
    kept.into_iter().take(MAX_SNAPSHOT_ADDRESSES).collect()
}

/// Revokes every socket of the deleted sessions and every socket from
/// `addrs` (the snapshot). The session of each revoked socket that still
/// exists (a session that has a socket at a snapshot address but another
/// last address, or that a socket registered after the attempted
/// transaction) is deleted right after, in a second delete outside the
/// transaction, its other sockets are revoked, and it is audited like the
/// others (`rule=1 op=<op_id>`). Then it waits up to 2 s until every relay
/// has ended. False when a relay did not end in time, or when a session
/// could not be deleted (fail closed): the caller fails the operation with
/// `input_interrupted` before any input.
pub async fn revoke_before_dispatch(
    store: &DeckStore,
    signed_out: &[SignedOut],
    addrs: &[String],
    op_id: &str,
) -> bool {
    let sessions: Vec<String> = signed_out.iter().map(|s| s.session_hash.clone()).collect();
    let (mut waiters, hit) = store.registry().revoke(&sessions, addrs);
    let mut extra: Vec<SignedOut> = Vec::new();
    let mut deleted_all = true;
    for (kind, hash) in hit {
        if sessions.contains(&hash) {
            continue;
        }
        match store.end_session_of(kind, hash.clone()).await {
            Ok(true) => extra.push(SignedOut {
                kind,
                session_hash: hash,
            }),
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(error = %e, "a session of a revoked socket could not be deleted");
                deleted_all = false;
            }
        }
    }
    if !extra.is_empty() {
        let hashes: Vec<String> = extra.iter().map(|s| s.session_hash.clone()).collect();
        waiters.extend(store.registry().revoke(&hashes, &[]).0);
        audit_rows(store, &extra, 1, op_id).await;
    }
    let ended = wait_all(waiters, REVOKE_WAIT).await;
    ended && deleted_all
}

/// Writes the audit row of each session of `signed_out`, for rule 1 of the
/// operation `op_id`. Call it after the commit of the `attempted`
/// transaction.
pub async fn audit_signed_out(store: &DeckStore, signed_out: &[SignedOut], op_id: &str) {
    audit_rows(store, signed_out, 1, op_id).await;
}

/// One `board.signed_out_for_input` row for each session: the actor is the
/// deck, the target is the session kind and the first 8 characters of the
/// session hash, and the detail is `rule=<n> op=<operation id or ->`. It
/// never holds an address or a text.
async fn audit_rows(store: &DeckStore, signed_out: &[SignedOut], rule: u8, op: &str) {
    for s in signed_out {
        let kind = match s.kind {
            SessionKind::Owner => "owner",
            SessionKind::Member => "member",
        };
        let prefix: String = s.session_hash.chars().take(AUDIT_HASH_CHARS).collect();
        store
            .audit_or_log(
                NewAudit::new("deck", AUDIT_ACTION, AuditResult::Ok)
                    .target(format!("{kind}:{prefix}"))
                    .detail(format!("rule={rule} op={op}")),
            )
            .await;
    }
}

/// The peer address of a request, if the connection recorded one (a test that
/// calls the router directly records none).
pub(crate) fn peer_ip(peer: &Option<Extension<PeerAddr>>) -> Option<IpAddr> {
    peer.as_ref().map(|Extension(p)| p.0.ip())
}

/// 401 `signed_out_for_input` with the text of §13.
fn signed_out_error() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        code::SIGNED_OUT_FOR_INPUT,
        text(code::SIGNED_OUT_FOR_INPUT).unwrap_or_default(),
    )
}

/// 403 `sign_in_blocked_for_input` with the text of §13.
pub(crate) fn sign_in_blocked_error() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        code::SIGN_IN_BLOCKED_FOR_INPUT,
        text(code::SIGN_IN_BLOCKED_FOR_INPUT).unwrap_or_default(),
    )
}

/// The sign-in refusal (§7.6 rule 2): a sign-in from an exposed address is
/// refused before any session, cookie, CSRF token or login token is made.
/// A request without a recorded peer address passes.
///
/// # Errors
/// 403 `sign_in_blocked_for_input`; 500 `internal` for a database error.
pub(crate) async fn sign_in_check(store: &DeckStore, ip: Option<IpAddr>) -> Result<(), ApiError> {
    let Some(ip) = ip else {
        return Ok(());
    };
    let exposed = store
        .exposed_addresses(store.now_ms())
        .await
        .map_err(ApiError::internal)?;
    if exposed.contains(&addr::canonical(ip)) {
        return Err(sign_in_blocked_error());
    }
    Ok(())
}

/// Rule 2 of the sign-out (§7.6), for an authenticated board request or
/// socket upgrade. For an owner or member principal (never a token or a
/// display):
///
/// - from an exposed address, the session is deleted, its sockets are
///   revoked, one audit row is written and the answer is 401
///   `signed_out_for_input`;
/// - from another address, the address of the session is recorded
///   ([`DeckStore::touch_session_addr`]), and for a socket upgrade
///   (`socket` is `Some`) the socket is registered; the returned [`Ticket`]
///   goes to the handler in a request extension.
///
/// # Errors
/// 401 `signed_out_for_input`; 500 `internal` for a database error (the
/// request is refused, never let through).
pub(crate) async fn guard_rule(
    store: &DeckStore,
    principal: &Principal,
    peer: Option<PeerAddr>,
    socket: Option<SocketKind>,
) -> Result<Option<Ticket>, ApiError> {
    let (kind, hash) = match principal {
        Principal::Owner(session) => (SessionKind::Owner, session.session_hash.clone()),
        Principal::Member(_, grant) => (SessionKind::Member, grant.session_hash.clone()),
        Principal::Token(..) | Principal::Display(_) => return Ok(None),
    };
    let Some(peer) = peer else {
        return Ok(None);
    };
    let addr = addr::canonical(peer.0.ip());
    let now = store.now_ms();
    let exposed = store
        .exposed_addresses(now)
        .await
        .map_err(ApiError::internal)?;
    if exposed.contains(&addr) {
        let existed = store
            .end_session_of(kind, hash.clone())
            .await
            .map_err(ApiError::internal)?;
        // The sockets end on their own flag; this refusal does not wait.
        drop(store.registry().revoke(std::slice::from_ref(&hash), &[]).0);
        if existed {
            let row = SignedOut {
                kind,
                session_hash: hash,
            };
            audit_rows(store, std::slice::from_ref(&row), 2, "-").await;
        }
        return Err(signed_out_error());
    }
    store
        .touch_session_addr(kind, &hash, &addr, now)
        .await
        .map_err(ApiError::internal)?;
    Ok(socket.map(|s| store.registry().register(&hash, kind, s, &addr)))
}

/// The admission of a socket at `on_upgrade`: reads that the session row
/// still exists and that the address of the socket is not exposed (both
/// reads begin after the guard registered the socket), then makes the entry
/// `Live` under the mutex of the registry. A socket that registered after
/// the sweep of an input operation, from an exposed address, is refused
/// here and its session is deleted (§7.6 rule 2). False: close the socket
/// at once and never start the relay. A database error counts as a missing
/// session (fail closed).
pub(crate) async fn admit_socket(store: &DeckStore, ticket: &Ticket) -> bool {
    let (kind, hash) = (ticket.kind(), ticket.session_hash().to_owned());
    let exposed = match store.addr_exposed_now(ticket.addr(), store.now_ms()).await {
        Ok(exposed) => exposed,
        Err(e) => {
            tracing::warn!(error = %e, "the exposure of a socket could not be read; the socket is closed");
            return false;
        }
    };
    if exposed {
        match store.end_session_of(kind, hash.clone()).await {
            Ok(true) => {
                let row = SignedOut {
                    kind,
                    session_hash: hash.clone(),
                };
                audit_rows(store, std::slice::from_ref(&row), 2, "-").await;
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(error = %e, "the session of a refused socket could not be deleted")
            }
        }
        drop(store.registry().revoke(std::slice::from_ref(&hash), &[]).0);
        return false;
    }
    let exists = match store.session_row_exists(kind, &hash).await {
        Ok(exists) => exists,
        Err(e) => {
            tracing::warn!(error = %e, "the session of a socket could not be read; the socket is closed");
            false
        }
    };
    store.registry().go_live(ticket, || exists)
}

#[cfg(test)]
mod dispatch_tests;
#[cfg(test)]
mod machine_tests;
#[cfg(test)]
mod store_tests;
