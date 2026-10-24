//! `fleetdeck service status` (design service.md §9, decisions 12, 20, 24).

use std::io::Write;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::program::{self, CopyState};
use super::{
    Context, Deps, Installed, KINDS, Kind, Platform, installed, launchd, systemd, wsl_task,
};

/// What `status` knows about one kind.
struct Row {
    kind: Kind,
    info: Option<Installed>,
    link: PathBuf,
    /// The target of the program link; `None` when the link or its target
    /// file is missing.
    target: Option<PathBuf>,
    /// This program against the copy; `None` when the check failed.
    update: Option<CopyState>,
    active: bool,
    /// systemd only.
    enabled: Option<bool>,
}

fn read(ctx: &Context, deps: &dyn Deps, kind: Kind) -> anyhow::Result<Row> {
    let info = installed(ctx, kind)?;
    let root = kind.root(&ctx.dirs);
    let link = root.join("current");
    let mut row = Row {
        kind,
        info,
        target: None,
        update: None,
        active: false,
        enabled: None,
        link,
    };
    if row.info.is_none() {
        return Ok(row);
    }
    row.target = std::fs::read_link(&row.link)
        .ok()
        .map(|t| root.join(t))
        .filter(|t| std::fs::metadata(t).is_ok());
    row.update = program::copy_state(&root, &ctx.exe).ok();
    match ctx.platform {
        Platform::Systemd => {
            row.active = systemd::is_active(deps, kind);
            row.enabled = Some(systemd::is_enabled(deps, kind));
        }
        Platform::Launchd => row.active = launchd::running(ctx, deps, kind) == Some(true),
    }
    Ok(row)
}

fn yes(flag: bool) -> &'static str {
    if flag { "yes" } else { "no" }
}

fn text(row: &Row, ctx: &Context) -> String {
    let Some(info) = &row.info else {
        return format!("{}: not installed\n", row.kind.name());
    };
    let target = row
        .target
        .as_ref()
        .map_or_else(|| "missing".to_owned(), |t| t.display().to_string());
    let mut out = format!(
        "{}: installed\n  file: {}\n  data directory: {}\n  program: {} -> {target}\n  running: {}\n",
        row.kind.name(),
        info.path.display(),
        info.marker.as_deref().unwrap_or("unknown"),
        row.link.display(),
        yes(row.active)
    );
    if row.update == Some(CopyState::Newer) {
        out.push_str(
            "  Update: this fleetdeck is newer than the service program. Run \
             `fleetdeck service start` or `fleetdeck service install`.\n",
        );
    }
    if let Some(enabled) = row.enabled {
        out.push_str(&format!("  enabled: {}\n", yes(enabled)));
    }
    out.push_str(&format!("  logs: {}\n", row.kind.logs(ctx)));
    out
}

fn update_word(state: CopyState) -> &'static str {
    match state {
        CopyState::Same => "same",
        CopyState::Newer => "newer",
        CopyState::Older => "older",
        CopyState::Missing => "missing",
    }
}

fn json_row(row: &Row) -> Value {
    let installed = row.info.is_some();
    let mut value = json!({
        "kind": row.kind.name(),
        "installed": installed,
        "data_dir": row.info.as_ref().and_then(|i| i.marker.clone()),
        "program": installed.then(|| row.link.display().to_string()),
        "program_target": row.target.as_ref().map(|t| t.display().to_string()),
        "active": row.active,
        "enabled": row.enabled,
    });
    if let Some(state) = row.update {
        value["program_update"] = json!(update_word(state));
    }
    value
}

/// Prints the status of the three kinds.
pub fn run(
    ctx: &Context,
    deps: &dyn Deps,
    json_form: bool,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let rows = KINDS
        .iter()
        .map(|kind| read(ctx, deps, *kind))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let systemd_platform = ctx.platform == Platform::Systemd;
    let linger = if systemd_platform {
        systemd::linger(ctx, deps)
    } else {
        None
    };
    let task = ctx.wsl.then(|| wsl_task::status(ctx, deps));
    if json_form {
        let mut logs = Map::new();
        for row in rows.iter().filter(|r| r.info.is_some()) {
            logs.insert(row.kind.name().to_owned(), json!(row.kind.logs(ctx)));
        }
        let platform = if systemd_platform {
            "systemd"
        } else {
            "launchd"
        };
        let value = json!({
            "platform": platform,
            "services": rows.iter().map(json_row).collect::<Vec<_>>(),
            "linger": linger,
            "wsl_task": task,
            "logs": logs,
        });
        writeln!(out, "{}", serde_json::to_string_pretty(&value)?)?;
        return Ok(());
    }
    for row in &rows {
        write!(out, "{}", text(row, ctx))?;
    }
    if systemd_platform {
        let word = match linger {
            Some(true) => "on",
            Some(false) => "off",
            None => "unknown",
        };
        writeln!(out, "linger: {word}")?;
    }
    if let Some(task) = task {
        writeln!(out, "Windows logon task: {task}")?;
    }
    Ok(())
}
