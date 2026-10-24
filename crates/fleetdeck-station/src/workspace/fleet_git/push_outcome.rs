//! The classification of a push (FG-2 and FG-8, design §5.5, revision 3, M2).
//! A pure rule: the porcelain line of the exact ref decides.

use fleetdeck_proto::fleet_git::code;

/// What a push did to the one ref (design §5.5, M2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// Exit 0 and the ref's status line has the flag ` `, `+`, `*` or `=`.
    Applied,
    /// The server answered for the ref (flag `!`): the code is
    /// `push_rejected`. A recovery read still confirms it.
    Rejected(&'static str),
    /// Every other case: the server may have taken the pack.
    Unknown,
}

/// Classifies a `git push --porcelain` run for `publish_ref` (design §5.5,
/// M2). `porcelain` is the stdout: one status line `<flag>\t<src>:<dst>\t<summary>`
/// for each ref. The line `L` is the one whose `<dst>` is
/// `refs/heads/<publish_ref>`.
#[must_use]
pub fn push_outcome(exit: i32, porcelain: &str, timed_out: bool, publish_ref: &str) -> PushOutcome {
    if timed_out {
        return PushOutcome::Unknown;
    }
    let dst = format!("refs/heads/{publish_ref}");
    let flag = porcelain.lines().find_map(|line| {
        let mut chars = line.chars();
        let flag = chars.next()?;
        let rest = chars.as_str().strip_prefix('\t')?;
        let refs = rest.split('\t').next()?;
        (refs.rsplit_once(':')?.1 == dst).then_some(flag)
    });
    match flag {
        Some('!') => PushOutcome::Rejected(code::PUSH_REJECTED),
        Some(' ' | '+' | '*' | '=') if exit == 0 => PushOutcome::Applied,
        _ => PushOutcome::Unknown,
    }
}

#[cfg(test)]
#[path = "push_outcome_tests.rs"]
mod tests;
