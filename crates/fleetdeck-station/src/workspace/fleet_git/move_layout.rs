//! The provider layouts of a session move (FG-9, ADR-010 §3, §4, §5.2): which
//! transcript files belong to a conversation, how the source lists them
//! (`export_files`) and where the target places them (`import_files`).
//!
//! - Claude: the main transcript `projects/<enc(cwd)>/<id>.jsonl` is required;
//!   the subagent files below `<id>/subagents/` are optional. The target
//!   writes them under the project directory of its own worktree path.
//! - Codex: the root rollout `sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl`
//!   keeps its relative path.
//!
//! The target computes every path itself from its provider home, its worktree
//! path and the native id (ADR-010 §5.2). The names in the manifest are only
//! checked, never used as a path.

use std::fs::File;
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt};
use std::path::{Path, PathBuf};

use fleetdeck_proto::command::check_native_session_id;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{
    MOVE_MAX_BYTES, MOVE_MAX_FILES, ManifestFile, TranscriptManifest, code,
};
use fleetdeck_proto::history::HistoryUnavailable;
use rustix::fs::{FileType, Mode, OFlags};

use super::move_export::{Candidate, Exported, MoveFileError, TOO_LARGE, assemble, sha256_of};
use crate::history::Fail;
use crate::history::lines::{Line, Lines};
use crate::history::locate::{SUBAGENT_FILE_MAX, Trust, check_meta, encode_cwd, entries, locate};
use crate::store::move_runs::ImportedFile;
use crate::workspace::files::{descend, file_flags, open_root};

/// What a provider kind can move (ADR-010 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveSupport {
    /// The transcript files move and the session resumes by its native id.
    Native,
    /// Only the branch moves (O30).
    BranchOnly,
}

/// The support of a provider kind: `Native` for Claude and Codex (the spike
/// passed, DL-230), `BranchOnly` for a kind that the station does not know.
#[must_use]
pub fn support(kind: ProviderKind) -> MoveSupport {
    match kind {
        ProviderKind::Claude | ProviderKind::Codex => MoveSupport::Native,
        ProviderKind::Unknown => MoveSupport::BranchOnly,
    }
}

/// The name of the staged file of the manifest entry `index` in the staging
/// folder of a move: the staging code (`move_import.rs`) writes the files
/// under these names and [`import_files`] reads them.
#[must_use]
pub fn staged_name(index: usize) -> String {
    format!("{index:03}")
}

/// How many records of the head of a file may pass before one names a
/// session.
const HEAD_RECORDS: usize = 1000;
/// The longest record line that the id check reads.
const LINE_MAX: usize = 16 * 1024 * 1024;
/// The longest `rel` of a manifest entry.
const REL_MAX: usize = 1024;
/// The text of a transfer that this station refuses.
const REFUSED: &str = "This station refused the conversation files of the move.";

fn refused(why: &str) -> MoveFileError {
    MoveFileError::new(code::MOVE_RESUME_FAILED, format!("{REFUSED} {why}"))
}

fn checksum() -> MoveFileError {
    MoveFileError::new(
        code::MOVE_CHECKSUM,
        "The conversation files did not arrive complete. Try again.",
    )
}

fn map_fail(fail: &Fail) -> MoveFileError {
    match fail {
        Fail::Unavailable(HistoryUnavailable::TooLarge) => {
            MoveFileError::new(code::MOVE_TOO_LARGE, TOO_LARGE)
        }
        Fail::Unavailable(_) => MoveFileError::not_found(),
        Fail::Busy => MoveFileError::new("busy", "The conversation files took too long to read."),
        _ => MoveFileError::new("internal", "The conversation files cannot be read."),
    }
}

// ----------------------------------------------------------------------- export

/// The id of a subagent file name (`agent-<id>.jsonl` or
/// `agent-<id>.meta.json`): `[A-Za-z0-9_-]`, 1 to 128 characters.
fn agent_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// `(key, id)` of a Claude subagent file name.
fn subagent_key(name: &str) -> Option<String> {
    let rest = name.strip_prefix("agent-")?;
    if let Some(id) = rest.strip_suffix(".meta.json") {
        return agent_id_ok(id).then(|| format!("subagent_meta:{id}"));
    }
    let id = rest.strip_suffix(".jsonl")?;
    agent_id_ok(id).then(|| format!("subagent:{id}"))
}

/// The optional subagent files of a Claude session, sorted by name. A file
/// that cannot be opened or checked is skipped: it is optional.
fn claude_subagents(home: &Path, cwd: &str, native: &str, trust: &Trust<'_>) -> Vec<Candidate> {
    let Ok(root) = open_root(home) else {
        return Vec::new();
    };
    let encoded = encode_cwd(cwd);
    let segments = ["projects", encoded.as_str(), native, "subagents"];
    let Ok(dir) = descend(&root, &segments) else {
        return Vec::new();
    };
    let mut visited = 0_usize;
    let Ok(mut listed) = entries(&dir, &mut visited, trust.limits) else {
        return Vec::new();
    };
    listed.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::new();
    for (name, kind) in listed {
        let (FileType::RegularFile, Some(key)) = (kind, subagent_key(&name)) else {
            continue;
        };
        let Ok(fd) = rustix::fs::openat(&dir, name.as_str(), file_flags(), Mode::empty()) else {
            continue;
        };
        let file = File::from(fd);
        let Ok(size) = check_meta(&file, trust, SUBAGENT_FILE_MAX) else {
            continue;
        };
        out.push(Candidate {
            key,
            rel: format!("projects/{encoded}/{native}/subagents/{name}"),
            file,
            size,
        });
    }
    out
}

/// Lists and opens the files of a native session below the provider `home`
/// of the source (design §12.3 step 2, ADR-010 §4). `cwd` is the working
/// folder of the source session. Every file is opened without following a
/// link, is a regular file, and is read from that descriptor.
///
/// # Errors
/// [`MoveFileError`]: `move_too_large` over the cap, `invalid_state` when the
/// required file is missing or not trusted, `busy` or `internal` otherwise.
pub fn export_files(
    home: &Path,
    kind: ProviderKind,
    cwd: &str,
    native: &str,
    trust: &Trust<'_>,
) -> Result<Exported, MoveFileError> {
    if support(kind) != MoveSupport::Native {
        return Err(MoveFileError::not_found());
    }
    let opened = locate(home, kind, cwd, native, trust).map_err(|f| map_fail(&f))?;
    let (key, optional) = match kind {
        ProviderKind::Codex => {
            // Only a rollout below `sessions/YYYY/MM/DD/` moves.
            if opened.path.components().count() != 5 || !opened.path.starts_with("sessions") {
                return Err(MoveFileError::not_found());
            }
            ("rollout", Vec::new())
        }
        _ => ("main", claude_subagents(&opened.home, cwd, native, trust)),
    };
    let required = Candidate {
        key: key.to_owned(),
        rel: opened.path.to_string_lossy().into_owned(),
        file: opened.file,
        size: opened.size,
    };
    assemble(kind, native, required, optional)
}

// ----------------------------------------------------------------------- import

/// One planned file: the entry, the staged file and the final path.
struct Planned<'a> {
    entry: &'a ManifestFile,
    staged: PathBuf,
    /// The folders below the provider home, then the file name.
    target: Vec<String>,
    /// The first record of the file must name the native id.
    names_session: bool,
}

/// A `rel` that is a plain relative path: no `..`, no empty part, no NUL.
fn rel_name(rel: &str) -> Option<&str> {
    if rel.is_empty() || rel.len() > REL_MAX || rel.contains(['\0', '\\']) || rel.starts_with('/') {
        return None;
    }
    let mut last = "";
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return None;
        }
        last = part;
    }
    Some(last)
}

/// `YYYY-MM-DDTHH-MM-SS` as `(YYYY, MM, DD)` when `text` has that shape.
fn rollout_date(text: &str) -> Option<(&str, &str, &str)> {
    let b = text.as_bytes();
    if b.len() != 19 {
        return None;
    }
    let digits = |from: usize, to: usize| b[from..to].iter().all(u8::is_ascii_digit);
    let shape = digits(0, 4)
        && b[4] == b'-'
        && digits(5, 7)
        && b[7] == b'-'
        && digits(8, 10)
        && b[10] == b'T'
        && digits(11, 13)
        && b[13] == b'-'
        && digits(14, 16)
        && b[16] == b'-'
        && digits(17, 19);
    shape.then(|| (&text[0..4], &text[5..7], &text[8..10]))
}

/// The target of one entry, computed from the provider `kind`, the native id
/// and the target working folder only (ADR-010 §5.2 items 1 and 2).
fn plan_target(
    kind: ProviderKind,
    entry: &ManifestFile,
    native: &str,
    target_cwd: &str,
) -> Result<(Vec<String>, bool), MoveFileError> {
    let name = rel_name(&entry.rel).ok_or_else(|| refused("A file name is not valid."))?;
    let bad = || refused("A file name is not one of this conversation.");
    let owned = |parts: &[&str]| parts.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>();
    match kind {
        ProviderKind::Claude => {
            let project = encode_cwd(target_cwd);
            let key = entry.key.as_str();
            if key == "main" {
                if name != format!("{native}.jsonl") {
                    return Err(bad());
                }
                return Ok((owned(&["projects", &project, name]), true));
            }
            let (id, wanted) = if let Some(id) = key.strip_prefix("subagent:") {
                (id, format!("agent-{id}.jsonl"))
            } else if let Some(id) = key.strip_prefix("subagent_meta:") {
                (id, format!("agent-{id}.meta.json"))
            } else {
                return Err(bad());
            };
            if !agent_id_ok(id) || name != wanted {
                return Err(bad());
            }
            Ok((
                owned(&["projects", &project, native, "subagents", name]),
                false,
            ))
        }
        ProviderKind::Codex => {
            let suffix = format!("-{native}.jsonl");
            let stamp = name
                .strip_prefix("rollout-")
                .and_then(|rest| rest.strip_suffix(suffix.as_str()))
                .and_then(rollout_date);
            let (true, Some((year, month, day))) = (entry.key == "rollout", stamp) else {
                return Err(bad());
            };
            Ok((owned(&["sessions", year, month, day, name]), true))
        }
        ProviderKind::Unknown => Err(refused("The provider is not supported.")),
    }
}

/// The manifest checks and the plan (design §12.3 step 3, ADR-010 §5.1, §5.2).
fn plan<'a>(
    kind: ProviderKind,
    target_cwd: &str,
    manifest: &'a TranscriptManifest,
    staged: &Path,
) -> Result<Vec<Planned<'a>>, MoveFileError> {
    let native = manifest.native_session_id.as_str();
    if check_native_session_id(native).is_err() {
        return Err(refused("The conversation id is not valid."));
    }
    if manifest.provider != kind {
        return Err(refused("The conversation is of another provider."));
    }
    if manifest.files.is_empty() || manifest.files.len() > MOVE_MAX_FILES {
        return Err(refused("The number of files is not valid."));
    }
    let total: u64 = manifest.files.iter().map(|f| f.size).sum();
    if total > MOVE_MAX_BYTES || manifest.total > MOVE_MAX_BYTES {
        return Err(MoveFileError::new(code::MOVE_TOO_LARGE, TOO_LARGE));
    }
    if total != manifest.total {
        return Err(checksum());
    }
    let mut planned: Vec<Planned<'_>> = Vec::with_capacity(manifest.files.len());
    for (index, entry) in manifest.files.iter().enumerate() {
        if entry.sha256.len() != 64
            || !entry
                .sha256
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(checksum());
        }
        let (target, names_session) = plan_target(kind, entry, native, target_cwd)?;
        if planned.iter().any(|p| p.target == target) {
            return Err(refused("A file is listed twice."));
        }
        planned.push(Planned {
            entry,
            staged: staged.join(staged_name(index)),
            target,
            names_session,
        });
    }
    if planned.iter().filter(|p| p.names_session).count() != 1 {
        return Err(refused("The conversation needs exactly one main file."));
    }
    Ok(planned)
}

/// Opens a staged file without following a link; a regular file only.
fn open_staged(path: &Path) -> Result<File, MoveFileError> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NOCTTY;
    let fd = rustix::fs::open(path, flags, Mode::empty())
        .map_err(|_| refused("A staged file is missing."))?;
    let file = File::from(fd);
    let meta = file
        .metadata()
        .map_err(|_| refused("A staged file cannot be read."))?;
    if !meta.is_file() {
        return Err(refused("A staged file is not a regular file."));
    }
    Ok(file)
}

/// The first record of `file` that names a session names `native`.
fn names_native(file: &File, size: u64, kind: ProviderKind, native: &str) -> bool {
    let mut lines = Lines::new(file, 0, size, LINE_MAX);
    for _ in 0..HEAD_RECORDS {
        let Ok(Some((_, line))) = lines.next_line() else {
            return false;
        };
        let Line::Record(bytes) = line else { continue };
        if let Some(named) = crate::history::parse::embedded_session(kind, bytes) {
            return named == native;
        }
    }
    false
}

/// Checks the size, the SHA-256 and (for the main file) the embedded id of a
/// staged file.
fn check_staged(p: &Planned<'_>, kind: ProviderKind, native: &str) -> Result<File, MoveFileError> {
    let file = open_staged(&p.staged)?;
    let size = file.metadata().map_or(0, |m| m.len());
    if size != p.entry.size {
        return Err(checksum());
    }
    let sha = sha256_of(&file, size).map_err(|_| checksum())?;
    if sha != p.entry.sha256 {
        return Err(checksum());
    }
    if p.names_session && !names_native(&file, size, kind, native) {
        return Err(refused("The conversation file names another conversation."));
    }
    Ok(file)
}

/// Makes the folder `home/<parts>`: a part that exists must be a folder (not a
/// link); a missing part is made with mode 0700.
fn ensure_dirs(home: &Path, parts: &[String]) -> Result<PathBuf, MoveFileError> {
    let fail = |why: &str| MoveFileError::new("internal", format!("The provider folder: {why}"));
    let mut path = home.to_path_buf();
    if std::fs::symlink_metadata(&path).is_err() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&path)
            .map_err(|e| fail(&e.to_string()))?;
    }
    for part in parts {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(refused("A provider folder is a link or a file.")),
            Err(_) => std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .map_err(|e| fail(&e.to_string()))?,
        }
    }
    Ok(path)
}

/// Copies `size` bytes of `from` into a new file `temp` (mode 0600, created
/// with `O_EXCL | O_NOFOLLOW`) and syncs it; returns the file.
fn write_temp(from: &File, size: u64, temp: &Path) -> Result<File, MoveFileError> {
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let io = |e: &dyn std::fmt::Display| MoveFileError::new("internal", format!("write: {e}"));
    let fd = rustix::fs::open(temp, flags, Mode::RUSR | Mode::WUSR).map_err(|e| io(&e))?;
    let out = File::from(fd);
    let mut buf = vec![0_u8; 64 * 1024];
    let mut at = 0_u64;
    while at < size {
        let want = usize::try_from((size - at).min(buf.len() as u64)).unwrap_or(buf.len());
        let n = from.read_at(&mut buf[..want], at).map_err(|e| io(&e))?;
        if n == 0 {
            return Err(checksum());
        }
        out.write_all_at(&buf[..n], at).map_err(|e| io(&e))?;
        at += n as u64;
    }
    out.sync_all().map_err(|e| io(&e))?;
    Ok(out)
}

/// Places the staged files of a native move in the provider `home` of the
/// target (design §12.3 step 4, ADR-010 §4, §5.2). `target_cwd` is the target
/// worktree path (Claude's project folder name comes from it). `staged` holds
/// the files under [`staged_name`].
///
/// Everything is checked before the first file is placed: the manifest, the
/// names, that no final file exists, and each staged file (size, SHA-256, the
/// embedded id). Then each file is written to a temporary name in its final
/// folder (mode 0600, `O_EXCL | O_NOFOLLOW`, synced), `record` is called with
/// the final path and the `dev` and `ino` of the file **before** the file
/// gets its final name, and the file is linked to its final name (a link
/// never overwrites). A failure removes the temporary file of the failing
/// step only: the files placed before it stay, and the caller deletes them by
/// the identity that `record` received.
///
/// # Errors
/// [`MoveFileError`]: `move_too_large`, `move_checksum` (a bad size, hash or
/// a short file), or `move_resume_failed` with a specific text (a bad name, a
/// bad embedded id, an existing file), or `internal`.
pub fn import_files(
    home: &Path,
    kind: ProviderKind,
    target_cwd: &str,
    manifest: &TranscriptManifest,
    staged: &Path,
    record: &mut dyn FnMut(&ImportedFile) -> Result<(), String>,
) -> Result<Vec<ImportedFile>, MoveFileError> {
    let planned = plan(kind, target_cwd, manifest, staged)?;
    let native = manifest.native_session_id.as_str();
    for p in &planned {
        let mut path = home.to_path_buf();
        path.extend(&p.target);
        if std::fs::symlink_metadata(&path).is_ok() {
            return Err(refused(
                "A conversation file with this name exists already.",
            ));
        }
    }
    let mut sources = Vec::with_capacity(planned.len());
    for p in &planned {
        sources.push(check_staged(p, kind, native)?);
    }
    let mut placed = Vec::with_capacity(planned.len());
    for (p, source) in planned.iter().zip(&sources) {
        let (name, folders) = p
            .target
            .split_last()
            .ok_or_else(|| refused("A file name is not valid."))?;
        let dir = ensure_dirs(home, folders)?;
        let temp = dir.join(format!(
            ".{name}.fdmove-{}",
            p.entry.sha256.get(..12).unwrap_or("")
        ));
        let fin = dir.join(name);
        let out = write_temp(source, p.entry.size, &temp)?;
        let meta = out.metadata();
        let step = (|| -> Result<ImportedFile, MoveFileError> {
            let meta = meta.map_err(|e| MoveFileError::new("internal", format!("stat: {e}")))?;
            let file = ImportedFile {
                path: fin.to_string_lossy().into_owned(),
                dev: meta.dev(),
                ino: meta.ino(),
            };
            record(&file).map_err(|e| MoveFileError::new("internal", e))?;
            std::fs::hard_link(&temp, &fin).map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    refused("A conversation file with this name exists already.")
                } else {
                    MoveFileError::new("internal", format!("link: {e}"))
                }
            })?;
            Ok(file)
        })();
        let _ = std::fs::remove_file(&temp);
        placed.push(step?);
    }
    Ok(placed)
}

#[cfg(test)]
#[path = "move_layout_tests.rs"]
pub(super) mod tests;

#[cfg(test)]
#[path = "move_export_tests.rs"]
mod export_tests;
