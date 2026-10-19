//! Tests of `station.tools`: the keyboard layouts, the plugin and the sudo
//! switch.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;

use fleetdeck_proto::desktop_tools::{FactStatus, PluginStatus, Switch};

use super::facts::collect;
use super::facts_tests::tools_of;
use super::probe::{ProbeRun, TOTAL_LIMIT};
use super::test_support::{FakeExec, Rig, exit};

fn keyboards(layouts: &[&str]) -> String {
    let list: Vec<String> = layouts
        .iter()
        .map(|l| {
            format!(r#"{{"address":"0xdead","name":"my-keyboard","layout":"{l}","variant":""}}"#)
        })
        .collect();
    format!(
        r#"{{"mice":[{{"name":"mouse"}}],"keyboards":[{}]}}"#,
        list.join(",")
    )
}

fn hypr(reply: ProbeRun) -> FakeExec {
    FakeExec::new(move |c| {
        if c.program.ends_with("hyprctl") {
            reply.clone()
        } else {
            exit(1, "")
        }
    })
}

#[tokio::test]
async fn the_layouts_are_parsed_filtered_sorted_and_deduplicated() {
    let rig = Rig::new();
    let found = rig.omarchy();
    rig.omarchy_exe("hyprctl");
    let text = keyboards(&["us, de ,us", "fr,,bad layout,ru;x,", "es"]);
    let exec = hypr(exit(0, &text));
    let tools = collect(&exec, &rig.base(), Some(&found), TOTAL_LIMIT).await;
    assert_eq!(tools.keyboard.status, FactStatus::Ok);
    assert_eq!(tools.keyboard.layouts, ["de", "es", "fr", "us"]);
    let call = exec
        .calls()
        .into_iter()
        .find(|c| c.program.ends_with("hyprctl"))
        .unwrap();
    assert_eq!(call.args, ["devices", "-j"]);
    let json = serde_json::to_string(&tools).unwrap();
    assert!(!json.contains("0xdead") && !json.contains("my-keyboard"));
}

#[tokio::test]
async fn at_most_eight_layouts_are_kept() {
    let rig = Rig::new();
    let found = rig.omarchy();
    rig.omarchy_exe("hyprctl");
    let all = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"].join(",");
    let exec = hypr(exit(0, &keyboards(&[&all])));
    let tools = collect(&exec, &rig.base(), Some(&found), TOTAL_LIMIT).await;
    assert_eq!(
        tools.keyboard.layouts,
        ["a", "b", "c", "d", "e", "f", "g", "h"]
    );
}

#[tokio::test]
async fn the_keyboard_is_unavailable_without_a_desktop_or_hyprctl() {
    let rig = Rig::new();
    rig.tool("hyprctl");
    let exec = hypr(exit(0, &keyboards(&["us"])));
    let none = collect(&exec, &rig.base(), None, TOTAL_LIMIT).await;
    assert_eq!(none.keyboard.status, FactStatus::Unavailable);
    assert!(none.keyboard.layouts.is_empty());
    let rig = Rig::new();
    let found = rig.omarchy();
    let missing = collect(&exec, &rig.base(), Some(&found), TOTAL_LIMIT).await;
    assert_eq!(missing.keyboard.status, FactStatus::Unavailable);
}

#[tokio::test]
async fn a_failed_keyboard_read_is_failed_with_no_layouts() {
    let rig = Rig::new();
    let found = rig.omarchy();
    rig.omarchy_exe("hyprctl");
    let cases = [
        exit(1, &keyboards(&["us"])),
        exit(0, "not json"),
        exit(0, r#"{"mice":[]}"#),
        exit(0, r#"{"keyboards":"us"}"#),
        ProbeRun::Timeout,
        ProbeRun::Failed,
    ];
    for reply in cases {
        let exec = hypr(reply);
        let tools = collect(&exec, &rig.base(), Some(&found), TOTAL_LIMIT).await;
        assert_eq!(tools.keyboard.status, FactStatus::Failed);
        assert!(tools.keyboard.layouts.is_empty());
        assert_eq!(tools.tools.len(), 9, "the other facts stay");
    }
}

fn validate(reply: ProbeRun) -> FakeExec {
    FakeExec::new(move |c| {
        if c.program.ends_with("omarchy-plugin-validate") {
            reply.clone()
        } else {
            exit(1, "")
        }
    })
}

#[tokio::test]
async fn the_plugin_status_follows_the_validate_exit_code() {
    let rig = Rig::new();
    let found = rig.omarchy();
    let folder = rig.plugin_dir();
    rig.omarchy_exe("omarchy-plugin-validate");
    let cases = [
        (exit(0, ""), PluginStatus::Valid),
        (exit(1, ""), PluginStatus::Invalid),
        (exit(2, ""), PluginStatus::Unknown),
        (ProbeRun::Timeout, PluginStatus::Unknown),
        (ProbeRun::Failed, PluginStatus::Unknown),
    ];
    for (reply, status) in cases {
        let exec = validate(reply);
        let tools = collect(&exec, &rig.base(), Some(&found), TOTAL_LIMIT).await;
        assert_eq!(tools.plugin.status, status);
        let call = exec
            .calls()
            .into_iter()
            .find(|c| c.program.ends_with("omarchy-plugin-validate"))
            .unwrap();
        assert_eq!(call.args, [folder.to_str().unwrap()]);
    }
}

#[tokio::test]
async fn the_plugin_is_unavailable_without_omarchy_a_folder_or_validate() {
    let rig = Rig::new();
    rig.omarchy_exe("omarchy-plugin-validate");
    rig.plugin_dir();
    let exec = validate(exit(0, ""));
    let base = rig.base();
    let hypr = collect(&exec, &base, Some(&rig.hyprland()), TOTAL_LIMIT).await;
    assert_eq!(hypr.plugin.status, PluginStatus::Unavailable);
    let none = collect(&exec, &base, None, TOTAL_LIMIT).await;
    assert_eq!(none.plugin.status, PluginStatus::Unavailable);
    assert!(
        exec.calls()
            .iter()
            .all(|c| !c.program.ends_with("omarchy-plugin-validate"))
    );

    let rig = Rig::new();
    let found = rig.omarchy();
    rig.omarchy_exe("omarchy-plugin-validate");
    let no_folder = collect(&exec, &rig.base(), Some(&found), TOTAL_LIMIT).await;
    assert_eq!(no_folder.plugin.status, PluginStatus::Unavailable);

    let rig = Rig::new();
    let found = rig.omarchy();
    rig.plugin_dir();
    let no_validate = collect(&exec, &rig.base(), Some(&found), TOTAL_LIMIT).await;
    assert_eq!(no_validate.plugin.status, PluginStatus::Unavailable);
}

#[tokio::test]
async fn the_plugin_folder_follows_the_config_home() {
    let rig = Rig::new();
    let found = rig.omarchy();
    rig.omarchy_exe("omarchy-plugin-validate");
    let exec = validate(exit(0, ""));
    // No HOME and no usable XDG_CONFIG_HOME: unknown.
    let mut env = rig.base();
    env.remove("HOME");
    let tools = collect(&exec, &env, Some(&found), TOTAL_LIMIT).await;
    assert_eq!(tools.plugin.status, PluginStatus::Unknown);
    // A relative XDG_CONFIG_HOME is ignored.
    env.set("HOME", &rig.home).unwrap();
    env.set("XDG_CONFIG_HOME", "relative/config").unwrap();
    rig.plugin_dir();
    let tools = collect(&exec, &env, Some(&found), TOTAL_LIMIT).await;
    assert_eq!(tools.plugin.status, PluginStatus::Valid);
    // An absolute XDG_CONFIG_HOME wins.
    let xdg = rig.dir.path().join("xdg");
    env.set("XDG_CONFIG_HOME", &xdg).unwrap();
    let tools = collect(&exec, &env, Some(&found), TOTAL_LIMIT).await;
    assert_eq!(tools.plugin.status, PluginStatus::Unavailable);
    fs::create_dir_all(xdg.join("omarchy/plugins/fleetdeck.fleet")).unwrap();
    let tools = collect(&exec, &env, Some(&found), TOTAL_LIMIT).await;
    assert_eq!(tools.plugin.status, PluginStatus::Valid);
}

fn sudo(reply: ProbeRun) -> FakeExec {
    FakeExec::new(move |c| {
        if c.program.ends_with("omarchy-sudo-passwordless") {
            reply.clone()
        } else {
            exit(1, "")
        }
    })
}

#[tokio::test]
async fn the_sudo_switch_follows_the_exit_code() {
    let rig = Rig::new();
    rig.tool("omarchy-sudo-passwordless");
    let cases = [
        (exit(0, ""), Switch::On),
        (exit(1, ""), Switch::Off),
        (exit(3, ""), Switch::Unknown),
        (ProbeRun::Timeout, Switch::Unknown),
        (ProbeRun::Failed, Switch::Unknown),
    ];
    for (reply, switch) in cases {
        let exec = sudo(reply);
        let tools = tools_of(&rig, &exec).await;
        assert_eq!(tools.passwordless_sudo, switch);
        let call = exec
            .calls()
            .into_iter()
            .find(|c| c.program.ends_with("omarchy-sudo-passwordless"))
            .unwrap();
        assert_eq!(call.args, ["--active"]);
        assert!(exec.calls().iter().all(|c| !c.program.ends_with("sudo")));
    }
}

#[tokio::test]
async fn the_sudo_switch_is_unknown_when_the_program_is_not_resolved() {
    let rig = Rig::new();
    let exec = sudo(exit(0, ""));
    let tools = tools_of(&rig, &exec).await;
    assert_eq!(tools.passwordless_sudo, Switch::Unknown);
    assert!(exec.calls().is_empty());
}
