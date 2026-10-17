//! Station Git operations of Fleet Git (design `docs/v2/design/fleet-git.md`,
//! task T1.2: FG-1 to FG-4). This module only declares and re-exports the
//! public items; every helper is private to its file.
//!
//! - `redact`: the redacted error line (§2.5).
//! - `identity`: the home check and the destination checks (§3.8).
//! - `fetch`: the fetch before a start and the start point (§4).
//! - `push`: the refspec, the backoff, the outcome of a push and the one
//!   serialized writer (§5).
//! - `push_outcome`: the classification of a push (§5.5, M2).
//! - `radar`: the predictive conflict radar (§10).
//! - `land`, `land_temp`: the Git work and the temporary folder of a land (§11.5, §11.6).
//! - `include`: the copy of ignored files into a new worktree (§7.2).
//! - `remote_branches`: `project.branches` and `project.fetch` (§13.1).
//! - `move_export`, `move_layout`, `move_stream`: the files of a session move (§12.3).
//! - `move_import`, `move_fetch`: the target side of a move, the staging and the Git work (§12.3).
//! - `home`: `project.ensure_home` and `project.home_status` (§8.4).

mod fetch;
mod home;
mod identity;
mod include;
mod land;
mod land_remote;
mod land_temp;
mod move_export;
mod move_fetch;
mod move_import;
mod move_layout;
mod move_stream;
mod move_upload;
mod push;
mod push_outcome;
mod radar;
mod redact;
mod remote_branches;

pub use fetch::{BaseStale, StartPoint, fetch_for_start};
pub use home::{ensure_home, git_version, home_status};
pub use identity::{PushDestination, Refusal, VerifiedUrl, check_home, effective_push_url};
pub use include::copy_includes;
pub use land::{
    Captured, Rebased, TempFacts, add_worktree, capture, conflict_files, has_identity, rebase,
    tested_unchanged,
};
pub use land_remote::{
    LandPush, PushEnd, RemoteTip, RunningPush, fetch_default, is_ancestor, local_tip, remote_tip,
    repo_path,
};
pub use land_temp::{TEMP_DIR, ensure_dir, identity as temp_identity, temp_path, temp_root};
pub use move_export::{Exported, MoveFileError, OpenFile};
pub use move_fetch::{
    PRIVATE_REF_PREFIX, add_move_worktree, check_publication, check_publication_values,
    fetch_publication, oid_valid, pick_publish_ref, private_ref,
};
pub use move_import::{
    Folder, Receiver, Removal, check_manifest, delete_cleanup, delete_file_by_identity,
    delete_folder_by_identity, place_files,
};
pub use move_layout::{MoveSupport, export_files, import_files, staged_name, support};
pub use move_stream::{serve_export, write_export};
pub use move_upload::{
    MoveUploads, Settled, UploadEvent, UploadGuard, UploadPort, mark_deleted, serve_upload,
};
pub use push::{
    OptionsProvider, PushFailure, PushJob, PushResult, PushWriter, RefspecError, backoff_delay,
    refspec_for,
};
pub use push_outcome::{PushOutcome, push_outcome};
pub use radar::{RadarEnv, RadarGuard, branch_name_ok, check_branches, run_radar};
pub use redact::redact;
pub use remote_branches::{fetch_project, list_branches, note_fetched};
