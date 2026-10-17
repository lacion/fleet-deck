use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::*;

#[test]
fn the_longest_root_that_holds_the_home_is_the_temp_root() {
    let roots = vec![
        PathBuf::from("/a"),
        PathBuf::from("/a/b"),
        PathBuf::from("/c"),
    ];
    assert_eq!(
        temp_root(&roots, Path::new("/a/b/home")),
        Some(PathBuf::from("/a/b"))
    );
    assert_eq!(
        temp_root(&roots, Path::new("/a/x")),
        Some(PathBuf::from("/a"))
    );
    assert_eq!(temp_root(&roots, Path::new("/z")), None);
}

#[test]
fn the_temp_path_is_a_digest_of_the_land_id_and_the_attempt() {
    let path = temp_path(Path::new("/r"), "land-0123456789abcdef", 2);
    let name = path.file_name().unwrap().to_str().unwrap();
    let (hex, attempt) = name.split_once('-').unwrap();
    assert_eq!(attempt, "2");
    assert_eq!(hex.len(), 16);
    assert!(
        hex.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(path.parent(), Some(Path::new("/r/.fleetdeck-land")));
    // The same id and attempt give the same path; another attempt does not.
    assert_eq!(path, temp_path(Path::new("/r"), "land-0123456789abcdef", 2));
    assert_ne!(path, temp_path(Path::new("/r"), "land-0123456789abcdef", 3));
}

#[test]
fn land_ids_one_millisecond_apart_get_different_paths() {
    // `land-<ULID>`: two ids of nearby milliseconds share their first
    // characters; a prefix of twelve characters would not tell them apart.
    let a = "land-01JBXYZ0000000000000000001";
    let b = "land-01JBXYZ0000100000000000000";
    assert_eq!(a[..12], b[..12]);
    assert_ne!(
        temp_path(Path::new("/r"), a, 1),
        temp_path(Path::new("/r"), b, 1)
    );
    // Two lands of different projects under one root, started together.
    let seen: std::collections::BTreeSet<PathBuf> = (0..50)
        .map(|n| temp_path(Path::new("/r"), &format!("land-01JBXYZ{n:019}"), 1))
        .collect();
    assert_eq!(seen.len(), 50);
}

#[test]
fn the_land_folder_is_made_private_and_an_existing_private_one_passes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    ensure_dir(&root).unwrap();
    let mode = std::fs::metadata(root.join(TEMP_DIR))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    ensure_dir(&root).unwrap();
}

#[test]
fn an_open_folder_a_file_and_a_link_are_unsafe() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let land = root.join(TEMP_DIR);
    std::fs::create_dir(&land).unwrap();
    std::fs::set_permissions(&land, std::fs::Permissions::from_mode(0o755)).unwrap();
    let err = ensure_dir(&root).unwrap_err();
    assert_eq!(err.code, code::LAND_TEMP_UNSAFE);
    assert!(err.text.contains(".fleetdeck-land"), "{}", err.text);
    std::fs::remove_dir(&land).unwrap();
    std::fs::write(&land, b"x").unwrap();
    assert_eq!(ensure_dir(&root).unwrap_err().code, code::LAND_TEMP_UNSAFE);
    std::fs::remove_file(&land).unwrap();
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &land).unwrap();
    assert_eq!(ensure_dir(&root).unwrap_err().code, code::LAND_TEMP_UNSAFE);
}
