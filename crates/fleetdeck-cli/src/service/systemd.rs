//! Linux and WSL: systemd user services (design service.md §5).

use std::path::{Path, PathBuf};

use anyhow::bail;

use super::{
    Context, Deps, Done, Kind, Outcome, Plan, Probe, QUERY, RELOAD, WRITE, act, command_line,
    query, wsl_task,
};

/// The time limit of `enable-linger`.
const LINGER_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

const SYSTEMCTL: &str = "systemctl";
const LOGINCTL: &str = "loginctl";

/// `["--user", ...rest]`.
fn user(rest: &[&str]) -> Vec<String> {
    std::iter::once("--user")
        .chain(rest.iter().copied())
        .map(str::to_owned)
        .collect()
}

/// `<config home>/systemd/user`.
pub fn unit_dir(ctx: &Context) -> anyhow::Result<PathBuf> {
    let base = match super::absolute_xdg("XDG_CONFIG_HOME", ctx.xdg_config_home.as_deref())? {
        Some(dir) => dir,
        None => ctx.home.join(".config"),
    };
    Ok(base.join("systemd").join("user"))
}

/// The unit file of `kind`.
pub fn file_path(ctx: &Context, kind: Kind) -> anyhow::Result<PathBuf> {
    Ok(unit_dir(ctx)?.join(kind.unit()))
}

/// The three checks of §5 before any change.
pub fn checks(ctx: &Context, deps: &dyn Deps) -> anyhow::Result<()> {
    if !ctx.systemd_booted {
        if ctx.wsl_kernel {
            bail!(
                "systemd is off in this WSL distro. Add these two lines to /etc/wsl.conf \
                 (needs sudo): `[boot]` and `systemd=true`. Then run `wsl.exe --shutdown` in \
                 Windows and open the distro again."
            );
        }
        bail!(
            "This machine does not run systemd. Run `fleetdeck up` under your own process supervisor."
        );
    }
    let args = user(&["show", "--property=UnitPath", "--value"]);
    let result = deps.run(SYSTEMCTL, &args, None, QUERY);
    let out = match result {
        Ok(out) if out.success() => out,
        other => {
            let detail = match &other {
                Ok(out) => out
                    .stderr_line()
                    .map_or_else(|| out.status_text(), str::to_owned),
                Err(e) => e.text(SYSTEMCTL, QUERY),
            };
            bail!(
                "The systemd user manager does not answer ({detail}). Log in to this user once \
                 (not with `su`), then run the command again."
            );
        }
    };
    let dir = unit_dir(ctx)?;
    let read = out.stdout.split_whitespace().any(|entry| {
        let entry = entry.trim_end_matches('/');
        Path::new(entry) == dir
    });
    if !read {
        bail!(
            "systemd does not read {}; set XDG_CONFIG_HOME the same for your shell and your \
             login, or unset it.",
            dir.display()
        );
    }
    Ok(())
}

fn quote_program(text: &str) -> String {
    let body = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{body}\"")
}

fn quote_argument(text: &str) -> String {
    let body = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    format!("\"{body}\"")
}

fn quote_environment(name: &str, value: &str) -> String {
    let body = format!("{name}={value}")
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{body}\"")
}

/// The text of the unit file. Fails before any change when a text has a
/// control character or an XDG value is relative.
pub fn render(ctx: &Context, kind: Kind, program: &Path) -> anyhow::Result<String> {
    let env = super::validate(ctx, program)?;
    let data = ctx.dirs.data.to_string_lossy();
    let exec: Vec<String> = std::iter::once(quote_program(&program.to_string_lossy()))
        .chain(ctx.exec_args(kind).iter().map(|a| quote_argument(a)))
        .collect();
    let mut text = format!(
        "# Written by fleetdeck service install. fleetdeck service uninstall removes it.\n\
         # fleetdeck-data: {data}\n\
         [Unit]\n\
         Description={}\n\
         Documentation=https://github.com/lacion/fleet-deck/blob/v2/docs/v2/operations.md\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={}\n",
        kind.description(),
        exec.join(" ")
    );
    for (name, value) in &env {
        text.push_str(&format!("Environment={}\n", quote_environment(name, value)));
    }
    if kind.keeps_agents() {
        text.push_str("KillMode=process\n");
    }
    text.push_str(
        "KillSignal=SIGTERM\nTimeoutStopSec=30\nRestart=on-failure\nRestartSec=2\nUMask=0077\n",
    );
    if kind == Kind::Deck {
        text.push_str("NoNewPrivileges=true\n");
    }
    text.push_str("\n[Install]\nWantedBy=default.target\n");
    Ok(text)
}

/// The directory of the data marker line of a unit file.
pub fn marker(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("# fleetdeck-data: "))
        .map(str::to_owned)
}

/// `systemctl --user is-active <unit>` is `active`.
pub fn is_active(deps: &dyn Deps, kind: Kind) -> bool {
    let args = user(&["is-active", kind.unit()]);
    query(deps, SYSTEMCTL, &args).is_some_and(|out| out.success() && out.stdout.trim() == "active")
}

/// `systemctl --user is-enabled <unit>` succeeds.
pub fn is_enabled(deps: &dyn Deps, kind: Kind) -> bool {
    let args = user(&["is-enabled", kind.unit()]);
    query(deps, SYSTEMCTL, &args).is_some_and(|out| out.success())
}

/// The linger state of the user: `Some(true)` for `yes`, `Some(false)` for
/// `no`, `None` when the query fails or says another text.
pub fn linger(ctx: &Context, deps: &dyn Deps) -> Option<bool> {
    let args = vec![
        "show-user".to_owned(),
        ctx.uid.to_string(),
        "--property=Linger".to_owned(),
        "--value".to_owned(),
    ];
    let out = query(deps, LOGINCTL, &args).filter(|out| out.success())?;
    match out.stdout.trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

fn enable_linger_args(ctx: &Context) -> Vec<String> {
    vec![
        "--no-ask-password".to_owned(),
        "enable-linger".to_owned(),
        ctx.uid.to_string(),
    ]
}

/// The linger step: the summary line (§5).
fn linger_line(ctx: &Context, deps: &dyn Deps) -> String {
    let on = format!(
        "Linger is on for {}: your user services start at boot, without a login.",
        ctx.user
    );
    let off = format!(
        "Your services start when {0} logs in. To start them at boot, run: \
         `sudo loginctl enable-linger {0}`.",
        ctx.user
    );
    match linger(ctx, deps) {
        Some(true) => on,
        Some(false) => {
            let args = enable_linger_args(ctx);
            match deps.run(LOGINCTL, &args, None, LINGER_LIMIT) {
                Ok(out) if out.success() => on,
                _ => off,
            }
        }
        None => off,
    }
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
    lines.push(command_line(SYSTEMCTL, &user(&["daemon-reload"])));
    lines.push(command_line(SYSTEMCTL, &user(&["enable", p.kind.unit()])));
    if linger(ctx, p.deps) == Some(false) {
        lines.push(command_line(LOGINCTL, &enable_linger_args(ctx)));
    }
    if ctx.wsl {
        lines.push(wsl_task::dry_line(ctx));
    }
    if !probe.other {
        lines.push(command_line(SYSTEMCTL, &user(&["restart", p.kind.unit()])));
    }
    Ok(lines)
}

/// The real install, after the checks, the validation and the probe.
pub fn install(p: &Plan) -> anyhow::Result<Done> {
    let (ctx, deps, kind) = (p.ctx, p.deps, p.kind);
    super::program::ensure(&p.root, &ctx.exe, &ctx.version, p.ulid)?;
    super::write_file(&p.path, &p.text)?;
    act(deps, SYSTEMCTL, &user(&["daemon-reload"]), RELOAD)?;
    act(deps, SYSTEMCTL, &user(&["enable", kind.unit()]), RELOAD)?;
    let linger = Some(linger_line(ctx, deps));
    let wsl = ctx.wsl.then(|| wsl_task::install(ctx, deps));
    let outcome = if super::other_running(ctx, deps, kind) {
        Outcome::Other
    } else {
        act(deps, SYSTEMCTL, &user(&["restart", kind.unit()]), WRITE)?;
        if super::wait_ready(ctx, deps, kind) {
            Outcome::Started
        } else {
            Outcome::NotReady
        }
    };
    Ok(Done {
        linger,
        wsl,
        outcome,
    })
}

/// `systemctl --user start <unit>`.
pub fn start(deps: &dyn Deps, kind: Kind) -> anyhow::Result<()> {
    act(deps, SYSTEMCTL, &user(&["start", kind.unit()]), WRITE).map(drop)
}

/// `systemctl --user stop <unit>`.
pub fn stop(deps: &dyn Deps, kind: Kind) -> anyhow::Result<()> {
    act(deps, SYSTEMCTL, &user(&["stop", kind.unit()]), WRITE).map(drop)
}

/// `systemctl --user disable --now <unit>`.
pub fn disable(deps: &dyn Deps, kind: Kind) -> anyhow::Result<()> {
    act(
        deps,
        SYSTEMCTL,
        &user(&["disable", "--now", kind.unit()]),
        WRITE,
    )
    .map(drop)
}

/// `systemctl --user daemon-reload`, once at the end of an uninstall.
pub fn reload_after_remove(deps: &dyn Deps) -> anyhow::Result<()> {
    act(deps, SYSTEMCTL, &user(&["daemon-reload"]), RELOAD)
        .map(drop)
        .map_err(|e| anyhow::anyhow!("The service files are removed, but {e}"))
}
