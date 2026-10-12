//! `workspace.list` (M2 P4-4, DL-061): the directories that the spawn form
//! of the board can pick. Only directories inside the allowed roots appear;
//! every entry is resolved, so a symlink cannot lead out of a root. Hidden
//! directories and names that are not UTF-8 are left out.

use std::path::Path;

use fleetdeck_proto::link::{WorkspaceEntry, WorkspaceListing, WorkspaceQuery};

use super::policy::{Policy, WorkspaceError};
use crate::workspace::provision;

/// The most entries in one answer.
pub const MAX_ENTRIES: usize = 500;
/// The most directory entries read from one directory.
pub const MAX_SCANNED: usize = 10_000;

/// Lists the roots (`path: None`) or the sub-directories of one directory.
///
/// # Errors
/// [`WorkspaceError`] when the directory is missing or outside the roots.
pub fn list(policy: &Policy, query: &WorkspaceQuery) -> Result<WorkspaceListing, WorkspaceError> {
    let Some(path) = &query.path else {
        let mut entries: Vec<WorkspaceEntry> = policy
            .allowed_roots()
            .iter()
            .filter(|r| r.is_dir())
            .filter_map(|r| entry(policy, r.to_str()?.to_owned(), r))
            .collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        return Ok(WorkspaceListing {
            path: None,
            parent: None,
            entries,
            truncated: false,
        });
    };
    let dir = policy.workspace(path)?;
    let inside = |p: &Path| policy.allowed_roots().iter().any(|r| p.starts_with(r));
    let parent = dir
        .parent()
        .filter(|p| inside(p))
        .and_then(|p| p.to_str().map(str::to_owned));
    let missing = || WorkspaceError::Missing(path.clone());
    let mut entries = Vec::new();
    let mut truncated = false;
    for (n, item) in std::fs::read_dir(&dir).map_err(|_| missing())?.enumerate() {
        if n >= MAX_SCANNED {
            truncated = true;
            break;
        }
        let Ok(item) = item else { continue };
        let Ok(name) = item.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let Ok(resolved) = item.path().canonicalize() else {
            continue;
        };
        if resolved.is_dir()
            && inside(&resolved)
            && let Some(e) = entry(policy, name, &resolved)
        {
            entries.push(e);
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    if entries.len() > MAX_ENTRIES {
        entries.truncate(MAX_ENTRIES);
        truncated = true;
    }
    Ok(WorkspaceListing {
        path: dir.to_str().map(str::to_owned),
        parent,
        entries,
        truncated,
    })
}

fn entry(policy: &Policy, name: String, resolved: &Path) -> Option<WorkspaceEntry> {
    let repository = resolved.join(".git").exists();
    // Repo catalog (M3 6-1, DL-201): facts of a main checkout, from files.
    let facts = if resolved.join(".git").is_dir() {
        provision::repo_facts(resolved)
    } else {
        provision::RepoFacts::default()
    };
    let group = policy.repo_group(resolved).map(str::to_owned).or_else(|| {
        (facts.remotes == 1)
            .then(|| facts.origin.as_deref().map(provision::normalize_origin))
            .flatten()
    });
    Some(WorkspaceEntry {
        name,
        path: resolved.to_str()?.to_owned(),
        repository,
        origin: facts.origin,
        default_branch: facts.default_branch,
        group,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::store::OfflineApprovalPolicy;

    fn policy(roots: Vec<PathBuf>) -> Policy {
        Policy::new(roots, 4, OfflineApprovalPolicy::Wait, BTreeMap::new())
    }

    fn query(path: Option<&Path>) -> WorkspaceQuery {
        WorkspaceQuery {
            path: path.map(|p| p.to_string_lossy().into_owned()),
        }
    }

    #[test]
    fn roots_and_directories_are_listed_without_hidden_files_or_escapes() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("work")).unwrap();
        let root = tmp.path().join("work").canonicalize().unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::create_dir_all(root.join("repo/.git")).unwrap();
        fs::create_dir_all(root.join("notes/deep")).unwrap();
        fs::create_dir(root.join(".hidden")).unwrap();
        fs::write(root.join("file.txt"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        std::os::unix::fs::symlink(root.join("notes"), root.join("link-to-notes")).unwrap();
        let p = policy(vec![root.clone(), tmp.path().join("gone")]);

        let roots = list(&p, &query(None)).unwrap();
        assert_eq!(roots.path, None);
        assert_eq!(roots.entries.len(), 1, "a missing root is left out");
        assert_eq!(roots.entries[0].path, root.to_str().unwrap());

        let top = list(&p, &query(Some(&root))).unwrap();
        let names: Vec<_> = top.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["link-to-notes", "notes", "repo"]);
        assert_eq!(top.parent, None, "the parent of a root is outside");
        assert!(top.entries[2].repository);
        assert!(!top.entries[1].repository);
        assert_eq!(
            top.entries[0].path,
            root.join("notes").to_str().unwrap(),
            "an entry resolves its symlink"
        );
        assert!(!top.truncated);

        let notes = list(&p, &query(Some(&root.join("notes")))).unwrap();
        assert_eq!(notes.parent.as_deref(), root.to_str());
        assert_eq!(notes.entries[0].name, "deep");
    }

    #[test]
    fn a_path_outside_the_roots_or_missing_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("work");
        fs::create_dir(&root).unwrap();
        let p = policy(vec![root.clone()]);
        assert!(matches!(
            list(&p, &query(Some(tmp.path()))),
            Err(WorkspaceError::Outside(_))
        ));
        assert!(matches!(
            list(&p, &query(Some(&root.join("nope")))),
            Err(WorkspaceError::Missing(_))
        ));
        assert!(matches!(
            list(&p, &query(Some(&root.join("..").join("work").join("..")))),
            Err(WorkspaceError::Outside(_))
        ));
    }

    #[test]
    fn a_large_directory_is_truncated() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..(MAX_ENTRIES + 3) {
            fs::create_dir(tmp.path().join(format!("d{i:04}"))).unwrap();
        }
        let p = policy(vec![tmp.path().to_path_buf()]);
        let all = list(&p, &query(Some(tmp.path()))).unwrap();
        assert_eq!(all.entries.len(), MAX_ENTRIES);
        assert!(all.truncated);
        assert_eq!(all.entries[0].name, "d0000");
    }
}
