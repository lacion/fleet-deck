//! Fleet Deck v2 link between a deck and its stations (PLAN §8, ADR-004
//! Revision 2): TLS with pinned keys, the signed handshake, pairing, the
//! control and data channels, and reconnect.
//!
//! - [`tls`]: mutual TLS 1.3 where each side trusts only pinned peer keys.
//! - [`net`]: bind, dial and accept with the D13 address rules and mTLS.
//! - [`transport`]: TCP, or FIFO pairs as a loopback stand-in where the host
//!   forbids sockets (DL-025).
//! - [`hello`]: the `Hello`, negotiation, transcript signatures, attempts.
//! - [`epochs`]: persistent per-peer link epochs (G1).
//! - [`table`]: the node-wide link table and its resolution rules (G1, G10).
//! - [`node`]: link setup (dial, listen) on top of all of the above.
//! - [`methods`]: the method table and the frame checks (G6, G7).
//! - [`control`]: the control channel: requests, stream frames, heartbeat
//!   and bounded shutdown (G8).
//! - [`mux`], [`data`], [`tokens`]: the data channel: streams with credit
//!   flow control, link budgets and single-use stream tokens (G2, G6, G9).
//! - [`live`]: a running link; one lifetime for both channels.
//! - [`pairing`]: invitations, proofs and pairing admission (G11, G12).
//! - [`join`]: the pair channel on top of [`node`].
//! - [`revoke`]: revocation and key rotation (G5).
//! - [`admission`]: the cap on pending handshakes of a listener (FDV2-001).
//! - [`reconnect`]: the dialer's backoff with jitter and its redial task.

pub mod admission;
pub mod control;
pub mod data;
pub mod desktop_authority;
pub mod epochs;
pub mod grants;
mod handshake;
pub mod hello;
pub mod join;
pub mod live;
pub mod methods;
mod methods_table;
pub mod mux;
pub mod net;
pub mod node;
mod pair_accept;
pub mod pair_dial;
pub mod pair_hub;
pub mod pair_request;
pub mod pairing;
pub mod reconnect;
pub mod retry_file;
pub mod revoke;
pub mod table;
pub mod tailnet_join;
pub mod tailnet_target;
pub mod tls;
pub mod tokens;
pub mod transport;
pub mod ws;

pub use control::{Control, ControlConfig, GrantSource, Grants, Inbound};
pub use data::{Data, DataEvent};
pub use epochs::{Epochs, LINK_EPOCHS_SQL};
pub use handshake::{DesktopEpochs, Established};
pub use join::pair;
pub use live::{DataLink, LiveLink, start};
pub use node::{LinkListener, ListenerKind, NodeConfig, OpenLink, TailnetLimits, dial, listen};
pub use pair_dial::{PairApproved, PairMode, open_pair, request_pairing};
pub use pairing::{
    Invitation, InvitationId, Invitations, PairError, PairLimiter, PairedPeer, PairingSink,
};
pub use reconnect::{Backoff, BackoffPolicy, DialerEvent, spawn_dialer};
pub use revoke::{
    REVOKED, RotateError, check_rotation, finish_rotation, revoke, rotate, sign_rotation,
};
pub use table::{Closing, Dialer, LinkId, LinkTable};
pub use tokens::{StreamToken, StreamTokens};
pub use ws::LinkError;

#[cfg(test)]
mod pair_wire_tests;
