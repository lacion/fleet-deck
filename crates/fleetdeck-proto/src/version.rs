//! Protocol versions and feature flags (PLAN §8.2 steps 2 and 5, L16).
//!
//! Behaviour depends on the negotiated feature set, not only on the version
//! number. Old fields stay for old peers; a new behaviour gets a new flag.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The oldest protocol version that this build accepts.
pub const PROTO_MIN: u32 = 1;
/// The newest protocol version that this build speaks.
pub const PROTO_MAX: u32 = 1;

/// Link and board feature flags of this build.
pub mod feature {
    /// Request / Response / Event / Ack / Ping on the control channel.
    pub const CONTROL: &str = "control.v1";
    /// Multiplexed byte streams with credit flow control on the data channel.
    pub const DATA_STREAMS: &str = "data_streams.v1";
    /// Stored accept/reject receipts for re-sent `command_id`s (§8.4).
    pub const COMMAND_RECEIPTS: &str = "command_receipts.v1";
    /// Snapshot batches when event replay is not possible (G4).
    pub const SNAPSHOTS: &str = "snapshots.v1";
    /// `session.revert`: conversation rollback plus workspace restore (PLAN
    /// §6.6, M3 DL-087). A peer without it never gets the method.
    pub const REVERT: &str = "revert.v1";
    /// `session.rename` (M3, DL-095).
    pub const RENAME: &str = "rename.v1";
    /// `session.set_auto_continue` (L19, ADR-007, M3 DL-127).
    pub const AUTO_CONTINUE: &str = "auto_continue.v1";
    /// `session.import`: a driven session that continues an existing native
    /// CLI session (M3 6-12, DL-308).
    pub const IMPORT: &str = "import.v1";
    /// `files.list`, `files.read`, `files.search` and `files.diff`: the
    /// read-only file service (PLAN §6.6, M3 DL-088).
    pub const FILES: &str = "files.v1";
    /// `shell.open`, `shell.resize`, `shell.close`, `shell.list` and
    /// `shell.attach`: the human shell (PLAN §6.8, M3 DL-404).
    pub const SHELL: &str = "shell.v1";
    /// `shell.handover` and `shell.release`: the TUI handover (DL-140).
    pub const SHELL_HANDOVER: &str = "shell.handover.v1";
    /// `session.coordinator_renew` and `session.coordinator_revoke` (the
    /// deck's binding lifecycle commands) and `coordinator.query` and
    /// `coordinator.call` (ADR-006 §6.1 `coordinator_v1`, M3 DL-212,
    /// DL-214).
    pub const COORDINATOR: &str = "coordinator.v1";
    /// `workspace.worktrees`: the read-only inventory of the worktrees that
    /// the station created for sessions (DL-142).
    pub const WORKTREES: &str = "workspace.worktrees.v1";
    /// `workspace.preflight`: the Git access check of a clone URL (DL-144).
    pub const PREFLIGHT: &str = "workspace.preflight.v1";
    /// `files.write_temp`: store a pasted image in the station's paste
    /// directory (DL-147).
    pub const WRITE_TEMP: &str = "files.write_temp.v1";
    /// `session.worktree_trash`, `_restore`, `_purge` and `_release`: the
    /// worktree trash (DL-150).
    pub const TRASH: &str = "workspace.trash.v1";
    /// `workspace.precheck`: the shell precheck of a schedule, run on the
    /// station (R5, DL-168).
    pub const PRECHECK: &str = "workspace.precheck.v1";

    /// Every feature that this build offers.
    pub const ALL: &[&str] = &[
        CONTROL,
        DATA_STREAMS,
        COMMAND_RECEIPTS,
        SNAPSHOTS,
        REVERT,
        RENAME,
        FILES,
        SHELL,
        SHELL_HANDOVER,
        COORDINATOR,
        AUTO_CONTINUE,
        IMPORT,
        WORKTREES,
        PREFLIGHT,
        WRITE_TEMP,
        TRASH,
        PRECHECK,
    ];
    /// Features without which a link is refused.
    pub const REQUIRED: &[&str] = &[CONTROL];
}

/// What one side offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct VersionOffer {
    pub proto_min: u32,
    pub proto_max: u32,
    pub features: Vec<String>,
}

impl VersionOffer {
    /// The offer of this build.
    #[must_use]
    pub fn this_build() -> Self {
        Self {
            proto_min: PROTO_MIN,
            proto_max: PROTO_MAX,
            features: feature::ALL.iter().map(|f| (*f).to_owned()).collect(),
        }
    }
}

/// The result of a successful negotiation; both sides compute the same value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct Negotiated {
    pub version: u32,
    /// Sorted intersection of both feature lists.
    pub features: Vec<String>,
}

impl Negotiated {
    /// True if both sides offered `feature`.
    #[must_use]
    pub fn has(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }
}

/// Why a negotiation failed. Both sides report the same reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NegotiationError {
    #[error(
        "no common protocol version (local {local_min}..={local_max}, peer {peer_min}..={peer_max})"
    )]
    NoCommonVersion {
        local_min: u32,
        local_max: u32,
        peer_min: u32,
        peer_max: u32,
    },
    #[error("an offer has proto_min {min} above proto_max {max}")]
    InvalidRange { min: u32, max: u32 },
    #[error("required feature `{0}` is not offered by both sides")]
    MissingRequiredFeature(String),
}

impl NegotiationError {
    /// Stable reason code for `close` frames.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NoCommonVersion { .. } | Self::InvalidRange { .. } => "no_common_version",
            Self::MissingRequiredFeature(_) => "missing_required_feature",
        }
    }
}

/// Picks the highest common version and the common features (§8.2 step 5).
///
/// The function is symmetric: `negotiate(a, b) == negotiate(b, a)`.
pub fn negotiate(
    local: &VersionOffer,
    peer: &VersionOffer,
) -> Result<Negotiated, NegotiationError> {
    // Check in a fixed order so that both sides report the same error.
    let mut ranges = [
        (local.proto_min, local.proto_max),
        (peer.proto_min, peer.proto_max),
    ];
    ranges.sort_unstable();
    for offer in ranges.map(|(proto_min, proto_max)| VersionOffer {
        proto_min,
        proto_max,
        features: Vec::new(),
    }) {
        if offer.proto_min > offer.proto_max {
            return Err(NegotiationError::InvalidRange {
                min: offer.proto_min,
                max: offer.proto_max,
            });
        }
    }
    let low = local.proto_min.max(peer.proto_min);
    let high = local.proto_max.min(peer.proto_max);
    if low > high {
        // Report the ranges in a fixed order so both sides print the same text.
        let (a, b) = if (local.proto_min, local.proto_max) <= (peer.proto_min, peer.proto_max) {
            (local, peer)
        } else {
            (peer, local)
        };
        return Err(NegotiationError::NoCommonVersion {
            local_min: a.proto_min,
            local_max: a.proto_max,
            peer_min: b.proto_min,
            peer_max: b.proto_max,
        });
    }
    let ours: BTreeSet<&str> = local.features.iter().map(String::as_str).collect();
    let theirs: BTreeSet<&str> = peer.features.iter().map(String::as_str).collect();
    let common: Vec<String> = ours
        .intersection(&theirs)
        .map(|f| (*f).to_owned())
        .collect();
    for required in feature::REQUIRED {
        if !common.iter().any(|f| f == required) {
            return Err(NegotiationError::MissingRequiredFeature(
                (*required).to_owned(),
            ));
        }
    }
    Ok(Negotiated {
        version: high,
        features: common,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn offer(min: u32, max: u32, features: &[&str]) -> VersionOffer {
        VersionOffer {
            proto_min: min,
            proto_max: max,
            features: features.iter().map(|f| (*f).to_owned()).collect(),
        }
    }

    #[test]
    fn the_highest_common_version_and_the_common_features_win() {
        let a = offer(1, 3, &["control.v1", "snapshots.v1", "x.v1"]);
        let b = offer(2, 5, &["snapshots.v1", "control.v1"]);
        let n = negotiate(&a, &b).unwrap();
        assert_eq!(n.version, 3);
        assert_eq!(n.features, vec!["control.v1", "snapshots.v1"]);
        assert!(n.has("snapshots.v1"));
        assert!(!n.has("x.v1"));
    }

    #[test]
    fn no_common_version_fails_with_the_same_text_on_both_sides() {
        let a = offer(1, 1, &["control.v1"]);
        let b = offer(2, 2, &["control.v1"]);
        let ab = negotiate(&a, &b).unwrap_err();
        let ba = negotiate(&b, &a).unwrap_err();
        assert_eq!(ab, ba);
        assert_eq!(ab.reason(), "no_common_version");
    }

    #[test]
    fn a_missing_required_feature_fails() {
        let err = negotiate(&offer(1, 1, &["control.v1"]), &offer(1, 1, &[])).unwrap_err();
        assert_eq!(err.reason(), "missing_required_feature");
    }

    #[test]
    fn this_build_negotiates_with_itself() {
        let me = VersionOffer::this_build();
        let n = negotiate(&me, &me).unwrap();
        assert_eq!(n.version, PROTO_MAX);
        assert_eq!(n.features.len(), feature::ALL.len());
    }

    proptest! {
        #[test]
        fn negotiation_is_symmetric(
            a in (0u32..6, 0u32..6), b in (0u32..6, 0u32..6),
            fa in proptest::collection::vec(0usize..4, 0..4),
            fb in proptest::collection::vec(0usize..4, 0..4),
        ) {
            let pick = |ix: &Vec<usize>| ix.iter().map(|i| feature::ALL[*i]).collect::<Vec<_>>();
            let x = offer(a.0, a.1, &pick(&fa));
            let y = offer(b.0, b.1, &pick(&fb));
            prop_assert_eq!(negotiate(&x, &y), negotiate(&y, &x));
        }
    }
}
