//! The Taildrive report and the exposure check (design §15.2, §15.4, B5,
//! M10): real folders, a fake `tailscale`, no network.

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fleetdeck_core::tailscale::TailscaleCli;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{
    Exposure, ExposureState, SharePathState, TaildriveShare, TaildriveState,
};
use fleetdeck_proto::member_deny::MemberDeny;

use super::*;
use crate::artifact_share::tests::{Rig, table};

/// A tree under one temporary folder.
struct Tree {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        Self { _tmp: tmp, root }
    }

    fn dir(&self, rel: &str) -> PathBuf {
        let path = self.root.join(rel);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn source(&self, deny: &[&str]) -> TaildriveSource {
        let extra: Vec<String> = deny.iter().map(|s| (*s).to_owned()).collect();
        TaildriveSource {
            cli: TailscaleCli::new("/no/such/tailscale"),
            data_dir: self.root.join("data"),
            home: Some(self.root.join("home")),
            member_deny: MemberDeny::new(&extra),
            instance_homes: Vec::new(),
            allowed_roots: Vec::new(),
            mounts: Default::default(),
        }
    }
}

fn inputs(homes: &[&Path], worktrees: &[&Path]) -> ExposureInputs {
    let text = |p: &&Path| p.to_string_lossy().into_owned();
    ExposureInputs {
        homes: homes.iter().map(text).collect(),
        worktrees: worktrees.iter().map(text).collect(),
        snapshot_seen: true,
    }
}

fn share_at(path: &Path) -> TaildriveShare {
    let path = path.to_string_lossy().into_owned();
    TaildriveShare {
        name: "out".to_owned(),
        candidates: vec![path.clone()],
        path,
        as_user: None,
        exposed: Vec::new(),
        exposure_state: ExposureState::Unknown,
        unknown_reason: None,
        path_state: SharePathState::Ok,
    }
}

/// The checked share.
fn check_with(
    source: &TaildriveSource,
    inputs: &ExposureInputs,
    share: TaildriveShare,
    limits: &WalkLimits,
) -> TaildriveShare {
    let listing = Listing {
        state: TaildriveState::Ok,
        reason: None,
        machine: Some("box".to_owned()),
        tailnet: None,
        shares: vec![share],
    };
    build_report(source, &listing, inputs, limits, 1)
        .shares
        .remove(0)
}

fn check(source: &TaildriveSource, inputs: &ExposureInputs, path: &Path) -> TaildriveShare {
    check_with(source, inputs, share_at(path), &WalkLimits::default())
}

#[test]
fn each_protected_kind_is_exposed_when_the_share_is_contains_or_is_inside_it() {
    let t = Tree::new();
    let src = t.source(&[]);
    let home = t.dir("home");
    let cases: Vec<(Exposure, PathBuf)> = vec![
        (Exposure::ProjectHome, t.dir("homes/p1")),
        (Exposure::Worktree, t.dir("trees/w1")),
        (Exposure::DataDir, t.dir("data")),
        (Exposure::ClaudeHome, t.dir("home/.claude")),
        (Exposure::CodexHome, t.dir("home/.codex")),
        (Exposure::Ssh, t.dir("home/.ssh")),
        (Exposure::Gnupg, t.dir("home/.gnupg")),
        (Exposure::Aws, t.dir("home/.aws")),
    ];
    let homes = [cases[0].1.as_path()];
    let trees = [cases[1].1.as_path()];
    let inp = inputs(&homes, &trees);
    for (kind, path) in &cases {
        // The share is the protected path.
        let is = check(&src, &inp, path);
        assert_eq!(is.exposure_state, ExposureState::Exposed, "{kind:?} is");
        assert!(is.exposed.contains(kind), "{kind:?} is");
        // The share is inside it.
        let inside = t.dir(&format!(
            "{}/inner",
            path.strip_prefix(&t.root).unwrap().display()
        ));
        let got = check(&src, &inp, &inside);
        assert_eq!(
            got.exposure_state,
            ExposureState::Exposed,
            "{kind:?} inside"
        );
        assert!(got.exposed.contains(kind), "{kind:?} inside");
        // The share contains it.
        let parent = path.parent().unwrap();
        let got = check(&src, &inp, parent);
        assert_eq!(
            got.exposure_state,
            ExposureState::Exposed,
            "{kind:?} contains"
        );
        assert!(got.exposed.contains(kind), "{kind:?} contains");
    }
    // A folder that touches no protected path is clear.
    let clear = t.dir("elsewhere/out");
    assert_eq!(
        check(&src, &inp, &clear).exposure_state,
        ExposureState::Clear
    );
    let _ = home;
}

#[test]
fn the_provider_instance_homes_are_protected() {
    let t = Tree::new();
    let mut src = t.source(&[]);
    let claude = t.dir("alt/claude");
    let codex = t.dir("alt/codex");
    src.instance_homes = vec![
        (ProviderKind::Claude, claude.clone()),
        (ProviderKind::Codex, codex.clone()),
    ];
    let inp = inputs(&[], &[]);
    assert_eq!(check(&src, &inp, &claude).exposed, [Exposure::ClaudeHome]);
    assert_eq!(check(&src, &inp, &codex).exposed, [Exposure::CodexHome]);
}

#[test]
fn a_protected_path_that_does_not_exist_is_skipped() {
    let t = Tree::new();
    let src = t.source(&[]);
    let gone = t.root.join("homes/gone");
    let inp = inputs(&[gone.as_path()], &[gone.as_path()]);
    let share = t.dir("exports/out");
    assert_eq!(
        check(&src, &inp, &share).exposure_state,
        ExposureState::Clear
    );
}

#[test]
fn a_component_of_the_share_path_that_the_deny_list_names_is_exposed() {
    let t = Tree::new();
    let src = t.source(&[]);
    let share = t.dir("secrets-share/out");
    let got = check(&src, &inputs(&[], &[]), &share);
    assert_eq!(got.exposed, [Exposure::MemberDeny]);
    let extra = t.source(&["private/"]);
    let share = t.dir("private/out");
    assert_eq!(
        check(&extra, &inputs(&[], &[]), &share).exposed,
        [Exposure::MemberDeny]
    );
}

#[test]
fn a_deny_listed_file_below_the_share_is_exposed() {
    let t = Tree::new();
    let share = t.dir("srv/exports");
    t.dir("srv/exports/secrets");
    std::fs::write(share.join("secrets/report.txt"), "x").unwrap();
    let got = check(&t.source(&[]), &inputs(&[], &[]), &share);
    assert_eq!(got.exposure_state, ExposureState::Exposed);
    assert_eq!(got.exposed, [Exposure::MemberDeny]);
}

#[test]
fn an_anchored_pattern_matches_below_a_share_inside_a_home() {
    let t = Tree::new();
    let home = t.dir("homes/p1");
    let share = t.dir("homes/p1/config");
    std::fs::write(share.join("prod.yml"), "x").unwrap();
    let src = t.source(&["config/prod.yml"]);
    let inp = inputs(&[home.as_path()], &[]);
    let protected = Protected::build(&src, &inp);
    // Relative to the share root the file is `prod.yml`; relative to the
    // home it is `config/prod.yml`.
    let walked = walk_share(&share, &protected, &src.member_deny, &WalkLimits::default());
    assert_eq!(walked.exposed, [Exposure::MemberDeny]);
    let got = check(&src, &inp, &share);
    assert!(got.exposed.contains(&Exposure::MemberDeny));
    assert!(got.exposed.contains(&Exposure::ProjectHome));
}

fn unknown_reason(share: &TaildriveShare) -> &str {
    assert_eq!(share.exposure_state, ExposureState::Unknown, "{share:?}");
    share.unknown_reason.as_deref().unwrap()
}

#[test]
fn a_share_with_more_than_20000_entries_is_unknown() {
    let t = Tree::new();
    let share = t.dir("big");
    for i in 0..20_001 {
        std::fs::write(share.join(format!("f{i}")), "").unwrap();
    }
    let got = check(&t.source(&[]), &inputs(&[], &[]), &share);
    assert_eq!(unknown_reason(&got), "more than 20 000 entries");
    // 20 000 entries are still read completely.
    std::fs::remove_file(share.join("f0")).unwrap();
    let got = check(&t.source(&[]), &inputs(&[], &[]), &share);
    assert_eq!(got.exposure_state, ExposureState::Clear);
}

#[test]
fn a_folder_nine_levels_deep_is_unknown_and_eight_is_clear() {
    let t = Tree::new();
    let share = t.dir("deep");
    let eight: Vec<String> = (1..=8).map(|n| format!("d{n}")).collect();
    t.dir(&format!("deep/{}", eight.join("/")));
    let got = check(&t.source(&[]), &inputs(&[], &[]), &share);
    assert_eq!(got.exposure_state, ExposureState::Clear);
    t.dir(&format!("deep/{}/d9", eight.join("/")));
    let got = check(&t.source(&[]), &inputs(&[], &[]), &share);
    assert_eq!(unknown_reason(&got), "deeper than 8 folders");
}

#[test]
fn a_walk_that_takes_too_long_is_unknown() {
    let t = Tree::new();
    let share = t.dir("slow");
    std::fs::write(share.join("a"), "").unwrap();
    let limits = WalkLimits {
        time: Duration::ZERO,
        ..WalkLimits::default()
    };
    let got = check_with(&t.source(&[]), &inputs(&[], &[]), share_at(&share), &limits);
    assert_eq!(unknown_reason(&got), "the check took more than 2 s");
}

#[test]
fn a_link_to_a_protected_path_is_exposed_and_a_link_to_another_folder_is_unknown() {
    let t = Tree::new();
    let ssh = t.dir("home/.ssh");
    let other = t.dir("other");
    let share = t.dir("srv/out");
    symlink(&ssh, share.join("keys")).unwrap();
    let got = check(&t.source(&[]), &inputs(&[], &[]), &share);
    assert_eq!(got.exposure_state, ExposureState::Exposed);
    assert_eq!(got.exposed, [Exposure::Ssh]);
    std::fs::remove_file(share.join("keys")).unwrap();
    symlink(&other, share.join("elsewhere")).unwrap();
    let got = check(&t.source(&[]), &inputs(&[], &[]), &share);
    assert_eq!(unknown_reason(&got), "a link to a folder");
    // A dangling link is skipped.
    std::fs::remove_file(share.join("elsewhere")).unwrap();
    symlink(t.root.join("nothing"), share.join("dangling")).unwrap();
    assert_eq!(
        check(&t.source(&[]), &inputs(&[], &[]), &share).exposure_state,
        ExposureState::Clear
    );
}

#[test]
fn a_folder_that_cannot_be_read_is_unknown() {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return;
    }
    let t = Tree::new();
    let share = t.dir("srv/out");
    let locked = t.dir("srv/out/locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let got = check(&t.source(&[]), &inputs(&[], &[]), &share);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(unknown_reason(&got), "a folder that cannot be read");
}

#[test]
fn an_ambiguous_share_is_exposed_when_any_candidate_is() {
    let t = Tree::new();
    let ssh = t.dir("home/.ssh");
    let mut share = share_at(&ssh);
    let padded = format!("{} ", ssh.display());
    share.path_state = SharePathState::Ambiguous;
    share.candidates = vec![ssh.to_string_lossy().into_owned(), padded];
    let got = check_with(
        &t.source(&[]),
        &inputs(&[], &[]),
        share,
        &WalkLimits::default(),
    );
    assert_eq!(got.exposure_state, ExposureState::Exposed);
    assert!(got.exposed.contains(&Exposure::Ssh));
    // With no candidate that exists, nothing is exposed now.
    let mut none = share_at(&t.root.join("nothing"));
    none.path_state = SharePathState::Ambiguous;
    none.candidates = vec![
        t.root.join("nothing").to_string_lossy().into_owned(),
        format!("{} ", t.root.join("nothing").display()),
    ];
    let got = check_with(
        &t.source(&[]),
        &inputs(&[], &[]),
        none,
        &WalkLimits::default(),
    );
    assert_eq!(got.exposure_state, ExposureState::Clear);
}

#[test]
fn a_missing_share_is_clear_and_every_share_is_unknown_before_the_first_snapshot() {
    let t = Tree::new();
    let mut gone = share_at(&t.root.join("gone"));
    gone.path_state = SharePathState::Missing;
    let got = check_with(
        &t.source(&[]),
        &inputs(&[], &[]),
        gone,
        &WalkLimits::default(),
    );
    assert_eq!(got.exposure_state, ExposureState::Clear);
    let mut before = inputs(&[], &[]);
    before.snapshot_seen = false;
    let share = t.dir("srv/out");
    let got = check(&t.source(&[]), &before, &share);
    assert_eq!(unknown_reason(&got), NO_HOMES_YET);
}

#[tokio::test]
async fn a_mount_path_inside_a_protected_path_is_exposed() {
    let rig = Rig::ready().await;
    let ssh = rig.home.join(".ssh");
    std::fs::create_dir_all(ssh.join("mnt")).unwrap();
    let source = rig.runtime.taildrive_source().unwrap();
    let kinds = path_exposure(&source, &rig.store, &ssh.join("mnt"))
        .await
        .unwrap();
    assert_eq!(kinds, [Exposure::Ssh]);
    let clear = path_exposure(&source, &rig.store, &rig.root.join("exports"))
        .await
        .unwrap();
    assert!(clear.is_empty(), "{clear:?}");
}

#[test]
fn a_failed_drive_list_gives_the_state_and_reason_of_the_table() {
    use TailscaleError as E;
    let reason = |e: E| drive_failure(&e);
    let un = TaildriveState::Unavailable;
    let kn = TaildriveState::Unknown;
    assert_eq!(
        reason(E::NotInstalled),
        (un, Some("tailscale is not installed".into()))
    );
    assert_eq!(
        reason(E::Timeout),
        (un, Some("tailscale drive list took more than 3 s".into()))
    );
    assert_eq!(
        reason(E::Exit(3)),
        (un, Some("tailscale drive list failed (exit 3)".into()))
    );
    for e in [E::NotRunning, E::Signal, E::Spawn("x".into())] {
        assert_eq!(reason(e), (un, Some("tailscale drive list failed".into())));
    }
    for e in [E::TooLarge, E::Parse("x".into())] {
        assert_eq!(
            reason(e),
            (kn, Some("Fleet Deck cannot read the share list".into()))
        );
    }
}

#[tokio::test]
async fn the_report_reads_the_shares_the_machine_and_the_tailnet() {
    let rig = Rig::ready().await;
    let report = rig.runtime.taildrive_report().unwrap();
    assert_eq!(report.state, TaildriveState::Ok);
    assert_eq!(report.machine.as_deref(), Some("box"));
    assert_eq!(rig.runtime.tailnet().as_deref(), Some("example.com"));
    assert_eq!(report.shares.len(), 1);
    assert_eq!(report.shares[0].exposure_state, ExposureState::Clear);
    // The argv log holds only `drive list` and `status --json`.
    assert_eq!(rig.runs("drive list"), 1);
    assert_eq!(rig.runs("status --json"), 1);
}

#[tokio::test]
async fn a_missing_or_unreadable_tailscale_gives_unavailable_or_unknown() {
    let rig = Rig::ready().await;
    let mut source = (*rig.runtime.taildrive_source().unwrap()).clone();
    source.cli = TailscaleCli::new(rig.root.join("no-tailscale"));
    rig.runtime.set_taildrive_source(source.clone());
    let report = rig.refresh().await;
    assert_eq!(report.state, TaildriveState::Unavailable);
    assert_eq!(report.reason.as_deref(), Some("tailscale is not installed"));
    assert!(report.shares.is_empty() && report.machine.is_none());
    // Output that the parser refuses.
    source.cli = TailscaleCli::new(rig.root.join("tailscale"));
    rig.runtime.set_taildrive_source(source);
    std::fs::write(rig.root.join("drive.txt"), "not a table\n").unwrap();
    let report = rig.refresh().await;
    assert_eq!(report.state, TaildriveState::Unknown);
    assert_eq!(
        report.reason.as_deref(),
        Some("Fleet Deck cannot read the share list")
    );
    // A table that reads while `status --json` does not: unknown (fail closed).
    std::fs::write(rig.root.join("drive.txt"), table(&[])).unwrap();
    std::fs::write(rig.root.join("status.json"), "{").unwrap();
    let report = rig.refresh().await;
    assert_eq!(report.state, TaildriveState::Unknown);
    assert_eq!(report.reason.as_deref(), Some("tailscale status failed"));
}

#[tokio::test]
async fn a_report_older_than_60_s_is_made_again_before_a_use() {
    let rig = Rig::ready().await;
    assert_eq!(rig.runs("drive list"), 1);
    // Inside the minute the cached report serves.
    rig.det.advance(Duration::from_secs(59));
    let same = fresh(&rig.runtime, &rig.store).await.unwrap();
    assert_eq!(rig.runs("drive list"), 1);
    // After it, `drive list` runs again, with a new revision.
    rig.det.advance(Duration::from_secs(2));
    let newer = fresh(&rig.runtime, &rig.store).await.unwrap();
    assert_eq!(rig.runs("drive list"), 2);
    assert!(newer.inventory_revision > same.inventory_revision);
    assert!(newer.checked_at > same.checked_at);
}

#[tokio::test]
async fn a_snapshot_makes_a_new_report_without_running_drive_list() {
    let rig = Rig::ready().await;
    let before = rig.runtime.taildrive_report().unwrap();
    let again = recompute(&rig.runtime, &rig.store).await.unwrap();
    assert_eq!(rig.runs("drive list"), 1);
    // `max(previous + 1, now)`: the clock did not move, so it is previous + 1.
    assert_eq!(again.inventory_revision, before.inventory_revision + 1);
    rig.det.advance(Duration::from_secs(5));
    let later = recompute(&rig.runtime, &rig.store).await.unwrap();
    assert_eq!(later.inventory_revision, later.checked_at as u64);
    assert!(later.inventory_revision > again.inventory_revision);
}

#[tokio::test]
async fn without_a_source_there_is_no_report() {
    let det = fleetdeck_testkit::Det::new();
    let store = crate::store::git_state::tests::store(&det).await;
    let runtime = FleetGitRuntime::new(std::sync::Arc::new(crate::workspace::GitOptions::isolated));
    assert!(fresh(&runtime, &store).await.is_none());
    assert!(refresh(&runtime, &store).await.is_none());
    assert!(runtime.taildrive_report().is_none());
}
