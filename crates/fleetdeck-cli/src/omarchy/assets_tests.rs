//! Golden-text tests of the embedded files (R13 P4 task C).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::io::Write as _;
use std::process::{Command, Stdio};

use super::*;

fn asset(path: &str) -> &'static Asset {
    FILES
        .iter()
        .find(|a| a.path == path)
        .unwrap_or_else(|| panic!("no asset {path}"))
}

#[test]
fn the_file_list_has_the_paths_and_modes_of_the_annex() {
    let got: Vec<(Base, &str, u32, bool)> = FILES
        .iter()
        .map(|a| (a.base, a.path, a.mode, a.only_if_absent))
        .collect();
    assert_eq!(
        got,
        vec![
            (
                Base::Omarchy,
                "plugins/fleetdeck.fleet/manifest.json",
                0o644,
                false
            ),
            (
                Base::Omarchy,
                "plugins/fleetdeck.fleet/Widget.qml",
                0o644,
                false
            ),
            (
                Base::Omarchy,
                "plugins/fleetdeck.fleet/Panel.qml",
                0o644,
                false
            ),
            (
                Base::Omarchy,
                "plugins/fleetdeck.fleet/Service.qml",
                0o644,
                false
            ),
            (
                Base::Omarchy,
                "plugins/fleetdeck.fleet/Model.js",
                0o644,
                false
            ),
            (
                Base::Omarchy,
                "plugins/fleetdeck.fleet/i18n/en.json",
                0o644,
                false
            ),
            (Base::Omarchy, "hooks/post-boot.d/fleetdeck", 0o755, false),
            (Base::Omarchy, "hooks/post-update.d/fleetdeck", 0o755, false),
            (Base::Omarchy, "hooks/theme-set.d/fleetdeck", 0o755, false),
            (Base::Omarchy, "themed/fleetdeck.json.tpl", 0o644, false),
            (Base::Fleetdeck, "omarchy/hypr.lua", 0o644, false),
            (Base::Omarchy, "extensions/omarchy-menu.jsonc", 0o644, true),
        ]
    );
}

#[test]
fn hypr_lua_is_the_comment_and_the_six_lines_of_the_annex() {
    let want = "\
-- Fleet Deck (installed by `fleetdeck omarchy install`). Reinstalling rewrites this file.
o.window(\"^fleetdeck-wall-[A-Za-z0-9_-]+$\", { border_size = 0 })
o.window(\"^fleetdeck-preview-[A-Za-z0-9_-]+$\", { float = true, size = { 1280, 800 } })
o.bind(\"SUPER + CTRL + F\", \"Fleet Deck panel\", \"omarchy-shell shell toggle fleetdeck.fleet\")
o.bind(\"SUPER + ALT + W\", \"Fleet Deck wall\", \"fleetdeck desktop wall show\")
o.bind(\"SUPER + ALT + N\", \"Fleet Deck: next session that needs you on the wall\", \"fleetdeck desktop wall next\")
o.bind(\"SUPER + ALT + M\", \"Fleet Deck: next wall mode\", \"fleetdeck desktop wall mode next\")
";
    assert_eq!(asset("omarchy/hypr.lua").text, want);
    assert_eq!(want.lines().count(), 7);
}

#[test]
fn the_theme_template_is_the_one_of_the_annex() {
    let want = "\
{
  \"mode\": \"{{ mode }}\",
  \"background\": \"{{ background }}\",
  \"foreground\": \"{{ foreground }}\",
  \"accent\": \"{{ accent }}\",
  \"muted\": \"{{ muted }}\",
  \"dark_background\": \"{{ dark_background }}\",
  \"dark_foreground\": \"{{ dark_foreground }}\",
  \"bright_foreground\": \"{{ bright_foreground }}\",
  \"selection\": \"{{ selection }}\",
  \"red\": \"{{ red }}\",
  \"yellow\": \"{{ yellow }}\"
}
";
    assert_eq!(asset("themed/fleetdeck.json.tpl").text, want);
}

#[test]
fn the_menu_file_is_a_comment_the_brace_three_rows_and_the_brace() {
    let text = asset(MENU_PATH).text;
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 6, "{text}");
    assert!(lines[0].starts_with("//"));
    assert_eq!(lines[1], "{");
    assert_eq!(lines[5], "}");
    assert_eq!(
        lines[2],
        "  \"fleet\": {\"icon\": \"\u{f0e8}\", \"label\": \"Fleet\"},"
    );
    assert_eq!(
        lines[3],
        "  \"fleet.board\": {\"icon\": \"\u{f0ac}\", \"label\": \"Open the board\", \"action\": \"fleetdeck desktop open\"},"
    );
    assert_eq!(
        lines[4],
        "  \"fleet.crash\": {\"icon\": \"\u{f188}\", \"label\": \"Diagnose the last crash\", \"action\": \"fleetdeck desktop crash --last\"}"
    );
    assert_eq!(menu_rows().len(), 3);
}

#[test]
fn each_hook_is_a_bash_script_that_ends_with_exit_0() {
    for (path, needles) in [
        (
            "hooks/post-boot.d/fleetdeck",
            vec![
                "Fleet Deck: fleetdeck is not on PATH.",
                "fleetdeck service start",
                "fleetdeck omarchy theme-sync",
            ],
        ),
        (
            "hooks/post-update.d/fleetdeck",
            vec![
                "fleetdeck doctor --brief",
                "omarchy-notification-send --app-name \"Fleet Deck\" -u normal \"Fleet Deck\" \"Fleet Deck: doctor found a problem after the Omarchy update\"",
            ],
        ),
        (
            "hooks/theme-set.d/fleetdeck",
            vec!["fleetdeck omarchy theme-sync"],
        ),
    ] {
        let text = asset(path).text;
        assert!(text.starts_with("#!/bin/bash\n"), "{path}");
        assert!(text.ends_with("exit 0\n"), "{path}");
        for needle in needles {
            assert!(text.contains(needle), "{path} lacks `{needle}`");
        }
        assert!(
            !text.contains("wall restore") && !text.contains("join"),
            "{path}: no wall restore and no join (Q2 A, P8)"
        );
    }
}

#[test]
fn each_hook_passes_bash_n() {
    let Ok(version) = Command::new("bash").arg("--version").output() else {
        println!("bash is not installed: the bash -n checks are skipped");
        return;
    };
    assert!(version.status.success());
    for path in [
        "hooks/post-boot.d/fleetdeck",
        "hooks/post-update.d/fleetdeck",
        "hooks/theme-set.d/fleetdeck",
    ] {
        let mut child = Command::new("bash")
            .args(["-n", "/dev/stdin"])
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(asset(path).text.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{path}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The rules of `omarchy-plugin-validate` (source copy 51bfd1d) that the
/// manifest can be checked against without the QML files.
#[test]
fn the_manifest_passes_the_validate_rules() {
    let text = asset("plugins/fleetdeck.fleet/manifest.json").text;
    let m: serde_json::Value = serde_json::from_str(text).unwrap();
    for field in [
        "schemaVersion",
        "id",
        "name",
        "version",
        "kinds",
        "entryPoints",
    ] {
        assert!(m.get(field).is_some(), "missing {field}");
    }
    assert_eq!(m["schemaVersion"], 1);
    let id = m["id"].as_str().unwrap();
    let grammar = id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    assert!(
        grammar && !id.contains("..") && !id.starts_with("omarchy."),
        "{id}"
    );
    assert_eq!(id, "fleetdeck.fleet");
    let allowed = ["bar", "barWidget", "menu", "overlay", "panel", "service"];
    let entry_points = m["entryPoints"].as_object().unwrap();
    for (name, value) in entry_points {
        assert!(allowed.contains(&name.as_str()), "{name}");
        let file = value.as_str().unwrap();
        let safe = !file.is_empty()
            && !file.starts_with('/')
            && !file.split('/').any(|part| part == ".." || part.is_empty());
        assert!(safe, "{file}");
        assert!(
            FILES
                .iter()
                .any(|a| a.base == Base::Omarchy
                    && a.path == format!("plugins/fleetdeck.fleet/{file}")),
            "entry point {file} is not in FILES"
        );
    }
    assert!(
        [
            "left",
            "centerBegin",
            "center",
            "centerEnd",
            "tray",
            "right"
        ]
        .contains(&m["barWidget"]["defaultSection"].as_str().unwrap())
    );
    assert_eq!(m["kinds"], serde_json::json!(["bar-widget", "panel"]));
}
