//! Tests of the compiled tool mapping.

use fleetdeck_proto::desktop_tools::tool;

use super::mapping::{REPOS, TOOLS, by_id, target};

#[test]
fn the_nine_tools_follow_the_order_of_the_wire_ids() {
    let ids: Vec<&str> = TOOLS.iter().map(|t| t.id).collect();
    assert_eq!(ids, tool::ALL);
}

#[test]
fn each_tool_has_its_executable_and_package() {
    let rows: Vec<_> = TOOLS.iter().map(|t| (t.id, t.exe, t.package)).collect();
    assert_eq!(
        rows,
        vec![
            ("wtype", "wtype", Some(("extra", "wtype"))),
            ("wf_recorder", "wf-recorder", Some(("extra", "wf-recorder"))),
            ("wlrctl", "wlrctl", None),
            ("grim", "grim", Some(("extra", "grim"))),
            ("busctl", "busctl", Some(("core", "systemd"))),
            ("jq", "jq", Some(("extra", "jq"))),
            ("hyprland", "Hyprland", Some(("extra", "hyprland"))),
            ("chromium", "chromium", Some(("extra", "chromium"))),
            ("notify_send", "notify-send", Some(("extra", "libnotify"))),
        ]
    );
}

#[test]
fn a_target_is_repo_slash_name_and_wlrctl_has_none() {
    let t = |id: &str| by_id(id).and_then(target);
    assert_eq!(t("wtype").as_deref(), Some("extra/wtype"));
    assert_eq!(t("busctl").as_deref(), Some("core/systemd"));
    assert_eq!(t("notify_send").as_deref(), Some("extra/libnotify"));
    assert_eq!(t("wlrctl"), None);
    assert!(by_id("nope").is_none());
}

#[test]
fn only_core_and_extra_are_allowed_and_every_package_uses_one() {
    assert_eq!(REPOS, ["core", "extra"]);
    for t in &TOOLS {
        if let Some((repo, _)) = t.package {
            assert!(REPOS.contains(&repo), "{}", t.id);
        }
    }
}

#[test]
fn the_owner_name_is_the_package_or_the_executable() {
    assert_eq!(by_id("busctl").map(|t| t.owner_name()), Some("systemd"));
    assert_eq!(by_id("wlrctl").map(|t| t.owner_name()), Some("wlrctl"));
}
