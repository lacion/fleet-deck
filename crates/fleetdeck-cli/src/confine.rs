//! `fleetdeck confine-exec` (B9, DL-200): the hidden helper that confines the
//! writes of a provisioning command (Git) to the allowed root objects with
//! Landlock, then executes it.
//!
//! The helper opens each write root with the no-follow walk, handles every
//! write right of the best available Landlock ABI, allows them below each root
//! descriptor and on `/dev/null`, and enforces the ruleset on itself. Reads
//! are not restricted. It prints one status line on stderr before the `exec`:
//! `fleetdeck-confine: fully_enforced`, `partially_enforced` or
//! `not_enforced`. Exit codes: 125 the helper failed, 126 the `exec` failed.

use std::ffi::OsString;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;

/// The exit code when the confinement cannot be built.
const EXIT_CONFINE: i32 = 125;
/// The exit code when the `exec` fails.
const EXIT_EXEC: i32 = 126;

/// Confines the process, then executes `program`. Returns only an exit code,
/// when the helper or the `exec` failed.
#[must_use]
pub fn run(write_roots: &[PathBuf], program: &[OsString]) -> i32 {
    let Some((program, args)) = program.split_first() else {
        return 2;
    };
    match restrict(write_roots) {
        Ok(status) => eprintln!("fleetdeck-confine: {status}"),
        Err(message) => {
            eprintln!("fleetdeck-confine: {message}");
            return EXIT_CONFINE;
        }
    }
    let error = std::process::Command::new(program).args(args).exec();
    eprintln!("fleetdeck-confine: {}: {error}", program.to_string_lossy());
    EXIT_EXEC
}

#[cfg(target_os = "linux")]
fn restrict(write_roots: &[PathBuf]) -> Result<&'static str, String> {
    use landlock::{
        ABI, AccessFs, PathBeneath, RestrictionStatus, Ruleset, RulesetAttr as _,
        RulesetCreatedAttr as _, RulesetError, RulesetStatus,
    };

    let abi = ABI::V5;
    let mut roots = Vec::new();
    for root in write_roots {
        let fd = fleetdeck_core::launch_dir::open_root(root)
            .map_err(|_| format!("cannot open {}", root.display()))?;
        roots.push(fd);
    }
    let null =
        std::fs::File::open("/dev/null").map_err(|e| format!("cannot open /dev/null: {e}"))?;
    let enforce = || -> Result<RestrictionStatus, RulesetError> {
        let mut ruleset = Ruleset::default()
            .handle_access(AccessFs::from_write(abi))?
            .create()?;
        for fd in &roots {
            // A rule can grant only rights that the ruleset handles: the
            // write rights (reads are not restricted).
            ruleset = ruleset.add_rule(PathBeneath::new(fd, AccessFs::from_write(abi)))?;
        }
        ruleset = ruleset.add_rule(PathBeneath::new(&null, AccessFs::WriteFile))?;
        ruleset.restrict_self()
    };
    let status = enforce().map_err(|e| format!("cannot enforce the ruleset: {e}"))?;
    Ok(match status.ruleset {
        RulesetStatus::FullyEnforced => "fully_enforced",
        RulesetStatus::PartiallyEnforced => "partially_enforced",
        RulesetStatus::NotEnforced => "not_enforced",
    })
}

/// Other systems: no confinement (macOS is deferred, B9 section 6).
#[cfg(not(target_os = "linux"))]
fn restrict(_write_roots: &[PathBuf]) -> Result<&'static str, String> {
    Ok("not_enforced")
}
