//! Swap races against the file service (M3 round 12): a directory below the
//! root is exchanged with a symlink to an outside directory while list,
//! read and walk search run. The descriptor-relative walk must never
//! return outside names or content, at any depth.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fleetdeck_proto::files::code;

use super::{list, read, walk_search};

const INSIDE: &str = "inside text\n";
const OUTSIDE: &str = "OUTSIDE secret\n";

/// `root/sub/{secret.txt, nested/secret.txt}` with inside content, and an
/// outside directory with the same names plus `only-outside.txt`.
fn trees() -> (tempfile::TempDir, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    for (dir, text) in [
        (root.path().join("sub"), INSIDE),
        (outside.path().to_path_buf(), OUTSIDE),
    ] {
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("secret.txt"), text).unwrap();
        std::fs::write(dir.join("nested/secret.txt"), text).unwrap();
    }
    std::fs::write(outside.path().join("only-outside.txt"), OUTSIDE).unwrap();
    (root, outside)
}

fn no_outside_entry(root: &Path, path: &str) -> bool {
    match list(root, path) {
        Ok(listing) => {
            assert!(
                listing.entries.iter().all(|e| e.name != "only-outside.txt"),
                "outside entry listed through {path}"
            );
            true
        }
        Err(e) => {
            assert!(
                e.code == code::NOT_FOUND || e.code == code::WRONG_KIND,
                "{e}"
            );
            false
        }
    }
}

fn no_outside_text(root: &Path, path: &str) -> bool {
    match read(root, path, None) {
        Ok(content) => {
            assert_eq!(content.text.as_deref(), Some(INSIDE), "read through {path}");
            true
        }
        Err(e) => {
            assert!(
                e.code == code::NOT_FOUND || e.code == code::WRONG_KIND,
                "{e}"
            );
            false
        }
    }
}

/// Exchanges `root/sub` and `root/swap` (a symlink to `outside`) again and
/// again with `renameat2(RENAME_EXCHANGE)`, so one of the two names is
/// always the real directory and the other the symlink.
#[cfg(target_os = "linux")]
fn swapper(root: &Path, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<u64> {
    let (a, b) = (root.join("sub"), root.join("swap"));
    std::thread::spawn(move || {
        let mut swaps = 0u64;
        while !stop.load(Ordering::Relaxed) {
            rustix::fs::renameat_with(
                rustix::fs::CWD,
                &a,
                rustix::fs::CWD,
                &b,
                rustix::fs::RenameFlags::EXCHANGE,
            )
            .unwrap();
            swaps += 1;
        }
        swaps
    })
}

#[cfg(target_os = "linux")]
#[test]
fn an_intermediate_directory_swapped_for_a_symlink_never_leaks_outside_names_or_content() {
    let (root, outside) = trees();
    std::os::unix::fs::symlink(outside.path(), root.path().join("swap")).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let handle = swapper(root.path(), Arc::clone(&stop));
    let (mut reads, mut lists) = (0u32, 0u32);
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut rounds = 0u32;
    while Instant::now() < deadline || rounds < 200 {
        rounds += 1;
        reads += u32::from(no_outside_text(root.path(), "sub/secret.txt"));
        reads += u32::from(no_outside_text(root.path(), "sub/nested/secret.txt"));
        lists += u32::from(no_outside_entry(root.path(), "sub"));
        lists += u32::from(no_outside_entry(root.path(), "sub/nested"));
        if rounds.is_multiple_of(8) {
            let found = walk_search(root.path(), "outside", true, 50);
            assert!(
                found.hits.is_empty(),
                "content search leaked: {:?}",
                found.hits
            );
            let found = walk_search(root.path(), "only-outside", false, 50);
            assert!(
                found.hits.is_empty(),
                "name search leaked: {:?}",
                found.hits
            );
        }
    }
    stop.store(true, Ordering::Relaxed);
    let swaps = handle.join().unwrap();
    // The race really ran: the directory moved many times, and the service
    // still answered from the real directory part of the time.
    assert!(swaps > 100, "only {swaps} swaps");
    assert!(reads > 0 && lists > 0, "reads {reads}, lists {lists}");
}

#[test]
fn a_symlink_at_any_depth_of_the_path_is_refused() {
    let (root, outside) = trees();
    std::os::unix::fs::symlink(outside.path(), root.path().join("sub/nested/link")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("top")).unwrap();
    for path in [
        "top/secret.txt",
        "top/nested/secret.txt",
        "sub/nested/link/secret.txt",
    ] {
        assert_eq!(
            read(root.path(), path, None).unwrap_err().code,
            code::NOT_FOUND,
            "{path}"
        );
    }
    for path in ["top", "top/nested", "sub/nested/link"] {
        assert_eq!(
            list(root.path(), path).unwrap_err().code,
            code::NOT_FOUND,
            "{path}"
        );
    }
    // The real paths still work.
    assert_eq!(
        read(root.path(), "sub/nested/secret.txt", None)
            .unwrap()
            .text
            .as_deref(),
        Some(INSIDE)
    );
    let listing = list(root.path(), "sub/nested").unwrap();
    assert!(listing.entries.iter().any(|e| e.name == "link"));
    let found = walk_search(root.path(), "outside", true, 50);
    assert!(found.hits.is_empty(), "{:?}", found.hits);
}

#[test]
fn a_root_that_is_a_symlink_is_not_opened() {
    let (root, outside) = trees();
    let link = root.path().join("rootlink");
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    assert_eq!(list(&link, "").unwrap_err().code, code::NOT_FOUND);
    assert_eq!(
        read(&link, "secret.txt", None).unwrap_err().code,
        code::NOT_FOUND
    );
    assert!(walk_search(&link, "outside", true, 50).hits.is_empty());
}
