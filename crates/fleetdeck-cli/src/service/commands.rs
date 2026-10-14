//! The five commands of `fleetdeck service` (design service.md §3, §8).

use std::io::Write;
use std::path::Path;

use anyhow::{Context as _, bail};
use fleetdeck_core::config::HostChoice;

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
    ulid: &str,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    // The service leaves these entries out of its PATH (NV-22).
    for note in &ctx.path_dropped {
        writeln!(out, "warning: PATH entry {note}; the service leaves it out")?;
    }
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
        "The service runs a copy of this program. Run `fleetdeck service install` again after \
         you build or install a new version."
    )?;
    writeln!(
        out,
        "The service uses the PATH of this shell. Run `fleetdeck service install` again after \
         you install a provider CLI in a new directory."
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
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    for (kind, _) in select(ctx, named)? {
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
