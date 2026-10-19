//! Tests of the refusals of `station.desktop_tools_plan`: the configuration
//! check and the dry run.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::fs::symlink;

use super::etc_check::EtcRoot;
use super::plan_tests::{ALL_MISSING_LINES, assert_blocked, dry, plan_of, rig_with_grim};
use super::probe::ProbeRun;
use super::store::PlanStore;
use super::test_support::{Etc, FakeExec, exit, peer};

const T0: i64 = super::plan_tests::T0;

#[tokio::test]
async fn a_config_check_failure_blocks_the_plan_and_runs_no_dry_run() {
    let rig = rig_with_grim();
    let store = PlanStore::new();
    let exec = FakeExec::new(|_| panic!("no child runs"));

    // A link.
    let etc = Etc::new();
    fs::remove_file(etc.path("pacman.conf")).unwrap();
    fs::write(etc.dir.path().join("real.conf"), "x").unwrap();
    symlink(etc.dir.path().join("real.conf"), etc.path("pacman.conf")).unwrap();
    assert_blocked(&plan_of(&rig, &etc.root, &exec, &store).await, &[]);
    let etc = Etc::new();
    fs::remove_dir_all(etc.path("pacman.d")).unwrap();
    fs::create_dir_all(etc.dir.path().join("elsewhere")).unwrap();
    symlink(etc.dir.path().join("elsewhere"), etc.path("pacman.d")).unwrap();
    assert_blocked(&plan_of(&rig, &etc.root, &exec, &store).await, &[]);

    // A non-root owner.
    let etc = Etc::new();
    let other = EtcRoot {
        owner: etc.root.owner + 1,
        ..etc.root.clone()
    };
    assert_blocked(&plan_of(&rig, &other, &exec, &store).await, &[]);

    // A group-writable or other-writable folder or file.
    for (rel, mode) in [
        ("", 0o775),
        ("", 0o757),
        ("pacman.d", 0o775),
        ("pacman.conf", 0o664),
        ("pacman.d/mirrorlist", 0o646),
    ] {
        let etc = Etc::new();
        etc.chmod(rel, mode);
        assert_blocked(&plan_of(&rig, &etc.root, &exec, &store).await, &[]);
    }

    // A missing file.
    let etc = Etc::new();
    fs::remove_file(etc.path("pacman.d/mirrorlist")).unwrap();
    assert_blocked(&plan_of(&rig, &etc.root, &exec, &store).await, &[]);
    let etc = Etc::new();
    fs::remove_file(etc.path("pacman.conf")).unwrap();
    assert_blocked(&plan_of(&rig, &etc.root, &exec, &store).await, &[]);
}

#[tokio::test]
async fn a_pacman_conf_over_one_mebibyte_blocks_the_plan() {
    let rig = rig_with_grim();
    let etc = Etc::new();
    fs::write(etc.path("pacman.conf"), vec![b'#'; 1024 * 1024 + 1]).unwrap();
    let exec = FakeExec::new(|_| panic!("no child runs"));
    assert_blocked(
        &plan_of(&rig, &etc.root, &exec, &PlanStore::new()).await,
        &[],
    );
    fs::write(etc.path("pacman.conf"), vec![b'#'; 1024 * 1024]).unwrap();
    let plan = plan_of(&rig, &etc.root, &dry(ALL_MISSING_LINES), &PlanStore::new()).await;
    assert!(plan.plan_id.is_some());
}

#[tokio::test]
async fn a_missing_pacman_blocks_the_plan() {
    let rig = rig_with_grim();
    let etc = Etc::new();
    let exec = FakeExec::new(|_| panic!("no child runs")).without_pacman();
    assert_blocked(
        &plan_of(&rig, &etc.root, &exec, &PlanStore::new()).await,
        &[],
    );
}

#[tokio::test]
async fn a_dry_run_that_fails_blocks_the_plan() {
    let rig = rig_with_grim();
    let etc = Etc::new();
    for reply in [
        exit(1, ALL_MISSING_LINES),
        ProbeRun::Timeout,
        ProbeRun::Failed,
    ] {
        let exec = FakeExec::new(move |_| reply.clone());
        let store = PlanStore::new();
        assert_blocked(&plan_of(&rig, &etc.root, &exec, &store).await, &[]);
        assert!(store.get("x", &peer(1), T0).is_none());
    }
}

#[tokio::test]
async fn a_foreign_repository_refuses_the_plan_with_the_package_names() {
    let rig = rig_with_grim();
    let etc = Etc::new();
    let store = PlanStore::new();
    let lines = "extra wtype 0.4-1\naur evil-dep 1-1\ncustom other 2-1\nextra wf-recorder 0.5.0-1\n\
extra jq 1.7.1-2\ncore systemd 256.7-1\nextra hyprland 0.45-1\nextra chromium 130.0-1\n\
extra libnotify 0.8-1\n";
    let exec = FakeExec::new(move |_| exit(0, lines));
    let plan = plan_of(&rig, &etc.root, &exec, &store).await;
    assert_blocked(&plan, &["evil-dep", "other"]);
    assert_eq!(plan.installable.len(), 7);
}

#[tokio::test]
async fn a_missing_target_is_refused_by_name() {
    let rig = rig_with_grim();
    let etc = Etc::new();
    let lines = "extra wtype 0.4-1\nextra jq 1.7.1-2\ncore systemd 256.7-1\nextra hyprland 0.45-1\n\
extra chromium 130.0-1\n";
    let exec = FakeExec::new(move |_| exit(0, lines));
    let plan = plan_of(&rig, &etc.root, &exec, &PlanStore::new()).await;
    assert_blocked(&plan, &["wf-recorder", "libnotify"]);
}

#[tokio::test]
async fn a_bad_line_refuses_the_whole_output() {
    let rig = rig_with_grim();
    let etc = Etc::new();
    for lines in [
        "extra wtype\n",
        "extra wtype 0.4-1 extra\n",
        "extra  wtype 0.4-1\n",
        "extra wtype 0.4-1\n\nextra jq 1-1\n",
        "https://host/wtype.pkg.tar.zst\n",
    ] {
        let exec = FakeExec::new(move |_| exit(0, lines));
        let plan = plan_of(&rig, &etc.root, &exec, &PlanStore::new()).await;
        assert_blocked(&plan, &[]);
    }
}

#[tokio::test]
async fn an_empty_output_refuses_every_target() {
    let rig = rig_with_grim();
    let etc = Etc::new();
    let plan = plan_of(&rig, &etc.root, &dry(""), &PlanStore::new()).await;
    assert_blocked(
        &plan,
        &[
            "wtype",
            "wf-recorder",
            "systemd",
            "jq",
            "hyprland",
            "chromium",
            "libnotify",
        ],
    );
}
