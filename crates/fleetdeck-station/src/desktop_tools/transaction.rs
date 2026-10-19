//! The dry run of the install plan (`board-gaps.md` §2.4, revision 4): the
//! complete package transaction must come from `core` and `extra`, and it
//! must hold every target.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use fleetdeck_proto::desktop_tools::ResolvedPackage;

use super::facts::PACMAN;
use super::mapping::REPOS;
use super::probe::{Exec, ProbeRun, Probes, pacman_env};

/// The longest time of the dry run.
pub const DRY_RUN_LIMIT: Duration = Duration::from_secs(30);

/// The result of the dry run.
#[derive(Debug, PartialEq, Eq)]
pub enum DryRun {
    /// The expected transaction, in the order of pacman.
    Plan(Vec<ResolvedPackage>),
    /// The transaction is refused. The names are the packages that are not
    /// allowed or missing; the list is empty when the dry run failed or its
    /// output is not valid.
    Blocked(Vec<String>),
}

/// Runs `/usr/bin/pacman -Sp --needed --print-format "%r %n %v" <targets>`
/// without root. `targets` are `<repo>/<name>` strings.
pub async fn dry_run(exec: &dyn Exec, targets: &[String]) -> DryRun {
    let mut args: Vec<OsString> = ["-Sp", "--needed", "--print-format", "%r %n %v"]
        .iter()
        .map(OsString::from)
        .collect();
    args.extend(targets.iter().map(OsString::from));
    let probes = Probes::new(exec, DRY_RUN_LIMIT);
    let run = probes
        .run(Path::new(PACMAN), &args, &pacman_env(), DRY_RUN_LIMIT)
        .await;
    let ProbeRun::Exit { code: 0, stdout } = run else {
        return DryRun::Blocked(Vec::new());
    };
    let Ok(text) = std::str::from_utf8(&stdout) else {
        return DryRun::Blocked(Vec::new());
    };
    let mut transaction = Vec::new();
    let mut refused = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let [repo, name, version] = fields[..] else {
            return DryRun::Blocked(Vec::new());
        };
        if [repo, name, version].iter().any(|f| f.is_empty()) {
            return DryRun::Blocked(Vec::new());
        }
        if !REPOS.contains(&repo) {
            refused.push(name.to_owned());
        }
        transaction.push(ResolvedPackage {
            repo: repo.to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
        });
    }
    for target in targets {
        let name = target.rsplit('/').next().unwrap_or(target);
        if !transaction.iter().any(|p| p.name == name) {
            refused.push(name.to_owned());
        }
    }
    if refused.is_empty() {
        DryRun::Plan(transaction)
    } else {
        DryRun::Blocked(refused)
    }
}
