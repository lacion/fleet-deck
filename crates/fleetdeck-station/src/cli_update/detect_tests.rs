#![allow(clippy::unwrap_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::*;

fn file(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn link(target: impl AsRef<Path>, at: &Path) {
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target.as_ref(), at).unwrap();
}

fn env(home: &Path, path: &[&Path]) -> ChildEnv {
    let mut env = ChildEnv::empty();
    env.set("HOME", home).unwrap();
    let joined = std::env::join_paths(path).unwrap();
    env.set("PATH", joined).unwrap();
    env
}

/// A native Claude: `home/.local/bin/claude` into `versions/2.1.309`, and a
/// `gpgv` on the PATH.
fn native(home: &Path) -> PathBuf {
    let version = home.join(".local/share/claude/versions/2.1.309");
    file(&version);
    link(&version, &home.join(".local/bin/claude"));
    file(&home.join("tools/gpgv"));
    home.join(".local/bin")
}

fn pbuf(items: &[&str]) -> Vec<PathBuf> {
    items.iter().map(PathBuf::from).collect()
}

fn no_links(_: &Path) -> Option<PathBuf> {
    None
}

#[test]
fn a_native_claude_with_the_managed_link_is_native() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let bin = native(home);
    let install = detect(
        ProviderKind::Claude,
        Path::new("claude"),
        &env(home, &[&bin, &home.join("tools")]),
    )
    .unwrap();
    assert_eq!(install.method, InstallMethod::Native);
    assert_eq!(install.blocked, None);
    assert_eq!(install.launcher, home.join(".local/bin/claude"));
    assert_eq!(
        install.real,
        home.join(".local/share/claude/versions/2.1.309")
    );
    let Tool::Native {
        link,
        versions,
        gpgv,
    } = install.tool
    else {
        panic!("not native");
    };
    assert_eq!(link, home.join(".local/bin/claude"));
    assert_eq!(versions, home.join(".local/share/claude/versions"));
    assert_eq!(gpgv, Some(home.join("tools/gpgv")));
}

#[test]
fn a_bun_global_codex_is_bun_and_the_shadowed_npm_copy_is_not_chosen() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let bun = home.join(".bun");
    file(&bun.join("install/global/node_modules/@openai/codex/bin/codex.js"));
    link(
        "../install/global/node_modules/@openai/codex/bin/codex.js",
        &bun.join("bin/codex"),
    );
    file(&bun.join("bin/bun"));
    // The shadowed npm copy, later on the PATH.
    let nvm = home.join(".nvm/node/bin");
    file(&nvm.join("npm"));
    file(&home.join(".nvm/node/lib/node_modules/@openai/codex/bin/codex.js"));
    link(
        "../lib/node_modules/@openai/codex/bin/codex.js",
        &nvm.join("codex"),
    );
    let mut e = env(home, &[&bun.join("bin"), &nvm]);
    // A BUN_INSTALL that names another folder changes nothing.
    e.set("BUN_INSTALL", home.join("elsewhere")).unwrap();
    let install = detect(ProviderKind::Codex, Path::new("codex"), &e).unwrap();
    assert_eq!(install.method, InstallMethod::Bun);
    assert_eq!(install.blocked, None);
    let Tool::Bun {
        bun_install,
        bun: bun_bin,
        modules,
        bin_link,
    } = install.tool
    else {
        panic!("not bun");
    };
    assert_eq!(bun_install, bun);
    assert_eq!(bun_bin, bun.join("bin/bun"));
    assert_eq!(modules, bun.join("install/global/node_modules"));
    assert_eq!(bin_link, bun.join("bin/codex"));
    assert!(install.chain.contains(&bin_link));
}

#[test]
fn an_npm_global_codex_with_npm_is_npm() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let prefix = home.join("nvm/node");
    file(&prefix.join("lib/node_modules/@openai/codex/bin/codex.js"));
    link(
        "../lib/node_modules/@openai/codex/bin/codex.js",
        &prefix.join("bin/codex"),
    );
    file(&prefix.join("bin/npm"));
    let install = detect(
        ProviderKind::Codex,
        Path::new("codex"),
        &env(home, &[&prefix.join("bin")]),
    )
    .unwrap();
    assert_eq!(install.method, InstallMethod::Npm);
    let Tool::Npm {
        prefix: p,
        npm,
        modules,
        bin_link,
    } = install.tool
    else {
        panic!("not npm");
    };
    assert_eq!(p, prefix);
    assert_eq!(npm, prefix.join("bin/npm"));
    assert_eq!(modules, prefix.join("lib/node_modules"));
    assert_eq!(bin_link, prefix.join("bin/codex"));
}

#[test]
fn an_outer_alias_before_the_managed_link_is_still_native() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    native(home);
    link(
        home.join(".local/bin/claude"),
        &home.join("outer/bin/claude"),
    );
    let install = detect(
        ProviderKind::Claude,
        Path::new("claude"),
        &env(home, &[&home.join("outer/bin"), &home.join("tools")]),
    )
    .unwrap();
    assert_eq!(install.method, InstallMethod::Native);
    assert_eq!(install.launcher, home.join("outer/bin/claude"));
    assert_eq!(install.chain.len(), 3);
}

#[test]
fn a_custom_link_into_versions_is_a_custom_launcher() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    native(home);
    link(
        home.join(".local/share/claude/versions/2.1.309"),
        &home.join("custom/bin/claude"),
    );
    let install = detect(
        ProviderKind::Claude,
        Path::new("claude"),
        &env(home, &[&home.join("custom/bin")]),
    )
    .unwrap();
    assert_eq!(install.method, InstallMethod::Unknown);
    assert_eq!(install.tool, Tool::None);
    assert_eq!(install.blocked, Some(reason::CUSTOM_LAUNCHER));
}

#[test]
fn an_absolute_version_file_is_a_custom_launcher() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    native(home);
    let version = home.join(".local/share/claude/versions/2.1.309");
    let install = detect(ProviderKind::Claude, &version, &env(home, &[])).unwrap();
    assert_eq!(install.blocked, Some(reason::CUSTOM_LAUNCHER));
    assert_eq!(install.method, InstallMethod::Unknown);
}

#[test]
fn the_managed_link_through_another_link_is_a_custom_launcher() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let version = home.join(".local/share/claude/versions/2.1.309");
    file(&version);
    link(&version, &home.join(".local/bin/other"));
    link(
        home.join(".local/bin/other"),
        &home.join(".local/bin/claude"),
    );
    let install = detect(
        ProviderKind::Claude,
        Path::new("claude"),
        &env(home, &[&home.join(".local/bin")]),
    )
    .unwrap();
    assert_eq!(install.blocked, Some(reason::CUSTOM_LAUNCHER));
}

#[test]
fn claude_under_npm_and_bun_is_report_only() {
    let home = Some(Path::new("/home/u"));
    let npm = pbuf(&["/p/lib/node_modules/@anthropic-ai/claude-code/cli.js"]);
    let (method, tool, blocked) = classify(ProviderKind::Claude, &npm, home, &no_links, &|_| true);
    assert_eq!(
        (method, tool, blocked),
        (
            InstallMethod::Npm,
            Tool::None,
            Some(reason::REPORT_ONLY_METHOD)
        )
    );
    let bun = pbuf(&["/home/u/.bun/install/global/node_modules/@anthropic-ai/claude-code/cli.js"]);
    let (method, tool, blocked) = classify(ProviderKind::Claude, &bun, home, &no_links, &|_| true);
    assert_eq!(
        (method, tool, blocked),
        (
            InstallMethod::Bun,
            Tool::None,
            Some(reason::REPORT_ONLY_METHOD)
        )
    );
}

#[test]
fn codex_npm_without_npm_or_without_its_link_is_blocked() {
    let real = "/p/lib/node_modules/@openai/codex/bin/codex.js";
    let with_link = pbuf(&["/p/bin/codex", real]);
    let (method, tool, blocked) =
        classify(ProviderKind::Codex, &with_link, None, &no_links, &|_| false);
    assert_eq!(method, InstallMethod::Npm);
    assert_eq!(tool, Tool::None);
    assert_eq!(blocked, Some(reason::TOOL_MISSING));
    let without_link = pbuf(&["/elsewhere/codex", real]);
    let (method, tool, blocked) =
        classify(ProviderKind::Codex, &without_link, None, &no_links, &|_| {
            true
        });
    assert_eq!(method, InstallMethod::Npm);
    assert_eq!(tool, Tool::None);
    assert_eq!(blocked, Some(reason::CUSTOM_LAUNCHER));
    // Bun the same.
    let bun = "/b/install/global/node_modules/@openai/codex/bin/codex.js";
    let (method, _, blocked) = classify(
        ProviderKind::Codex,
        &pbuf(&["/b/bin/codex", bun]),
        None,
        &no_links,
        &|_| false,
    );
    assert_eq!(
        (method, blocked),
        (InstallMethod::Bun, Some(reason::TOOL_MISSING))
    );
    let (method, _, blocked) = classify(
        ProviderKind::Codex,
        &pbuf(&["/x/codex", bun]),
        None,
        &no_links,
        &|_| true,
    );
    assert_eq!(
        (method, blocked),
        (InstallMethod::Bun, Some(reason::CUSTOM_LAUNCHER))
    );
}

#[test]
fn homebrew_system_and_unknown_paths_are_report_only() {
    let cases = [
        ("/opt/homebrew/bin/claude", InstallMethod::Homebrew),
        (
            "/usr/local/Caskroom/claude-code/2.1.309/claude",
            InstallMethod::Homebrew,
        ),
        (
            "/home/linuxbrew/.linuxbrew/Cellar/codex/1/bin/codex",
            InstallMethod::Homebrew,
        ),
        ("/usr/bin/claude", InstallMethod::SystemPackage),
        ("/opt/claude/bin/claude", InstallMethod::SystemPackage),
        ("/srv/tools/codex", InstallMethod::Unknown),
        ("/home/u/.codex/bin/codex", InstallMethod::Unknown),
    ];
    for (path, want) in cases {
        let (method, tool, blocked) = classify(
            ProviderKind::Claude,
            &pbuf(&[path]),
            Some(Path::new("/home/u")),
            &no_links,
            &|_| true,
        );
        assert_eq!(method, want, "{path}");
        assert_eq!(tool, Tool::None);
        assert_eq!(blocked, Some(reason::REPORT_ONLY_METHOD));
    }
}

#[test]
fn paths_are_compared_by_components_not_by_text() {
    // `/usr2` and `/opt/homebrew2` are not system or Homebrew paths.
    for path in ["/usr2/bin/claude", "/opt/homebrew2/bin/claude"] {
        let (method, _, _) = classify(
            ProviderKind::Claude,
            &pbuf(&[path]),
            None,
            &no_links,
            &|_| true,
        );
        let want = if path.starts_with("/opt") {
            InstallMethod::SystemPackage
        } else {
            InstallMethod::Unknown
        };
        assert_eq!(method, want, "{path}");
    }
}

#[test]
fn a_native_install_without_gpgv_is_blocked() {
    let home = Path::new("/home/u");
    let link_path = home.join(".local/bin/claude");
    let real = home.join(".local/share/claude/versions/2.1.309");
    let chain = vec![link_path.clone(), real.clone()];
    let hop = |p: &Path| (p == link_path).then(|| real.clone());
    let (method, tool, blocked) =
        classify(ProviderKind::Claude, &chain, Some(home), &hop, &|_| false);
    assert_eq!(method, InstallMethod::Native);
    assert!(matches!(tool, Tool::Native { gpgv: None, .. }));
    assert_eq!(blocked, Some(reason::GPGV_MISSING));
    // With one of the fixed places present.
    let fixed = |p: &Path| p == Path::new("/usr/bin/gpgv");
    let (_, tool, blocked) = classify(ProviderKind::Claude, &chain, Some(home), &hop, &fixed);
    assert_eq!(blocked, None);
    assert!(matches!(tool, Tool::Native { gpgv: Some(g), .. } if g == Path::new("/usr/bin/gpgv")));
}

#[test]
fn a_chain_of_eight_hops_is_followed_and_nine_are_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let target = root.join("target/tool");
    file(&target);
    let mut previous = target;
    for n in 1..=9 {
        let next = root.join(format!("l{n}/tool"));
        link(&previous, &next);
        previous = next;
        if n == 8 {
            let ok = detect(ProviderKind::Codex, &previous, &env(root, &[])).unwrap();
            assert_eq!(ok.chain.len(), 9);
        }
    }
    assert_eq!(
        detect(ProviderKind::Codex, &previous, &env(root, &[])),
        Err(reason::NOT_FOUND)
    );
}

#[test]
fn a_missing_launcher_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        detect(
            ProviderKind::Claude,
            Path::new("claude"),
            &env(dir.path(), &[dir.path()])
        ),
        Err(reason::NOT_FOUND)
    );
}
