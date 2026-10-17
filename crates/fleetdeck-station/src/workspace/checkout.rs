//! The top of a Git checkout: a main checkout (`.git` is a directory) or a
//! linked worktree (`.git` is a file `gitdir: <path>`). A linked worktree
//! counts only when its Git directory is inside the allowed roots, so a
//! worktree start never writes Git data outside them.

use std::path::Path;

/// The longest `.git` file of a linked worktree that is read.
const MAX_GITFILE: u64 = 4096;

/// True when `repo` is the top directory of a checkout. `inside` says
/// whether a canonical directory is inside the allowed roots.
pub fn is_checkout_top(repo: &Path, inside: &impl Fn(&Path) -> bool) -> bool {
    let dotgit = repo.join(".git");
    let Ok(meta) = std::fs::symlink_metadata(&dotgit) else {
        return false;
    };
    if meta.is_dir() {
        return true;
    }
    if !meta.is_file() || meta.len() > MAX_GITFILE {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(&dotgit) else {
        return false;
    };
    let Some(target) = text.trim_end().strip_prefix("gitdir: ") else {
        return false;
    };
    if target.is_empty() || target.contains('\n') {
        return false;
    }
    let target = Path::new(target);
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        repo.join(target)
    };
    target
        .canonicalize()
        .is_ok_and(|t| t.is_dir() && inside(&t))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn everywhere(_: &Path) -> bool {
        true
    }

    #[test]
    fn a_main_checkout_and_a_linked_worktree_are_tops() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir_all(main.join(".git/worktrees/w")).unwrap();
        assert!(is_checkout_top(&main, &everywhere));
        let linked = tmp.path().join("w");
        std::fs::create_dir_all(&linked).unwrap();
        let gitdir = main.join(".git/worktrees/w");
        std::fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        assert!(is_checkout_top(&linked, &everywhere));
        // A relative gitdir resolves against the worktree.
        std::fs::write(linked.join(".git"), "gitdir: ../main/.git/worktrees/w\n").unwrap();
        assert!(is_checkout_top(&linked, &everywhere));
    }

    #[test]
    fn a_gitdir_outside_the_roots_a_bad_file_or_a_link_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let gitdir = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&gitdir).unwrap();
        let linked = tmp.path().join("w");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        let canonical = gitdir.canonicalize().unwrap();
        assert!(!is_checkout_top(&linked, &|p: &Path| p != canonical));
        std::fs::write(linked.join(".git"), "not a gitdir line\n").unwrap();
        assert!(!is_checkout_top(&linked, &everywhere));
        std::fs::write(linked.join(".git"), "gitdir: /no/such/dir\n").unwrap();
        assert!(!is_checkout_top(&linked, &everywhere));
        std::fs::remove_file(linked.join(".git")).unwrap();
        std::os::unix::fs::symlink(&gitdir, linked.join(".git")).unwrap();
        assert!(!is_checkout_top(&linked, &everywhere));
        assert!(!is_checkout_top(&tmp.path().join("none"), &everywhere));
    }
}
