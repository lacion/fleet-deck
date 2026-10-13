//! TypeScript bindings for the board (PLAN §11, §12: ts-rs).
//!
//! `export_all` writes every wire type and an `index.ts` into a directory.
//! The committed copy lives in `web/src/gen/`; the drift test
//! (`tests/ts_drift.rs`) regenerates it into a temporary directory and fails
//! if a file differs, is missing, or is extra. Regenerate with
//! `make ts-types`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ts_rs::{Config, TS};

use crate::artifacts::{
    Artifact, ArtifactDetail, ArtifactFile, ArtifactFileQuery, ArtifactGetQuery, ArtifactKind,
    ArtifactList, ArtifactListQuery, ArtifactPublish, ArtifactPublishRequest, ArtifactPublished,
    ArtifactVersion, DeckArtifact, DeckArtifactDetail, ShareLink, ShareRequest, ShareView,
};
use crate::board::{
    AddStationRequest, ArchiveChange, ArchiveItem, ArchiveList, ArchiveRef, BoardClientMessage,
    BoardServerMessage, CommandRequest, DeckCommand, DeckSession, DeviceLink, DeviceLinkEntry,
    FleetExtras, PairingInvitation, SessionKey, StationView, WsTicket,
};
use crate::capability::{Capabilities, Operation, Unsupported};
use crate::command::{CommandEnvelope, DeckCommandState, Receipt};
use crate::conflicts::FileConflict;
use crate::event::{Event, EventEnvelope};
use crate::files::{
    FileContent, FileListQuery, FileListing, FileReadQuery, FileSearch, FileSearchQuery, TurnDiff,
    TurnDiffQuery,
};
use crate::link::{
    ControlFrame, PasteImage, PastedImage, Preflight, PreflightQuery, ReceiptQuery, ReplayParams,
    Scope, StationInfo, StreamMode, StreamStart, WorkspaceListing, WorkspaceQuery, WorktreeList,
    WorktreesQuery,
};
use crate::mutation::{
    CoordinatorCallEnvelope, CoordinatorQueryEnvelope, KeyRotationEnvelope,
    StationConfigureEnvelope,
};
use crate::orchestration::{
    AssignOutcome, AssignRequest, GrantView, MailRequest, MailView, TaskCompletion, TaskRequest,
    TaskView,
};
use crate::plans::{DeckPlan, PlanMarkRequest, PlanStatus};
use crate::session::{CardStatus, SessionSummary};
use crate::version::{Negotiated, VersionOffer};

/// The ts-rs configuration: 64-bit integers are `number` (sequence numbers
/// and Unix ms stay below 2^53).
#[must_use]
pub fn config(out_dir: &Path) -> Config {
    Config::new().with_large_int("number").with_out_dir(out_dir)
}

/// Writes all bindings and `index.ts` into `out_dir`.
pub fn export_all(out_dir: &Path) -> io::Result<()> {
    let cfg = config(out_dir);
    let results = [
        EventEnvelope::export_all(&cfg),
        Event::export_all(&cfg),
        Capabilities::export_all(&cfg),
        Operation::export_all(&cfg),
        Unsupported::export_all(&cfg),
        CommandEnvelope::export_all(&cfg),
        StationConfigureEnvelope::export_all(&cfg),
        KeyRotationEnvelope::export_all(&cfg),
        CoordinatorCallEnvelope::export_all(&cfg),
        CoordinatorQueryEnvelope::export_all(&cfg),
        crate::shell::ShellEnvelope::export_all(&cfg),
        crate::shell::ShellInfo::export_all(&cfg),
        crate::shell::ShellAttach::export_all(&cfg),
        Receipt::export_all(&cfg),
        DeckCommandState::export_all(&cfg),
        CommandRequest::export_all(&cfg),
        DeckCommand::export_all(&cfg),
        DeckSession::export_all(&cfg),
        FleetExtras::export_all(&cfg),
        StationView::export_all(&cfg),
        SessionKey::export_all(&cfg),
        BoardServerMessage::export_all(&cfg),
        BoardClientMessage::export_all(&cfg),
        WsTicket::export_all(&cfg),
        DeviceLink::export_all(&cfg),
        ArchiveRef::export_all(&cfg),
        ArchiveList::export_all(&cfg),
        ArchiveItem::export_all(&cfg),
        ArchiveChange::export_all(&cfg),
        DeviceLinkEntry::export_all(&cfg),
        PairingInvitation::export_all(&cfg),
        AddStationRequest::export_all(&cfg),
        ControlFrame::export_all(&cfg),
        Scope::export_all(&cfg),
        ReplayParams::export_all(&cfg),
        StreamMode::export_all(&cfg),
        StreamStart::export_all(&cfg),
        StationInfo::export_all(&cfg),
        WorkspaceQuery::export_all(&cfg),
        WorkspaceListing::export_all(&cfg),
        WorktreesQuery::export_all(&cfg),
        PreflightQuery::export_all(&cfg),
        Preflight::export_all(&cfg),
        PasteImage::export_all(&cfg),
        PastedImage::export_all(&cfg),
        WorktreeList::export_all(&cfg),
        ReceiptQuery::export_all(&cfg),
        FileListQuery::export_all(&cfg),
        FileListing::export_all(&cfg),
        FileReadQuery::export_all(&cfg),
        FileContent::export_all(&cfg),
        FileSearchQuery::export_all(&cfg),
        FileSearch::export_all(&cfg),
        TurnDiffQuery::export_all(&cfg),
        TurnDiff::export_all(&cfg),
        SessionSummary::export_all(&cfg),
        CardStatus::export_all(&cfg),
        VersionOffer::export_all(&cfg),
        Negotiated::export_all(&cfg),
        AssignRequest::export_all(&cfg),
        AssignOutcome::export_all(&cfg),
        MailRequest::export_all(&cfg),
        MailView::export_all(&cfg),
        TaskRequest::export_all(&cfg),
        TaskCompletion::export_all(&cfg),
        TaskView::export_all(&cfg),
        GrantView::export_all(&cfg),
        DeckPlan::export_all(&cfg),
        FileConflict::export_all(&cfg),
        PlanStatus::export_all(&cfg),
        PlanMarkRequest::export_all(&cfg),
        Artifact::export_all(&cfg),
        ArtifactKind::export_all(&cfg),
        ArtifactVersion::export_all(&cfg),
        ArtifactPublish::export_all(&cfg),
        ArtifactPublished::export_all(&cfg),
        ArtifactListQuery::export_all(&cfg),
        ArtifactList::export_all(&cfg),
        ArtifactGetQuery::export_all(&cfg),
        ArtifactDetail::export_all(&cfg),
        ArtifactFileQuery::export_all(&cfg),
        ArtifactFile::export_all(&cfg),
        ArtifactPublishRequest::export_all(&cfg),
        DeckArtifact::export_all(&cfg),
        DeckArtifactDetail::export_all(&cfg),
        ShareRequest::export_all(&cfg),
        ShareLink::export_all(&cfg),
        ShareView::export_all(&cfg),
    ];
    for result in results {
        result.map_err(io::Error::other)?;
    }
    write_index(out_dir)
}

/// Every `.ts` file under `dir`, relative and sorted, without `index.ts`.
pub fn binding_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    collect(dir, dir, &mut out)?;
    out.retain(|p| p != Path::new("index.ts"));
    out.sort();
    Ok(out)
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(root, &path, out)?;
        } else if path.extension().is_some_and(|e| e == "ts") {
            let rel = path.strip_prefix(root).map_err(io::Error::other)?;
            out.push(rel.to_path_buf());
        }
    }
    Ok(())
}

fn write_index(out_dir: &Path) -> io::Result<()> {
    let mut text =
        String::from("// Generated by fleetdeck-proto (`make ts-types`). Do not edit this file.\n");
    for file in binding_files(out_dir)? {
        let module = file.with_extension("");
        let module = module.to_string_lossy().replace('\\', "/");
        text.push_str(&format!("export * from \"./{module}\";\n"));
    }
    fs::write(out_dir.join("index.ts"), text)
}

/// Compares two binding directories. Returns one line per difference.
pub fn diff_dirs(expected: &Path, actual: &Path) -> io::Result<Vec<String>> {
    let mut problems = Vec::new();
    let want = all_files(expected)?;
    let have = if actual.is_dir() {
        all_files(actual)?
    } else {
        Vec::new()
    };
    for file in &want {
        if !have.contains(file) {
            problems.push(format!("missing: {}", file.display()));
        } else if fs::read(expected.join(file))? != fs::read(actual.join(file))? {
            problems.push(format!("stale: {}", file.display()));
        }
    }
    for file in &have {
        if !want.contains(file) {
            problems.push(format!("extra: {}", file.display()));
        }
    }
    Ok(problems)
}

fn all_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = binding_files(dir)?;
    if dir.join("index.ts").is_file() {
        files.push(PathBuf::from("index.ts"));
    }
    Ok(files)
}
