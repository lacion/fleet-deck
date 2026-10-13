//! The changes of a work tree without `git status` (DL-143).
//!
//! `git status` can run a configured clean or process filter, and a filter
//! that is added after a check still runs. The scan reads the index with
//! plain-listing commands and reads the files itself. No command here can run
//! a filter, hook, fsmonitor, textconv, external diff or lazy fetch.
//!
//! A file counts as changed when its type, executable bit or content
//! differs from the index. The content is hashed only when the stat data of
//! the index entry does not match the file, or when the file is as new as the
//! index (racily clean). A repository with a clean filter or an end-of-line
//! conversion is over-reported: the raw file content differs from the blob.

use std::collections::{BTreeSet, HashSet};
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path};

use ring::digest;

use crate::workspace::git::nul_fields;
use crate::workspace::{Git, GitError};

/// The deepest level of nested submodules that the scan follows.
const MAX_DEPTH: u8 = 3;
/// A file above this size counts as changed without hashing.
const MAX_HASH_BYTES: u64 = 64 * 1024 * 1024;

const MODE_FILE: u32 = 0o100_644;
const MODE_EXEC: u32 = 0o100_755;
const MODE_LINK: u32 = 0o120_000;
const MODE_GITLINK: u32 = 0o160_000;

/// What the scan found in one repository and its submodules.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::workspace) struct Scan {
    /// The tracked paths that differ from HEAD or from the index.
    pub(in crate::workspace) changed: u32,
    /// The untracked entries that no ignore rule hides.
    pub(in crate::workspace) untracked: u32,
    /// The ignored entries; `None` when they could not be listed.
    pub(in crate::workspace) ignored: Option<u32>,
}

/// One entry of `git ls-files --stage --debug`.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::workspace) struct IndexEntry {
    pub(in crate::workspace) mode: u32,
    pub(in crate::workspace) oid: String,
    pub(in crate::workspace) stage: u8,
    pub(in crate::workspace) path: Vec<u8>,
    pub(in crate::workspace) ctime: (u32, u32),
    pub(in crate::workspace) mtime: (u32, u32),
    pub(in crate::workspace) ino: u32,
    pub(in crate::workspace) size: u32,
}

fn lazy_fetch_off() -> [(&'static str, &'static OsStr); 1] {
    [("GIT_NO_LAZY_FETCH", OsStr::new("1"))]
}

/// Every Git command of the inventory goes through this function (and
/// [`status`] and [`text`]): `GIT_NO_LAZY_FETCH=1` stops a fetch from a
/// promisor remote.
pub(in crate::workspace) async fn run(git: &Git, args: &[&str]) -> Result<Vec<u8>, GitError> {
    git.call(args, &lazy_fetch_off(), None).await
}

/// [`run`] with the exit code: `(code, stdout, stderr)`.
pub(in crate::workspace) async fn status(
    git: &Git,
    args: &[&str],
) -> Result<(i32, Vec<u8>, String), GitError> {
    git.status(args, &lazy_fetch_off(), None).await
}

/// [`run`] with stdout as text.
pub(in crate::workspace) async fn text(git: &Git, args: &[&str]) -> Result<String, GitError> {
    let out = run(git, args).await?;
    String::from_utf8(out).map_err(|e| GitError::Utf8(e.to_string()))
}

/// A command in `root`: `git -C <root> <args>`.
async fn run_in(git: &Git, root: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut all = vec!["-C", root];
    all.extend_from_slice(args);
    run(git, &all).await.map_err(|e| e.to_string())
}

/// The changes of the work tree `root` and of its submodules, `depth` levels
/// down.
pub(in crate::workspace) async fn scan(git: &Git, root: &Path, depth: u8) -> Result<Scan, String> {
    let root_str = root
        .to_str()
        .ok_or_else(|| format!("the path is not UTF-8: {}", root.display()))?;
    let mut changed: BTreeSet<Vec<u8>> = BTreeSet::new();

    // a. Unmerged paths.
    let unmerged = run_in(git, root_str, &["ls-files", "-z", "--unmerged"]).await?;
    changed.extend(unmerged_paths(&unmerged)?);

    // The index, once: for an unborn branch and for the work tree check.
    let staged = run_in(git, root_str, &["ls-files", "-z", "--stage", "--debug"]).await?;
    let entries = parse_debug(&staged)?;

    // b. Index against HEAD.
    let head = status(
        git,
        &["-C", root_str, "rev-parse", "--verify", "--quiet", "HEAD"],
    )
    .await
    .map_err(|e| e.to_string())?;
    if head.0 == 0 {
        let raw = run_in(
            git,
            root_str,
            &[
                "diff-index",
                "--cached",
                "-z",
                "--raw",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                "--ignore-submodules=none",
                "HEAD",
            ],
        )
        .await?;
        changed.extend(raw_paths(&raw)?);
    } else if head.0 == 1 {
        // An unborn branch: every index entry is new work.
        changed.extend(entries.iter().map(|e| e.path.clone()));
    } else {
        return Err(format!(
            "git rev-parse HEAD failed ({}): {}",
            head.0, head.2
        ));
    }

    // c. Work tree against the index.
    let format = run_in(git, root_str, &["rev-parse", "--show-object-format"]).await?;
    let format = match String::from_utf8_lossy(&format).trim() {
        "sha1" => Format::Sha1,
        "sha256" => Format::Sha256,
        other => return Err(format!("unknown object format: {other}")),
    };
    let skipped = run_in(git, root_str, &["ls-files", "-z", "-t"]).await?;
    let skipped = skip_worktree_paths(&skipped);
    let index_path = run_in(git, root_str, &["rev-parse", "--git-path", "index"]).await?;
    let index_path = root.join(OsStr::from_bytes(
        String::from_utf8_lossy(&index_path).trim().as_bytes(),
    ));
    let files = {
        let root = root.to_path_buf();
        tokio::task::spawn_blocking(move || {
            check_files(&root, &entries, &skipped, format, &index_path)
        })
        .await
        .map_err(|e| format!("the file check did not end: {e}"))??
    };
    changed.extend(files.changed);

    let mut scan = Scan {
        changed: 0,
        untracked: 0,
        ignored: Some(0),
    };
    for (path, oid) in files.submodules {
        let sub_root = root.join(OsStr::from_bytes(&path));
        if depth >= MAX_DEPTH {
            return Err("submodules nest too deep to check".to_owned());
        }
        let sub_str = sub_root
            .to_str()
            .ok_or_else(|| format!("the path is not UTF-8: {}", sub_root.display()))?;
        let (code, out, stderr) = status(
            git,
            &["-C", sub_str, "rev-parse", "--verify", "--quiet", "HEAD"],
        )
        .await
        .map_err(|e| e.to_string())?;
        match code {
            0 => {
                if String::from_utf8_lossy(&out).trim() != oid {
                    changed.insert(path.clone());
                }
            }
            // An unborn HEAD differs from the recorded commit.
            1 => {
                changed.insert(path.clone());
            }
            _ => {
                return Err(format!(
                    "git rev-parse HEAD in a submodule ({code}): {stderr}"
                ));
            }
        }
        let sub = scan_level(git, &sub_root, depth + 1).await?;
        scan.changed = scan.changed.saturating_add(sub.changed);
        scan.untracked = scan.untracked.saturating_add(sub.untracked);
        scan.ignored = scan
            .ignored
            .zip(sub.ignored)
            .map(|(a, b)| a.saturating_add(b));
    }

    // d. Untracked entries.
    let others = run_in(
        git,
        root_str,
        &[
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
            "--directory",
            "--no-empty-directory",
        ],
    )
    .await?;
    // e. Ignored entries; a failure leaves them unknown.
    let ignored = run_in(
        git,
        root_str,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    )
    .await
    .ok();

    scan.changed = scan.changed.saturating_add(count(changed.len()));
    scan.untracked = scan
        .untracked
        .saturating_add(count(nul_fields(&others).len()));
    scan.ignored = scan
        .ignored
        .zip(ignored)
        .map(|(a, out)| a.saturating_add(count(nul_fields(&out).len())));
    Ok(scan)
}

/// The recursion of [`scan`], boxed for the recursive call.
fn scan_level<'a>(
    git: &'a Git,
    root: &'a Path,
    depth: u8,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Scan, String>> + Send + 'a>> {
    Box::pin(scan(git, root, depth))
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The paths of `git ls-files -z --unmerged`: `<mode> <oid> <stage>\t<path>`.
fn unmerged_paths(out: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    nul_fields(out)
        .into_iter()
        .map(|field| {
            field
                .iter()
                .position(|b| *b == b'\t')
                .map(|tab| field[tab + 1..].to_vec())
                .ok_or_else(|| "git ls-files --unmerged: an entry has no path".to_owned())
        })
        .collect()
}

/// The paths of `git diff-index -z --raw --no-renames`: a `:`-line, then its
/// path.
fn raw_paths(out: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let mut paths = Vec::new();
    let mut fields = nul_fields(out).into_iter();
    while let Some(record) = fields.next() {
        if !record.starts_with(b":") {
            return Err("git diff-index: an unexpected record".to_owned());
        }
        let path = fields
            .next()
            .ok_or_else(|| "git diff-index: a record has no path".to_owned())?;
        paths.push(path.to_vec());
    }
    Ok(paths)
}

/// The paths of `git ls-files -z -t` with the tag `S` (skip-worktree).
fn skip_worktree_paths(out: &[u8]) -> HashSet<Vec<u8>> {
    nul_fields(out)
        .into_iter()
        .filter_map(|field| field.strip_prefix(b"S ").map(<[u8]>::to_vec))
        .collect()
}

/// Parses `git ls-files -z --stage --debug`: per entry `<mode> <oid>
/// <stage>\t<path>` and a NUL, then five text lines (ctime, mtime, dev and
/// ino, uid and gid, size and flags).
pub(in crate::workspace) fn parse_debug(out: &[u8]) -> Result<Vec<IndexEntry>, String> {
    let bad = |what: &str| format!("git ls-files --debug: {what}");
    let mut entries = Vec::new();
    let mut pos = 0;
    while pos < out.len() {
        let nul = out[pos..]
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| bad("an entry is not NUL-terminated"))?;
        let header = &out[pos..pos + nul];
        pos += nul + 1;
        let tab = header
            .iter()
            .position(|b| *b == b'\t')
            .ok_or_else(|| bad("an entry has no path"))?;
        let meta = String::from_utf8_lossy(&header[..tab]).into_owned();
        let mut words = meta.split(' ');
        let mode = words
            .next()
            .and_then(|m| u32::from_str_radix(m, 8).ok())
            .ok_or_else(|| bad("a bad mode"))?;
        let oid = words.next().ok_or_else(|| bad("no object id"))?.to_owned();
        let stage = words
            .next()
            .and_then(|s| s.parse::<u8>().ok())
            .ok_or_else(|| bad("a bad stage"))?;
        let mut entry = IndexEntry {
            mode,
            oid,
            stage,
            path: header[tab + 1..].to_vec(),
            ctime: (0, 0),
            mtime: (0, 0),
            ino: 0,
            size: 0,
        };
        for _ in 0..5 {
            let nl = out[pos..]
                .iter()
                .position(|b| *b == b'\n')
                .ok_or_else(|| bad("a stat block is cut off"))?;
            let line = String::from_utf8_lossy(&out[pos..pos + nl]).into_owned();
            pos += nl + 1;
            for pair in line.trim_start().split('\t') {
                let (key, value) = pair
                    .split_once(": ")
                    .ok_or_else(|| bad("a bad stat line"))?;
                match key {
                    "ctime" => entry.ctime = pair_u32(value).ok_or_else(|| bad("a bad ctime"))?,
                    "mtime" => entry.mtime = pair_u32(value).ok_or_else(|| bad("a bad mtime"))?,
                    "ino" => entry.ino = value.parse().map_err(|_| bad("a bad ino"))?,
                    "size" => entry.size = value.parse().map_err(|_| bad("a bad size"))?,
                    _ => {}
                }
            }
        }
        entries.push(entry);
    }
    Ok(entries)
}

fn pair_u32(value: &str) -> Option<(u32, u32)> {
    let (secs, nanos) = value.split_once(':')?;
    Some((secs.parse().ok()?, nanos.parse().ok()?))
}

#[derive(Clone, Copy)]
enum Format {
    Sha1,
    Sha256,
}

/// The Git blob id of `bytes`: the hash of `blob <len>\0<bytes>`, in lower
/// case hex.
pub(in crate::workspace) fn blob_oid(sha256: bool, bytes: &[u8]) -> String {
    let algorithm = if sha256 {
        &digest::SHA256
    } else {
        &digest::SHA1_FOR_LEGACY_USE_ONLY
    };
    let mut ctx = digest::Context::new(algorithm);
    ctx.update(format!("blob {}\0", bytes.len()).as_bytes());
    ctx.update(bytes);
    let mut hex = String::new();
    for byte in ctx.finish().as_ref() {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// The low 32 bits, as Git stores a stat field.
fn low32(value: u64) -> u32 {
    u32::try_from(value & 0xFFFF_FFFF).unwrap_or(0)
}

fn low32_signed(value: i64) -> u32 {
    low32(u64::from_ne_bytes(value.to_ne_bytes()))
}

struct Files {
    changed: Vec<Vec<u8>>,
    /// The initialized submodules: path and the commit in the index.
    submodules: Vec<(Vec<u8>, String)>,
}

/// Compares every stage-0 entry with the file on disk. Blocking.
fn check_files(
    root: &Path,
    entries: &[IndexEntry],
    skipped: &HashSet<Vec<u8>>,
    format: Format,
    index_path: &Path,
) -> Result<Files, String> {
    let index_mtime = std::fs::metadata(index_path)
        .map(|m| (m.mtime(), m.mtime_nsec()))
        .map_err(|e| format!("cannot read the index file: {e}"))?;
    let mut files = Files {
        changed: Vec::new(),
        submodules: Vec::new(),
    };
    for entry in entries.iter().filter(|e| e.stage == 0) {
        let relative = Path::new(OsStr::from_bytes(&entry.path));
        if relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return Err(format!(
                "an unsafe path in the index: {}",
                relative.display()
            ));
        }
        let full = root.join(relative);
        let meta = match std::fs::symlink_metadata(&full) {
            Ok(meta) => meta,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                if skipped.contains(&entry.path) {
                    return Err(
                        "a sparse checkout: files outside the sparse set are not checked"
                            .to_owned(),
                    );
                }
                files.changed.push(entry.path.clone());
                continue;
            }
            Err(e) => return Err(format!("cannot read {}: {e}", relative.display())),
        };
        let kind = meta.file_type();
        let type_ok = match entry.mode {
            MODE_FILE | MODE_EXEC => kind.is_file(),
            MODE_LINK => kind.is_symlink(),
            MODE_GITLINK => kind.is_dir(),
            other => return Err(format!("an unknown mode in the index: {other:o}")),
        };
        if !type_ok {
            files.changed.push(entry.path.clone());
            continue;
        }
        if entry.mode == MODE_GITLINK {
            // A submodule without `.git` is not initialized: not changed.
            if std::fs::symlink_metadata(full.join(".git")).is_ok() {
                files
                    .submodules
                    .push((entry.path.clone(), entry.oid.clone()));
            }
            continue;
        }
        if entry.mode != MODE_LINK && (meta.mode() & 0o111 != 0) != (entry.mode == MODE_EXEC) {
            files.changed.push(entry.path.clone());
            continue;
        }
        let stat_equal = low32(meta.size()) == entry.size
            && (low32_signed(meta.mtime()), low32_signed(meta.mtime_nsec())) == entry.mtime
            && (low32_signed(meta.ctime()), low32_signed(meta.ctime_nsec())) == entry.ctime
            && low32(meta.ino()) == entry.ino;
        let racy = (meta.mtime(), meta.mtime_nsec()) >= index_mtime;
        if stat_equal && !racy {
            continue;
        }
        if content_differs(&full, entry, matches!(format, Format::Sha256))? {
            files.changed.push(entry.path.clone());
        }
    }
    Ok(files)
}

/// Whether the blob id of the file (or of the link target) differs from the
/// index. A file is opened with `O_NOFOLLOW`.
fn content_differs(full: &Path, entry: &IndexEntry, sha256: bool) -> Result<bool, String> {
    let bytes = if entry.mode == MODE_LINK {
        std::fs::read_link(full)
            .map_err(|e| format!("cannot read the link {}: {e}", full.display()))?
            .as_os_str()
            .as_bytes()
            .to_vec()
    } else {
        let flags = i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).unwrap_or(0);
        let mut file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(flags)
            .open(full)
        {
            Ok(file) => file,
            // The file changed or went away since the stat call.
            Err(_) => return Ok(true),
        };
        let len = file
            .metadata()
            .map_err(|e| format!("cannot read {}: {e}", full.display()))?
            .len();
        if len > MAX_HASH_BYTES {
            return Ok(true);
        }
        let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
        file.read_to_end(&mut bytes)
            .map_err(|e| format!("cannot read {}: {e}", full.display()))?;
        bytes
    };
    Ok(blob_oid(sha256, &bytes) != entry.oid)
}
