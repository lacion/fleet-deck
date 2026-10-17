//! The config drift report of this station (design §14, FG-11, O33): the
//! hashes of the provider config files. A hash is of the tree, never of the
//! absolute path, and no file content leaves the station.

use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{ConfigDigest, DigestItem, DriftItem};
use ring::digest::{Context, SHA256};
use tokio::sync::watch;

use crate::fleet_git_runtime::FleetGitRuntime;
use crate::manager::SessionManager;

/// The most entries in a watched folder.
const MAX_ENTRIES: usize = 10_000;
/// The largest file.
const MAX_FILE: u64 = 4 * 1024 * 1024;
/// The most bytes of file content in one item.
const MAX_TOTAL: u64 = 64 * 1024 * 1024;

/// How often the digest is computed again (design §14.2).
pub const DRIFT_EVERY: Duration = Duration::from_secs(10 * 60);

const ABSENT: &str = "absent";
const UNREADABLE: &str = "unreadable";
const TOO_LARGE: &str = "too_large";

/// The four items and their paths below `$HOME`, in the order of the digest.
const ITEMS: [(DriftItem, &str); 4] = [
    (DriftItem::ClaudeMd, ".claude/CLAUDE.md"),
    (DriftItem::ClaudeAgents, ".claude/agents"),
    (DriftItem::ClaudeSkills, ".claude/skills"),
    (DriftItem::CodexAgentsMd, ".codex/AGENTS.md"),
];

/// The digest of the four items below `home`. Without a home every item is
/// `absent`. It reads files: call it from a blocking context.
#[must_use]
pub fn compute(
    home: Option<&Path>,
    claude_version: Option<String>,
    codex_version: Option<String>,
    now_ms: i64,
) -> ConfigDigest {
    let items = ITEMS
        .iter()
        .map(|(name, rel)| DigestItem {
            name: *name,
            hash: home.map_or_else(|| ABSENT.to_owned(), |h| hash_item(&h.join(rel))),
        })
        .collect();
    ConfigDigest {
        claude_version,
        codex_version,
        items,
        computed_at: now_ms,
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// The hash of one item (design §14.2): 64 hex characters, `absent`,
/// `unreadable` or `too_large`. A link at the top is followed once; inside a
/// folder no link is followed.
#[must_use]
pub fn hash_item(path: &Path) -> String {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ABSENT.to_owned(),
        Err(_) => return UNREADABLE.to_owned(),
    };
    let result = if meta.is_file() {
        hash_file(path, meta.len())
    } else if meta.is_dir() {
        hash_tree(path)
    } else {
        Err(UNREADABLE)
    };
    result.unwrap_or_else(str::to_owned)
}

/// The content of a file, at most [`MAX_FILE`] bytes.
fn read_file(path: &Path, size: u64) -> Result<Vec<u8>, &'static str> {
    if size > MAX_FILE {
        return Err(TOO_LARGE);
    }
    let file = std::fs::File::open(path).map_err(|_| UNREADABLE)?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UNREADABLE)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(TOO_LARGE);
    }
    Ok(bytes)
}

/// A file: SHA-256 of `"f\0" <size> "\0" <bytes>`.
fn hash_file(path: &Path, size: u64) -> Result<String, &'static str> {
    let bytes = read_file(path, size)?;
    let mut ctx = Context::new(&SHA256);
    ctx.update(format!("f\0{}\0", bytes.len()).as_bytes());
    ctx.update(&bytes);
    Ok(hex(ctx.finish().as_ref()))
}

/// One entry of a walked folder.
struct Entry {
    /// The path below the folder, with `/`.
    rel: Vec<u8>,
    path: std::path::PathBuf,
    meta: std::fs::Metadata,
}

/// Walks `root` without following links.
fn walk(root: &Path) -> Result<Vec<Entry>, &'static str> {
    let mut out: Vec<Entry> = Vec::new();
    let mut pending = vec![(root.to_path_buf(), Vec::<u8>::new())];
    while let Some((dir, prefix)) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|_| UNREADABLE)? {
            let entry = entry.map_err(|_| UNREADABLE)?;
            if out.len() >= MAX_ENTRIES {
                return Err(TOO_LARGE);
            }
            let mut rel = prefix.clone();
            if !rel.is_empty() {
                rel.push(b'/');
            }
            rel.extend_from_slice(entry.file_name().as_bytes());
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).map_err(|_| UNREADABLE)?;
            if meta.is_dir() {
                pending.push((path.clone(), rel.clone()));
            }
            out.push(Entry { rel, path, meta });
        }
    }
    Ok(out)
}

/// A folder: one SHA-256 over its entries, sorted by the bytes of the relative
/// path. A file adds `"f\0" <rel> "\0" <size> "\0" <bytes>`, a link
/// `"l\0" <rel> "\0" <target bytes>`, a folder `"d\0" <rel> "\0"`, another type
/// `"o\0" <rel> "\0"`.
fn hash_tree(root: &Path) -> Result<String, &'static str> {
    let mut entries = walk(root)?;
    entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    let total: u64 = entries
        .iter()
        .filter(|e| e.meta.is_file())
        .map(|e| e.meta.len())
        .sum();
    if total > MAX_TOTAL
        || entries
            .iter()
            .any(|e| e.meta.len() > MAX_FILE && e.meta.is_file())
    {
        return Err(TOO_LARGE);
    }
    let mut ctx = Context::new(&SHA256);
    for e in &entries {
        let kind = e.meta.file_type();
        if kind.is_file() {
            let bytes = read_file(&e.path, e.meta.len())?;
            ctx.update(b"f\0");
            ctx.update(&e.rel);
            ctx.update(format!("\0{}\0", bytes.len()).as_bytes());
            ctx.update(&bytes);
        } else if kind.is_symlink() {
            let target = std::fs::read_link(&e.path).map_err(|_| UNREADABLE)?;
            ctx.update(b"l\0");
            ctx.update(&e.rel);
            ctx.update(b"\0");
            ctx.update(target.as_os_str().as_bytes());
        } else if kind.is_dir() {
            ctx.update(b"d\0");
            ctx.update(&e.rel);
            ctx.update(b"\0");
        } else {
            ctx.update(b"o\0");
            ctx.update(&e.rel);
            ctx.update(b"\0");
        }
    }
    Ok(hex(ctx.finish().as_ref()))
}

/// The version of the first provider instance of `kind` in `manager`.
async fn first_version(manager: &SessionManager, kind: ProviderKind) -> Option<String> {
    manager
        .providers()
        .await
        .into_iter()
        .find(|p| p.provider == kind)
        .and_then(|p| p.version)
}

/// Computes the digest once and stores it in `runtime`.
pub async fn refresh(runtime: &FleetGitRuntime, manager: &SessionManager, home: Option<&Path>) {
    let claude = first_version(manager, ProviderKind::Claude).await;
    let codex = first_version(manager, ProviderKind::Codex).await;
    let now = manager.store().now_ms();
    let home = home.map(Path::to_path_buf);
    match tokio::task::spawn_blocking(move || compute(home.as_deref(), claude, codex, now)).await {
        Ok(digest) => runtime.set_config(digest),
        Err(e) => tracing::warn!(error = %e, "the config digest failed"),
    }
}

/// The drift task: the digest at start, then every `every`, until `shutdown`
/// is set (design §14.2).
pub async fn run(
    runtime: Arc<FleetGitRuntime>,
    manager: Arc<SessionManager>,
    home: Option<std::path::PathBuf>,
    every: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => refresh(&runtime, &manager, home.as_deref()).await,
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "drift_tests.rs"]
mod tests;
