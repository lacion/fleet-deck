//! App ids (PLAN §6.3, L6). App ids are primary keys; native provider ids go
//! into `provider_refs` and are never used as global keys.
//!
//! Every id is a string on the wire. Ids that the deck or the station mint
//! (`SessionId`, `CommandId`, `IncarnationId`, ...) are ULIDs; ids that an
//! adapter allocates inside a session (`TurnId`, `NodeId`, `RequestId`, ...)
//! are ordinals such as `turn-3`, so replays are deterministic.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
        #[serde(transparent)]
        #[ts(export_to = "ids/")]
        pub struct $name(pub String);

        impl $name {
            /// Wraps a string as this id.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// The id as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

string_id!(
    /// One driven agent session (ULID, minted by the station or the deck).
    SessionId
);
string_id!(
    /// One user request inside a session; it can span several native turns.
    RunId
);
string_id!(
    /// One provider turn.
    TurnId
);
string_id!(
    /// One tool call, approval, or subagent inside a run (`parent_node_id` links children).
    NodeId
);
string_id!(
    /// One runtime request (approval or question).
    RequestId
);
string_id!(
    /// One host input (prompt, steer, follow-up) whose fate the provider reports.
    InputId
);
string_id!(
    /// One captured plan.
    PlanId
);
string_id!(
    /// One mutating command (ULID minted by the deck or the local CLI, A4).
    CommandId
);
string_id!(
    /// A paired station (the fingerprint of its identity key).
    StationId
);
string_id!(
    /// One life of a station database; a reset database gets a new value (§6.4).
    IncarnationId
);
string_id!(
    /// A provider instance of a station (`[[provider_instance]] id`, L10).
    ProviderInstanceId
);
