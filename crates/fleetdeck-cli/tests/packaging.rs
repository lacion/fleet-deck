//! M3 item 7-5 (PLAN Phase 7 "Packaging", DL-400): static checks of the
//! cargo-dist config, the systemd and launchd units and the station container
//! file, plus the offline smoke test of the built `fleetdeck` binary: every
//! command that a unit or the image starts must be a real command of this
//! binary. No container runtime, systemd or launchd is needed.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn toml_file(rel: &str) -> toml::Table {
    read(rel)
        .parse::<toml::Table>()
        .unwrap_or_else(|e| panic!("parse {rel}: {e}"))
}

/// Runs the built binary with `args` plus `--help`: clap accepts it only for
/// a real command, and it has no side effect.
fn assert_real_command(args: &[&str], from: &str) {
    let out = Command::new(env!("CARGO_BIN_EXE_fleetdeck"))
        .args(args)
        .arg("--help")
        .env_remove("FLEETDECK_DATA_DIR")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{from}: `fleetdeck {}` is not a command: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn str_list<'a>(table: &'a toml::Table, key: &str) -> Vec<&'a str> {
    table[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} is not a list"))
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect()
}

fn workspace_rust_version() -> String {
    let cargo = toml_file("Cargo.toml");
    cargo["workspace"]["package"]["rust-version"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The bun version that CI pins (`bun-version: x` in v2.yml).
fn ci_bun_version() -> String {
    read(".github/workflows/v2.yml")
        .lines()
        .find_map(|l| l.trim().strip_prefix("bun-version:"))
        .unwrap()
        .trim()
        .to_owned()
}

#[test]
fn the_cargo_dist_config_releases_fleetdeck_for_linux_and_macos_with_the_packaging_files() {
    let config = toml_file("dist-workspace.toml");
    assert_eq!(
        str_list(config["workspace"].as_table().unwrap(), "members"),
        ["cargo:."]
    );
    let dist = config["dist"].as_table().unwrap();
    let mut targets = str_list(dist, "targets");
    targets.sort_unstable();
    assert_eq!(
        targets,
        [
            "aarch64-apple-darwin",
            "aarch64-unknown-linux-gnu",
            "x86_64-apple-darwin",
            "x86_64-unknown-linux-gnu",
        ]
    );
    let installers = str_list(dist, "installers");
    assert!(installers.contains(&"shell") && installers.contains(&"homebrew"));
    // The units start ~/.cargo/bin/fleetdeck: the installer must put it there.
    assert_eq!(dist["install-path"].as_str(), Some("CARGO_HOME"));
    for file in str_list(dist, "include") {
        assert!(root().join(file).is_file(), "include {file} is missing");
    }
    // cargo-dist resolves the build setup relative to .github/workflows.
    let setup = dist["github-build-setup"].as_str().unwrap();
    let setup = read(&format!(".github/workflows/{setup}"));
    assert!(
        setup.contains("bun run --cwd web build"),
        "the board is not built"
    );
    assert!(setup.contains(&format!("bun-version: {}", ci_bun_version())));
    // publish = false crates are skipped unless they opt in.
    let cli = toml_file("crates/fleetdeck-cli/Cargo.toml");
    assert_eq!(
        cli["package"]["metadata"]["dist"]["dist"].as_bool(),
        Some(true)
    );
    let bins = cli["bin"].as_array().unwrap();
    assert_eq!(bins[0]["name"].as_str(), Some("fleetdeck"));
}

/// `key=value` lines of a systemd unit (all sections).
fn unit_values(text: &str, key: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix(&format!("{key}=")))
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_systemd_units_start_real_commands_and_the_station_unit_keeps_its_agents() {
    for (unit, args) in [
        (
            "packaging/systemd/fleetdeck-station.service",
            ["station", "serve"],
        ),
        (
            "packaging/systemd/fleetdeck-deck.service",
            ["deck", "serve"],
        ),
    ] {
        let text = read(unit);
        for section in ["[Unit]", "[Service]", "[Install]"] {
            assert!(text.contains(section), "{unit}: no {section}");
        }
        let exec = unit_values(&text, "ExecStart");
        assert_eq!(exec.len(), 1, "{unit}: one ExecStart");
        let mut words = exec[0].split_whitespace();
        assert_eq!(words.next(), Some("%h/.cargo/bin/fleetdeck"), "{unit}");
        let rest: Vec<&str> = words.collect();
        assert_eq!(rest, args, "{unit}");
        assert_real_command(&rest, unit);
        assert_eq!(unit_values(&text, "Restart"), ["on-failure"], "{unit}");
        assert_eq!(unit_values(&text, "UMask"), ["0077"], "{unit}");
        assert_eq!(unit_values(&text, "WantedBy"), ["default.target"], "{unit}");
    }
    // ADR-005 §7: a station stop or upgrade must not kill the shims.
    let station = read("packaging/systemd/fleetdeck-station.service");
    assert_eq!(unit_values(&station, "KillMode"), ["process"]);
}

/// Checks that every element tag of a plist is closed in order, and returns
/// the `<string>` values.
fn plist_strings(text: &str, file: &str) -> Vec<String> {
    let mut stack: Vec<String> = Vec::new();
    let mut strings = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        let after = &rest[start + 1..];
        let end = after
            .find('>')
            .unwrap_or_else(|| panic!("{file}: open tag"));
        let tag = &after[..end];
        let body_start = &after[end + 1..];
        rest = body_start;
        if tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        if let Some(name) = tag.strip_prefix('/') {
            assert_eq!(
                stack.pop().as_deref(),
                Some(name),
                "{file}: </{name}> out of order"
            );
        } else if !tag.ends_with('/') {
            let name = tag.split_whitespace().next().unwrap().to_owned();
            if name == "string" {
                let close = body_start.find("</string>").unwrap();
                strings.push(
                    body_start[..close]
                        .replace("&amp;", "&")
                        .replace("&gt;", ">")
                        .replace("&lt;", "<"),
                );
            }
            stack.push(name);
        }
    }
    assert!(stack.is_empty(), "{file}: unclosed {stack:?}");
    strings
}

#[test]
fn the_launchd_agents_are_well_formed_and_start_real_commands() {
    for (label, args) in [
        ("dev.fleetdeck.station", ["station", "serve"]),
        ("dev.fleetdeck.deck", ["deck", "serve"]),
    ] {
        let file = format!("packaging/launchd/{label}.plist");
        let text = read(&file);
        let strings = plist_strings(&text, &file);
        assert!(text.contains(&format!("<key>Label</key>\n    <string>{label}</string>")));
        // ProgramArguments: /bin/sh -c "<mkdir> && exec <bin> <args> 2>><log>".
        let sh = strings.iter().position(|s| s == "/bin/sh").unwrap();
        assert_eq!(strings[sh + 1], "-c");
        let script = &strings[sh + 2];
        let exec = script.split("exec ").nth(1).unwrap();
        let mut words = exec.split_whitespace();
        assert_eq!(
            words.next(),
            Some("\"$HOME/.cargo/bin/fleetdeck\""),
            "{file}"
        );
        let cmd: Vec<&str> = words.take_while(|w| !w.starts_with("2>")).collect();
        assert_eq!(cmd, args, "{file}");
        assert_real_command(&cmd, &file);
        assert!(text.contains("<key>RunAtLoad</key>\n    <true/>"), "{file}");
        assert!(
            text.contains("<key>Umask</key>\n    <integer>63</integer>"),
            "{file}"
        );
    }
    // ADR-005 §7: a stop of the station agent must not end the shims.
    let station = read("packaging/launchd/dev.fleetdeck.station.plist");
    assert!(station.contains("<key>AbandonProcessGroup</key>\n    <true/>"));
}

/// The JSON-array form of a Containerfile instruction.
fn exec_form(line: &str) -> Vec<String> {
    serde_json::from_str(line).unwrap_or_else(|e| panic!("not an exec form `{line}`: {e}"))
}

#[test]
fn the_container_file_builds_a_non_root_station_image_with_pinned_versions() {
    let file = "packaging/container/Containerfile";
    let text = read(file);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    // Every base image has an explicit tag; Rust and bun match the repository.
    let froms: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.strip_prefix("FROM "))
        .collect();
    assert_eq!(froms.len(), 3);
    for from in &froms {
        let image = from.split_whitespace().next().unwrap();
        let tag = image.rsplit_once(':').map(|(_, t)| t).unwrap_or("");
        assert!(
            !tag.is_empty() && tag != "latest",
            "{file}: unpinned {image}"
        );
    }
    assert!(froms[0].starts_with(&format!("docker.io/oven/bun:{} ", ci_bun_version())));
    assert!(froms[1].starts_with(&format!(
        "docker.io/library/rust:{}-",
        workspace_rust_version()
    )));
    // Every build-context COPY source exists, and every workspace member is copied.
    let mut copied = Vec::new();
    for copy in lines.iter().filter_map(|l| l.strip_prefix("COPY ")) {
        if copy.starts_with("--from=") {
            continue;
        }
        let words: Vec<&str> = copy.split_whitespace().collect();
        for src in &words[..words.len() - 1] {
            assert!(root().join(src).exists(), "{file}: COPY {src} is missing");
            copied.push((*src).to_owned());
        }
    }
    let cargo = toml_file("Cargo.toml");
    for member in str_list(cargo["workspace"].as_table().unwrap(), "members") {
        assert!(
            copied.iter().any(|c| member.starts_with(&format!("{c}/"))),
            "{file}: workspace member {member} is not copied"
        );
    }
    assert!(text.contains("cargo build --locked --release -p fleetdeck-cli"));
    // The runtime stage: non-root user, tini, the station command.
    let runtime_start = lines.iter().rposition(|l| l.starts_with("FROM ")).unwrap();
    let runtime = &lines[runtime_start..];
    assert!(runtime.contains(&"USER fleetdeck"), "{file}: runs as root");
    let entry = runtime
        .iter()
        .find_map(|l| l.strip_prefix("ENTRYPOINT "))
        .unwrap();
    assert_eq!(
        exec_form(entry),
        ["/usr/bin/tini", "--", "/usr/local/bin/fleetdeck"]
    );
    let cmd = exec_form(runtime.iter().find_map(|l| l.strip_prefix("CMD ")).unwrap());
    let cmd: Vec<&str> = cmd.iter().map(String::as_str).collect();
    assert_eq!(cmd, ["station", "serve"]);
    assert_real_command(&cmd, file);
    assert!(text.contains("FLEETDECK_DATA_DIR=/data"));
    // The build context leaves out the large and private directories.
    let ignore = read(".dockerignore");
    for entry in ["target", "**/node_modules", ".git"] {
        assert!(
            ignore.lines().any(|l| l == entry),
            ".dockerignore: no {entry}"
        );
    }
}

#[test]
fn the_offline_built_binary_reports_its_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_fleetdeck"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    let cargo = toml_file("Cargo.toml");
    let version = cargo["workspace"]["package"]["version"].as_str().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("fleetdeck {version}")
    );
}

#[test]
fn the_generated_service_units_start_real_commands() {
    assert_real_command(&["up"], "service both");
    assert_real_command(&["deck", "serve"], "service deck");
    assert_real_command(&["station", "serve"], "service station");
    assert_real_command(&["service", "install"], "service");
}
