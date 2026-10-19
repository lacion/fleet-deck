//! The post-check after the exit of the transaction (`board-gaps.md` §2.9):
//! the installed versions of the expected packages against the plan.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use fleetdeck_proto::desktop_tools::{PostCheck, ResolvedPackage};

use crate::omarchy_admin::child::{RunResult, Spec, run};

/// The longest time of the post-check.
pub const LIMIT: Duration = Duration::from_secs(30);

/// Runs `<program> -Q -- <names>` and compares the answer with
/// `transaction`: `Ok` when the exit code is 0 and every package is
/// installed with the version of the plan; `TransactionChanged` for anything
/// else (another version, a missing package, a failed or late query).
pub async fn check(program: &Path, transaction: &[ResolvedPackage]) -> PostCheck {
    if transaction.is_empty() {
        return PostCheck::Ok;
    }
    let mut args: Vec<OsString> = vec!["-Q".into(), "--".into()];
    args.extend(transaction.iter().map(|p| OsString::from(&p.name)));
    let env = [
        (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        (OsString::from("LANG"), OsString::from("C")),
    ];
    let result = run(&Spec {
        program,
        args: &args,
        env: &env,
        extra: &[],
        stdin: None,
        limit: LIMIT,
    })
    .await;
    let Ok(RunResult::Exit {
        code: Some(0),
        stdout,
        ..
    }) = result
    else {
        return PostCheck::TransactionChanged;
    };
    let Ok(text) = std::str::from_utf8(&stdout) else {
        return PostCheck::TransactionChanged;
    };
    let installed: Vec<(&str, &str)> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            match (fields.next(), fields.next(), fields.next()) {
                (Some(name), Some(version), None) => Some((name, version)),
                _ => None,
            }
        })
        .collect();
    let same = transaction
        .iter()
        .all(|p| installed.contains(&(p.name.as_str(), p.version.as_str())));
    if same {
        PostCheck::Ok
    } else {
        PostCheck::TransactionChanged
    }
}
