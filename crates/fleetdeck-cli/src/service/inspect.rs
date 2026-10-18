//! The inspection of the installed station service for the provisioning
//! bootstrap (P8 T3b, `omarchy-p8.md` 3.2 step 3, decision D13): in-process,
//! never by the text of another command.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io;
use std::path::Path;

use fleetdeck_station::upgrade::CURRENT;

use super::{Context, Deps, Kind, Platform, all_installed, probe, query};

/// What the bootstrap finds of the station service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceInspect {
    /// No station service is installed.
    Absent,
    /// The installed station service is ours: same data directory, and its
    /// program is the running binary.
    Matching { active: bool },
    /// Another service, data directory or program is there (or the
    /// installation cannot be read).
    Conflict,
}

/// The SHA-256 of a file, as 64 lowercase hex digits (64 KiB reads).
///
/// # Errors
/// The file cannot be read.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    sha256_reader(File::open(path)?)
}

/// The SHA-256 of everything that `reader` gives, as 64 lowercase hex digits
/// (64 KiB reads).
///
/// # Errors
/// The reader fails.
pub fn sha256_reader(mut file: impl io::Read) -> io::Result<String> {
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        context.update(buffer.get(..n).unwrap_or_default());
    }
    let digest = context.finish();
    let mut out = String::with_capacity(64);
    for byte in digest.as_ref() {
        // Writing to a `String` cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

/// The `current` program of the station release directory is the running
/// binary (`ctx.exe`), byte for byte (by SHA-256).
fn program_matches(ctx: &Context) -> bool {
    let root = Kind::Station.root(&ctx.dirs);
    let Ok(target) = fs::read_link(root.join(CURRENT)) else {
        return false;
    };
    match (sha256_file(&root.join(target)), sha256_file(&ctx.exe)) {
        (Ok(copy), Ok(own)) => copy == own,
        _ => false,
    }
}

/// The `argv` that the installed station service must run: the release
/// `current` link and the arguments that the install renders.
fn expected_argv(ctx: &Context) -> Vec<String> {
    let root = Kind::Station.root(&ctx.dirs);
    std::iter::once(root.join(CURRENT).to_string_lossy().into_owned())
        .chain(ctx.exec_args(Kind::Station))
        .collect()
}

/// The effective `ExecStart` of the unit (drop-ins included) is exactly one
/// entry whose `path=` and `argv[]=` are the expected program and arguments.
/// The line is `{ path=<p> ; argv[]=<a> ; ignore_errors=<..> ; ... }`.
fn systemd_command_matches(ctx: &Context, deps: &dyn Deps) -> bool {
    let args: Vec<String> = [
        "--user",
        "show",
        Kind::Station.unit(),
        "-p",
        "ExecStart",
        "--value",
    ]
    .iter()
    .map(|a| (*a).to_owned())
    .collect();
    let Some(out) = query(deps, "systemctl", &args).filter(|out| out.success()) else {
        return false;
    };
    let mut lines = out.stdout.lines().filter(|l| !l.trim().is_empty());
    let (Some(line), None) = (lines.next(), lines.next()) else {
        return false;
    };
    let parsed = line
        .trim()
        .strip_prefix("{ ")
        .and_then(|l| l.strip_suffix(" }"))
        .and_then(|l| l.strip_prefix("path="))
        .and_then(|l| l.split_once(" ; argv[]="))
        .and_then(|(path, rest)| Some((path, rest.split_once(" ; ignore_errors=")?.0)));
    let expected = expected_argv(ctx);
    parsed.is_some_and(|(path, argv)| {
        expected.first().is_some_and(|program| path == program) && argv == expected.join(" ")
    })
}

/// Undoes the escaping of the plist writer (the five entities).
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The `<string>` items of the `ProgramArguments` array of a plist that the
/// install wrote; `None` when the array has another shape.
fn plist_arguments(text: &str) -> Option<Vec<String>> {
    let rest = text.split_once("<key>ProgramArguments</key>")?.1;
    let rest = rest.trim_start().strip_prefix("<array>")?;
    let (array, _) = rest.split_once("</array>")?;
    array
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            l.strip_prefix("<string>")
                .and_then(|l| l.strip_suffix("</string>"))
                .map(unescape)
        })
        .collect()
}

/// The `ProgramArguments` of the installed plist are exactly the expected
/// program and arguments (launchd has no drop-ins).
fn launchd_command_matches(ctx: &Context, plist: &Path) -> bool {
    let Ok(text) = fs::read_to_string(plist) else {
        return false;
    };
    plist_arguments(&text).is_some_and(|argv| argv == expected_argv(ctx))
}

/// Inspects the station service. An error while it reads the installation
/// is a [`ServiceInspect::Conflict`].
#[must_use]
pub fn inspect(ctx: &Context, deps: &dyn Deps) -> ServiceInspect {
    let Ok(found) = all_installed(ctx) else {
        return ServiceInspect::Conflict;
    };
    if found.iter().any(|(kind, _)| *kind == Kind::Both) {
        return ServiceInspect::Conflict;
    }
    let Some((_, info)) = found.iter().find(|(kind, _)| *kind == Kind::Station) else {
        return ServiceInspect::Absent;
    };
    let data = ctx.dirs.data.to_string_lossy();
    if info.marker.as_deref() != Some(data.as_ref()) || !program_matches(ctx) {
        return ServiceInspect::Conflict;
    }
    let command = match ctx.platform {
        Platform::Systemd => systemd_command_matches(ctx, deps),
        Platform::Launchd => launchd_command_matches(ctx, &info.path),
    };
    if !command {
        return ServiceInspect::Conflict;
    }
    ServiceInspect::Matching {
        active: probe(ctx, deps, Kind::Station).active,
    }
}

#[cfg(test)]
#[path = "inspect_tests.rs"]
mod tests;
