//! Phase 0 spike S4: link protocol v1 (PLAN §8, ADR-004).
//!
//! - [`secret`]: `Secret<T>`, a wrapper that cannot be serialized or printed.
//! - [`identity`]: Ed25519 node identity, fingerprints, self-signed certificates.
//! - [`addr`]: the D13 network scope (loopback, LAN, tailnet) for bind, dial
//!   and accepted peers.
//! - [`tls`]: mutual TLS 1.3 where each side trusts only pinned peer keys.
//! - [`net`]: bind, dial and accept with the address rules and mTLS applied.
//! - [`hello`]: the link handshake (Hello, negotiation, transcript signatures).
//! - [`frames`]: control frames and the method table with scopes.
//! - [`mux`]: data channel streams with credit flow control (sans-IO).
//! - [`link`]: control + data WebSocket channels over mTLS, link epochs,
//!   invitation pairing on the pair channel.
//! - [`pairing`]: `fdinv1:` invitations and their single-use store (§8.5).
//! - [`reconnect`]: jittered exponential backoff and the redialing supervisor.

pub mod addr;
pub mod frames;
pub mod hello;
pub mod identity;
pub mod link;
pub mod mux;
pub mod net;
pub mod pairing;
pub mod reconnect;
pub mod secret;
pub mod tls;
