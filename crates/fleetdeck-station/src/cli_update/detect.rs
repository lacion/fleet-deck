//! Detection of the install method of a provider CLI (design §3.3).
//!
//! [`classify`] is pure: the link chain of the launcher and two probes of
//! the file system give the method, the tool that updates it and the reason
//! when the station does not update it. [`detect`] resolves the launcher on
//! the instance `PATH` and calls it.

use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::cli_update::{InstallMethod, reason};
use fleetdeck_proto::event::ProviderKind;

/// The most hops of a link chain that [`detect`] follows.
pub const MAX_HOPS: usize = 8;

/// One provider CLI on one station.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub kind: ProviderKind,
    pub method: InstallMethod,
    /// The path that the launch runs (the PATH hit, or the absolute `binary`).
    pub launcher: PathBuf,
    /// The link chain of `launcher`: `launcher` first, then each hop of
    /// `read_link` made absolute, at most 8 hops; the last entry is `real`.
    pub chain: Vec<PathBuf>,
    /// `launcher` with every link resolved.
    pub real: PathBuf,
    pub tool: Tool,
    /// Set when the method is report only or a tool is missing.
    pub blocked: Option<&'static str>,
}

/// The tool that updates an install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
    /// `link` is `home/.local/bin/claude` (the launcher link that the vendor
    /// installer manages); `versions` is `home/.local/share/claude/versions`;
    /// `gpgv` is the verifier.
    Native {
        link: PathBuf,
        versions: PathBuf,
        gpgv: Option<PathBuf>,
    },
    /// Codex only. `modules` is `<prefix>/lib/node_modules`, `bin_link` is
    /// `<prefix>/bin/codex` (a member of `chain`), `npm` is `<prefix>/bin/npm`.
    Npm {
        prefix: PathBuf,
        npm: PathBuf,
        modules: PathBuf,
        bin_link: PathBuf,
    },
    /// Codex only. `modules` is `<bun_install>/install/global/node_modules`,
    /// `bin_link` is `<bun_install>/bin/codex` (a member of `chain`), `bun`
    /// is `<bun_install>/bin/bun`.
    Bun {
        bun_install: PathBuf,
        bun: PathBuf,
        modules: PathBuf,
        bin_link: PathBuf,
    },
    None,
}

/// The places where `gpgv` is looked for after the instance `PATH`.
pub const GPGV_FALLBACKS: [&str; 4] = [
    "/usr/bin/gpgv",
    "/opt/homebrew/bin/gpgv",
    "/usr/local/bin/gpgv",
    "/home/linuxbrew/.linuxbrew/bin/gpgv",
];

/// The folders whose content is a Homebrew install.
const HOMEBREW_ROOTS: [&str; 4] = [
    "/opt/homebrew",
    "/usr/local/Caskroom",
    "/usr/local/Cellar",
    "/home/linuxbrew/.linuxbrew",
];

type Classified = (InstallMethod, Tool, Option<&'static str>);

/// The folder in front of `needle` in `path`, when the components of
/// `needle` follow each other in `path`. Components, never substrings.
fn prefix_before(path: &Path, needle: &[&str]) -> Option<PathBuf> {
    let parts: Vec<Component<'_>> = path.components().collect();
    let n = needle.len();
    if parts.len() <= n {
        return None;
    }
    (0..=parts.len() - n - 1).find_map(|start| {
        let hit = needle
            .iter()
            .enumerate()
            .all(|(i, want)| parts[start + i].as_os_str() == std::ffi::OsStr::new(want));
        hit.then(|| parts[..start].iter().collect())
    })
}

/// `^\d{1,6}\.\d{1,6}\.\d{1,6}$` for one file name.
fn is_version_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(fleetdeck_proto::cli_update::is_release)
}

/// The `gpgv` of the fixed places that exist.
fn fixed_gpgv(exists: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    GPGV_FALLBACKS.iter().map(PathBuf::from).find(|p| exists(p))
}

fn classify_claude(
    chain: &[PathBuf],
    real: &Path,
    home: Option<&Path>,
    read_link: &dyn Fn(&Path) -> Option<PathBuf>,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<Classified> {
    if let Some(home) = home {
        let versions = home.join(".local/share/claude/versions");
        let link = home.join(".local/bin/claude");
        // Rule 1: the managed link, one hop, straight into `versions`.
        if chain.contains(&link)
            && let Some(hop) = read_link(&link)
            && hop.parent() == Some(versions.as_path())
            && hop.file_name().is_some_and(is_version_name)
            && real == hop
        {
            let gpgv = fixed_gpgv(exists);
            let blocked = gpgv.is_none().then_some(reason::GPGV_MISSING);
            return Some((
                InstallMethod::Native,
                Tool::Native {
                    link,
                    versions,
                    gpgv,
                },
                blocked,
            ));
        }
        // Rule 2: a version file by any other way.
        if real.starts_with(&versions) && real != versions {
            return Some((
                InstallMethod::Unknown,
                Tool::None,
                Some(reason::CUSTOM_LAUNCHER),
            ));
        }
    }
    // Rule 3: Claude by npm or Bun is report only.
    if prefix_before(
        real,
        &["lib", "node_modules", "@anthropic-ai", "claude-code"],
    )
    .is_some()
    {
        return Some((
            InstallMethod::Npm,
            Tool::None,
            Some(reason::REPORT_ONLY_METHOD),
        ));
    }
    if prefix_before(
        real,
        &[
            "install",
            "global",
            "node_modules",
            "@anthropic-ai",
            "claude-code",
        ],
    )
    .is_some()
    {
        return Some((
            InstallMethod::Bun,
            Tool::None,
            Some(reason::REPORT_ONLY_METHOD),
        ));
    }
    None
}

fn classify_codex(
    chain: &[PathBuf],
    real: &Path,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<Classified> {
    // Rule 4, 6 and 7: npm.
    if let Some(prefix) = prefix_before(real, &["lib", "node_modules", "@openai", "codex"]) {
        let bin_link = prefix.join("bin/codex");
        let npm = prefix.join("bin/npm");
        let modules = prefix.join("lib/node_modules");
        if !chain.contains(&bin_link) {
            return Some((
                InstallMethod::Npm,
                Tool::None,
                Some(reason::CUSTOM_LAUNCHER),
            ));
        }
        if !exists(&npm) {
            return Some((InstallMethod::Npm, Tool::None, Some(reason::TOOL_MISSING)));
        }
        return Some((
            InstallMethod::Npm,
            Tool::Npm {
                prefix,
                npm,
                modules,
                bin_link,
            },
            None,
        ));
    }
    // Rule 5, 6 and 7: Bun. `bun_install` comes from `real`, never from
    // the environment.
    if let Some(bun_install) = prefix_before(
        real,
        &["install", "global", "node_modules", "@openai", "codex"],
    ) {
        let bin_link = bun_install.join("bin/codex");
        let bun = bun_install.join("bin/bun");
        let modules = bun_install.join("install/global/node_modules");
        if !chain.contains(&bin_link) {
            return Some((
                InstallMethod::Bun,
                Tool::None,
                Some(reason::CUSTOM_LAUNCHER),
            ));
        }
        if !exists(&bun) {
            return Some((InstallMethod::Bun, Tool::None, Some(reason::TOOL_MISSING)));
        }
        return Some((
            InstallMethod::Bun,
            Tool::Bun {
                bun_install,
                bun,
                modules,
                bin_link,
            },
            None,
        ));
    }
    None
}

/// Pure: classifies the link chain with `home`. `read_link` answers one
/// hop (made absolute) or `None` for a file that is not a link; `exists`
/// answers for a path.
///
/// The `gpgv` of a native install is looked for in the fixed places only;
/// [`detect`] adds the instance `PATH`.
#[must_use]
pub fn classify(
    kind: ProviderKind,
    chain: &[PathBuf],
    home: Option<&Path>,
    read_link: &dyn Fn(&Path) -> Option<PathBuf>,
    exists: &dyn Fn(&Path) -> bool,
) -> Classified {
    let Some(real) = chain.last() else {
        return (
            InstallMethod::Unknown,
            Tool::None,
            Some(reason::REPORT_ONLY_METHOD),
        );
    };
    let by_kind = match kind {
        ProviderKind::Claude => classify_claude(chain, real, home, read_link, exists),
        ProviderKind::Codex => classify_codex(chain, real, exists),
        ProviderKind::Unknown => {
            return (
                InstallMethod::Unknown,
                Tool::None,
                Some(reason::REPORT_ONLY_METHOD),
            );
        }
    };
    if let Some(found) = by_kind {
        return found;
    }
    // Rule 8: Homebrew.
    if HOMEBREW_ROOTS.iter().any(|root| real.starts_with(root)) {
        return (
            InstallMethod::Homebrew,
            Tool::None,
            Some(reason::REPORT_ONLY_METHOD),
        );
    }
    // Rule 9: a system package.
    if real.starts_with("/usr") || real.starts_with("/opt") {
        return (
            InstallMethod::SystemPackage,
            Tool::None,
            Some(reason::REPORT_ONLY_METHOD),
        );
    }
    // Rule 10.
    (
        InstallMethod::Unknown,
        Tool::None,
        Some(reason::REPORT_ONLY_METHOD),
    )
}

fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The first executable `name` in the folders of `path_var`.
fn on_path(name: &str, path_var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let path_var = path_var?;
    std::env::split_paths(path_var)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|p| is_executable_file(p))
}

/// The `gpgv` verifier: the first executable on `path_var`, then the fixed
/// places.
#[must_use]
pub fn find_gpgv(path_var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    on_path("gpgv", path_var).or_else(|| fixed_gpgv(&is_executable_file))
}

/// `path` without `.` and with `..` applied by text.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// One hop of a link, made absolute; `None` for a file that is not a link.
fn hop_of(path: &Path) -> Option<PathBuf> {
    let target = std::fs::read_link(path).ok()?;
    if target.is_absolute() {
        Some(normalize(&target))
    } else {
        Some(normalize(&path.parent()?.join(target)))
    }
}

/// Resolves the launcher on the instance PATH (same rule as the launch),
/// builds `chain` (more than 8 hops: `Err(not_found)`) and calls
/// [`classify`].
///
/// # Errors
/// [`reason::NOT_FOUND`] when the launcher is not found or the chain is too
/// long.
pub fn detect(kind: ProviderKind, binary: &Path, env: &ChildEnv) -> Result<Install, &'static str> {
    let path_var = env.get("PATH");
    let launcher = if binary.components().count() > 1 || binary.is_absolute() {
        if is_executable_file(binary) {
            binary.to_path_buf()
        } else {
            return Err(reason::NOT_FOUND);
        }
    } else {
        let name = binary.to_str().ok_or(reason::NOT_FOUND)?;
        on_path(name, path_var).ok_or(reason::NOT_FOUND)?
    };
    let mut chain = vec![launcher.clone()];
    loop {
        let last = chain.last().ok_or(reason::NOT_FOUND)?;
        let Some(next) = hop_of(last) else { break };
        if chain.len() > MAX_HOPS {
            return Err(reason::NOT_FOUND);
        }
        chain.push(next);
    }
    let real = chain.last().ok_or(reason::NOT_FOUND)?.clone();
    let home = env.get("HOME").map(PathBuf::from);
    let (method, mut tool, mut blocked) =
        classify(kind, &chain, home.as_deref(), &hop_of, &|p: &Path| {
            p.exists()
        });
    // The instance PATH comes before the fixed places (§3.3).
    if let Tool::Native { gpgv, .. } = &mut tool
        && let Some(found) = find_gpgv(path_var)
    {
        *gpgv = Some(found);
        if blocked == Some(reason::GPGV_MISSING) {
            blocked = None;
        }
    }
    Ok(Install {
        kind,
        method,
        launcher,
        chain,
        real,
        tool,
        blocked,
    })
}

#[cfg(test)]
#[path = "detect_tests.rs"]
mod tests;
