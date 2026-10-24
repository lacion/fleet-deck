//! The directive of a wall display (R13 §4.5, §5A item 3, §5D items 3 to 5):
//! the typed body that the owner sends, and the view that the display polls.
//! A directive holds immutable identifiers, never a URL.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{SessionId, StationId};

/// The most stations in a directive filter.
pub const MAX_FILTER_STATIONS: usize = 64;
/// The longest repository basename in a directive filter, in bytes.
pub const MAX_FILTER_REPO: usize = 255;
/// The largest directive body, in bytes.
pub const MAX_DIRECTIVE_BODY: usize = 8 * 1024;

/// A session, by station id and session id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "display/")]
pub struct SessionRef {
    pub station_id: StationId,
    pub session_id: SessionId,
}

/// One version of an artifact. The version is explicit and at least 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "display/")]
pub struct ArtifactRef {
    pub station_id: StationId,
    pub artifact_id: String,
    pub version: u32,
}

/// A filter that can only narrow the ceiling of the display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "display/")]
pub struct DirectiveFilter {
    /// Station ids, at most [`MAX_FILTER_STATIONS`].
    pub stations: Vec<StationId>,
    /// A repository basename, at most [`MAX_FILTER_REPO`] bytes.
    #[serde(default)]
    pub repo: Option<String>,
}

/// The mode of a directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "display/")]
pub enum DirectiveMode {
    Mission,
    Focus,
    Review,
    Race,
    Artifact,
    Ambient,
    /// B17: a page of the allow-list of the deck (`wall_pages`).
    Page,
}

/// The body of `POST /api/displays/{id}/directive` and the directive that a
/// display reads. One variant for each mode; a field that does not belong to
/// the mode is a parse error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "display/")]
pub enum DirectiveBody {
    Mission {
        #[serde(default)]
        pin: bool,
        #[serde(default)]
        filter: Option<DirectiveFilter>,
    },
    Focus {
        session: SessionRef,
        #[serde(default)]
        pin: bool,
        #[serde(default)]
        filter: Option<DirectiveFilter>,
    },
    Review {
        session: SessionRef,
        #[serde(default)]
        pin: bool,
        #[serde(default)]
        filter: Option<DirectiveFilter>,
    },
    Race {
        batch_id: String,
        #[serde(default)]
        pin: bool,
        #[serde(default)]
        filter: Option<DirectiveFilter>,
    },
    Artifact {
        artifact: ArtifactRef,
        /// B17 wall apps: run the file as an app when the display allows it.
        #[serde(default)]
        app: bool,
        #[serde(default)]
        pin: bool,
        #[serde(default)]
        filter: Option<DirectiveFilter>,
    },
    Ambient {
        #[serde(default)]
        pin: bool,
        #[serde(default)]
        filter: Option<DirectiveFilter>,
    },
    /// B17: a page of the allow-list, by id and never by URL.
    Page {
        page: crate::wall_pages::PageRef,
        #[serde(default)]
        pin: bool,
        #[serde(default)]
        filter: Option<DirectiveFilter>,
    },
}

impl DirectiveBody {
    /// The `mission` directive that a display shows when none is stored.
    #[must_use]
    pub fn mission() -> Self {
        Self::Mission {
            pin: false,
            filter: None,
        }
    }

    #[must_use]
    pub fn mode(&self) -> DirectiveMode {
        match self {
            Self::Mission { .. } => DirectiveMode::Mission,
            Self::Focus { .. } => DirectiveMode::Focus,
            Self::Review { .. } => DirectiveMode::Review,
            Self::Race { .. } => DirectiveMode::Race,
            Self::Artifact { .. } => DirectiveMode::Artifact,
            Self::Ambient { .. } => DirectiveMode::Ambient,
            Self::Page { .. } => DirectiveMode::Page,
        }
    }

    /// The filter, when the directive has one.
    #[must_use]
    pub fn filter(&self) -> Option<&DirectiveFilter> {
        match self {
            Self::Mission { filter, .. }
            | Self::Focus { filter, .. }
            | Self::Review { filter, .. }
            | Self::Race { filter, .. }
            | Self::Artifact { filter, .. }
            | Self::Ambient { filter, .. }
            | Self::Page { filter, .. } => filter.as_ref(),
        }
    }
}

/// `GET /api/displays/self/directive`: the validated directive and its
/// generation. A display without a stored directive reads `mission`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct DirectiveView {
    pub generation: u64,
    pub directive: DirectiveBody,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_mode_takes_only_its_own_fields() {
        let ok = r#"{"mode":"focus","session":{"station_id":"s","session_id":"x"},"pin":true}"#;
        assert!(serde_json::from_str::<DirectiveBody>(ok).is_ok());
        for bad in [
            r#"{"mode":"focus"}"#,
            r#"{"mode":"mission","session":{"station_id":"s","session_id":"x"}}"#,
            r#"{"mode":"race","batch_id":"b","artifact":{"station_id":"s","artifact_id":"a","version":1}}"#,
            r#"{"mode":"artifact","artifact":{"station_id":"s","artifact_id":"a"}}"#,
            r#"{"mode":"other"}"#,
            r#"{"mode":"mission","url":"https://x"}"#,
        ] {
            assert!(serde_json::from_str::<DirectiveBody>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn page_takes_only_its_own_fields() {
        let ok = r#"{"mode":"page","page":{"page_id":"01J0000000000000000000000A"},"pin":true}"#;
        let parsed: DirectiveBody = serde_json::from_str(ok).unwrap();
        assert_eq!(parsed.mode(), DirectiveMode::Page);
        assert!(parsed.filter().is_none());
        for bad in [
            r#"{"mode":"page"}"#,
            r#"{"mode":"page","page":{"page_id":"a"},"url":"https://x"}"#,
            r#"{"mode":"page","page":{"page_id":"a","url":"https://x"}}"#,
            r#"{"mode":"page","page":{"page_id":"a"},"session":{"station_id":"s","session_id":"x"}}"#,
            r#"{"mode":"mission","page":{"page_id":"a"}}"#,
        ] {
            assert!(serde_json::from_str::<DirectiveBody>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn mission_is_the_default_and_has_a_mode() {
        let m = DirectiveBody::mission();
        assert_eq!(m.mode(), DirectiveMode::Mission);
        assert!(m.filter().is_none());
        let parsed: DirectiveBody = serde_json::from_str(r#"{"mode":"mission"}"#).unwrap();
        assert_eq!(parsed, m);
    }
}
