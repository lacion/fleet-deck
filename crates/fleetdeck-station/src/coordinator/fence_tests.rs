//! The binding fence file (DL-194 T4b).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt as _;

use super::*;

fn digest(n: u32) -> String {
    format!("{n:064x}")
}

fn entry(shim: &str, n: u32) -> Entry {
    (shim.to_owned(), digest(n))
}

fn fence() -> (Fence, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    (Fence::new(tmp.path().join("run")), tmp)
}

fn text(f: &Fence) -> String {
    std::fs::read_to_string(f.path()).unwrap()
}

#[test]
fn an_absent_file_is_missing_and_ensure_creates_the_header_only() {
    let (f, _tmp) = fence();
    assert_eq!(f.read(), FenceState::Missing);
    f.ensure().unwrap();
    assert_eq!(text(&f), "fleetdeck-fence 1\n");
    assert_eq!(f.read(), FenceState::Entries(BTreeSet::new()));
    // A second ensure changes nothing.
    f.add(&[entry("a", 1)], "revoke").unwrap();
    f.ensure().unwrap();
    assert_eq!(
        f.read(),
        FenceState::Entries(BTreeSet::from([entry("a", 1)]))
    );
}

#[test]
fn add_and_remove_keep_the_other_entries() {
    let (f, _tmp) = fence();
    f.add(&[entry("a", 1), entry("b", 2)], "revoke").unwrap();
    f.add(&[entry("a", 3), entry("a", 1)], "rotate").unwrap();
    assert_eq!(
        f.read(),
        FenceState::Entries(BTreeSet::from([
            entry("a", 1),
            entry("a", 3),
            entry("b", 2)
        ]))
    );
    f.remove(&[entry("a", 1), entry("zz", 9)]).unwrap();
    assert_eq!(
        f.read(),
        FenceState::Entries(BTreeSet::from([entry("a", 3), entry("b", 2)]))
    );
    let lines: Vec<_> = text(&f).lines().map(str::to_owned).collect();
    assert_eq!(lines[0], FENCE_HEADER);
    assert_eq!(lines[1], format!("a {}", digest(3)));
    assert_eq!(lines[2], format!("b {}", digest(2)));
}

#[test]
fn remove_on_a_missing_file_writes_nothing() {
    let (f, _tmp) = fence();
    f.remove(&[entry("a", 1)]).unwrap();
    assert_eq!(f.read(), FenceState::Missing);
}

#[test]
fn a_bad_header_a_bad_line_or_too_many_lines_make_the_file_unreadable() {
    let (f, _tmp) = fence();
    std::fs::create_dir_all(&f.dir).unwrap();
    let good = format!("a {}", digest(1));
    let too_many: String = (0..=MAX_FENCE_ENTRIES as u32)
        .map(|n| format!("a {}\n", digest(n)))
        .collect();
    let cases = [
        format!("fleetdeck-fence 2\n{good}\n"),
        format!("{good}\n"),
        String::new(),
        format!("{FENCE_HEADER}\na\n"),
        format!("{FENCE_HEADER}\na b c\n"),
        format!("{FENCE_HEADER}\na {}\n", "A".repeat(64)),
        format!("{FENCE_HEADER}\na {}\n", "a".repeat(63)),
        format!("{FENCE_HEADER}\na.b {}\n", digest(1)),
        format!("{FENCE_HEADER}\n\n"),
        format!("{FENCE_HEADER}\n {}\n", digest(1)),
        format!("{FENCE_HEADER}\n{too_many}"),
    ];
    for case in cases {
        std::fs::write(f.path(), &case).unwrap();
        assert_eq!(f.read(), FenceState::Unreadable, "{case:?}");
    }
    std::fs::write(f.path(), [0xff, 0xfe]).unwrap();
    assert_eq!(f.read(), FenceState::Unreadable);
    // Exactly the limit is readable.
    let at_limit: String = (0..MAX_FENCE_ENTRIES as u32)
        .map(|n| format!("a {}\n", digest(n)))
        .collect();
    std::fs::write(f.path(), format!("{FENCE_HEADER}\n{at_limit}")).unwrap();
    assert!(matches!(f.read(), FenceState::Entries(e) if e.len() == MAX_FENCE_ENTRIES));
}

#[test]
fn an_unreadable_fence_takes_no_write_but_reset_rewrites_it() {
    let (f, _tmp) = fence();
    std::fs::create_dir_all(&f.dir).unwrap();
    std::fs::write(f.path(), "junk\n").unwrap();
    let add = f.add(&[entry("a", 1)], "revoke").unwrap_err();
    assert!(matches!(add, FenceError::Io(e) if e.kind() == ErrorKind::InvalidData));
    assert!(f.remove(&[entry("a", 1)]).is_err());
    assert_eq!(std::fs::read_to_string(f.path()).unwrap(), "junk\n");
    f.reset().unwrap();
    assert_eq!(f.read(), FenceState::Entries(BTreeSet::new()));
}

#[test]
fn a_full_fence_refuses_and_writes_nothing() {
    let (f, _tmp) = fence();
    let all: Vec<Entry> = (0..MAX_FENCE_ENTRIES as u32)
        .map(|n| entry("a", n))
        .collect();
    f.add(&all, "revoke").unwrap();
    let before = text(&f);
    let err = f.add(&[entry("b", 1)], "revoke").unwrap_err();
    assert!(matches!(err, FenceError::Full));
    assert_eq!(text(&f), before);
    // An entry that is there already is no growth.
    f.add(&[entry("a", 5)], "revoke").unwrap();
    assert_eq!(text(&f), before);
}

#[test]
fn a_malformed_entry_is_refused_before_it_could_poison_the_file() {
    let (f, _tmp) = fence();
    f.add(&[entry("a", 1)], "revoke").unwrap();
    let before = text(&f);
    for bad in [
        ("a b".to_owned(), digest(2)),
        ("a".to_owned(), "xyz".to_owned()),
    ] {
        assert!(f.add(&[bad], "revoke").is_err());
    }
    assert_eq!(text(&f), before);
}

#[test]
fn the_file_is_0600_and_the_directory_0700() {
    let (f, _tmp) = fence();
    f.add(&[entry("a", 1)], "revoke").unwrap();
    let file = std::fs::metadata(f.path()).unwrap().permissions().mode();
    let dir = std::fs::metadata(&f.dir).unwrap().permissions().mode();
    assert_eq!(file & 0o777, 0o600);
    assert_eq!(dir & 0o777, 0o700);
    // No tmp file stays after a write.
    assert!(!f.dir.join(TMP).exists());
}

#[test]
fn a_leftover_tmp_file_is_removed_and_never_read() {
    let (f, _tmp) = fence();
    f.add(&[entry("a", 1)], "revoke").unwrap();
    std::fs::write(
        f.dir.join(TMP),
        format!("{FENCE_HEADER}\nb {}\n", digest(2)),
    )
    .unwrap();
    assert_eq!(
        f.read(),
        FenceState::Entries(BTreeSet::from([entry("a", 1)]))
    );
    f.clean_tmp();
    assert!(!f.dir.join(TMP).exists());
    f.clean_tmp();
    assert_eq!(
        f.read(),
        FenceState::Entries(BTreeSet::from([entry("a", 1)]))
    );
}

#[test]
fn a_write_to_a_read_only_directory_fails_and_keeps_the_old_content() {
    let (f, _tmp) = fence();
    f.add(&[entry("a", 1)], "revoke").unwrap();
    let before = text(&f);
    std::fs::set_permissions(&f.dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = f.add(&[entry("b", 2)], "revoke");
    std::fs::set_permissions(&f.dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    if rustix::process::geteuid().is_root() {
        return;
    }
    assert!(matches!(result, Err(FenceError::Io(_))));
    assert_eq!(text(&f), before);
}
