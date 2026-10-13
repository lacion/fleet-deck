//! A clone start that reuses a checkout of the same repository: the decision
//! reads the target directory and its `.git/config`, and runs no `git`.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::store::WorkspacePlan;
use crate::workspace::provision::{self, Refused};

/// The longest `.git/config` that the station reads.
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

/// What a clone start does with its target directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloneTarget {
    /// The target is new: clone into it.
    Clone(WorkspacePlan, PathBuf),
    /// The target is a clone of the same repository: start in place.
    Reuse(PathBuf),
}

fn invalid(message: impl Into<String>) -> Refused {
    Refused {
        forbidden: false,
        message: message.into(),
    }
}

fn forbidden(message: impl Into<String>) -> Refused {
    Refused {
        forbidden: true,
        message: message.into(),
    }
}

/// The plan of a clone start into `target`: a clone when `target` does not
/// exist, else the reuse of `target` when it is a plain clone of `url`.
///
/// # Errors
/// [`Refused`]: a bad URL or a new target as in [`provision::clone_plan`];
/// an existing target that is a link, outside the allowed roots, not a plain
/// Git clone, or a clone of another repository.
pub fn clone_or_reuse(
    url: &str,
    target: &str,
    canonical_inside: impl Fn(&Path) -> Option<PathBuf>,
) -> Result<CloneTarget, Refused> {
    provision::check_url(url, &canonical_inside)?;
    let path = Path::new(target);
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return provision::clone_plan(url, target, canonical_inside)
            .map(|(plan, path)| CloneTarget::Clone(plan, path));
    };
    let canonical = meta
        .is_dir()
        .then(|| canonical_inside(path))
        .flatten()
        .ok_or_else(|| forbidden("the clone directory must be inside the allowed roots"))?;
    let shown = path.display();
    match std::fs::symlink_metadata(path.join(".git")) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            return Err(invalid(format!(
                "`{shown}` exists and is not a plain clone"
            )));
        }
        Err(_) => {
            return Err(invalid(format!(
                "`{shown}` exists and is not a Git repository"
            )));
        }
    }
    let config = read_config(&path.join(".git").join("config"))
        .ok_or_else(|| invalid(format!("`{shown}` has an unreadable Git config")))?;
    let origin =
        origin_url(&config).ok_or_else(|| invalid(format!("`{shown}` has no origin remote")))?;
    if normalize(&origin) == normalize(url) {
        Ok(CloneTarget::Reuse(canonical))
    } else {
        Err(invalid(format!(
            "`{shown}` exists and is a clone of another repository"
        )))
    }
}

/// The text of a Git config: at most 64 KiB, UTF-8.
fn read_config(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// The first `url` of a `[remote "origin"]` section.
fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
            continue;
        }
        if !in_origin {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("url")
        {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            return Some(value.to_owned());
        }
    }
    None
}

/// The comparison form of a clone URL: `host/path` without the scheme, the
/// user, the port, a trailing `/` and a trailing `.git`. The host is lower
/// case; the path is kept as it is.
fn normalize(url: &str) -> String {
    let url = url.trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    if let Some((_, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        return format!("{}/{path}", host_of(host_port));
    }
    if let Some((user_host, path)) = url.split_once(':') {
        let host = user_host.rsplit_once('@').map_or(user_host, |(_, h)| h);
        return format!("{}/{path}", host.to_ascii_lowercase());
    }
    url.to_owned()
}

/// The lower-case host of `host[:port]`, also for a bracketed IPv6 address.
fn host_of(host_port: &str) -> String {
    let host = match host_port.strip_prefix('[') {
        Some(inner) => inner.split(']').next().unwrap_or(inner),
        None => host_port.split(':').next().unwrap_or(host_port),
    };
    host.to_ascii_lowercase()
}

#[cfg(test)]
#[path = "clone_reuse_tests.rs"]
mod tests;
