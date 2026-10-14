//! macOS: launchd user agents (design service.md §7).

use std::path::{Path, PathBuf};

use super::runner::Output;
use super::{
    Context, Deps, Done, Kind, Outcome, Plan, Probe, RELOAD, WRITE, act, command_line, failure,
    query,
};

const LAUNCHCTL: &str = "launchctl";

/// The text that replaces "Started." when no graphical login exists.
pub const NO_DOMAIN: &str = "Installed. It starts at your next graphical login.";

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn domain(ctx: &Context) -> String {
    format!("gui/{}", ctx.uid)
}

fn target(ctx: &Context, kind: Kind) -> String {
    format!("gui/{}/{}", ctx.uid, kind.label())
}

/// `<home>/Library/LaunchAgents/<label>.plist`.
pub fn file_path(ctx: &Context, kind: Kind) -> PathBuf {
    ctx.home
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", kind.label()))
}

/// `<home>/Library/Logs/fleetdeck/<log name>.log`.
pub fn log_path(ctx: &Context, kind: Kind) -> PathBuf {
    ctx.home
        .join("Library/Logs/fleetdeck")
        .join(format!("{}.log", kind.log_name()))
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

fn hex(text: &str) -> String {
    text.bytes().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<String> {
    if text.is_empty() || !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}

/// The directory of the data marker comment of a plist. A marker that does
/// not decode is no marker.
pub fn marker(text: &str) -> Option<String> {
    let rest = text.split("<!-- fleetdeck-data-hex: ").nth(1)?;
    let (digits, _) = rest.split_once(" -->")?;
    unhex(digits)
}

fn string(key: &str, value: &str) -> String {
    format!(
        "    <key>{key}</key>\n    <string>{}</string>\n",
        escape(value)
    )
}

/// The text of the plist. Fails before any change when a text has a control
/// character or an XDG value is relative.
pub fn render(ctx: &Context, kind: Kind, program: &Path) -> anyhow::Result<String> {
    let env = super::validate(ctx, program)?;
    let log = log_path(ctx, kind).to_string_lossy().into_owned();
    let mut text = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <!-- fleetdeck-data-hex: {} -->\n\
         <plist version=\"1.0\">\n<dict>\n",
        hex(&ctx.dirs.data.to_string_lossy())
    );
    text.push_str(&string("Label", kind.label()));
    text.push_str("    <key>ProgramArguments</key>\n    <array>\n");
    let program = program.to_string_lossy();
    for arg in
        std::iter::once(program.as_ref()).chain(ctx.exec_args(kind).iter().map(String::as_str))
    {
        text.push_str(&format!("        <string>{}</string>\n", escape(arg)));
    }
    text.push_str("    </array>\n    <key>EnvironmentVariables</key>\n    <dict>\n");
    for (name, value) in &env {
        text.push_str(&format!(
            "        <key>{}</key>\n        <string>{}</string>\n",
            escape(name),
            escape(value)
        ));
    }
    text.push_str("    </dict>\n");
    text.push_str(&string("StandardOutPath", &log));
    text.push_str(&string("StandardErrorPath", &log));
    text.push_str(
        "    <key>RunAtLoad</key>\n    <true/>\n\
         \x20   <key>KeepAlive</key>\n    <dict>\n        <key>SuccessfulExit</key>\n        <false/>\n    </dict>\n\
         \x20   <key>ExitTimeOut</key>\n    <integer>30</integer>\n\
         \x20   <key>Umask</key>\n    <integer>63</integer>\n\
         \x20   <key>ProcessType</key>\n    <string>Background</string>\n",
    );
    if kind.keeps_agents() {
        text.push_str("    <key>AbandonProcessGroup</key>\n    <true/>\n");
    }
    text.push_str("</dict>\n</plist>\n");
    Ok(text)
}

/// The answer of `launchctl print gui/<uid>/<label>`: `None` when the job
/// is not loaded, else whether a line says `state = running`.
pub fn running(ctx: &Context, deps: &dyn Deps, kind: Kind) -> Option<bool> {
    let args = strings(&["print", &target(ctx, kind)]);
    let out = query(deps, LAUNCHCTL, &args).filter(Output::success)?;
    Some(out.stdout.lines().any(|l| l.trim() == "state = running"))
}

/// `launchctl print gui/<uid>` succeeds: a graphical login exists.
pub fn has_domain(ctx: &Context, deps: &dyn Deps) -> bool {
    let args = strings(&["print", &domain(ctx)]);
    query(deps, LAUNCHCTL, &args).is_some_and(|out| out.success())
}

fn not_loaded(out: &Output) -> bool {
    matches!(out.status, Some(3 | 113))
        || out.stderr.contains("No such process")
        || out.stderr.contains("Could not find service")
}

/// `launchctl bootout gui/<uid>/<label>`; a job that is not loaded is no
/// error.
pub fn bootout(ctx: &Context, deps: &dyn Deps, kind: Kind) -> anyhow::Result<()> {
    let args = strings(&["bootout", &target(ctx, kind)]);
    match deps.run(LAUNCHCTL, &args, None, WRITE) {
        Ok(out) if out.success() || not_loaded(&out) => Ok(()),
        other => Err(failure(LAUNCHCTL, &args, WRITE, &other)),
    }
}

fn enable(ctx: &Context, deps: &dyn Deps, kind: Kind) -> anyhow::Result<()> {
    act(
        deps,
        LAUNCHCTL,
        &strings(&["enable", &target(ctx, kind)]),
        RELOAD,
    )
    .map(drop)
}

fn bootstrap_args(ctx: &Context, kind: Kind) -> anyhow::Result<Vec<String>> {
    let path = file_path(ctx, kind);
    Ok(strings(&[
        "bootstrap",
        &domain(ctx),
        &path.to_string_lossy(),
    ]))
}

fn bootstrap(ctx: &Context, deps: &dyn Deps, kind: Kind) -> anyhow::Result<()> {
    act(deps, LAUNCHCTL, &bootstrap_args(ctx, kind)?, WRITE).map(drop)
}

/// The commands of `--dry-run`, in the real order.
pub fn dry_commands(p: &Plan, probe: &Probe) -> anyhow::Result<Vec<String>> {
    let ctx = p.ctx;
    let mut lines = Vec::new();
    lines.extend(super::program::dry_line(
        &p.root,
        &ctx.exe,
        &ctx.version,
        p.ulid,
    )?);
    if probe.loaded {
        lines.push(command_line(
            LAUNCHCTL,
            &strings(&["bootout", &target(ctx, p.kind)]),
        ));
    }
    if has_domain(ctx, p.deps) {
        lines.push(command_line(
            LAUNCHCTL,
            &strings(&["enable", &target(ctx, p.kind)]),
        ));
        if !probe.other {
            lines.push(command_line(LAUNCHCTL, &bootstrap_args(ctx, p.kind)?));
        }
    }
    Ok(lines)
}

/// Creates the log directory (0700) and the empty log file (0600), then
/// writes the plist.
fn write_all(ctx: &Context, kind: Kind, path: &Path, text: &str) -> anyhow::Result<()> {
    let log = log_path(ctx, kind);
    if let Some(dir) = log.parent() {
        super::private_dir(dir)?;
    }
    super::private_file(&log)?;
    super::write_file(path, text)
}

/// The real install, after the checks, the validation and the probe.
pub fn install(p: &Plan, probe: &Probe) -> anyhow::Result<Done> {
    let (ctx, deps, kind) = (p.ctx, p.deps, p.kind);
    let has_domain = has_domain(ctx, deps);
    super::program::ensure(&p.root, &ctx.exe, &ctx.version, p.ulid)?;
    if probe.loaded {
        bootout(ctx, deps, kind)?;
    }
    if !has_domain {
        write_all(ctx, kind, &p.path, &p.text)?;
        return Ok(Done {
            linger: None,
            wsl: None,
            outcome: Outcome::NoDomain,
        });
    }
    write_all(ctx, kind, &p.path, &p.text)?;
    enable(ctx, deps, kind)?;
    // The job was stopped by the bootout above: its sockets belong to it,
    // not to another process.
    let outcome = if super::other_running_after(ctx, deps, kind, probe.active) {
        Outcome::Other
    } else {
        bootstrap(ctx, deps, kind)?;
        if super::wait_ready(ctx, deps, kind) {
            Outcome::Started
        } else {
            Outcome::NotReady
        }
    };
    Ok(Done {
        linger: None,
        wsl: None,
        outcome,
    })
}

/// The start step of `start`: enable; bootstrap when not loaded, kickstart
/// when loaded but not running.
pub fn start(ctx: &Context, deps: &dyn Deps, kind: Kind, probe: &Probe) -> anyhow::Result<()> {
    enable(ctx, deps, kind)?;
    if !probe.loaded {
        bootstrap(ctx, deps, kind)
    } else if !probe.active {
        act(
            deps,
            LAUNCHCTL,
            &strings(&["kickstart", &target(ctx, kind)]),
            WRITE,
        )
        .map(drop)
    } else {
        Ok(())
    }
}

/// The stop step: bootout (the file stays).
pub fn stop(ctx: &Context, deps: &dyn Deps, kind: Kind) -> anyhow::Result<()> {
    bootout(ctx, deps, kind)
}
