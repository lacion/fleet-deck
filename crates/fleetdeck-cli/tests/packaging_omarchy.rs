//! R13 P8 T5 (design `docs/v2/design/omarchy-p8.md` section 5.1): static checks
//! of the Omarchy first-login hook and its XDG autostart entry, and of their
//! place in every release archive. The hook is run only on the two paths that
//! never reach `fleetdeck provision-bootstrap`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const HOOK: &str = "packaging/omarchy/provision-first-login";
const DESKTOP: &str = "packaging/omarchy/fleetdeck-provision.desktop";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn sh_exists() -> bool {
    Command::new("sh")
        .args(["-c", "exit 0"])
        .status()
        .is_ok_and(|s| s.success())
}

/// Runs the hook with `sh`, an empty environment and `env`.
fn run_hook(env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new("sh");
    cmd.arg(root().join(HOOK))
        .env_clear()
        .env("PATH", "/usr/bin:/bin");
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("run the hook")
}

#[test]
fn both_files_are_in_every_release_archive() {
    let dist = read("dist-workspace.toml")
        .parse::<toml::Table>()
        .expect("parse dist-workspace.toml");
    let include: Vec<&str> = dist["dist"]["include"]
        .as_array()
        .expect("include is a list")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for file in [HOOK, DESKTOP] {
        assert!(include.contains(&file), "{file} is not in the include list");
        assert!(root().join(file).is_file(), "{file} is missing");
    }
}

#[test]
fn the_hook_is_a_posix_script_that_runs_only_the_bootstrap() {
    let hook = read(HOOK);
    assert!(
        hook.starts_with("#!/bin/sh\n"),
        "the hook is not a #!/bin/sh script"
    );
    assert!(
        hook.contains("exec /usr/bin/fleetdeck provision-bootstrap --file"),
        "the hook does not exec the bootstrap"
    );
    for word in [
        "sudo", "pacman", "curl", "wget", "yay", "paru", "git ", "npm", "pip",
    ] {
        assert!(!hook.contains(word), "the hook contains `{word}`");
    }
}

#[test]
fn the_hook_passes_the_shell_syntax_check() {
    if !sh_exists() {
        println!("note: sh is not available, the syntax check is skipped");
        return;
    }
    let out = Command::new("sh")
        .arg("-n")
        .arg(root().join(HOOK))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "sh -n failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_login_without_the_provisioning_file_exits_0_with_no_output() {
    if !sh_exists() {
        println!("note: sh is not available, the hook run is skipped");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let out = run_hook(&[("HOME", home.path().to_str().unwrap())]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "stdout: {:?}", out.stdout);
    assert!(out.stderr.is_empty(), "stderr: {:?}", out.stderr);

    // An absolute XDG_CONFIG_HOME without the file behaves the same.
    let xdg = tempfile::tempdir().unwrap();
    let out = run_hook(&[
        ("HOME", home.path().to_str().unwrap()),
        ("XDG_CONFIG_HOME", xdg.path().to_str().unwrap()),
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
}

#[test]
fn a_relative_xdg_config_home_exits_1_with_the_exact_line() {
    if !sh_exists() {
        println!("note: sh is not available, the hook run is skipped");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let out = run_hook(&[
        ("HOME", home.path().to_str().unwrap()),
        ("XDG_CONFIG_HOME", "relative"),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fleetdeck: XDG_CONFIG_HOME must be an absolute path.\n"
    );
}

#[test]
fn an_unset_home_exits_1_with_the_exact_line() {
    if !sh_exists() {
        println!("note: sh is not available, the hook run is skipped");
        return;
    }
    let out = run_hook(&[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fleetdeck: HOME is not set.\n"
    );
}

#[test]
fn the_hook_passes_the_private_config_path_to_the_bootstrap() {
    let hook = read(HOOK);
    assert!(
        hook.contains(r#"exec /usr/bin/fleetdeck provision-bootstrap --file "$file""#),
        "the exec line differs"
    );
    assert!(
        hook.contains("file=$config_home/fleetdeck/provision.toml"),
        "the provisioning file path differs"
    );
}

#[test]
fn the_desktop_entry_has_exactly_the_specified_keys() {
    let desktop = read(DESKTOP);
    let lines: Vec<&str> = desktop
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    assert_eq!(
        lines,
        [
            "[Desktop Entry]",
            "Type=Application",
            "Name=Fleet Deck provisioning",
            "Comment=Finish the provisioning of this Fleet Deck station at the first login.",
            "Exec=/usr/lib/fleetdeck/provision-first-login",
            "NoDisplay=true",
            "Terminal=false",
            "X-GNOME-Autostart-enabled=true",
        ]
    );
}
