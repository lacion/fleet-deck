//! The git branch of a workspace without a `git` process (M2 P4-2, the card
//! shows repo/branch, PLAN §11). It reads `HEAD` of the nearest repository:
//! `.git` is a directory, or a file `gitdir: <path>` (a worktree or a
//! submodule). Every read is bounded; anything unusual gives `None`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The most bytes read from `HEAD` or a `.git` file.
const MAX_READ: u64 = 4096;
/// The most directories walked up from the workspace.
const MAX_DEPTH: usize = 64;
/// The longest branch name kept.
pub const MAX_BRANCH_CHARS: usize = 200;

fn read_small(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_READ).read_to_string(&mut text).ok()?;
    Some(text)
}

/// The git directory of the repository that contains `dir`.
fn git_dir(dir: &Path) -> Option<PathBuf> {
    for candidate in dir.ancestors().take(MAX_DEPTH) {
        let dot_git = candidate.join(".git");
        let Ok(meta) = fs::symlink_metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            return Some(dot_git);
        }
        if meta.is_file() {
            let text = read_small(&dot_git)?;
            let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
            let target = Path::new(target);
            return Some(if target.is_absolute() {
                target.to_path_buf()
            } else {
                candidate.join(target)
            });
        }
        return None;
    }
    None
}

/// A branch name that is safe to show: printable, no control characters,
/// at most [`MAX_BRANCH_CHARS`].
fn plain(name: &str) -> Option<String> {
    let ok = !name.is_empty()
        && name.chars().count() <= MAX_BRANCH_CHARS
        && !name.chars().any(char::is_control);
    ok.then(|| name.to_owned())
}

/// The current branch of the repository that contains `dir`; `None` outside
/// a repository, with a detached `HEAD`, or for a `HEAD` that is not a
/// branch reference.
#[must_use]
pub fn current_branch(dir: &Path) -> Option<String> {
    let head = read_small(&git_dir(dir)?.join("HEAD"))?;
    let reference = head.lines().next()?.strip_prefix("ref:")?.trim();
    plain(reference.strip_prefix("refs/heads/")?)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn repo(head: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/HEAD"), head).unwrap();
        dir
    }

    #[test]
    fn a_branch_is_read_from_head_also_in_a_subdirectory() {
        let dir = repo("ref: refs/heads/fd/alpha-7\n");
        assert_eq!(current_branch(dir.path()).as_deref(), Some("fd/alpha-7"));
        let sub = dir.path().join("src/deep");
        fs::create_dir_all(&sub).unwrap();
        assert_eq!(current_branch(&sub).as_deref(), Some("fd/alpha-7"));
    }

    #[test]
    fn a_worktree_gitdir_file_is_followed() {
        let main = repo("ref: refs/heads/main\n");
        let wt_git = main.path().join(".git/worktrees/wt");
        fs::create_dir_all(&wt_git).unwrap();
        fs::write(wt_git.join("HEAD"), "ref: refs/heads/fd/bravo\n").unwrap();
        let wt = tempfile::tempdir().unwrap();
        fs::write(
            wt.path().join(".git"),
            format!("gitdir: {}\n", wt_git.display()),
        )
        .unwrap();
        assert_eq!(current_branch(wt.path()).as_deref(), Some("fd/bravo"));
    }

    #[test]
    fn detached_heads_missing_repositories_and_odd_names_give_none() {
        let detached = repo("0123456789abcdef0123456789abcdef01234567\n");
        assert_eq!(current_branch(detached.path()), None);
        let tag = repo("ref: refs/tags/v1\n");
        assert_eq!(current_branch(tag.path()), None);
        let control = repo("ref: refs/heads/a\u{1b}[31m\n");
        assert_eq!(current_branch(control.path()), None);
        let long = repo(&format!("ref: refs/heads/{}\n", "x".repeat(201)));
        assert_eq!(current_branch(long.path()), None);
        let none = tempfile::tempdir().unwrap();
        // A temp directory is not inside a repository on a normal machine;
        // the walk may still find one above it, so only check no panic.
        let _ = current_branch(none.path());
        assert_eq!(current_branch(Path::new("/nonexistent/fd/x")), None);
    }
}
