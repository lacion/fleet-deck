//! The shim half of resume on the shim host (DL-182): the launch boot
//! evidence of the journal (design §4.1, test 23), the inspection of the
//! exit report (§1a), the N and N−1 encodings (test 21) and the escaped
//! descendant (test 15). Real `fleetdeck shim` processes with `/bin/sh`
//! providers; the test process plays the station.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;

use fleetdeck_providers::host::shim::client::AttachResult;
use fleetdeck_providers::host::shim::identity::{BootIdReader, boot_id};
use fleetdeck_providers::host::shim::journal::{LaunchEvidence, Record};
use fleetdeck_providers::host::shim::launch::LaunchReply;
use fleetdeck_providers::host::shim::wire::{FromShim, ToShim};
use fleetdeck_station::manager::shims;
use fleetdeck_station::store::{ShimRow, ShimState};

mod shim_rig;
use shim_rig::*;

/// The rig with a journal whose `created` record holds `boot`.
fn rig_with_boot(script: &str, boot: Option<&str>) -> Rig {
    let rig = Rig::new(script);
    std::fs::remove_file(rig.files.journal()).unwrap();
    rig.files.create_journal(INC, boot).unwrap();
    rig
}

fn real_boot() -> String {
    boot_id().unwrap()
}

/// Launches and expects the refusal text of a launch that did not spawn.
async fn refusal(rig: &Rig) -> String {
    let mut launched = rig.launch().await;
    let LaunchReply::Failed {
        spawn_attempted,
        error,
    } = &launched.reply
    else {
        panic!("{:?}", launched.reply);
    };
    assert!(!spawn_attempted, "no spawn was attempted");
    let error = error.clone();
    launched.child.wait().await.unwrap();
    error
}

#[tokio::test]
async fn a_shim_whose_boot_id_differs_from_its_journal_refuses_to_spawn_the_provider() {
    let rig = rig_with_boot(shim_rig::ECHO, Some("a-boot-that-is-not-this-one"));
    let error = refusal(&rig).await;
    assert!(
        error.contains("the boot id differs from the launch journal"),
        "{error}"
    );
    // No provider, no socket, no `spawn_attempted`.
    assert!(rig.rec().is_empty());
    assert!(!rig.spec.socket_path.exists());
    let records = rig.files.records().unwrap();
    assert!(!records.contains(&Record::SpawnAttempted), "{records:?}");
    // A late fence still reads the journal as a launch that never started.
    assert!(matches!(
        rig.files.fence(INC).unwrap(),
        fleetdeck_providers::host::shim::journal::Fence::Fenced(LaunchEvidence::NeverStarted)
    ));
}

#[tokio::test]
async fn an_empty_boot_id_in_the_journal_refuses_too() {
    let rig = rig_with_boot(shim_rig::ECHO, Some(""));
    let error = refusal(&rig).await;
    assert!(
        error.contains("the boot id differs from the launch journal"),
        "{error}"
    );
    assert!(
        !rig.files
            .records()
            .unwrap()
            .contains(&Record::SpawnAttempted)
    );
}

#[tokio::test]
async fn the_shim_spawns_on_the_boot_of_its_journal_and_for_a_journal_without_a_boot_id() {
    // The boot id of this machine: the provider starts.
    let rig = rig_with_boot(shim_rig::ECHO, Some(&real_boot()));
    let (launched, _) = rig.ready().await;
    end(&rig, launched, 1).await;
    // A journal of an older station has no boot id: the old behavior.
    let rig = rig_with_boot(shim_rig::ECHO, None);
    let (launched, _) = rig.ready().await;
    end(&rig, launched, 1).await;
}

#[tokio::test]
async fn a_clean_exit_reports_a_zero_count_with_the_inspection_and_an_older_station_gets_no_inspection()
 {
    let rig = Rig::new("echo done");
    let (launched, _) = rig.ready().await;
    // The station of this build (protocol 2).
    let (mut att, attached) = rig.attach(1).await;
    let FromShim::Attached { exit, .. } = attached else {
        panic!("{attached:?}");
    };
    let seen = read_until(&mut att, |s| exited(s).is_some() || exit.is_some()).await;
    let report = exited(&seen).cloned().or(exit).unwrap();
    assert_eq!(report.descendants_remaining, Some(0));
    assert_eq!(report.inspection, 1, "a complete inspection");
    // A station of the older build (protocol 1) on the same shim: the same
    // zero count, but without the field that it does not know.
    let result = rig
        .connect()
        .await
        .attach_with_protocol(&rig.key, INC, 2, 1, WAIT)
        .await
        .unwrap();
    let AttachResult::Attached { frame, .. } = result else {
        panic!("refused");
    };
    let FromShim::Attached { exit, .. } = frame else {
        panic!("{frame:?}");
    };
    let older = exit.expect("the shim holds its exit report");
    assert_eq!(older.descendants_remaining, Some(0));
    assert_eq!(older.inspection, 0, "version 1 has no inspection field");
    end_after_exit(&rig, 3).await;
    drop(launched);
}

/// A child that ends with the provider comes back to the shim in another
/// process group than the shim's: `waitpid(-1)` reaps its zombie, so the
/// census is clean and the exit is a proven one (DL-430).
#[tokio::test]
async fn a_child_that_ends_with_the_provider_leaves_a_proven_exit() {
    let rig = Rig::new("sh -c 'true &'; sleep 0.3; echo done");
    let (launched, _) = rig.ready().await;
    let (mut att, _) = rig.attach(1).await;
    let seen = read_until(&mut att, |s| exited(s).is_some()).await;
    let report = exited(&seen).unwrap();
    assert_eq!(report.descendants_remaining, Some(0), "{report:?}");
    assert_eq!(report.inspection, 1);
    assert_eq!(
        shims::exit_reason(Some(report)),
        shims::reason::EXITED,
        "the end is proven"
    );
    end_after_exit(&rig, 2).await;
    drop(launched);
}

#[tokio::test]
async fn a_station_of_the_newer_build_picks_the_highest_protocol_and_the_older_one_stays_possible()
{
    let rig = Rig::new(shim_rig::ECHO);
    let (launched, _) = rig.ready().await;
    let conn = rig.connect().await;
    assert_eq!(conn.hello.protocols, vec![2, 1]);
    drop(conn);
    // A station of the previous build attaches with version 1.
    let result = rig
        .connect()
        .await
        .attach_with_protocol(&rig.key, INC, 1, 1, WAIT)
        .await
        .unwrap();
    assert!(matches!(result, AttachResult::Attached { .. }));
    // A version that no build speaks is refused as incompatible.
    let result = rig
        .connect()
        .await
        .attach_with_protocol(&rig.key, INC, 2, 99, WAIT)
        .await
        .unwrap();
    assert!(matches!(
        result,
        AttachResult::Refused(fleetdeck_providers::host::shim::wire::Refusal::Incompatible)
    ));
    end(&rig, launched, 3).await;
}

/// A process that left the provider's group and session before the
/// provider exited normally (test 15): the exit is a clean one with a zero
/// count, so a resume is admitted; the owner's check lists the process for
/// information, and it does not block.
#[tokio::test]
async fn a_detached_descendant_does_not_make_a_clean_exit_unproven_and_the_check_lists_it() {
    let rig = Rig::new("setsid sleep 120 </dev/null >/dev/null 2>&1 & sleep 0.4; echo done");
    let (launched, provider_pid) = rig.ready().await;
    let LaunchReply::Ready { provider_identity } = &launched.reply else {
        panic!("{:?}", launched.reply);
    };
    let shim_pid = launched.child.id().unwrap();
    let (mut att, _) = rig.attach(1).await;
    let seen = read_until(&mut att, |s| exited(s).is_some()).await;
    let report = exited(&seen).unwrap();
    assert_eq!(
        report.descendants_remaining,
        Some(0),
        "it left the group and the session"
    );
    assert_eq!(report.inspection, 1);
    assert_eq!(
        shims::exit_reason(Some(report)),
        shims::reason::EXITED,
        "the end is proven"
    );

    // The owner's check of a lost copy of this run, while the shim still
    // keeps the orphan as its child.
    let row = ShimRow {
        shim_id: rig.spec.shim_id.clone(),
        session_id: "s".into(),
        incarnation: INC.to_owned(),
        state: ShimState::Lost,
        generation: 1,
        socket_path: PathBuf::from("/x"),
        provider_identity: Some(provider_identity.clone()),
        cursor: 0,
        next_write_seq: 1,
        reason: Some("execution_unknown".to_owned()),
        inspection_version: 0,
        shim_pid: Some(shim_pid),
        proof: None,
        proof_boot_id: None,
        launch_boot_id: Some(real_boot()),
        native_target: None,
        created_at: 1,
        station_proof: None,
        station_proof_at: None,
    };
    let reader: BootIdReader = Arc::new(|| Ok(real_boot()));
    let check = shims::check_row(&reader, &row);
    assert!(check.same_boot && check.complete, "{check:?}");
    assert!(
        !check.still_runs(),
        "an escaped process does not block: {check:?}"
    );
    assert_eq!(check.escaped.len(), 1, "{check:?}");
    assert_eq!(check.escaped[0].command, "sleep");
    // Cleanup: the orphan is not part of any shim that the rig reaps.
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &check.escaped[0].pid.to_string()])
        .status();
    let _ = provider_pid;
    end_after_exit(&rig, 2).await;
    drop(launched);
}

/// Acknowledges the exit so that the shim ends.
async fn end_after_exit(rig: &Rig, generation: u64) {
    let (mut att, _) = rig.attach(generation).await;
    att.send(&ToShim::AckExit { generation }, &[])
        .await
        .unwrap();
}
