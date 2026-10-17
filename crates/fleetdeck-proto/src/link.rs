//! Control channel frames (ADR-004 §2.6 with GATE-M1 G4, G6, G8).
//!
//! JSON text frames tagged by `type`. Handshake and pairing frames belong to
//! `fleetdeck-link`; the method table with scopes and roles is there too.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::capability::Capabilities;
use crate::event::{EventEnvelope, ProviderKind};
use crate::ids::{CommandId, IncarnationId, ProviderInstanceId, SessionId};
use crate::resources::StationResources;
use crate::session::SessionSummary;

/// The largest control frame in bytes (ADR-004 §2.6).
pub const MAX_CONTROL_FRAME_BYTES: usize = 1024 * 1024;

/// The most sessions in one snapshot batch (DL-051). A station sends every
/// session that has not exited and then the newest exited ones up to this
/// limit; a deck refuses a `snapshot_begin` above it and a
/// `snapshot_session` beyond the announced count, so an unfinished batch
/// holds at most this many summaries.
pub const MAX_SNAPSHOT_SESSIONS: u64 = 4096;

/// A typed error on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[error("{code}: {message}")]
#[ts(export_to = "link/")]
pub struct WireError {
    /// Stable code, for example `forbidden` or `unknown_method`.
    pub code: String,
    pub message: String,
    /// Typed details, for example a `Reject` or an `Unsupported` result.
    pub data: Option<Value>,
}

impl WireError {
    /// An error without details.
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            data: None,
        }
    }
}

/// The result of a request: `{ "ok": value }` or `{ "err": {...} }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "link/")]
pub enum Outcome {
    Ok(Value),
    Err(WireError),
}

/// A permission that a pairing grants to the peer key (PLAN §9, L15).
/// The method table of `fleetdeck-link` names the scope of each method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "link/")]
pub enum Scope {
    /// Read station state, sessions, events and artifacts.
    Read,
    /// Start, prompt, steer, interrupt, configure and stop sessions.
    Drive,
    /// Answer approvals and questions.
    Approve,
    /// Station configuration and key rotation.
    Admin,
    /// Station → deck: coordinator calls of agents on that station (L14).
    Coordinate,
}

impl Scope {
    /// Every scope, in table order.
    pub const ALL: [Self; 5] = [
        Self::Read,
        Self::Drive,
        Self::Approve,
        Self::Admin,
        Self::Coordinate,
    ];

    /// The wire name (`read`, `drive`, ...).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Drive => "drive",
            Self::Approve => "approve",
            Self::Admin => "admin",
            Self::Coordinate => "coordinate",
        }
    }
}

/// The projection of one session at a watermark (G4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct SessionSnapshot {
    pub summary: SessionSummary,
}

/// The lifecycle state of one coordinator binding in a snapshot
/// projection (ADR-006 §5.5, §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "link/")]
pub enum CoordinatorBindingPhase {
    Pending,
    Active,
    Expired,
    Revoked,
}

/// One coordinator binding of a coordinator-capable snapshot (ADR-006
/// §6.1): the binding fields and the lifecycle state at the watermark. No
/// secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "link/")]
pub struct CoordinatorBindingState {
    pub binding_id: String,
    pub grant_id: String,
    pub session_id: SessionId,
    pub generation: u32,
    pub provider_instance: String,
    /// `None` while the binding is pending.
    pub native_id: Option<String>,
    pub state: CoordinatorBindingPhase,
}

/// One control frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export_to = "link/")]
#[expect(
    clippy::large_enum_variant,
    reason = "a frame lives only between decode and dispatch; boxing every event \
              and snapshot would add an allocation per frame (DL-059)"
)]
pub enum ControlFrame {
    Request {
        /// Unique per direction, chosen by the sender.
        id: u64,
        /// Required for a mutating method.
        command_id: Option<CommandId>,
        method: String,
        params: Value,
    },
    Response {
        id: u64,
        #[serde(flatten)]
        outcome: Outcome,
    },
    /// Station → deck: one committed event.
    Event {
        incarnation: IncarnationId,
        #[serde(flatten)]
        envelope: EventEnvelope,
    },
    /// Deck → station after the read model committed `station_seq`.
    Ack {
        incarnation: IncarnationId,
        station_seq: u64,
    },
    /// Station → deck: a snapshot batch starts (needs `snapshots.v1`).
    SnapshotBegin {
        incarnation: IncarnationId,
        /// Every event up to this `station_seq` is in the snapshot.
        watermark: u64,
        session_count: u64,
    },
    SnapshotSession {
        incarnation: IncarnationId,
        snapshot: SessionSnapshot,
    },
    /// The batch is complete; sessions that the deck has but the batch did
    /// not name are removed.
    SnapshotEnd {
        incarnation: IncarnationId,
        watermark: u64,
        /// The coordinator binding projection at the watermark (ADR-006
        /// §6.1, needs `coordinator.v1`). `None`: an ordinary summary
        /// snapshot, which never opens the coordinator lifecycle fence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        coordinator: Option<Vec<CoordinatorBindingState>>,
    },
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    /// The last frame before the socket closes.
    Close {
        reason: String,
        detail: String,
    },
}

/// `events.replay` params: where the deck's read model stands for this
/// station (G4). `incarnation` is `None` for a deck that never synced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "link/")]
pub struct ReplayParams {
    pub incarnation: Option<IncarnationId>,
    /// The deck's cursor: the highest `station_seq` that it applied.
    pub after: u64,
}

/// How a station continues the event stream of a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "link/")]
pub enum StreamMode {
    /// `event` frames after the deck's cursor.
    Replay,
    /// A snapshot batch first (other incarnation, cursor older than the
    /// retained history, cursor ahead, or `session.snapshot`), then events
    /// after its watermark.
    Snapshot,
}

/// The response to `events.replay` and `session.snapshot`. The frames of
/// the stream follow the response on the same channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct StreamStart {
    pub mode: StreamMode,
    pub incarnation: IncarnationId,
    /// The replay cursor, or the snapshot watermark.
    pub from: u64,
}

/// The response to `station.info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct StationInfo {
    pub name: String,
    pub incarnation: IncarnationId,
    /// The committed head `station_seq`.
    pub head: u64,
    pub live_sessions: u64,
    /// The station build version (M2 P4-2). Absent from a station built
    /// before P4-2 (L16: new fields are optional).
    #[serde(default)]
    pub version: Option<String>,
    /// `max_sessions` of `station.toml`: the session capacity.
    #[serde(default)]
    pub max_sessions: Option<u32>,
    /// `allow_shell` of `station.toml`: the human shell and the TUI
    /// handover are allowed (DL-141). Absent from an older station.
    #[serde(default)]
    pub allow_shell: Option<bool>,
    /// `allow_setup_cmd` of `station.toml`: a session start can run a setup
    /// command (DL-148). Absent from an older station.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub allow_setup_cmd: Option<bool>,
    /// The configured provider instances with their probed versions.
    #[serde(default)]
    pub providers: Vec<StationProvider>,
    /// The tokens that the station's model help used today (DL-167, R10).
    /// Absent from a station without model help, or with it switched off.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub assist_tokens_today: Option<u64>,
    /// The daily token cap of the station's model help. Absent when the
    /// model help is off.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub assist_cap: Option<u64>,
    /// The disk, memory and CPU of the station host (DL-193). Absent from an
    /// older station.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resources: Option<StationResources>,
    /// The desktop of the station (R13 §6.2): present when a desktop is
    /// detected, also with `allow_desktop` off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub desktop: Option<crate::desktop::DesktopInfo>,
    /// The traits of the station for placement (FG-6). Absent from an older
    /// station.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub traits: Option<crate::fleet_git::StationTraits>,
    /// The hashes of the provider config files (FG-11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub config: Option<crate::fleet_git::ConfigDigest>,
    /// The Taildrive shares of the station (FG-12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub taildrive: Option<crate::fleet_git::TaildriveReport>,
    /// The Fleet Git settings of the station (FG-5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fleet_git: Option<crate::fleet_git::FleetGitInfo>,
}

/// One configured provider instance of a station (`[[provider_instance]]`,
/// L10) as `station.info` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct StationProvider {
    pub instance: ProviderInstanceId,
    pub provider: ProviderKind,
    /// The CLI version from the provider probe; `None` when the probe
    /// failed or the station has no adapter for the provider yet.
    pub version: Option<String>,
    /// The sign-in state of the probe: `signed_in`, `signed_out` or
    /// `unknown`.
    pub auth: String,
    /// The capabilities that a new session of this instance starts with,
    /// under the station's settings (M2 P5-3, DL-065). The spawn form
    /// offers their modes. `None` when the station has no adapter for the
    /// instance, or from a station before P5-3.
    #[serde(default)]
    pub capabilities: Option<Capabilities>,
    /// How the probed version compares with the adapter's tested minimum and
    /// its deny list (R8.2, DL-162). `unknown` from an older station.
    #[serde(default)]
    pub version_status: VersionStatus,
}

/// The verdict of the provider version check (R8.2, DL-162).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "link/")]
pub enum VersionStatus {
    /// At or above the oldest version that the fixtures and smoke cover.
    Ok,
    /// Older than the tested minimum: a warning, not a refusal.
    OlderThanTested,
    /// A version that is known to break the protocol: starts are refused.
    Broken,
    /// No version, or a version that does not parse.
    #[default]
    #[serde(other)]
    Unknown,
}

/// `workspace.list` params (M2 P4-4, DL-061): the directory to list; `None`
/// lists the station's allowed roots.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "link/")]
pub struct WorkspaceQuery {
    #[serde(default)]
    pub path: Option<String>,
}

/// The `workspace.list` answer: the sub-directories of `path` that a session
/// may use, sorted by name. Hidden directories and entries that resolve
/// outside the allowed roots are left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct WorkspaceListing {
    /// The canonical directory; `None` for the list of roots.
    pub path: Option<String>,
    /// The directory above `path` inside the allowed roots; `None` at a root.
    pub parent: Option<String>,
    pub entries: Vec<WorkspaceEntry>,
    /// More entries exist than the answer holds.
    pub truncated: bool,
}

/// One directory of a [`WorkspaceListing`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct WorkspaceEntry {
    pub name: String,
    /// The canonical path; use it as `SessionSpec.cwd`.
    pub path: String,
    /// The directory holds a `.git` entry.
    pub repository: bool,
    /// Repo catalog (M3 6-1, DL-201): the `origin` URL without credentials,
    /// query or fragment; `None` without an origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub origin: Option<String>,
    /// The default branch (`origin/HEAD`, else the current branch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub default_branch: Option<String>,
    /// The conflict-radar group: the owner's `repo_groups` choice, else the
    /// normalized origin of a repository with exactly one remote; `None`
    /// for several or no remotes (PLAN §6.6: a heuristic, not an identity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub group: Option<String>,
}

/// `workspace.preflight` params (DL-144): the Git URL that a clone would use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "link/")]
pub struct PreflightQuery {
    pub url: String,
}

/// The `workspace.preflight` answer (DL-144): whether the station can read
/// the remote with its own Git credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct Preflight {
    pub ok: bool,
    /// The scrubbed end of the Git error text (at most 2000 characters);
    /// empty when `ok`.
    pub message: String,
    /// What the owner can do about the failure, when the station knows.
    pub hint: Option<String>,
}

/// `workspace.precheck` params (R5, DL-168): the shell command of a
/// schedule and the folder where it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "link/")]
pub struct PrecheckQuery {
    pub cwd: String,
    pub cmd: String,
}

/// The `workspace.precheck` answer (R5, DL-168): how the command ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct PrecheckResult {
    /// The exit code; `None` when a signal ended the command or it timed
    /// out.
    pub exit_code: Option<i32>,
    /// The command ran longer than the 60 s limit and was stopped.
    pub timed_out: bool,
    /// The end of the output (stdout and stderr together), scrubbed.
    pub output_tail: String,
}

/// `files.write_temp` params (DL-147): one pasted image. The `upload_id`
/// makes a retry return the same path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "link/")]
pub struct PasteImage {
    /// 8 to 64 characters of `[A-Za-z0-9_-]`.
    pub upload_id: String,
    /// `png`, `jpg`, `gif` or `webp`.
    pub ext: String,
    /// The image bytes in standard base64.
    pub data_base64: String,
}

/// The `files.write_temp` answer (DL-147): the absolute path of the stored
/// image on the station.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct PastedImage {
    pub path: String,
}

/// `workspace.worktrees` params (DL-142): none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "link/")]
pub struct WorktreesQuery {}

/// One worktree that the station created for a session (DL-142). Read only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct WorktreeInfo {
    pub session_id: SessionId,
    #[serde(default)]
    pub title: Option<String>,
    /// The path recorded in the session summary.
    pub path: String,
    /// `git rev-parse --abbrev-ref HEAD` in the path; `None` when Git failed
    /// or HEAD is detached.
    #[serde(default)]
    pub branch: Option<String>,
    /// The main checkout: the first entry of `git worktree list --porcelain`.
    #[serde(default)]
    pub main: Option<String>,
    pub exists: bool,
    /// `git worktree list --porcelain` names this path.
    pub registered: bool,
    /// Changed, staged and untracked entries (DL-143).
    #[serde(default)]
    pub dirty: Option<u32>,
    /// Ignored entries (`git ls-files --others --ignored`).
    #[serde(default)]
    pub ignored: Option<u32>,
    /// Commits of the branch on no fetched remote ref (`git rev-list --count
    /// HEAD --not --remotes`); without any remote ref: commits that are not
    /// on the HEAD of the main checkout.
    #[serde(default)]
    pub unpushed: Option<u32>,
    /// `remotes` or `main`: what `unpushed` compares with.
    #[serde(default)]
    pub compared_with: Option<String>,
    #[serde(default)]
    pub last_commit: Option<WorktreeCommit>,
    /// The session has a process, a current run, or a terminal hold.
    pub live: bool,
    pub verdict: WorktreeVerdict,
    /// Why the verdict is `unknown`: the Git error, a timeout, `outside the
    /// allowed roots`.
    #[serde(default)]
    pub note: Option<String>,
    /// The trash operation of this worktree, while one is open or failed
    /// (DL-150).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub trash: Option<WorktreeTrash>,
}

/// The state of the trash operation of a worktree (DL-150).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct WorktreeTrash {
    /// `trashing`, `trashed`, `restoring`, `purging` or `failed`.
    pub state: String,
    /// With `failed`: `trash`, `restore` or `purge`.
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    pub trash_path: String,
    /// Unix ms.
    #[ts(type = "number")]
    pub trashed_at: i64,
}

/// The newest commit of a worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct WorktreeCommit {
    pub subject: String,
    /// Unix ms.
    pub at: i64,
}

/// What the station knows about the work in a worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "link/")]
pub enum WorktreeVerdict {
    Missing,
    Live,
    HasWork,
    Clean,
    Unknown,
    /// The folder is in the trash, or a move runs (DL-150).
    Trashed,
}

/// The `workspace.worktrees` answer: newest session first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct WorktreeList {
    pub worktrees: Vec<WorktreeInfo>,
}

/// `command.receipt` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "link/")]
pub struct ReceiptQuery {
    pub command_id: CommandId,
}

impl ControlFrame {
    /// The `type` tag of the frame.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Request { .. } => "request",
            Self::Response { .. } => "response",
            Self::Event { .. } => "event",
            Self::Ack { .. } => "ack",
            Self::SnapshotBegin { .. } => "snapshot_begin",
            Self::SnapshotSession { .. } => "snapshot_session",
            Self::SnapshotEnd { .. } => "snapshot_end",
            Self::Ping { .. } => "ping",
            Self::Pong { .. } => "pong",
            Self::Close { .. } => "close",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use serde_json::json;

    fn round_trip(frame: &ControlFrame) -> Value {
        let v = serde_json::to_value(frame).unwrap();
        let back: ControlFrame = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(&back, frame);
        assert_eq!(v["type"], frame.type_name());
        v
    }

    #[test]
    fn responses_have_ok_or_err_beside_the_id() {
        let ok = ControlFrame::Response {
            id: 4,
            outcome: Outcome::Ok(json!({"a": 1})),
        };
        assert_eq!(
            round_trip(&ok),
            json!({"type": "response", "id": 4, "ok": {"a": 1}})
        );
        let err = ControlFrame::Response {
            id: 5,
            outcome: Outcome::Err(WireError::new("forbidden", "no")),
        };
        assert_eq!(
            round_trip(&err),
            json!({"type": "response", "id": 5, "err": {"code": "forbidden", "message": "no", "data": null}})
        );
    }

    #[test]
    fn events_flatten_the_envelope_and_carry_the_incarnation() {
        let frame = ControlFrame::Event {
            incarnation: "inc-1".into(),
            envelope: EventEnvelope {
                station_seq: 9,
                session_id: "s1".into(),
                session_seq: 2,
                at: 1,
                proto_version: 1,
                raw_ref: None,
                event: Event::ModelChanged { model: "m".into() }.into(),
            },
        };
        let v = round_trip(&frame);
        assert_eq!(v["incarnation"], "inc-1");
        assert_eq!(v["station_seq"], 9);
        assert_eq!(v["event"]["kind"], "model.changed");
    }

    #[test]
    fn every_other_frame_round_trips() {
        let summary = crate::session::SessionSummary::new("s1".into());
        for frame in [
            ControlFrame::Request {
                id: 1,
                command_id: Some("01J0000000000000000000000A".into()),
                method: "session.interrupt".into(),
                params: json!({"session_id": "s1"}),
            },
            ControlFrame::Ack {
                incarnation: "i".into(),
                station_seq: 3,
            },
            ControlFrame::SnapshotBegin {
                incarnation: "i".into(),
                watermark: 3,
                session_count: 1,
            },
            ControlFrame::SnapshotSession {
                incarnation: "i".into(),
                snapshot: SessionSnapshot { summary },
            },
            ControlFrame::SnapshotEnd {
                incarnation: "i".into(),
                watermark: 3,
                coordinator: None,
            },
            ControlFrame::Ping { nonce: 1 },
            ControlFrame::Pong { nonce: 1 },
            ControlFrame::Close {
                reason: "superseded".into(),
                detail: String::new(),
            },
        ] {
            round_trip(&frame);
        }
    }

    #[test]
    fn a_provider_from_a_station_before_p5_3_has_no_capabilities() {
        let old =
            json!({"instance": "codex", "provider": "codex", "version": null, "auth": "unknown"});
        let p: StationProvider = serde_json::from_value(old).unwrap();
        assert_eq!(p.capabilities, None);
        assert_eq!(p.version_status, VersionStatus::Unknown);
        let new = StationProvider {
            capabilities: Some(Capabilities::minimal()),
            ..p
        };
        let back: StationProvider =
            serde_json::from_value(serde_json::to_value(&new).unwrap()).unwrap();
        assert_eq!(back, new);
    }

    #[test]
    fn the_version_status_has_snake_case_names_and_an_unknown_fallback() {
        let all = [
            (VersionStatus::Ok, "ok"),
            (VersionStatus::OlderThanTested, "older_than_tested"),
            (VersionStatus::Broken, "broken"),
            (VersionStatus::Unknown, "unknown"),
        ];
        for (status, name) in all {
            assert_eq!(serde_json::to_value(status).unwrap(), json!(name));
            let back: VersionStatus = serde_json::from_value(json!(name)).unwrap();
            assert_eq!(back, status);
        }
        let newer: VersionStatus = serde_json::from_value(json!("from_the_future")).unwrap();
        assert_eq!(newer, VersionStatus::Unknown);
    }
}
