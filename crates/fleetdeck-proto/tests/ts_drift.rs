//! TS-types drift check (PLAN §11): the committed bindings in
//! `web/src/gen/` must equal a fresh export. Fix with `make ts-types`.

use std::path::PathBuf;

fn committed_dir() -> PathBuf {
    // Read the manifest dir at run time first: cargo and nextest set it, and a
    // test binary that cargo reuses from a shared target dir must still check
    // the checkout that runs it, not the one that compiled it.
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")
        .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from);
    manifest.join("../../web/src/gen")
}

#[test]
fn committed_ts_bindings_match_the_rust_types() {
    let fresh = tempfile::tempdir().unwrap();
    fleetdeck_proto::ts::export_all(fresh.path()).unwrap();
    let problems = fleetdeck_proto::ts::diff_dirs(fresh.path(), &committed_dir()).unwrap();
    assert!(
        problems.is_empty(),
        "web/src/gen is out of date; run `make ts-types`:\n{}",
        problems.join("\n")
    );
}

#[test]
fn the_drift_check_detects_stale_missing_and_extra_files() {
    let fresh = tempfile::tempdir().unwrap();
    fleetdeck_proto::ts::export_all(fresh.path()).unwrap();
    let copy = tempfile::tempdir().unwrap();
    fleetdeck_proto::ts::export_all(copy.path()).unwrap();
    assert!(
        fleetdeck_proto::ts::diff_dirs(fresh.path(), copy.path())
            .unwrap()
            .is_empty()
    );

    let event = copy.path().join("event/Event.ts");
    std::fs::write(&event, "// stale\n").unwrap();
    std::fs::remove_file(copy.path().join("ids/SessionId.ts")).unwrap();
    std::fs::write(copy.path().join("Old.ts"), "export type Old = 1;\n").unwrap();
    let problems = fleetdeck_proto::ts::diff_dirs(fresh.path(), copy.path()).unwrap();
    assert!(
        problems.contains(&"stale: event/Event.ts".to_owned()),
        "{problems:?}"
    );
    assert!(
        problems.contains(&"missing: ids/SessionId.ts".to_owned()),
        "{problems:?}"
    );
    assert!(
        problems.contains(&"extra: Old.ts".to_owned()),
        "{problems:?}"
    );
}
