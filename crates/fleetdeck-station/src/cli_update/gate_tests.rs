use super::*;

const NOW: i64 = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;

fn input<'a>(kind: ProviderKind, installed: Option<&'a str>, target: &'a str) -> GateInput<'a> {
    GateInput {
        kind,
        installed,
        target,
        pinned: false,
        shim_host: false,
        published_at: Some(NOW - 100 * HOUR),
        now_ms: NOW,
        min_age_hours: 48,
    }
}

#[test]
fn a_target_that_is_not_a_release_is_refused_first() {
    for bad in ["2.1.310-beta", "--help", "2.1", "", "1.2.3.4"] {
        let i = input(ProviderKind::Claude, Some("2.1.300"), bad);
        assert_eq!(gate(&i), Err(reason::NOT_A_VERSION), "{bad}");
    }
}

#[test]
fn a_target_below_the_tested_minimum_is_refused() {
    let i = input(ProviderKind::Claude, Some("2.1.300"), "2.1.288");
    assert_eq!(gate(&i), Err(reason::BELOW_TESTED));
    let i = input(ProviderKind::Codex, Some("0.163.0"), "0.159.0");
    assert_eq!(gate(&i), Err(reason::BELOW_TESTED));
}

#[test]
fn a_target_below_the_handoff_minimum_is_refused() {
    // Tested from 2.1.289, verified for the handoff from 2.1.296.
    let i = input(ProviderKind::Claude, Some("2.1.300"), "2.1.290");
    assert_eq!(gate(&i), Err(reason::BELOW_HANDOFF));
}

#[test]
fn a_shim_host_needs_the_bridge_minimum() {
    // Codex 0.162.0 passes the handoff (0.160.0) but not the bridge (0.162.1).
    let mut i = input(ProviderKind::Codex, Some("0.160.0"), "0.162.0");
    assert_eq!(gate(&i), Ok(GateVerdict::Install));
    i.shim_host = true;
    assert_eq!(gate(&i), Err(reason::BELOW_HANDOFF));
    i.target = "0.162.1";
    assert_eq!(gate(&i), Ok(GateVerdict::Install));
}

#[test]
fn an_installed_version_at_or_above_the_target_is_up_to_date() {
    let i = input(ProviderKind::Claude, Some("2.1.310"), "2.1.310");
    assert_eq!(gate(&i), Ok(GateVerdict::UpToDate));
    // No downgrade without a pin.
    let i = input(ProviderKind::Claude, Some("2.1.312"), "2.1.310");
    assert_eq!(gate(&i), Ok(GateVerdict::UpToDate));
    // A probe answer with text around the version.
    let i = input(
        ProviderKind::Claude,
        Some("2.1.310 (Claude Code)"),
        "2.1.310",
    );
    assert_eq!(gate(&i), Ok(GateVerdict::UpToDate));
    let i = input(ProviderKind::Codex, Some("codex-cli 0.164.0"), "0.164.0");
    assert_eq!(gate(&i), Ok(GateVerdict::UpToDate));
}

#[test]
fn a_pin_below_the_installed_version_installs_and_the_same_pin_is_up_to_date() {
    let mut i = input(ProviderKind::Claude, Some("2.1.312"), "2.1.310");
    i.pinned = true;
    assert_eq!(gate(&i), Ok(GateVerdict::Install));
    i.installed = Some("2.1.310");
    assert_eq!(gate(&i), Ok(GateVerdict::UpToDate));
}

#[test]
fn a_target_younger_than_the_soak_time_waits() {
    let mut i = input(ProviderKind::Claude, Some("2.1.300"), "2.1.310");
    i.published_at = Some(NOW - 10 * HOUR);
    assert_eq!(gate(&i), Err(reason::TOO_NEW));
    i.published_at = Some(NOW - 48 * HOUR);
    assert_eq!(gate(&i), Ok(GateVerdict::Install));
}

#[test]
fn an_unknown_publish_time_refuses_unless_the_soak_is_off_or_pinned() {
    let mut i = input(ProviderKind::Claude, Some("2.1.300"), "2.1.310");
    i.published_at = None;
    assert_eq!(gate(&i), Err(reason::PUBLISH_TIME_UNKNOWN));
    i.min_age_hours = 0;
    assert_eq!(gate(&i), Ok(GateVerdict::Install));
    i.min_age_hours = 48;
    i.pinned = true;
    assert_eq!(gate(&i), Ok(GateVerdict::Install));
}

#[test]
fn an_unknown_installed_version_installs() {
    let i = input(ProviderKind::Claude, None, "2.1.310");
    assert_eq!(gate(&i), Ok(GateVerdict::Install));
    let i = input(ProviderKind::Claude, Some("garbage"), "2.1.310");
    assert_eq!(gate(&i), Ok(GateVerdict::Install));
}

#[test]
fn the_checks_run_in_the_order_of_the_design() {
    // Below the tested minimum and too new at once: the tested check wins.
    let mut i = input(ProviderKind::Claude, Some("2.1.300"), "2.1.288");
    i.published_at = Some(NOW);
    assert_eq!(gate(&i), Err(reason::BELOW_TESTED));
    // Up to date before the soak check.
    let mut i = input(ProviderKind::Claude, Some("2.1.310"), "2.1.310");
    i.published_at = None;
    assert_eq!(gate(&i), Ok(GateVerdict::UpToDate));
}
