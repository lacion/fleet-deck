//! The start recovery after a crash in A2, with the real installer and a
//! real npm layout (pre-PR review finding 1): the way back uses the install
//! that the journal row names, also when the launcher is dangling and when
//! another Codex comes first on `PATH`. The fake CLIs are `#!/bin/sh`
//! scripts.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use fleetdeck_testkit::Det;

use super::super::activate::{journal_tool, rollback_codex};
use super::super::run::tests::{CODEX_FROM, CODEX_TARGET, open_store, test_config};
use super::super::service::RealInstaller;
use super::super::tree_digest::tree_digest;
use super::*;
use crate::store::cli_update_journal::{MovedEntry, NewJournal};

fn write_exec(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A fake Codex: `--version` and the two answers of the handshake. Only
/// shell builtins: `PATH` holds no system folder, so no system Codex can
/// answer a detection.
fn codex_body(version: &str) -> String {
    format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'codex-cli {version}'; exit 0; fi\n\
         read a\nread b\nread c\n\
         echo '{{\"id\":\"probe-init\",\"result\":{{}}}}'\n\
         echo '{{\"id\":\"probe-account\",\"result\":{{}}}}'\n\
         while read -r x; do :; done\n"
    )
}

/// An npm prefix with Codex `version`: the package and `bin/codex`.
fn npm_prefix(prefix: &Path, version: &str) {
    let modules = prefix.join("lib/node_modules");
    write_exec(
        &modules.join("@openai/codex/bin/codex.js"),
        &codex_body(version),
    );
    std::fs::write(modules.join("@openai/codex/lib.txt"), version).unwrap();
    write_exec(&prefix.join("bin/npm"), "#!/bin/sh\nexit 0\n");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::os::unix::fs::symlink(
        "../lib/node_modules/@openai/codex/bin/codex.js",
        prefix.join("bin/codex"),
    )
    .unwrap();
}

struct World {
    dir: tempfile::TempDir,
    det: Det,
    store: Store,
    modules: PathBuf,
    root: PathBuf,
    sm: PathBuf,
    shadow: PathBuf,
    original: String,
    moved: Vec<MovedEntry>,
}

/// The journal row of attempt `rc1` in the phase `activating`, as A1 left
/// it, for the npm prefix `home/npm`. A second npm prefix `home/shadow`
/// holds another Codex.
async fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let det = Det::new();
    let store = open_store(dir.path(), &det).await;
    let prefix = dir.path().join("home/npm");
    npm_prefix(&prefix, CODEX_FROM);
    let shadow = dir.path().join("home/shadow");
    npm_prefix(&shadow, "9.9.9");
    let modules = prefix.join("lib/node_modules");
    let root = prefix.join("lib/.fd-cli-update/rc1");
    let sm = root.join("p/lib/node_modules");
    write_exec(
        &sm.join("@openai/codex/bin/codex.js"),
        &codex_body(CODEX_TARGET),
    );
    std::fs::create_dir_all(sm.join("@openai/codex-linux-x64")).unwrap();
    std::fs::write(sm.join("@openai/codex-linux-x64/bin.txt"), "platform").unwrap();
    let original = tree_digest(&modules).unwrap();
    let a = new_attempt(
        "rc1",
        ProviderKind::Codex,
        UpdateTrigger::Owner,
        InstallMethod::Npm,
        "latest",
        Some(CODEX_FROM),
        Some(CODEX_TARGET),
    );
    let j = NewJournal {
        provider: ProviderKind::Codex,
        method: InstallMethod::Npm,
        from_version: CODEX_FROM.to_owned(),
        target_version: CODEX_TARGET.to_owned(),
        launcher: prefix.join("bin/codex"),
        dest: modules.clone(),
        stage_root: root.clone(),
        rollback_sha256: None,
    };
    store.cli_update_begin(a, j).await.unwrap();
    let names = [
        "@openai/codex".to_owned(),
        "@openai/codex-linux-x64".to_owned(),
    ];
    let act = rollback_codex(&modules, &root, &names).unwrap();
    let moved = act.moved.clone();
    store
        .cli_update_journal_activate(ProviderKind::Codex, act)
        .await
        .unwrap();
    World {
        dir,
        det,
        store,
        modules,
        root,
        sm,
        shadow,
        original,
        moved,
    }
}

impl World {
    /// The first `n` renames of A2, in the order of `activate_codex`.
    fn renames(&self, n: usize) {
        let mut done = 0;
        for e in &self.moved {
            let at = self.modules.join(&e.name);
            if at.exists() && done < n {
                let old = self.root.join("old").join(&e.name);
                std::fs::create_dir_all(old.parent().unwrap()).unwrap();
                std::fs::rename(&at, &old).unwrap();
                done += 1;
            }
            if done < n {
                std::fs::create_dir_all(at.parent().unwrap()).unwrap();
                std::fs::rename(self.sm.join(&e.name), &at).unwrap();
                done += 1;
            }
        }
    }

    /// The environment of the instance: `shadowed` puts the other Codex
    /// first on `PATH`.
    fn env(&self, shadowed: bool) -> ChildEnv {
        let home = self.dir.path().join("home");
        let mut path = Vec::new();
        if shadowed {
            path.push(self.shadow.join("bin"));
        }
        path.push(home.join("npm/bin"));
        let mut env = ChildEnv::empty();
        env.set("HOME", &home).unwrap();
        env.set("PATH", std::env::join_paths(path).unwrap())
            .unwrap();
        env
    }

    async fn recover(&self, env: ChildEnv) {
        let installer = RealInstaller::new();
        let targets = [ProviderTarget {
            kind: ProviderKind::Codex,
            binary: "codex".into(),
            env,
        }];
        let station = self.dir.path().join("station");
        recover(&Recovery {
            store: &self.store,
            installer: &installer,
            targets: &targets,
            station_dir: &station,
            curl: Path::new("/nonexistent/curl-must-not-run"),
            ids: &self.det.ids(),
            cfg: &test_config(),
            limit: Duration::from_secs(120),
        })
        .await;
    }
}

#[tokio::test]
async fn the_start_recovery_restores_after_each_rename_of_a2() {
    // 0: none; 1: the original is in R/old and bin/codex dangles; 2: the
    // new package is in place; 3: the platform package too.
    for shadowed in [false, true] {
        for point in 0..=3 {
            let w = world().await;
            let shadow_before = tree_digest(&w.shadow.join("lib/node_modules")).unwrap();
            w.renames(point);
            let env = w.env(shadowed);
            if point == 1 && !shadowed {
                // The launcher dangles: a detection cannot find the install.
                assert!(detect(ProviderKind::Codex, Path::new("codex"), &env).is_err());
            }
            w.recover(env).await;
            let at = format!("point {point}, shadowed {shadowed}");
            assert_eq!(
                w.store.cli_update_journals().await.unwrap().len(),
                0,
                "{at}"
            );
            let last = w
                .store
                .cli_update_last(ProviderKind::Codex)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(last.outcome, UpdateOutcome::RolledBack, "{at}");
            assert_eq!(last.trigger, UpdateTrigger::Recovery, "{at}");
            assert!(
                w.store.cli_update_quarantines().await.unwrap().is_empty(),
                "{at}"
            );
            assert_eq!(tree_digest(&w.modules).unwrap(), w.original, "{at}");
            assert_eq!(
                tree_digest(&w.shadow.join("lib/node_modules")).unwrap(),
                shadow_before,
                "{at}"
            );
        }
    }
}

#[test]
fn the_journal_names_the_install_without_a_launcher() {
    let rec = |method, dest: &str| RecoveryRecord {
        provider: ProviderKind::Codex,
        attempt_id: "a1".into(),
        marker: "a1".into(),
        method,
        phase: JournalPhase::Activating,
        from_version: "1.0.0".into(),
        target_version: "2.0.0".into(),
        launcher: "/gone".into(),
        dest: dest.into(),
        stage_root: "/s".into(),
        rollback_path: None,
        rollback_sha256: None,
        moved: Vec::new(),
        installer_pgid: None,
    };
    assert_eq!(
        journal_tool(&rec(
            InstallMethod::Native,
            "/h/.local/share/claude/versions/2.1.310"
        )),
        Some(Tool::Native {
            link: "/h/.local/bin/claude".into(),
            versions: "/h/.local/share/claude/versions".into(),
            gpgv: None,
        })
    );
    assert_eq!(
        journal_tool(&rec(InstallMethod::Npm, "/p/lib/node_modules")),
        Some(Tool::Npm {
            prefix: "/p".into(),
            npm: "/p/bin/npm".into(),
            modules: "/p/lib/node_modules".into(),
            bin_link: "/p/bin/codex".into(),
        })
    );
    assert_eq!(
        journal_tool(&rec(InstallMethod::Bun, "/b/install/global/node_modules")),
        Some(Tool::Bun {
            bun_install: "/b".into(),
            bun: "/b/bin/bun".into(),
            modules: "/b/install/global/node_modules".into(),
            bin_link: "/b/bin/codex".into(),
        })
    );
    assert_eq!(journal_tool(&rec(InstallMethod::Npm, "/x")), None);
    assert_eq!(
        journal_tool(&rec(InstallMethod::Native, "/x/2.1.310")),
        None
    );
    assert_eq!(journal_tool(&rec(InstallMethod::Homebrew, "/x")), None);
}
