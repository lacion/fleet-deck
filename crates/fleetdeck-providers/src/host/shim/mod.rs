//! The durable shim host, ADR-005 (Accepted, revision 3; PASS sa-34468515).
//!
//! This module holds the parts of the shim that have no process or socket:
//! the wire protocol ([`wire`]), the attach proof and the peer check
//! ([`proof`]), the stdout buffer with absolute offsets and backpressure
//! ([`output`]), the stdin write records with admission, takeover and
//! eviction ([`writes`]), the effect table of `write_status` answers
//! ([`settle`]) and provider identity ([`identity`]).
//!
//! Slice 2 adds the shim process ([`server`], run by `fleetdeck shim
//! --id`), its launch journal, lock, key and exit files and the socket
//! placement ([`journal`]), the launch frame ([`launch`]) and the station
//! side of the socket ([`client`]). The station's `ShimHost`, the `shims`
//! table, adapter checkpoints and recovery come in later steps of 7-1. Until the real-process tests of ADR-005 §9 pass, the station host
//! stays `direct` (ADR-005 §2 "Selection").

pub mod attach;
pub mod client;
pub mod host;
pub mod identity;
pub mod journal;
pub mod launch;
pub mod output;
pub mod proof;
pub mod server;
pub mod settle;
pub mod start;
pub mod stdin;
pub mod wire;
pub mod writes;

#[cfg(test)]
#[path = "../shim_tests.rs"]
mod tests;
