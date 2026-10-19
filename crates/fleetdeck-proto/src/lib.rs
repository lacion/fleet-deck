//! Fleet Deck v2 wire types (PLAN §12, doctrine rule 9).
//!
//! This crate is the single source of truth for every shape that crosses a
//! process boundary: canonical events (PLAN §6.3), capabilities (§6.2),
//! commands and receipts (§6.4, §8.4), control frames (§8.3) and the
//! protocol version (§8.2). The TypeScript types of the board are generated
//! from it ([`ts::export_all`]).

pub mod accounts;
pub mod artifacts;
pub mod assist;
pub mod automation;
pub mod board;
pub mod branches;
pub mod capability;
pub mod capture;
pub mod command;
pub mod conflicts;
pub mod coordinator_desktop;
pub mod coordinator_desktop_text;
pub mod coordinator_desktop_wire;
pub mod coordinator_input;
pub mod coordinator_spec;
pub mod desktop;
pub mod desktop_authority;
pub mod desktop_text;
pub mod desktop_tools;
pub mod display;
pub mod display_changes;
pub mod display_directive;
pub mod display_event;
pub mod event;
pub mod execution;
pub mod execution_stop;
pub mod files;
pub mod fleet_git;
pub mod fleet_summary;
pub mod fold;
pub mod history;
pub mod ids;
pub mod input;
pub mod integrations;
pub mod launch;
pub mod link;
pub mod member_deny;
pub mod mutation;
pub mod native;
pub mod omarchy_admin;
pub mod omarchy_repair;
pub mod orchestration;
pub mod palette;
pub mod plans;
pub mod recording;
pub mod resources;
pub mod review;
pub mod session;
pub mod settings;
pub mod share;
pub mod shell;
pub mod status;
pub mod subagent;
pub mod ts;
pub mod untrusted;
pub mod version;

pub use capability::{Capabilities, Degradation, Operation, Unsupported};
pub use command::{CommandEnvelope, Receipt, Reject, RejectCode, SessionCommand};
pub use event::{Event, EventBody, EventEnvelope, input_text_of};
pub use ids::*;
pub use link::{ControlFrame, CoordinatorBindingPhase, CoordinatorBindingState, Scope};
pub use mutation::Mutation;
pub use session::{CardStatus, SessionSummary};
pub use status::card_status;
pub use version::{PROTO_MAX, PROTO_MIN};
