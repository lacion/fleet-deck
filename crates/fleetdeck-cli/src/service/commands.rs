//! The five commands of `fleetdeck service` (design service.md §3, §8).

use std::io::Write;
use std::path::Path;

use anyhow::{Context as _, bail};
use fleetdeck_core::config::HostChoice;

use super::path::lost_programs;
use super::program::{self, CopyState};
use super::{
    Context, Deps, Done, Kind, Outcome, Plan, Platform, all_installed, launchd, probe, select,
    systemd, wait_ready, wsl_task,
};

/// The text of a refusal to start while another process runs.
const OTHER: &str = "Fleet Deck runs now in another process. Stop it (Ctrl-C in its terminal), \
                     then run `fleetdeck service start`.";

fn other_installed() -> String {
    format!("Installed for automatic start. {OTHER}")
}

fn no_start(kind: Kind, ctx: &Context) -> anyhow::Error {
    anyhow::anyhow!(
        "The service did not start. Look at its log: {}",
        kind.logs(ctx)
    )
}

/// The conflict and data-directory rules of §3. Nothing changes.
fn conflicts(ctx: &Context, kind: Kind) -> anyhow::Result<()> {
    let found = all_installed(ctx)?;
    let has = |k: Kind| found.iter().any(|(f, _)| *f == k);
    let clash = match kind {
        Kind::Both => [Kind::Deck, Kind::Station].into_iter().find(|k| has(*k)),
        Kind::Deck | Kind::Station => has(Kind::Both).then_some(Kind::Both),
    };
    if let Some(other) = clash {
        let name = other.name();
        bail!(
            "The {name} service is installed. Remove it first: `fleetdeck service uninstall {name}`."
        );
    }
    if let Some((_, info)) = found.iter().find(|(k, _)| *k == kind) {
        let data = ctx.dirs.data.to_string_lossy();
        if info.marker.as_deref() != Some(data.as_ref()) {
            bail!(
                "The {name} service runs Fleet Deck with the data directory {dir}. Remove it \
                 first: `fleetdeck service uninstall {name}`.",
                name = kind.name(),
                dir = info.marker.as_deref().unwrap_or("unknown")
            );
        }
    }
    Ok(())
}

/// `fleetdeck service install`.
pub fn install(
    ctx: &Context,
    deps: &dyn Deps,
    kind: Kind,
    dry_run: bool,
    verbose: bool,
    ulid: &str,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    path_report(ctx, verbose, out)?;
    if ctx.platform == Platform::Systemd {
        systemd::checks(ctx, deps)?;
    }
    let root = kind.root(&ctx.dirs);
    let link = root.join("current");
    let path = kind.file(ctx)?;
    let text = match ctx.platform {
        Platform::Systemd => systemd::render(ctx, kind, &link)?,
        Platform::Launchd => launchd::render(ctx, kind, &link)?,
    };
    conflicts(ctx, kind)?;
    let first = probe(ctx, deps, kind);
    let plan = Plan {
        ctx,
        deps,
        kind,
        root,
        path,
        text,
        // The name of the release is not known before the copy.
        ulid: if dry_run { "<ulid>" } else { ulid },
    };
    if dry_run {
        let commands = match ctx.platform {
            Platform::Systemd => systemd::dry_commands(&plan, &first)?,
            Platform::Launchd => launchd::dry_commands(&plan, &first)?,
        };
        writeln!(out, "{}", plan.path.display())?;
        write!(out, "{}", plan.text)?;
        for line in commands {
            writeln!(out, "{line}")?;
        }
        return Ok(());
    }
    let done = match ctx.platform {
        Platform::Systemd => systemd::install(&plan)?,
        Platform::Launchd => launchd::install(&plan, &first)?,
    };
    summary(&plan, &done, &link, out)?;
    if done.outcome == Outcome::NotReady {
        return Err(no_start(kind, ctx));
    }
    Ok(())
}

/// What the install says about the `PATH`: the lost programs, one summary
/// line, and with `verbose` each folder that the service leaves out (NV-22).
fn path_report(ctx: &Context, verbose: bool, out: &mut dyn Write) -> anyhow::Result<()> {
    if let Some(shell) = &ctx.shell_path {
        for (name, dir) in lost_programs(shell, ctx.path.as_deref()) {
            let reason = ctx
                .path_dropped
                .iter()
                .find(|(entry, _)| *entry == dir)
                .map_or("not on the service PATH", |(_, reason)| reason.as_str());
            writeln!(
                out,
                "warning: the service cannot run `{name}`: it is in {dir}, which the service \
                 leaves out ({reason})."
            )?;
        }
    }
    if ctx.path_dropped.is_empty() {
        return Ok(());
    }
    let kept = ctx
        .path
        .as_deref()
        .map_or(0, |p| p.split(':').filter(|e| !e.is_empty()).count());
    writeln!(
        out,
        "PATH: the service keeps {kept} folder{} and leaves out {} that other users can change \
         or that do not exist. Run `fleetdeck service install --verbose` to list them.",
        if kept == 1 { "" } else { "s" },
        ctx.path_dropped.len()
    )?;
    if verbose {
        for (entry, reason) in &ctx.path_dropped {
            writeln!(out, "  {entry}: {reason}")?;
        }
    }
    Ok(())
}

fn host_line(host: HostChoice) -> &'static str {
    match host {
        HostChoice::Shim => {
            "Provider host: shim. Agents keep running through a service stop or restart."
        }
        HostChoice::Direct => {
            "Provider host: direct. The station ends its agents when the service stops. \
             Set host = \"shim\" in station.toml to keep them running."
        }
    }
}

fn summary(plan: &Plan, done: &Done, link: &Path, out: &mut dyn Write) -> anyhow::Result<()> {
    let (ctx, kind) = (plan.ctx, plan.kind);
    writeln!(
        out,
        "Installed the {} service: {}",
        kind.name(),
        plan.path.display()
    )?;
    let target =
        std::fs::read_link(link).map_or_else(|_| "missing".to_owned(), |t| t.display().to_string());
    writeln!(out, "Program: {} -> {target}", link.display())?;
    writeln!(out, "Data directory: {}", ctx.dirs.data.display())?;
    if kind.keeps_agents()
        && let Some(host) = ctx.host
    {
        writeln!(out, "{}", host_line(host))?;
    }
    for line in [&done.linger, &done.wsl].into_iter().flatten() {
        writeln!(out, "{line}")?;
    }
    match done.outcome {
        Outcome::Started => writeln!(out, "Started.")?,
        Outcome::Other => writeln!(out, "{}", other_installed())?,
        Outcome::NoDomain => writeln!(out, "{}", launchd::NO_DOMAIN)?,
        Outcome::NotReady => {}
    }
    writeln!(out, "Logs: {}", kind.logs(ctx))?;
    if ctx.platform == Platform::Launchd {
        writeln!(
            out,
            "A user agent starts when you log in and ends when you log out."
        )?;
    }
    writeln!(
        out,
        "Run `fleetdeck service install` again after you install a new Fleet Deck version or a \
         provider CLI in a new folder."
    )?;
    Ok(())
}

/// `fleetdeck service uninstall`.
pub fn uninstall(
    ctx: &Context,
    deps: &dyn Deps,
    named: Option<Kind>,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let found = select(ctx, named)?;
    let mut data = None;
    for (kind, info) in &found {
        match ctx.platform {
            Platform::Systemd => systemd::disable(deps, *kind)?,
            Platform::Launchd => launchd::bootout(ctx, deps, *kind)?,
        }
        std::fs::remove_file(&info.path)
            .with_context(|| format!("cannot remove {}", info.path.display()))?;
        if data.is_none() {
            data.clone_from(&info.marker);
        }
        writeln!(out, "Removed the {} service.", kind.name())?;
    }
    if ctx.platform == Platform::Systemd {
        systemd::reload_after_remove(deps)?;
    }
    if ctx.wsl
        && all_installed(ctx)?.is_empty()
        && let Err(warning) = wsl_task::remove(ctx, deps)
    {
        writeln!(out, "{warning}")?;
    }
    let data = data.unwrap_or_else(|| ctx.dirs.data.display().to_string());
    writeln!(out, "Your data and the program copies in {data} stay.")?;
    Ok(())
}

/// `fleetdeck service start`.
pub fn start(
    ctx: &Context,
    deps: &dyn Deps,
    named: Option<Kind>,
    ulid: &str,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    for (kind, _) in select(ctx, named)? {
        // A failed comparison never stops the start: the service keeps its program.
        let state = program::copy_state(&kind.root(&ctx.dirs), &ctx.exe).unwrap_or_else(|e| {
            let _ = writeln!(
                out,
                "Note: cannot compare this fleetdeck with the program of the {} service ({e:#}). \
                 The service keeps its program.",
                kind.name()
            );
            CopyState::Same
        });
        match state {
            CopyState::Newer => {
                writeln!(
                    out,
                    "This fleetdeck is newer than the program of the {} service. Installing it.",
                    kind.name()
                )?;
                install(ctx, deps, kind, false, false, ulid, out)?;
                continue;
            }
            CopyState::Older => writeln!(
                out,
                "Note: this fleetdeck ({}) is older than the program of the {} service. The \
                 service keeps its program. Run `fleetdeck service install` to use this one.",
                ctx.exe.display(),
                kind.name()
            )?,
            CopyState::Same | CopyState::Missing => {}
        }
        let seen = probe(ctx, deps, kind);
        if seen.other {
            bail!("{OTHER}");
        }
        match ctx.platform {
            Platform::Systemd => systemd::start(deps, kind)?,
            Platform::Launchd => launchd::start(ctx, deps, kind, &seen)?,
        }
        if !wait_ready(ctx, deps, kind) {
            return Err(no_start(kind, ctx));
        }
        writeln!(out, "Started the {} service.", kind.name())?;
    }
    Ok(())
}

/// `fleetdeck service stop`.
pub fn stop(
    ctx: &Context,
    deps: &dyn Deps,
    named: Option<Kind>,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    for (kind, _) in select(ctx, named)? {
        match ctx.platform {
            Platform::Systemd => systemd::stop(deps, kind)?,
            Platform::Launchd => launchd::stop(ctx, deps, kind)?,
        }
        writeln!(
            out,
            "Stopped the {} service. It starts again at the next boot or login.",
            kind.name()
        )?;
    }
    Ok(())
}
