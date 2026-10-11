//! Fleet Deck v2 wire types (PLAN §12, doctrine rule 9).
//!
//! This crate is the single source of truth for every shape that crosses a
//! process boundary: canonical events (PLAN §6.3), capabilities (§6.2),
//! commands and receipts (§6.4, §8.4), control frames (§8.3) and the
//! protocol version (§8.2). The TypeScript types of the board are generated
//! from it ([`ts::export_all`]).

pub mod capability;
pub mod command;
pub mod event;
pub mod fold;
pub mod ids;
pub mod link;
pub mod mutation;
pub mod session;
pub mod ts;
pub mod version;

pub use capability::{Capabilities, Degradation, Operation, Unsupported};
pub use command::{CommandEnvelope, Receipt, Reject, RejectCode, SessionCommand};
pub use event::{Event, EventBody, EventEnvelope};
pub use ids::*;
pub use link::ControlFrame;
pub use mutation::Mutation;
pub use session::SessionSummary;
pub use version::{PROTO_MAX, PROTO_MIN};
