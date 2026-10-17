//! `files.display_changes` (R13 §5A item 5, §5D items 13 and 14): the change
//! summary and, on request, the patches of a session for a wall display.
//!
//! It uses the engine of `files.session_diff` ([`super::raw`]): the private
//! paths are already left out. The member deny list always applies (the
//! `member` flag of the call does not matter). A file that the deny list
//! hides becomes an anonymous marker with no path, no counts and no patch.
//! The markers come after the visible files, and the summary counts the
//! visible files only. A binary file has unknown counts, not zero.
//!
//! A patch section above [`MAX_DISPLAY_PATCH_FILE`] is left out and the file
//! is marked `too_large`. The station cap of the whole patch stays
//! [`fleetdeck_proto::files::MAX_DIFF_BYTES`]; when the engine cut the diff,
//! the files after the cut are not listed and `truncated` is true.

use fleetdeck_proto::display_changes::{
    DisplayChanges, DisplayChangesQuery, DisplayFileChange, MAX_DISPLAY_PATCH_FILE,
};
use fleetdeck_proto::event::DiffSummary;
use fleetdeck_proto::files::SessionDiffQuery;
use fleetdeck_proto::member_deny::{MemberDeny, header_paths};

use super::super::GitOptions;
use super::super::private_paths::PrivatePaths;
use super::raw;
use crate::manager::policy::Policy;
use crate::store::Store;
use crate::workspace::files::FileError;

/// The line that the engine adds at the end of a cut patch.
const CUT_MARK: &str = "\n[diff truncated]\n";

/// `files.display_changes`.
///
/// # Errors
/// [`FileError`] as `files.session_diff`: `unknown_session`, `no_base`,
/// `no_diff`, `outside_roots`, `missing_workspace`, `timeout`.
pub async fn run(
    store: &Store,
    policy: &Policy,
    private: &PrivatePaths,
    git: &GitOptions,
    query: DisplayChangesQuery,
) -> Result<DisplayChanges, FileError> {
    let want_patch = query.patch;
    let raw = raw(
        store,
        policy,
        private,
        git,
        SessionDiffQuery {
            session_id: query.session_id.clone(),
            max_bytes: query.max_bytes,
        },
    )
    .await?;
    let (files, summary) = build(policy.member_deny(), &raw.patch, raw.summary, want_patch);
    Ok(DisplayChanges {
        session_id: query.session_id,
        scope: raw.scope,
        truncated: raw.truncated,
        summary,
        files,
    })
}

/// The files of a patch and the summary without the hidden files.
fn build(
    deny: &MemberDeny,
    patch: &str,
    mut summary: DiffSummary,
    want_patch: bool,
) -> (Vec<DisplayFileChange>, DiffSummary) {
    let filtered = deny.filter_patch(patch);
    summary.files_changed = summary.files_changed.saturating_sub(filtered.hidden_files);
    summary.insertions = summary
        .insertions
        .saturating_sub(filtered.hidden_insertions);
    summary.deletions = summary.deletions.saturating_sub(filtered.hidden_deletions);
    let text = filtered
        .patch
        .strip_suffix(CUT_MARK)
        .unwrap_or(&filtered.patch);
    let mut files = Vec::new();
    let mut hidden = filtered.hidden_files as usize;
    for section in sections(text) {
        let stats = Stats::of(section);
        let path = section_path(section).filter(|p| !deny.denies(p));
        let Some(path) = path else {
            // A section that the deny list did not hide but that has no
            // readable path is hidden too (fail closed).
            hidden += 1;
            summary.files_changed = summary.files_changed.saturating_sub(1);
            summary.insertions = summary.insertions.saturating_sub(stats.added.unwrap_or(0));
            summary.deletions = summary.deletions.saturating_sub(stats.removed.unwrap_or(0));
            continue;
        };
        let too_large = want_patch && section.len() as u64 > MAX_DISPLAY_PATCH_FILE;
        files.push(DisplayFileChange {
            path: Some(path),
            hidden: false,
            added: stats.added,
            removed: stats.removed,
            binary: stats.binary,
            patch: (want_patch && !too_large).then(|| section.to_owned()),
            too_large,
        });
    }
    files.extend((0..hidden).map(|_| DisplayFileChange {
        path: None,
        hidden: true,
        added: None,
        removed: None,
        binary: false,
        patch: None,
        too_large: false,
    }));
    (files, summary)
}

/// The `diff --git` sections of a patch, each with its header line. Content
/// lines start with a space, `+`, `-` or `\`, so a header line is never
/// content.
fn sections(patch: &str) -> Vec<&str> {
    let mut starts: Vec<usize> = Vec::new();
    let mut at = 0;
    for line in patch.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            starts.push(at);
        }
        at += line.len();
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, &start)| &patch[start..starts.get(i + 1).copied().unwrap_or(patch.len())])
        .collect()
}

/// The line counts of one section.
struct Stats {
    added: Option<u64>,
    removed: Option<u64>,
    binary: bool,
}

impl Stats {
    fn of(section: &str) -> Self {
        let (mut added, mut removed) = (0u64, 0u64);
        let (mut in_hunk, mut binary) = (false, false);
        for line in section.lines() {
            if in_hunk {
                match line.as_bytes().first() {
                    Some(b'+') => added += 1,
                    Some(b'-') => removed += 1,
                    _ => {}
                }
            } else if line.starts_with("@@") {
                in_hunk = true;
            } else if line.starts_with("Binary files ") {
                binary = true;
            }
        }
        if binary {
            Self {
                added: None,
                removed: None,
                binary,
            }
        } else {
            Self {
                added: Some(added),
                removed: Some(removed),
                binary,
            }
        }
    }
}

/// The path of a section: the new name of a rename, else the path that both
/// sides of the header share. `None` when the header cannot be read.
fn section_path(section: &str) -> Option<String> {
    let first = section.lines().next()?;
    let rest = first.strip_prefix("diff --git ")?;
    let candidates = header_paths(rest)?;
    if rest.starts_with('"') {
        // Quoted names: the candidates are the old and the new name.
        return candidates.last().cloned();
    }
    let renamed = section
        .lines()
        .find_map(|l| l.strip_prefix("rename to "))
        .filter(|to| !to.starts_with('"'));
    if let Some(to) = renamed {
        return candidates.iter().any(|c| c == to).then(|| to.to_owned());
    }
    candidates
        .chunks_exact(2)
        .find(|pair| pair[0] == pair[1])
        .map(|pair| pair[0].clone())
}

#[cfg(test)]
#[path = "display_changes_tests.rs"]
mod display_changes_tests;
