//! The inspection of the installed station service for the provisioning
//! bootstrap (P8 T3b, `omarchy-p8.md` 3.2 step 3, decision D13): in-process,
//! never by the text of another command.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::path::Path;

use fleetdeck_station::upgrade::CURRENT;

use super::{Context, Deps, Kind, all_installed, probe};

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
    let mut file = File::open(path)?;
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
    ServiceInspect::Matching {
        active: probe(ctx, deps, Kind::Station).active,
    }
}

#[cfg(test)]
#[path = "inspect_tests.rs"]
mod tests;
