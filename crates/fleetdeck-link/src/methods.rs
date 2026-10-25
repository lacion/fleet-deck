//! The link method table and the authorization of control frames
//! (ADR-004 §9.6, GATE-M1 G6 and G7).
//!
//! Every check uses the roles that were bound at pairing (the role of the
//! pinned peer key and the own role), never the dial direction. The table
//! must equal the table in ADR-004 §9.6; `tests/method_table.rs` compares
//! them.

use std::collections::BTreeSet;

use fleetdeck_proto::link::{Scope, WireError};
use fleetdeck_proto::version::Negotiated;
#[cfg(test)]
use fleetdeck_proto::version::feature;

use crate::tls::Role;

/// Who may call a method, by the roles bound at pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callers {
    /// The deck calls, the station answers.
    DeckToStation,
    /// The station calls, the deck answers.
    StationToDeck,
    /// Either side calls the other.
    Either,
}

impl Callers {
    /// True if a node with role `caller` may call a node with role `receiver`.
    #[must_use]
    pub fn allows(self, caller: Role, receiver: Role) -> bool {
        match self {
            Self::DeckToStation => caller == Role::Deck && receiver == Role::Station,
            Self::StationToDeck => caller == Role::Station && receiver == Role::Deck,
            Self::Either => caller != receiver,
        }
    }
}

/// One row of the method table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodSpec {
    pub name: &'static str,
    pub callers: Callers,
    pub scope: Scope,
    /// A feature that both sides must have negotiated.
    pub feature: Option<&'static str>,
    /// A mutating method needs a `command_id` and a typed envelope.
    pub mutating: bool,
}

// The method table moved to `methods_table.rs` for the module size rule (T1.0).
pub use crate::methods_table::METHODS;

/// The row of `name`.
#[must_use]
pub fn method(name: &str) -> Option<&'static MethodSpec> {
    METHODS.iter().find(|s| s.name == name)
}

/// The request error codes of ADR-004 §2.11 and §9.8.
pub mod code {
    pub const UNKNOWN_METHOD: &str = "unknown_method";
    pub const WRONG_ROLE: &str = "wrong_role";
    pub const FEATURE_NOT_NEGOTIATED: &str = "feature_not_negotiated";
    pub const FORBIDDEN: &str = "forbidden";
    pub const MISSING_COMMAND_ID: &str = "missing_command_id";
    pub const TOO_MANY_REQUESTS: &str = "too_many_requests";
    pub const PROTOCOL_ERROR: &str = "protocol_error";
    pub const TIMEOUT: &str = "timeout";
}

/// Checks a request in the ADR order: `unknown_method`, `wrong_role`,
/// `feature_not_negotiated`, `forbidden`, `missing_command_id`.
///
/// `granted` is `None` for the caller's own pre-check, which cannot know the
/// scopes that the receiver granted.
pub fn authorize(
    name: &str,
    caller: Role,
    receiver: Role,
    command_id: Option<&str>,
    negotiated: &Negotiated,
    granted: Option<&BTreeSet<Scope>>,
) -> Result<&'static MethodSpec, WireError> {
    let spec = method(name)
        .ok_or_else(|| WireError::new(code::UNKNOWN_METHOD, format!("no link method `{name}`")))?;
    if !spec.callers.allows(caller, receiver) {
        return Err(WireError::new(
            code::WRONG_ROLE,
            format!(
                "a {} cannot call `{name}` on a {}",
                role_name(caller),
                role_name(receiver)
            ),
        ));
    }
    if let Some(f) = spec.feature
        && !negotiated.has(f)
    {
        return Err(WireError::new(
            code::FEATURE_NOT_NEGOTIATED,
            format!("`{name}` needs feature {f}, which this link did not negotiate"),
        ));
    }
    if let Some(granted) = granted
        && !granted.contains(&spec.scope)
    {
        return Err(WireError::new(
            code::FORBIDDEN,
            format!(
                "`{name}` needs scope {}, which the pairing did not grant",
                spec.scope.name()
            ),
        ));
    }
    if spec.mutating && command_id.is_none_or(str::is_empty) {
        return Err(WireError::new(
            code::MISSING_COMMAND_ID,
            format!("`{name}` changes state and needs a command_id"),
        ));
    }
    Ok(spec)
}

#[path = "methods_frame.rs"]
mod frame;
pub use frame::{FrameRefused, check_frame, role_name};

#[cfg(test)]
#[path = "methods_tests.rs"]
mod tests;
