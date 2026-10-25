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
    work_tree(dir).map(|(_, git)| git)
}

/// The work tree root and the git directory of the repository that
/// contains `dir`.
fn work_tree(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    for candidate in dir.ancestors().take(MAX_DEPTH) {
        let dot_git = candidate.join(".git");
        let Ok(meta) = fs::symlink_metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            return Some((candidate.to_path_buf(), dot_git));
        }
        if meta.is_file() {
            let text = read_small(&dot_git)?;
            let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
            let target = Path::new(target);
            let git = if target.is_absolute() {
                target.to_path_buf()
            } else {
                candidate.join(target)
            };
            return Some((candidate.to_path_buf(), git));
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

/// The most bytes read from a repository `config`.
const MAX_CONFIG: u64 = 64 * 1024;

/// The work tree root of the repository that contains `dir` and its
/// normalized origin (M3 6-7, the conflict radar's repo grouping, DL-306).
/// The origin is a heuristic (PLAN §6.6): it is `None` when the repository
/// has no remote or more than one remote.
#[must_use]
pub fn repo_of(dir: &Path) -> Option<(PathBuf, Option<String>)> {
    let (root, git) = work_tree(dir)?;
    // A linked worktree keeps the shared config in its `commondir`.
    let common = read_small(&git.join("commondir"))
        .map(|c| {
            let c = PathBuf::from(c.trim());
            if c.is_absolute() { c } else { git.join(c) }
        })
        .unwrap_or_else(|| git.clone());
    let origin = fs::File::open(common.join("config")).ok().and_then(|f| {
        let mut text = String::new();
        f.take(MAX_CONFIG).read_to_string(&mut text).ok()?;
        single_remote_url(&text)
    });
    Some((root, origin.and_then(|u| normalize_remote(&u))))
}

/// The `url` of the only `[remote "..."]` section, if exactly one exists.
fn single_remote_url(config: &str) -> Option<String> {
    let mut remotes = 0;
    let mut url = None;
    let mut in_remote = false;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            in_remote = line.starts_with("[remote ");
            remotes += usize::from(in_remote);
        } else if in_remote
            && url.is_none()
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == "url"
        {
            url = Some(v.trim().to_owned());
        }
    }
    (remotes == 1).then_some(url).flatten()
}

/// `git@host:org/repo.git`, `ssh://git@host/org/repo`, `https://user@host/org/repo.git/`
/// → `host/org/repo` (host in lower case). A local path or an unusual URL gives `None`.
#[must_use]
pub fn normalize_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = if let Some((_, rest)) = url.split_once("://") {
        rest.to_owned()
    } else {
        let (host, path) = url.split_once(':')?;
        if host.contains('/') || path.starts_with('/') && host.is_empty() {
            return None;
        }
        format!("{host}/{path}")
    };
    let rest = rest.rsplit_once('@').map_or(rest.as_str(), |(_, r)| r);
    let (host, path) = rest.split_once('/')?;
    let host = host.split(':').next()?.to_ascii_lowercase();
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let ok = !host.is_empty()
        && !path.is_empty()
        && !format!("{host}{path}").chars().any(char::is_control);
    ok.then(|| plain(&format!("{host}/{path}"))).flatten()
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

    #[test]
    fn remote_urls_normalize_to_host_and_path() {
        for url in [
            "git@GitHub.com:Org/repo.git",
            "ssh://git@github.com/Org/repo",
            "https://user@github.com/Org/repo.git/",
            "https://github.com:443/Org/repo",
        ] {
            assert_eq!(
                normalize_remote(url).as_deref(),
                Some("github.com/Org/repo"),
                "{url}"
            );
        }
        assert_eq!(normalize_remote("/srv/git/repo.git"), None);
        assert_eq!(normalize_remote("relative/path"), None);
    }

    #[test]
    fn the_repo_root_and_its_single_origin_are_found_from_a_subdirectory() {
        let dir = repo("ref: refs/heads/main\n");
        fs::write(
            dir.path().join(".git/config"),
            "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = git@github.com:o/r.git\n",
        )
        .unwrap();
        let sub = dir.path().join("src");
        fs::create_dir_all(&sub).unwrap();
        let (root, origin) = repo_of(&sub).unwrap();
        assert_eq!(root, dir.path());
        assert_eq!(origin.as_deref(), Some("github.com/o/r"));
        // Two remotes: the grouping needs an owner choice, so no origin.
        fs::write(
            dir.path().join(".git/config"),
            "[remote \"a\"]\n\turl = git@h:o/r\n[remote \"b\"]\n\turl = git@h:o/s\n",
        )
        .unwrap();
        assert_eq!(repo_of(&sub).unwrap().1, None);
    }
}
