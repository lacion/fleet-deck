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

pub mod control;
pub mod epochs;
pub mod grants;
mod handshake;
pub mod hello;
pub mod methods;
pub mod net;
pub mod node;
pub mod table;
pub mod tls;
pub mod transport;
pub mod ws;

pub use control::{Control, ControlConfig, GrantSource, Grants, Inbound, LiveLink};
pub use epochs::{Epochs, LINK_EPOCHS_SQL};
pub use handshake::Established;
pub use node::{LinkListener, NodeConfig, OpenLink, dial, listen};
pub use table::{Closing, Dialer, LinkId, LinkTable};
pub use ws::LinkError;
