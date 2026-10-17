//! `EventBody` and `EventEnvelope` (moved from `event.rs` for the module size rule, T1.0).

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use ts_rs::TS;

use super::{Event, RawRef};
use crate::ids::SessionId;

/// An event body: a known [`Event`], or an unknown kind kept verbatim (L16).
#[derive(Debug, Clone, PartialEq)]
pub enum EventBody {
    Known(Event),
    Unknown {
        kind: String,
        payload: serde_json::Map<String, Value>,
    },
}

impl EventBody {
    /// The dotted kind name.
    #[must_use]
    pub fn kind_name(&self) -> &str {
        match self {
            Self::Known(event) => event.kind_name(),
            Self::Unknown { kind, .. } => kind,
        }
    }

    /// The known event, if any.
    #[must_use]
    pub fn known(&self) -> Option<&Event> {
        match self {
            Self::Known(event) => Some(event),
            Self::Unknown { .. } => None,
        }
    }
}

impl From<Event> for EventBody {
    fn from(event: Event) -> Self {
        Self::Known(event)
    }
}

impl Serialize for EventBody {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Known(event) => event.serialize(serializer),
            Self::Unknown { payload, .. } => payload.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for EventBody {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let payload = serde_json::Map::<String, Value>::deserialize(deserializer)?;
        let kind = match payload.get("kind") {
            Some(Value::String(kind)) => kind.clone(),
            Some(_) => return Err(D::Error::custom("event `kind` is not a string")),
            None => return Err(D::Error::missing_field("kind")),
        };
        if !Event::KINDS.contains(&kind.as_str()) {
            return Ok(Self::Unknown { kind, payload });
        }
        serde_json::from_value(Value::Object(payload))
            .map(Self::Known)
            .map_err(|err| D::Error::custom(format!("invalid `{kind}` event: {err}")))
    }
}

/// One stored event with its sequence numbers (PLAN §6.3, §6.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct EventEnvelope {
    /// Per-station monotonic counter; the deck's resync cursor.
    pub station_seq: u64,
    pub session_id: SessionId,
    /// Per-session counter for UI ordering.
    pub session_seq: u64,
    /// Unix ms.
    pub at: i64,
    pub proto_version: u32,
    pub raw_ref: Option<RawRef>,
    /// An unknown kind is typed as `Event` in TypeScript; the board's reducer
    /// ignores kinds that it does not know.
    #[ts(as = "Event")]
    pub event: EventBody,
}
