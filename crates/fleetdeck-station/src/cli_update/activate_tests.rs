//! Tests of the native bind, the rollback copy, the activation and the way
//! back. The fake CLIs are `#!/bin/sh` scripts. The curl of the context
//! writes a file when it runs: the way back must never run it.
#![allow(clippy::expect_used)]

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::Mutex;

use async_trait::async_trait;
use fleetdeck_proto::cli_update::InstallMethod;

use super::*;
use crate::cli_update::stage::{PgidSink, bind_native, probe_env};
use crate::store::cli_update_journal::JournalPhase;

struct NoSink;

#[async_trait]
impl PgidSink for NoSink {
    async fn record(&self, _pgid: i32) {}
}

struct Exdev(Mutex<u32>);

impl Fs for Exdev {
    fn hard_link(&self, _from: &Path, _to: &Path) -> io::Result<()> {
        *self.0.lock().expect("count") += 1;
        Err(io::Error::from_raw_os_error(
            rustix::io::Errno::XDEV.raw_os_error(),
        ))
    }
}

fn write_exec(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    std::fs::write(path, body).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// A fake Claude: `--version` and the `initialize` answer.
fn claude_body(version: &str) -> String {
    format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo '{version} (Claude Code)'; exit 0; fi\n\
         read line\n\
         echo '{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"probe-init\",\"response\":{{\"models\":[]}}}}}}'\n\
         cat >/dev/null\n"
    )
}

/// A fake Codex: `--version` and the two answers.
fn codex_body(version: &str) -> String {
    format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'codex-cli {version}'; exit 0; fi\n\
         read a\nread b\nread c\n\
         echo '{{\"id\":\"probe-init\",\"result\":{{}}}}'\n\
         echo '{{\"id\":\"probe-account\",\"result\":{{}}}}'\n\
         cat >/dev/null\n"
    )
}

fn sha(path: &Path) -> String {
    sha256_file(path).expect("sha")
}

fn probe_environment() -> ChildEnv {
    let mut base = ChildEnv::empty();
    base.set("PATH", "/usr/bin:/bin").expect("PATH");
    probe_env(&base, "mk").expect("env")
}

struct World {
    dir: tempfile::TempDir,
    sink: NoSink,
}

impl World {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("tempdir"),
            sink: NoSink,
        }
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn ctx(&self) -> Ctx<'_> {
        Ctx {
            attempt_id: "a1",
            marker: "mk",
            station_dir: self.dir.path(),
            sink: &self.sink,
            curl: Path::new("/nonexistent/curl-must-not-run"),
        }
    }
}

// ---- native -------------------------------------------------------------

#[test]
fn a_wrong_checksum_runs_nothing_and_changes_nothing() {
    let w = World::new();
    let versions = w.p("share/versions");
    std::fs::create_dir_all(&versions).expect("versions");
    write_exec(&w.p("share/versions/2.1.300"), &claude_body("2.1.300"));
    let link = w.p("bin/claude");
    std::fs::create_dir_all(link.parent().expect("parent")).expect("bin");
    std::os::unix::fs::symlink(versions.join("2.1.300"), &link).expect("link");
    let ran = w.p("ran");
    let staged = native_stage(&versions, "a1");
    write_exec(&staged, &format!("#!/bin/sh\ntouch {}\n", ran.display()));
    let got = bind_native(&staged, &versions, "2.1.301", &"0".repeat(64));
    assert_eq!(got, Err(Failure::Refused(reason::CHECKSUM_MISMATCH)));
    assert!(!ran.exists());
    assert!(!versions.join("2.1.301").exists());
    assert!(!staged.exists());
    assert_eq!(
        std::fs::read_link(&link).expect("link"),
        versions.join("2.1.300")
    );
}

fn native_stage(versions: &Path, id: &str) -> PathBuf {
    crate::cli_update::stage::native_stage_file(versions, id)
}

#[test]
fn s3_refuses_to_replace_a_different_target_and_reuses_the_same_one() {
    let w = World::new();
    let versions = w.p("versions");
    std::fs::create_dir_all(&versions).expect("versions");
    let staged = native_stage(&versions, "a1");
    std::fs::write(&staged, "new binary").expect("write");
    let want = sha(&staged);
    std::fs::write(versions.join("2.1.301"), "other bytes").expect("write");
    assert_eq!(
        bind_native(&staged, &versions, "2.1.301", &want),
        Err(Failure::Refused(reason::CHECKSUM_MISMATCH))
    );
    assert_eq!(
        std::fs::read(versions.join("2.1.301")).expect("read"),
        b"other bytes"
    );
    // The same bytes already there: used, not replaced.
    std::fs::write(versions.join("2.1.301"), "new binary").expect("write");
    std::fs::write(&staged, "new binary").expect("write");
    let placed = bind_native(&staged, &versions, "2.1.301", &want).expect("same");
    assert!(!placed.created);
    assert!(!staged.exists());
    // Nothing there: created, mode 0755.
    std::fs::remove_file(versions.join("2.1.301")).expect("rm");
    std::fs::write(&staged, "new binary").expect("write");
    let placed = bind_native(&staged, &versions, "2.1.301", &want).expect("new");
    assert!(placed.created);
    let mode = std::fs::metadata(&placed.path)
        .expect("meta")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
}

#[test]
fn a1_is_a_hard_link_and_a_copy_on_exdev() {
    let w = World::new();
    let real = w.p("versions/2.1.300");
    write_exec(&real, &claude_body("2.1.300"));
    let want = sha(&real);
    let act = rollback_native(&RealFs, w.dir.path(), "a1", &real, &want).expect("link");
    assert_eq!(
        act.rollback_path,
        rollback_dir(w.dir.path(), "a1").join("claude")
    );
    assert_eq!(act.rollback_sha256.as_deref(), Some(want.as_str()));
    assert_eq!(
        std::fs::metadata(&act.rollback_path).expect("m").ino(),
        std::fs::metadata(&real).expect("m").ino()
    );
    let fs = Exdev(Mutex::new(0));
    let act = rollback_native(&fs, w.dir.path(), "a2", &real, &want).expect("copy");
    assert_eq!(*fs.0.lock().expect("count"), 1);
    assert_ne!(
        std::fs::metadata(&act.rollback_path).expect("m").ino(),
        std::fs::metadata(&real).expect("m").ino()
    );
    assert_eq!(sha(&act.rollback_path), want);
    let bad = rollback_native(&RealFs, w.dir.path(), "a3", &real, &"1".repeat(64));
    assert_eq!(bad, Err(Failure::Refused(reason::CHECKSUM_MISMATCH)));
}

#[test]
fn the_swap_is_one_rename_over_the_link() {
    let w = World::new();
    let (v1, v2) = (w.p("versions/1"), w.p("versions/2"));
    write_exec(&v1, "one");
    write_exec(&v2, "two");
    let link = w.p("bin/claude");
    std::fs::create_dir_all(link.parent().expect("parent")).expect("bin");
    std::os::unix::fs::symlink(&v1, &link).expect("link");
    activate_native(&link, &v2, "a1").expect("swap");
    assert_eq!(std::fs::read_link(&link).expect("link"), v2);
    let left = std::fs::read_dir(link.parent().expect("parent"))
        .expect("dir")
        .count();
    assert_eq!(left, 1, "no .fd-new link stays");
}

#[tokio::test]
async fn restore_copies_the_rollback_copy_back_when_the_vendor_removed_the_version() {
    let w = World::new();
    let versions = w.p("share/versions");
    let from = versions.join("2.1.300");
    write_exec(&from, &claude_body("2.1.300"));
    write_exec(&versions.join("2.1.301"), &claude_body("2.1.301"));
    let want = sha(&from);
    let act = rollback_native(&RealFs, w.dir.path(), "a1", &from, &want).expect("a1");
    let link = w.p("bin/claude");
    std::fs::create_dir_all(link.parent().expect("parent")).expect("bin");
    activate_native(&link, &versions.join("2.1.301"), "a1").expect("a2");
    std::fs::remove_file(&from).expect("vendor cleanup");
    let rec = record(
        &link,
        &versions,
        "2.1.300",
        ProviderKind::Claude,
        InstallMethod::Native,
    );
    let rec = RecoveryRecord {
        rollback_path: Some(act.rollback_path),
        rollback_sha256: Some(want.clone()),
        ..rec
    };
    let tool = Tool::Native {
        link: link.clone(),
        versions: versions.clone(),
        gpgv: None,
    };
    let got = restore(&w.ctx(), &rec, &tool, &probe_environment()).await;
    assert_eq!(got, Ok(()));
    assert_eq!(sha(&from), want);
    assert_eq!(std::fs::read_link(&link).expect("link"), from);
    // A second run changes nothing and passes again.
    assert_eq!(
        restore(&w.ctx(), &rec, &tool, &probe_environment()).await,
        Ok(())
    );
}

#[tokio::test]
async fn restore_fails_when_the_rollback_copy_has_a_wrong_checksum() {
    let w = World::new();
    let versions = w.p("share/versions");
    let link = w.p("bin/claude");
    std::fs::create_dir_all(&versions).expect("versions");
    let copy = w.p("rollback/claude");
    write_exec(&copy, "tampered");
    let rec = RecoveryRecord {
        rollback_path: Some(copy),
        rollback_sha256: Some("2".repeat(64)),
        ..record(
            &link,
            &versions,
            "2.1.300",
            ProviderKind::Claude,
            InstallMethod::Native,
        )
    };
    let tool = Tool::Native {
        link,
        versions,
        gpgv: None,
    };
    let got = restore(&w.ctx(), &rec, &tool, &probe_environment()).await;
    assert_eq!(got, Err(Failure::Failed(ROLLBACK_MISSING)));
}

fn record(
    launcher: &Path,
    dest: &Path,
    from: &str,
    provider: ProviderKind,
    method: InstallMethod,
) -> RecoveryRecord {
    RecoveryRecord {
        provider,
        attempt_id: "a1".into(),
        marker: "mk".into(),
        method,
        phase: JournalPhase::Activating,
        from_version: from.into(),
        target_version: "9.9.9".into(),
        launcher: launcher.to_path_buf(),
        dest: dest.to_path_buf(),
        stage_root: dest.to_path_buf(),
        rollback_path: None,
        rollback_sha256: None,
        moved: Vec::new(),
        installer_pgid: None,
    }
}

// ---- Codex by npm or Bun -------------------------------------------------

struct Codex {
    w: World,
    modules: PathBuf,
    root: PathBuf,
    sm: PathBuf,
    launcher: PathBuf,
    tool: Tool,
    original: String,
    moved: Vec<String>,
}

fn codex_world() -> Codex {
    let w = World::new();
    let modules = w.p("prefix/lib/node_modules");
    let root = w.p("prefix/lib/.fd-cli-update/a1");
    let sm = root.join("p/lib/node_modules");
    write_exec(
        &modules.join("@openai/codex/bin/codex.js"),
        &codex_body("1.0.0"),
    );
    std::fs::write(modules.join("@openai/codex/lib.txt"), "old lib").expect("write");
    std::fs::write(modules.join("other.txt"), "untouched").expect("write");
    write_exec(&sm.join("@openai/codex/bin/codex.js"), &codex_body("2.0.0"));
    std::fs::create_dir_all(sm.join("@openai/codex-linux-x64")).expect("dir");
    std::fs::write(sm.join("@openai/codex-linux-x64/bin.txt"), "platform").expect("write");
    let launcher = w.p("prefix/bin/codex");
    std::fs::create_dir_all(launcher.parent().expect("parent")).expect("bin");
    std::os::unix::fs::symlink(modules.join("@openai/codex/bin/codex.js"), &launcher)
        .expect("link");
    let tool = Tool::Npm {
        prefix: w.p("prefix"),
        npm: w.p("prefix/bin/npm"),
        modules: modules.clone(),
        bin_link: launcher.clone(),
    };
    let original = tree_digest(&modules).expect("digest");
    Codex {
        w,
        modules,
        root,
        sm,
        launcher,
        tool,
        original,
        moved: vec!["@openai/codex".into(), "@openai/codex-linux-x64".into()],
    }
}

fn codex_record(c: &Codex, act: &Activation) -> RecoveryRecord {
    RecoveryRecord {
        rollback_path: Some(act.rollback_path.clone()),
        moved: act.moved.clone(),
        stage_root: c.root.clone(),
        ..record(
            &c.launcher,
            &c.modules,
            "1.0.0",
            ProviderKind::Codex,
            InstallMethod::Npm,
        )
    }
}

#[test]
fn a1_records_the_digest_of_each_original_and_null_for_a_new_package() {
    let c = codex_world();
    let act = rollback_codex(&c.modules, &c.root, &c.moved).expect("a1");
    assert_eq!(act.rollback_path, c.root.join("old"));
    assert_eq!(act.moved.len(), 2);
    assert_eq!(
        act.moved[0].digest,
        Some(tree_digest(&c.modules.join("@openai/codex")).expect("digest"))
    );
    assert_eq!(act.moved[1].digest, None);
    // The originals stay in place until A2.
    assert!(c.modules.join("@openai/codex/lib.txt").exists());
}

#[tokio::test]
async fn a2_then_a_failed_v_restores_the_original_tree() {
    let c = codex_world();
    let act = rollback_codex(&c.modules, &c.root, &c.moved).expect("a1");
    activate_codex(&c.modules, &c.sm, &c.root, &act.moved).expect("a2");
    assert_ne!(tree_digest(&c.modules).expect("digest"), c.original);
    // V: the launcher runs 2.0.0, so a target of 3.0.0 fails.
    let env = probe_environment();
    let v = verify_codex(&c.w.ctx(), &c.launcher, &c.modules, "3.0.0", &env).await;
    assert_eq!(v, Err(Failure::Failed(reason::VERSION_MISMATCH)));
    let ok = verify_codex(&c.w.ctx(), &c.launcher, &c.modules, "2.0.0", &env).await;
    assert_eq!(ok, Ok(()));
    let rec = codex_record(&c, &act);
    assert_eq!(restore(&c.w.ctx(), &rec, &c.tool, &env).await, Ok(()));
    assert_eq!(tree_digest(&c.modules).expect("digest"), c.original);
    assert!(c.root.join("failed").exists());
}

#[tokio::test]
async fn restore_is_idempotent_at_each_crash_point() {
    let env = probe_environment();
    // 0: the original is moved, the stage is not.
    // 1: both are moved.
    // 2: one entry is restored already (restore ran once, then again).
    for point in 0..3 {
        let c = codex_world();
        let act = rollback_codex(&c.modules, &c.root, &c.moved).expect("a1");
        let rec = codex_record(&c, &act);
        let old = c.root.join("old");
        std::fs::create_dir_all(old.join("@openai")).expect("old");
        std::fs::rename(c.modules.join("@openai/codex"), old.join("@openai/codex")).expect("mv");
        if point >= 1 {
            std::fs::rename(c.sm.join("@openai/codex"), c.modules.join("@openai/codex"))
                .expect("mv");
            std::fs::rename(
                c.sm.join("@openai/codex-linux-x64"),
                c.modules.join("@openai/codex-linux-x64"),
            )
            .expect("mv");
        }
        if point == 2 {
            assert_eq!(restore(&c.w.ctx(), &rec, &c.tool, &env).await, Ok(()));
        }
        assert_eq!(
            restore(&c.w.ctx(), &rec, &c.tool, &env).await,
            Ok(()),
            "point {point}"
        );
        assert_eq!(
            tree_digest(&c.modules).expect("digest"),
            c.original,
            "point {point}"
        );
    }
}

#[tokio::test]
async fn restore_uses_no_network() {
    let c = codex_world();
    let marker = c.w.p("curl-ran");
    let curl = c.w.p("fake/curl");
    write_exec(
        &curl,
        &format!("#!/bin/sh\ntouch {}\nexit 1\n", marker.display()),
    );
    let act = rollback_codex(&c.modules, &c.root, &c.moved).expect("a1");
    activate_codex(&c.modules, &c.sm, &c.root, &act.moved).expect("a2");
    let ctx = Ctx {
        curl: &curl,
        ..c.w.ctx()
    };
    let rec = codex_record(&c, &act);
    assert_eq!(
        restore(&ctx, &rec, &c.tool, &probe_environment()).await,
        Ok(())
    );
    assert!(!marker.exists());
}

#[test]
fn stale_stage_folders_are_removed_unless_their_id_is_kept() {
    let w = World::new();
    for id in ["a1", "a2", "a3"] {
        std::fs::create_dir_all(w.p(&format!("stage/{id}"))).expect("dir");
    }
    clean_stale(&w.p("stage"), "", &["a2".to_owned()]);
    assert!(!w.p("stage/a1").exists());
    assert!(w.p("stage/a2").exists());
    assert!(!w.p("stage/a3").exists());
    std::fs::create_dir_all(w.p("v")).expect("v");
    std::fs::write(w.p("v/.fd-stage-b1"), "x").expect("w");
    std::fs::write(w.p("v/2.1.300"), "x").expect("w");
    clean_stale(&w.p("v"), ".fd-stage-", &[]);
    assert!(!w.p("v/.fd-stage-b1").exists());
    assert!(w.p("v/2.1.300").exists());
}
