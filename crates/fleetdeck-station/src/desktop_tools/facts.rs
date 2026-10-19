//! `station.tools` (`board-gaps.md` §2.3): parsed facts about the desktop
//! tools. The answer has no path, no raw output, no environment value and no
//! device identifier. A failed probe leaves the other facts usable.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::desktop_tools::{
    FactStatus, KeyboardFacts, PluginFacts, PluginStatus, Probe, StationTools, Switch, ToolFact,
    ToolsDesktop, VersionSource,
};
use serde_json::Value;

use super::mapping::{TOOLS, Tool};
use super::probe::{Exec, PROBE_LIMIT, ProbeRun, Probes, env_pairs, pacman_env};
use crate::desktop::detect::Found;
use crate::desktop::env::desktop_env;
use crate::desktop::run::resolve;

/// The package manager.
pub const PACMAN: &str = "/usr/bin/pacman";
/// The most keyboard layouts in the answer.
const MAX_LAYOUTS: usize = 8;
/// The folder of the Fleet Hyprland plugin below the config home.
const PLUGIN_DIR: [&str; 3] = ["omarchy", "plugins", "fleetdeck.fleet"];

/// The desktop of one call and the environment of its children.
pub struct Context {
    pub desktop: ToolsDesktop,
    pub env: ChildEnv,
}

/// The desktop that `found` describes and the environment that the desktop
/// operations use: the desktop environment of `found`, else `base`.
#[must_use]
pub fn context(base: &ChildEnv, found: Option<&Found>) -> Context {
    let desktop = match found {
        None => ToolsDesktop::None,
        Some(f) if f.omarchy.is_some() => ToolsDesktop::Omarchy,
        Some(_) => ToolsDesktop::Hyprland,
    };
    let env = found
        .and_then(|f| desktop_env(base, f).ok())
        .unwrap_or_else(|| base.clone());
    Context { desktop, env }
}

/// The facts of every tool, the keyboard layouts, the plugin and the sudo
/// switch. The whole call lasts `total` at most.
pub async fn collect(
    exec: &dyn Exec,
    base: &ChildEnv,
    found: Option<&Found>,
    total: Duration,
) -> StationTools {
    let probes = Probes::new(exec, total);
    let ctx = context(base, found);
    let env = env_pairs(&ctx.env);
    let desktop = ctx.desktop.clone();
    let (tools, keyboard, plugin, passwordless_sudo) = tokio::join!(
        futures_util::future::join_all(TOOLS.iter().map(|t| tool_fact(&probes, &ctx.env, t))),
        keyboard(&probes, &ctx.env, &env, found.is_some()),
        plugin(&probes, base, &ctx, &env),
        sudo(&probes, &ctx.env, &env),
    );
    StationTools {
        sampled_at: super::now_ms(),
        desktop,
        tools,
        keyboard,
        plugin,
        passwordless_sudo,
    }
}

fn valid_version(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b':' | b'+' | b'_' | b'~' | b'-')
        })
}

/// `(package, version)` of the line `<path> is owned by <pkg> <ver>\n`.
fn parse_owned(stdout: &[u8], path: &Path) -> Option<(String, String)> {
    let text = std::str::from_utf8(stdout).ok()?;
    let line = text.strip_suffix('\n').unwrap_or(text);
    let rest = line.strip_prefix(&format!("{} is owned by ", path.to_str()?))?;
    let mut fields = rest.split(' ');
    let (package, version) = (fields.next()?, fields.next()?);
    if fields.next().is_some() || package.is_empty() || version.is_empty() {
        return None;
    }
    Some((package.to_owned(), version.to_owned()))
}

fn fact(tool: &Tool, present: bool, probe: Probe) -> ToolFact {
    ToolFact {
        id: tool.id.to_owned(),
        present,
        version: None,
        version_source: VersionSource::None,
        package_matches_executable: None,
        probe,
    }
}

async fn tool_fact(probes: &Probes<'_>, env: &ChildEnv, tool: &Tool) -> ToolFact {
    let Some(path) = resolve(env, tool.exe) else {
        return fact(tool, false, Probe::Missing);
    };
    let pacman = Path::new(PACMAN);
    if !probes.exists(pacman) {
        return fact(tool, true, Probe::Unknown);
    }
    let args = [OsString::from("-Qo"), path.clone().into_os_string()];
    match probes.run(pacman, &args, &pacman_env(), PROBE_LIMIT).await {
        ProbeRun::Exit { code: 0, stdout } => match parse_owned(&stdout, &path) {
            Some((package, version)) => {
                let valid = valid_version(&version);
                ToolFact {
                    version: valid.then_some(version),
                    version_source: if valid {
                        VersionSource::Package
                    } else {
                        VersionSource::None
                    },
                    package_matches_executable: Some(package == tool.owner_name()),
                    ..fact(tool, true, Probe::Ok)
                }
            }
            None => fact(tool, true, Probe::Failed),
        },
        ProbeRun::Exit { code: 1, .. } => ToolFact {
            package_matches_executable: Some(false),
            ..fact(tool, true, Probe::Ok)
        },
        ProbeRun::Timeout => fact(tool, true, Probe::Timeout),
        ProbeRun::Exit { .. } | ProbeRun::Failed => fact(tool, true, Probe::Failed),
    }
}

fn valid_layout(text: &str) -> bool {
    (1..=32).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// The layouts of `hyprctl devices -j`: every comma-separated part of each
/// keyboard that is valid, without duplicates, sorted, at most eight.
fn parse_layouts(stdout: &[u8]) -> Option<Vec<String>> {
    let json: Value = serde_json::from_slice(stdout).ok()?;
    let keyboards = json.get("keyboards")?.as_array()?;
    let mut layouts: Vec<String> = keyboards
        .iter()
        .filter_map(|k| k.get("layout")?.as_str())
        .flat_map(|text| text.split(','))
        .map(str::trim)
        .filter(|part| valid_layout(part))
        .map(str::to_owned)
        .collect();
    layouts.sort();
    layouts.dedup();
    layouts.truncate(MAX_LAYOUTS);
    Some(layouts)
}

async fn keyboard(
    probes: &Probes<'_>,
    env: &ChildEnv,
    pairs: &[(OsString, OsString)],
    desktop_found: bool,
) -> KeyboardFacts {
    let state = |status, layouts| KeyboardFacts { status, layouts };
    let hyprctl = if desktop_found {
        resolve(env, "hyprctl")
    } else {
        None
    };
    let Some(hyprctl) = hyprctl else {
        return state(FactStatus::Unavailable, Vec::new());
    };
    let args = [OsString::from("devices"), OsString::from("-j")];
    match probes.run(&hyprctl, &args, pairs, PROBE_LIMIT).await {
        ProbeRun::Exit { code: 0, stdout } => parse_layouts(&stdout).map_or_else(
            || state(FactStatus::Failed, Vec::new()),
            |l| state(FactStatus::Ok, l),
        ),
        _ => state(FactStatus::Failed, Vec::new()),
    }
}

/// The config home: `XDG_CONFIG_HOME` of `base` when it is absolute, else
/// `<HOME>/.config`.
fn config_home(base: &ChildEnv) -> Option<PathBuf> {
    if let Some(xdg) = base.get("XDG_CONFIG_HOME").map(PathBuf::from)
        && xdg.is_absolute()
    {
        return Some(xdg);
    }
    let home = base.get("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".config"))
}

async fn plugin(
    probes: &Probes<'_>,
    base: &ChildEnv,
    ctx: &Context,
    pairs: &[(OsString, OsString)],
) -> PluginFacts {
    let status = plugin_status(probes, base, ctx, pairs).await;
    PluginFacts { status }
}

async fn plugin_status(
    probes: &Probes<'_>,
    base: &ChildEnv,
    ctx: &Context,
    pairs: &[(OsString, OsString)],
) -> PluginStatus {
    if ctx.desktop != ToolsDesktop::Omarchy {
        return PluginStatus::Unavailable;
    }
    let Some(config) = config_home(base) else {
        return PluginStatus::Unknown;
    };
    let folder = PLUGIN_DIR.iter().fold(config, |p, part| p.join(part));
    if !probes.exists(&folder) {
        return PluginStatus::Unavailable;
    }
    let Some(validate) = resolve(&ctx.env, "omarchy-plugin-validate") else {
        return PluginStatus::Unavailable;
    };
    let args = [folder.into_os_string()];
    match probes.run(&validate, &args, pairs, PROBE_LIMIT).await {
        ProbeRun::Exit { code: 0, .. } => PluginStatus::Valid,
        ProbeRun::Exit { code: 1, .. } => PluginStatus::Invalid,
        _ => PluginStatus::Unknown,
    }
}

async fn sudo(probes: &Probes<'_>, env: &ChildEnv, pairs: &[(OsString, OsString)]) -> Switch {
    let Some(program) = resolve(env, "omarchy-sudo-passwordless") else {
        return Switch::Unknown;
    };
    let args = [OsString::from("--active")];
    match probes.run(&program, &args, pairs, PROBE_LIMIT).await {
        ProbeRun::Exit { code: 0, .. } => Switch::On,
        ProbeRun::Exit { code: 1, .. } => Switch::Off,
        _ => Switch::Unknown,
    }
}
