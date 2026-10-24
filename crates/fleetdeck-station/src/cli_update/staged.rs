//! The binding of staged npm and Bun packages to the signed integrity
//! (design §3.6, Codex step S3).
//!
//! Pure over a file tree: nothing runs and nothing is fetched. The package
//! manager checked each tarball against the integrity that it recorded in
//! its lockfile, so a lockfile whose every entry equals the signed
//! integrity binds the extracted bytes to the signed document.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use fleetdeck_proto::cli_update::reason;
use serde_json::Value;

use super::verify::VerifiedPackage;

/// The lockfile of the staged install and its kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lockfile {
    /// `.package-lock.json` (npm 7 or later), the hidden lockfile.
    Npm(PathBuf),
    /// `bun.lock` (the text lockfile).
    Bun(PathBuf),
}

/// The package that carries the `bin` map and the entry point.
const MAIN_PACKAGE: &str = "@openai/codex";

/// Bind the staged global folder `sm` to the verified set `set`. It returns
/// `moved`: the sorted names of the top-level packages of `sm`.
///
/// (a) every lockfile entry names a `name@version` of `set` with its
/// integrity; (b) every package folder on disk is in `set` and in the
/// lockfile; (c) the `bin` map of the staged main package equals
/// `current_bin`.
///
/// # Errors
/// `tool_too_old` (no text lockfile), `unverified_package`,
/// `integrity_mismatch`, `unsupported_layout`.
pub fn verify_staged(
    sm: &Path,
    lockfile: Lockfile,
    set: &[VerifiedPackage],
    current_bin: &Value,
) -> Result<Vec<String>, &'static str> {
    let entries = read_lockfile(&lockfile)?;
    let mut locked: BTreeSet<(String, String)> = BTreeSet::new();
    for e in &entries {
        let Some(p) = set
            .iter()
            .find(|p| p.name == e.name && p.version == e.version)
        else {
            return Err(reason::UNVERIFIED_PACKAGE);
        };
        if e.integrity.as_deref() != Some(p.integrity.as_str()) {
            return Err(reason::INTEGRITY_MISMATCH);
        }
        locked.insert((e.name.clone(), e.version.clone()));
    }
    let mut moved = Vec::new();
    scan_modules(sm, true, set, &locked, &mut moved)?;
    moved.sort();
    let main = sm.join(MAIN_PACKAGE).join("package.json");
    let doc = read_json(&main).ok_or(reason::UNSUPPORTED_LAYOUT)?;
    if doc.get("bin").unwrap_or(&Value::Null) != current_bin {
        return Err(reason::UNSUPPORTED_LAYOUT);
    }
    Ok(moved)
}

/// One package entry of a lockfile.
struct LockEntry {
    name: String,
    version: String,
    integrity: Option<String>,
}

fn read_lockfile(lock: &Lockfile) -> Result<Vec<LockEntry>, &'static str> {
    match lock {
        Lockfile::Npm(path) => {
            let text = fs::read_to_string(path).map_err(|_| reason::TOOL_TOO_OLD)?;
            let doc: Value = serde_json::from_str(&text).map_err(|_| reason::UNSUPPORTED_LAYOUT)?;
            let packages = doc
                .get("packages")
                .and_then(Value::as_object)
                .ok_or(reason::UNSUPPORTED_LAYOUT)?;
            let mut out = Vec::new();
            for (key, v) in packages {
                if key.is_empty() {
                    continue;
                }
                let version = v
                    .get("version")
                    .and_then(Value::as_str)
                    .ok_or(reason::UNSUPPORTED_LAYOUT)?;
                // An alias entry carries the real name; else the folder name.
                let name = match v.get("name").and_then(Value::as_str) {
                    Some(n) => n.to_string(),
                    None => key
                        .rsplit_once("node_modules/")
                        .map_or(key.as_str(), |(_, n)| n)
                        .to_string(),
                };
                out.push(LockEntry {
                    name,
                    version: version.to_string(),
                    integrity: v
                        .get("integrity")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                });
            }
            Ok(out)
        }
        Lockfile::Bun(path) => {
            // A missing `bun.lock` (only the binary `bun.lockb`) is an old Bun.
            let text = fs::read_to_string(path).map_err(|_| reason::TOOL_TOO_OLD)?;
            let doc: Value = serde_json::from_str(&strip_trailing_commas(&text))
                .map_err(|_| reason::UNSUPPORTED_LAYOUT)?;
            let packages = doc
                .get("packages")
                .and_then(Value::as_object)
                .ok_or(reason::UNSUPPORTED_LAYOUT)?;
            let mut out = Vec::new();
            for v in packages.values() {
                let arr = v.as_array().ok_or(reason::UNSUPPORTED_LAYOUT)?;
                let spec = arr
                    .first()
                    .and_then(Value::as_str)
                    .ok_or(reason::UNSUPPORTED_LAYOUT)?;
                let (name, version) = split_spec(spec).ok_or(reason::UNSUPPORTED_LAYOUT)?;
                let integrity = arr.last().and_then(Value::as_str).map(str::to_string);
                out.push(LockEntry {
                    name,
                    version,
                    integrity,
                });
            }
            Ok(out)
        }
    }
}

/// `<name>@<version>` with a scope `@` at index 0; an `npm:` alias spec
/// (`x@npm:<name>@<version>`) names the real package.
fn split_spec(spec: &str) -> Option<(String, String)> {
    let spec = spec
        .find("@npm:")
        .map_or(spec, |i| &spec[i + "@npm:".len()..]);
    let at = spec.rfind('@').filter(|i| *i > 0)?;
    let (name, version) = (&spec[..at], &spec[at + 1..]);
    (!name.is_empty() && !version.is_empty()).then(|| (name.to_string(), version.to_string()))
}

/// Remove each comma that is followed (after white space) by `}` or `]`.
/// String-aware: a comma inside a string stays.
fn strip_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let (mut in_str, mut escaped) = (false, false);
    for (i, &c) in chars.iter().enumerate() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            ',' => {
                let next = chars[i + 1..].iter().find(|n| !n.is_whitespace());
                if !matches!(next, Some('}' | ']')) {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// Check every package folder under the `node_modules`-like folder `dir`
/// (b). `top` collects the names of the top-level packages.
fn scan_modules(
    dir: &Path,
    is_top: bool,
    set: &[VerifiedPackage],
    locked: &BTreeSet<(String, String)>,
    top: &mut Vec<String>,
) -> Result<(), &'static str> {
    let rd = fs::read_dir(dir).map_err(|_| reason::UNSUPPORTED_LAYOUT)?;
    for item in rd {
        let item = item.map_err(|_| reason::UNSUPPORTED_LAYOUT)?;
        let fname = item.file_name().to_string_lossy().into_owned();
        if fname.starts_with('.') {
            continue;
        }
        let meta = fs::symlink_metadata(item.path()).map_err(|_| reason::UNSUPPORTED_LAYOUT)?;
        // A link or a file is not a package folder: it cannot be bound.
        if !meta.file_type().is_dir() {
            return Err(reason::UNVERIFIED_PACKAGE);
        }
        if fname.starts_with('@') {
            let scope = fs::read_dir(item.path()).map_err(|_| reason::UNSUPPORTED_LAYOUT)?;
            for sub in scope {
                let sub = sub.map_err(|_| reason::UNSUPPORTED_LAYOUT)?;
                let sname = sub.file_name().to_string_lossy().into_owned();
                if sname.starts_with('.') {
                    continue;
                }
                check_package(&sub.path(), set, locked)?;
                if is_top {
                    top.push(format!("{fname}/{sname}"));
                }
            }
        } else {
            check_package(&item.path(), set, locked)?;
            if is_top {
                top.push(fname);
            }
        }
    }
    Ok(())
}

fn check_package(
    dir: &Path,
    set: &[VerifiedPackage],
    locked: &BTreeSet<(String, String)>,
) -> Result<(), &'static str> {
    let meta = fs::symlink_metadata(dir).map_err(|_| reason::UNSUPPORTED_LAYOUT)?;
    if !meta.file_type().is_dir() {
        return Err(reason::UNVERIFIED_PACKAGE);
    }
    let doc = read_json(&dir.join("package.json")).ok_or(reason::UNVERIFIED_PACKAGE)?;
    let name = doc.get("name").and_then(Value::as_str).unwrap_or_default();
    let version = doc
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let key = (name.to_string(), version.to_string());
    if !set.iter().any(|p| p.name == name && p.version == version) || !locked.contains(&key) {
        return Err(reason::UNVERIFIED_PACKAGE);
    }
    let nested = dir.join("node_modules");
    if fs::symlink_metadata(&nested).is_ok() {
        scan_modules(&nested, false, set, locked, &mut Vec::new())?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "staged_tests.rs"]
mod tests;
