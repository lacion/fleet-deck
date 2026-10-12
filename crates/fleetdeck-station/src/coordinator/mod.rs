//! The coordinator MCP endpoint of a station (M3 6-5, ADR-006 §5).
//!
//! A coordinator session talks to the station over a loopback-only MCP
//! Streamable HTTP endpoint. The caller identity is the binding that its
//! credential names (§5.3); no request field can change it. The station
//! checks the request shape, the credential, the binding state and the
//! granted tool classes; the deck decides every operation (§6.2).

pub mod bindings;
pub mod forward;
pub mod handoff;
pub mod ingress;
mod jsonrpc;
pub mod tools;

pub use bindings::{Binding, BindingState, Bindings, Caller, ToolClass, digest, mint};
pub use forward::LinkForward;
pub use handoff::{Handoff, HandoffError};
pub use ingress::{Forward, Ingress, Limits};

#[cfg(test)]
#[path = "ingress_tests.rs"]
mod ingress_tests;
