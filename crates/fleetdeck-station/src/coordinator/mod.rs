//! The coordinator MCP endpoint of a station (M3 6-5, ADR-006 §5).
//!
//! A coordinator session talks to the station over a loopback-only MCP
//! Streamable HTTP endpoint. The caller identity is the binding that its
//! credential names (§5.3); no request field can change it. The station
//! checks the request shape, the credential, the binding state and the
//! granted tool classes; the deck decides every operation (§6.2).

mod annotations;
pub mod bindings;
pub mod bridge;
pub mod desktop_forward;
pub mod desktop_tools;
pub mod fence;
pub mod fleet_tools;
pub mod forward;
mod git_tools;
pub mod handoff;
pub mod ingress;
mod jsonrpc;
mod mcp_server;
mod mcp_service;
pub mod outbox;
mod permit_io;
pub mod socket;
mod tool_class;
pub mod tools;

pub use bindings::{
    Binding, BindingState, Bindings, BridgeEndpoint, Caller, ToolClass, digest, mint,
};
pub use forward::{LinkForward, Transport};
pub use handoff::{Handoff, HandoffError};
pub use ingress::{FleetTools, Forward, Ingress, Limits};
pub use outbox::CallOutbox;
pub use socket::BridgeSocket;

#[cfg(test)]
#[path = "catalog_p1_tests.rs"]
mod catalog_p1_tests;
#[cfg(test)]
#[path = "desktop_forward_tests.rs"]
mod desktop_forward_tests;
#[cfg(test)]
#[path = "desktop_tools_tests.rs"]
mod desktop_tools_tests;
#[cfg(test)]
#[path = "fleet_control_tests.rs"]
mod fleet_control_tests;
#[cfg(test)]
#[path = "ingress_tests.rs"]
mod ingress_tests;
#[cfg(test)]
#[path = "mcp_p1_auth_tests.rs"]
mod mcp_p1_auth_tests;
#[cfg(test)]
#[path = "mcp_p1_rig_tests.rs"]
mod mcp_p1_rig_tests;
#[cfg(test)]
#[path = "mcp_p1_tests.rs"]
mod mcp_p1_tests;
#[cfg(test)]
#[path = "outbox_tests.rs"]
mod outbox_tests;
