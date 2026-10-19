//! Tests of `station.tools`: the tool states, the keyboard, the plugin and
//! the sudo switch.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use fleetdeck_proto::desktop_tools::{Probe, StationTools, ToolFact, ToolsDesktop, VersionSource};

use super::facts::collect;
use super::probe::{PROBE_LIMIT, ProbeRun, TOTAL_LIMIT};
use super::test_support::{Call, FakeExec, Rig, exit};

/// The `-Qo` reply for `owners` (`exe name` and the text after `owned by`);
/// every other path is not owned.
fn qo(call: &Call, owners: &[(&str, &str)]) -> Option<ProbeRun> {
    if call.program != Path::new("/usr/bin/pacman")
        || call.args.first().map(String::as_str) != Some("-Qo")
    {
        return None;
    }
    let path = &call.args[1];
    let name = Path::new(path).file_name().unwrap().to_str().unwrap();
    let reply = owners.iter().find(|(exe, _)| *exe == name);
    Some(match reply {
        Some((_, owner)) => exit(0, &format!("{path} is owned by {owner}\n")),
        None => exit(1, ""),
    })
}

pub(super) async fn tools_of(rig: &Rig, exec: &FakeExec) -> StationTools {
    collect(exec, &rig.base(), None, TOTAL_LIMIT).await
}

fn fact<'a>(tools: &'a StationTools, id: &str) -> &'a ToolFact {
    tools.tools.iter().find(|t| t.id == id).unwrap()
}

#[tokio::test]
async fn an_owned_executable_has_a_package_version() {
    let rig = Rig::new();
    rig.tool("wtype");
    rig.tool("busctl");
    rig.tool("wlrctl");
    let exec = FakeExec::new(|c| {
        qo(
            c,
            &[
                ("wtype", "wtype 0.4-1"),
                ("busctl", "systemd 256.7-1"),
                ("wlrctl", "wlrctl 0.2.2-1"),
            ],
        )
        .unwrap()
    });
    let tools = tools_of(&rig, &exec).await;
    let wtype = fact(&tools, "wtype");
    assert!(wtype.present);
    assert_eq!(wtype.version.as_deref(), Some("0.4-1"));
    assert_eq!(wtype.version_source, VersionSource::Package);
    assert_eq!(wtype.package_matches_executable, Some(true));
    assert_eq!(wtype.probe, Probe::Ok);
    assert_eq!(
        fact(&tools, "busctl").package_matches_executable,
        Some(true)
    );
    assert_eq!(
        fact(&tools, "wlrctl").package_matches_executable,
        Some(true)
    );
}

#[tokio::test]
async fn an_executable_of_another_package_does_not_match() {
    let rig = Rig::new();
    rig.tool("wtype");
    let exec = FakeExec::new(|c| qo(c, &[("wtype", "wtype-git 0.5-1")]).unwrap());
    let tools = tools_of(&rig, &exec).await;
    let wtype = fact(&tools, "wtype");
    assert_eq!(wtype.package_matches_executable, Some(false));
    assert_eq!(wtype.version.as_deref(), Some("0.5-1"));
    assert_eq!(wtype.probe, Probe::Ok);
}

#[tokio::test]
async fn a_not_owned_executable_has_no_version() {
    let rig = Rig::new();
    rig.tool("grim");
    let exec = FakeExec::new(|c| qo(c, &[]).unwrap());
    let tools = tools_of(&rig, &exec).await;
    let grim = fact(&tools, "grim");
    assert!(grim.present);
    assert_eq!(grim.version, None);
    assert_eq!(grim.version_source, VersionSource::None);
    assert_eq!(grim.package_matches_executable, Some(false));
    assert_eq!(grim.probe, Probe::Ok);
}

#[tokio::test]
async fn a_bad_version_text_is_dropped_and_the_owner_is_kept() {
    let rig = Rig::new();
    rig.tool("jq");
    for bad in [
        "jq bad/version".to_owned(),
        format!("jq {}", "1".repeat(65)),
        "jq 1.7;rm".to_owned(),
    ] {
        let owner = bad.clone();
        let exec = FakeExec::new(move |c| qo(c, &[("jq", &owner)]).unwrap());
        let tools = tools_of(&rig, &exec).await;
        let jq = fact(&tools, "jq");
        assert_eq!(jq.version, None, "{bad}");
        assert_eq!(jq.version_source, VersionSource::None, "{bad}");
        assert_eq!(jq.package_matches_executable, Some(true), "{bad}");
    }
}

#[tokio::test]
async fn a_version_with_the_allowed_characters_is_kept() {
    let rig = Rig::new();
    rig.tool("jq");
    let exec = FakeExec::new(|c| qo(c, &[("jq", "jq 1:1.7.1_rc~2+git-3")]).unwrap());
    let tools = tools_of(&rig, &exec).await;
    assert_eq!(
        fact(&tools, "jq").version.as_deref(),
        Some("1:1.7.1_rc~2+git-3")
    );
}

#[tokio::test]
async fn a_timeout_a_failure_and_a_bad_output_are_not_ok() {
    let rig = Rig::new();
    rig.tool("grim");
    let cases = [
        (ProbeRun::Timeout, Probe::Timeout),
        (ProbeRun::Failed, Probe::Failed),
        (exit(2, ""), Probe::Failed),
        (exit(0, "garbage\n"), Probe::Failed),
        (exit(0, "/other/grim is owned by grim 1-1\n"), Probe::Failed),
        (exit(0, "grim is owned by grim 1-1 extra\n"), Probe::Failed),
    ];
    for (reply, probe) in cases {
        let exec = FakeExec::new(move |_| reply.clone());
        let tools = tools_of(&rig, &exec).await;
        let grim = fact(&tools, "grim");
        assert!(grim.present);
        assert_eq!(grim.probe, probe);
        assert_eq!(grim.version, None);
        assert_eq!(grim.version_source, VersionSource::None);
        assert_eq!(grim.package_matches_executable, None);
    }
}

#[tokio::test]
async fn a_missing_tool_is_missing_and_runs_no_child() {
    let rig = Rig::new();
    let exec = FakeExec::new(|_| ProbeRun::Failed);
    let tools = tools_of(&rig, &exec).await;
    assert_eq!(tools.tools.len(), 9);
    for t in &tools.tools {
        assert!(!t.present, "{}", t.id);
        assert_eq!(t.probe, Probe::Missing);
        assert_eq!(t.version, None);
        assert_eq!(t.version_source, VersionSource::None);
        assert_eq!(t.package_matches_executable, None);
    }
    assert!(exec.calls().is_empty());
}

#[tokio::test]
async fn without_pacman_a_present_tool_is_unknown() {
    let rig = Rig::new();
    rig.tool("grim");
    let exec = FakeExec::new(|_| ProbeRun::Failed).without_pacman();
    let tools = tools_of(&rig, &exec).await;
    let grim = fact(&tools, "grim");
    assert!(grim.present);
    assert_eq!(grim.probe, Probe::Unknown);
    assert_eq!(grim.version, None);
    assert_eq!(grim.version_source, VersionSource::None);
    assert_eq!(grim.package_matches_executable, None);
    assert!(exec.calls().is_empty());
}

#[tokio::test]
async fn the_probe_argv_and_environment_are_fixed() {
    let rig = Rig::new();
    let path = rig.tool("wtype");
    let exec = FakeExec::new(|c| qo(c, &[]).unwrap());
    tools_of(&rig, &exec).await;
    let calls = exec.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].program, Path::new("/usr/bin/pacman"));
    assert_eq!(calls[0].args, ["-Qo", path.to_str().unwrap()]);
    assert_eq!(
        calls[0].env,
        [
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("LANG".to_owned(), "C".to_owned())
        ]
    );
    assert!(calls[0].limit <= PROBE_LIMIT);
}

#[tokio::test]
async fn the_answer_has_no_path_and_no_raw_output() {
    let rig = Rig::new();
    rig.tool("wtype");
    let exec = FakeExec::new(|c| qo(c, &[("wtype", "wtype 0.4-1")]).unwrap());
    let tools = tools_of(&rig, &exec).await;
    let json = serde_json::to_string(&tools).unwrap();
    assert!(!json.contains(rig.dir.path().to_str().unwrap()));
    assert!(!json.contains("is owned by"));
}

#[tokio::test]
async fn the_desktop_is_omarchy_hyprland_or_none() {
    let rig = Rig::new();
    let exec = FakeExec::new(|_| ProbeRun::Failed);
    let base = rig.base();
    let none = collect(&exec, &base, None, TOTAL_LIMIT).await;
    assert_eq!(none.desktop, ToolsDesktop::None);
    let found = rig.hyprland();
    let hypr = collect(&exec, &base, Some(&found), TOTAL_LIMIT).await;
    assert_eq!(hypr.desktop, ToolsDesktop::Hyprland);
    let found = rig.omarchy();
    let omarchy = collect(&exec, &base, Some(&found), TOTAL_LIMIT).await;
    assert_eq!(omarchy.desktop, ToolsDesktop::Omarchy);
}

#[tokio::test]
async fn the_desktop_environment_resolves_the_tools() {
    let rig = Rig::new();
    let found = rig.omarchy();
    rig.omarchy_exe("wtype");
    let exec = FakeExec::new(|c| qo(c, &[("wtype", "wtype 0.4-1")]).unwrap());
    let tools = collect(&exec, &rig.base(), Some(&found), TOTAL_LIMIT).await;
    assert!(fact(&tools, "wtype").present);
    let plain = collect(&exec, &rig.base(), None, TOTAL_LIMIT).await;
    assert!(!fact(&plain, "wtype").present);
}

#[tokio::test]
async fn a_stuck_probe_leaves_the_other_facts_usable() {
    let rig = Rig::new();
    rig.tool("wtype");
    rig.tool("grim");
    let exec = FakeExec::new(|c| match qo(c, &[("grim", "grim 1.4-1")]) {
        Some(_) if c.args[1].ends_with("wtype") => ProbeRun::Timeout,
        Some(reply) => reply,
        None => ProbeRun::Failed,
    });
    let tools = tools_of(&rig, &exec).await;
    assert_eq!(fact(&tools, "wtype").probe, Probe::Timeout);
    assert_eq!(fact(&tools, "grim").version.as_deref(), Some("1.4-1"));
}
