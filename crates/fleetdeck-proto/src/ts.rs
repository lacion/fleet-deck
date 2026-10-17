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

use crate::accounts as acc;
use crate::artifacts::{
    Artifact, ArtifactDetail, ArtifactFile, ArtifactFileQuery, ArtifactGetQuery, ArtifactKind,
    ArtifactList, ArtifactListQuery, ArtifactPublish, ArtifactPublishRequest, ArtifactPublished,
    ArtifactVersion, DeckArtifact, DeckArtifactDetail, ShareLink, ShareRequest, ShareView,
    ViewLink, ViewRequest,
};
use crate::assist::{AssistAnswer, AssistKind, AssistRequest, AssistRoute, AssistSession};
use crate::automation::{Schedule, ScheduleInput, ScheduleRun, ScheduleRunState, ScheduleView};
use crate::board::{
    ActivityItem, ActivityKind, ActivityList, AddStationRequest, ArchiveChange, ArchiveItem,
    ArchiveList, ArchiveRef, BoardClientMessage, BoardServerMessage, CertTrust, CommandRequest,
    DeckCommand, DeckSession, DeviceLink, DeviceLinkEntry, FleetExtras, PairingInvitation,
    ReadMark, SessionKey, StationView, WsTicket,
};
use crate::branches::{BranchDiffQuery, BranchesQuery, RepoBranch, RepoBranches};
use crate::capability::{Capabilities, Operation, Unsupported};
use crate::command::{CommandEnvelope, DeckCommandState, Receipt};
use crate::conflicts::FileConflict;
use crate::desktop as dk;
use crate::desktop_authority::DesktopAuthority;
use crate::display::{
    DisplayBinding, DisplayLinkMade, DisplayList, DisplaySelf, DisplayView, PairingPoll,
    PairingStarted, PairingState, PendingPairing,
};
use crate::display_changes::{DisplayChanges, DisplayChangesQuery, DisplayFileChange};
use crate::display_directive::{
    ArtifactRef, DirectiveBody, DirectiveFilter, DirectiveMode, DirectiveView, SessionRef,
};
use crate::display_event::{
    DisplayDisk, DisplayEvent, DisplayItem, DisplayRequestState, DisplayResources,
    DisplaySessionLoad,
};
use crate::event::{Event, EventEnvelope};
use crate::execution::{ExecutionCheck, ExecutionCheckQuery, ProcessInfo};
use crate::files::{
    CheckpointRef, FileContent, FileListQuery, FileListing, FileReadQuery, FileSearch,
    FileSearchQuery, SessionDiff, SessionDiffQuery, TurnDiff, TurnDiffQuery,
};
use crate::fleet_summary::{
    DeckState, FleetSummary, NeedsYouItem, SummaryCounts, SummaryStation, SummaryUsage,
};
use crate::history::{
    HistoryItem, HistoryKind, HistoryState, HistoryTool, HistoryUnavailable, SessionHistory,
    SessionHistoryQuery, ToolResult,
};
use crate::integrations as it;
use crate::link::{
    ControlFrame, PasteImage, PastedImage, PrecheckQuery, PrecheckResult, Preflight,
    PreflightQuery, ReceiptQuery, ReplayParams, Scope, StationInfo, StreamMode, StreamStart,
    WorkspaceListing, WorkspaceQuery, WorktreeList, WorktreesQuery,
};
use crate::mutation::{
    CoordinatorCallEnvelope, CoordinatorQueryEnvelope, KeyRotationEnvelope,
    StationConfigureEnvelope,
};
use crate::native::{
    HomeState, ImportState, KnownSession, NativeHome, NativePreview, NativePreviewQuery,
    NativeSession, NativeSessionList, NativeSessionsQuery, TranscriptPin,
};
use crate::orchestration::{
    AssignOutcome, AssignRequest, GrantView, GroupMailRequest, MailRequest, MailView,
    TaskCompletion, TaskRequest, TaskView, TaskWaitAnswer,
};
use crate::palette::{PaletteColors, PaletteMode, StationPalette};
use crate::plans::{DeckPlan, PlanMarkRequest, PlanStatus};
use crate::resources::{
    CpuLoad, DiskSpace, MemoryUse, ResourceKind, ResourceWarning, SessionLoad, StationResources,
};
use crate::session::{CardStatus, SessionSummary};
use crate::settings::{
    Apply, Durability, FieldKind, FieldMeta, RestartAnswer, RestartItem, RestartKind,
    RestartRequest, SecretSet, SecretSetParams, SettingsChange, SettingsSaved, SettingsSetParams,
    SettingsView,
};
use crate::share::{
    ChatMessage, ChatPost, DriveGrant, DriverState, JoinRequest, NewShare, NewSpace, ShareCreated,
    ShareHistory, ShareInfo, ShareKind, ShareRole, SharedScope, SpaceSession, SpaceView, Steering,
};
use crate::subagent::{SubagentLog, SubagentQuery};
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
        CertTrust::export_all(&cfg),
        PairingInvitation::export_all(&cfg),
        AddStationRequest::export_all(&cfg),
        ControlFrame::export_all(&cfg),
        Scope::export_all(&cfg),
        ReplayParams::export_all(&cfg),
        StreamMode::export_all(&cfg),
        StreamStart::export_all(&cfg),
        StationInfo::export_all(&cfg),
        dk::DesktopKind::export_all(&cfg),
        dk::MonitorInfo::export_all(&cfg),
        dk::WallWindow::export_all(&cfg),
        dk::DesktopAllowed::export_all(&cfg),
        dk::DesktopTools::export_all(&cfg),
        dk::DesktopInfo::export_all(&cfg),
        dk::Urgency::export_all(&cfg),
        dk::DesktopAction::export_all(&cfg),
        dk::DesktopActResult::export_all(&cfg),
        dk::DesktopActBody::export_all(&cfg),
        dk::DesktopInfoQuery::export_all(&cfg),
        DesktopAuthority::export_all(&cfg),
        StationResources::export_all(&cfg),
        DiskSpace::export_all(&cfg),
        MemoryUse::export_all(&cfg),
        CpuLoad::export_all(&cfg),
        SessionLoad::export_all(&cfg),
        ResourceKind::export_all(&cfg),
        ResourceWarning::export_all(&cfg),
        WorkspaceQuery::export_all(&cfg),
        WorkspaceListing::export_all(&cfg),
        WorktreesQuery::export_all(&cfg),
        PreflightQuery::export_all(&cfg),
        Preflight::export_all(&cfg),
        PrecheckQuery::export_all(&cfg),
        PrecheckResult::export_all(&cfg),
        Schedule::export_all(&cfg),
        ScheduleRun::export_all(&cfg),
        ScheduleRunState::export_all(&cfg),
        ScheduleView::export_all(&cfg),
        ScheduleInput::export_all(&cfg),
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
        CheckpointRef::export_all(&cfg),
        TurnDiffQuery::export_all(&cfg),
        TurnDiff::export_all(&cfg),
        SessionDiffQuery::export_all(&cfg),
        SessionDiff::export_all(&cfg),
        SessionHistoryQuery::export_all(&cfg),
        SessionHistory::export_all(&cfg),
        HistoryItem::export_all(&cfg),
        HistoryKind::export_all(&cfg),
        HistoryState::export_all(&cfg),
        HistoryTool::export_all(&cfg),
        HistoryUnavailable::export_all(&cfg),
        SubagentQuery::export_all(&cfg),
        SubagentLog::export_all(&cfg),
        NativeSessionsQuery::export_all(&cfg),
        NativeSessionList::export_all(&cfg),
        NativeHome::export_all(&cfg),
        HomeState::export_all(&cfg),
        NativeSession::export_all(&cfg),
        KnownSession::export_all(&cfg),
        ImportState::export_all(&cfg),
        TranscriptPin::export_all(&cfg),
        NativePreviewQuery::export_all(&cfg),
        NativePreview::export_all(&cfg),
        BranchesQuery::export_all(&cfg),
        RepoBranches::export_all(&cfg),
        RepoBranch::export_all(&cfg),
        BranchDiffQuery::export_all(&cfg),
        ToolResult::export_all(&cfg),
        SessionSummary::export_all(&cfg),
        ReadMark::export_all(&cfg),
        ActivityItem::export_all(&cfg),
        ActivityKind::export_all(&cfg),
        ActivityList::export_all(&cfg),
        CardStatus::export_all(&cfg),
        VersionOffer::export_all(&cfg),
        Negotiated::export_all(&cfg),
        AssignRequest::export_all(&cfg),
        AssignOutcome::export_all(&cfg),
        MailRequest::export_all(&cfg),
        MailView::export_all(&cfg),
        GroupMailRequest::export_all(&cfg),
        TaskRequest::export_all(&cfg),
        TaskCompletion::export_all(&cfg),
        TaskView::export_all(&cfg),
        TaskWaitAnswer::export_all(&cfg),
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
        DisplayView::export_all(&cfg),
        DisplayBinding::export_all(&cfg),
        SessionRef::export_all(&cfg),
        ArtifactRef::export_all(&cfg),
        DirectiveFilter::export_all(&cfg),
        DirectiveMode::export_all(&cfg),
        DirectiveBody::export_all(&cfg),
        DirectiveView::export_all(&cfg),
        DisplayChangesQuery::export_all(&cfg),
        DisplayFileChange::export_all(&cfg),
        DisplayChanges::export_all(&cfg),
        DisplayRequestState::export_all(&cfg),
        DisplayItem::export_all(&cfg),
        DisplayEvent::export_all(&cfg),
        DisplayDisk::export_all(&cfg),
        DisplaySessionLoad::export_all(&cfg),
        DisplayResources::export_all(&cfg),
        PendingPairing::export_all(&cfg),
        DisplayList::export_all(&cfg),
        PairingStarted::export_all(&cfg),
        PairingState::export_all(&cfg),
        PairingPoll::export_all(&cfg),
        DisplaySelf::export_all(&cfg),
        DeckState::export_all(&cfg),
        SummaryCounts::export_all(&cfg),
        NeedsYouItem::export_all(&cfg),
        SummaryUsage::export_all(&cfg),
        SummaryStation::export_all(&cfg),
        FleetSummary::export_all(&cfg),
        PaletteMode::export_all(&cfg),
        PaletteColors::export_all(&cfg),
        StationPalette::export_all(&cfg),
        DisplayLinkMade::export_all(&cfg),
        ShareRequest::export_all(&cfg),
        ShareLink::export_all(&cfg),
        ShareView::export_all(&cfg),
        ViewRequest::export_all(&cfg),
        ViewLink::export_all(&cfg),
        it::ForgeKind::export_all(&cfg),
        it::TrackerKind::export_all(&cfg),
        it::ForgeBinding::export_all(&cfg),
        it::TrackerBinding::export_all(&cfg),
        it::Binding::export_all(&cfg),
        it::IssueRef::export_all(&cfg),
        it::IssueSummary::export_all(&cfg),
        it::IssueComment::export_all(&cfg),
        it::IssueDetail::export_all(&cfg),
        it::PrState::export_all(&cfg),
        it::ReviewState::export_all(&cfg),
        it::ChecksState::export_all(&cfg),
        it::PrSummary::export_all(&cfg),
        it::PrGroups::export_all(&cfg),
        it::PrList::export_all(&cfg),
        it::PrFile::export_all(&cfg),
        it::PrDetail::export_all(&cfg),
        it::CheckFailure::export_all(&cfg),
        it::ChecksFailing::export_all(&cfg),
        it::StatusQuery::export_all(&cfg),
        it::CliStatus::export_all(&cfg),
        it::TrackerStatus::export_all(&cfg),
        it::IntegrationsStatus::export_all(&cfg),
        it::IssueSearch::export_all(&cfg),
        it::IssueList::export_all(&cfg),
        it::IssueGet::export_all(&cfg),
        it::AssetQuery::export_all(&cfg),
        it::Asset::export_all(&cfg),
        it::PrsList::export_all(&cfg),
        it::PrRef::export_all(&cfg),
        it::PrFetchHead::export_all(&cfg),
        it::PrHead::export_all(&cfg),
        it::PrCreate::export_all(&cfg),
        it::PrComment::export_all(&cfg),
        it::ReviewEvent::export_all(&cfg),
        it::PrReview::export_all(&cfg),
        it::IssueCommentWrite::export_all(&cfg),
        it::IssueTransition::export_all(&cfg),
        it::WriteResult::export_all(&cfg),
        it::Project::export_all(&cfg),
        it::ProjectInput::export_all(&cfg),
        it::ProjectList::export_all(&cfg),
        it::BindingCheck::export_all(&cfg),
        it::ProjectPreflight::export_all(&cfg),
        it::StartSessions::export_all(&cfg),
        it::StartedSession::export_all(&cfg),
        it::StartFailure::export_all(&cfg),
        it::StartedSessions::export_all(&cfg),
        it::PrCreateBody::export_all(&cfg),
        it::CommentBody::export_all(&cfg),
        it::ReviewBody::export_all(&cfg),
        it::TransitionBody::export_all(&cfg),
        crate::review::ReviewSide::export_all(&cfg),
        crate::review::AnchorKind::export_all(&cfg),
        crate::review::AuthorKind::export_all(&cfg),
        crate::review::CommentState::export_all(&cfg),
        crate::review::Delivery::export_all(&cfg),
        crate::review::ReviewComment::export_all(&cfg),
        crate::review::ReviewThread::export_all(&cfg),
        crate::review::ReviewSummary::export_all(&cfg),
        crate::review::ReviewThreads::export_all(&cfg),
        crate::review::NewComment::export_all(&cfg),
        crate::review::CommentPatch::export_all(&cfg),
        crate::review::SendReview::export_all(&cfg),
        crate::review::SendAgain::export_all(&cfg),
        ShareKind::export_all(&cfg),
        ShareRole::export_all(&cfg),
        Steering::export_all(&cfg),
        NewShare::export_all(&cfg),
        ShareCreated::export_all(&cfg),
        ShareInfo::export_all(&cfg),
        ShareHistory::export_all(&cfg),
        NewSpace::export_all(&cfg),
        SpaceView::export_all(&cfg),
        SpaceSession::export_all(&cfg),
        JoinRequest::export_all(&cfg),
        SharedScope::export_all(&cfg),
        ChatMessage::export_all(&cfg),
        ChatPost::export_all(&cfg),
        DriveGrant::export_all(&cfg),
        DriverState::export_all(&cfg),
        FieldKind::export_all(&cfg),
        Apply::export_all(&cfg),
        FieldMeta::export_all(&cfg),
        Durability::export_all(&cfg),
        SettingsView::export_all(&cfg),
        SettingsChange::export_all(&cfg),
        SettingsSaved::export_all(&cfg),
        SecretSet::export_all(&cfg),
        acc::AccountKind::export_all(&cfg),
        acc::AccountMethod::export_all(&cfg),
        acc::AccountStatus::export_all(&cfg),
        acc::AccountView::export_all(&cfg),
        acc::AppView::export_all(&cfg),
        acc::AppSet::export_all(&cfg),
        acc::TokenSet::export_all(&cfg),
        acc::ForgeLoginStart::export_all(&cfg),
        acc::ForgeLogin::export_all(&cfg),
        acc::ForgeLoginState::export_all(&cfg),
        acc::ForgeLoginQuery::export_all(&cfg),
        acc::AccountRef::export_all(&cfg),
        acc::OAuthStartParams::export_all(&cfg),
        acc::OAuthStarted::export_all(&cfg),
        acc::OAuthFinishParams::export_all(&cfg),
        acc::Container::export_all(&cfg),
        acc::Containers::export_all(&cfg),
        acc::AccountsList::export_all(&cfg),
        SettingsSetParams::export_all(&cfg),
        SecretSetParams::export_all(&cfg),
        RestartRequest::export_all(&cfg),
        RestartKind::export_all(&cfg),
        RestartItem::export_all(&cfg),
        RestartAnswer::export_all(&cfg),
        AssistKind::export_all(&cfg),
        AssistRequest::export_all(&cfg),
        AssistAnswer::export_all(&cfg),
        AssistRoute::export_all(&cfg),
        AssistSession::export_all(&cfg),
        ExecutionCheckQuery::export_all(&cfg),
        ExecutionCheck::export_all(&cfg),
        ProcessInfo::export_all(&cfg),
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
