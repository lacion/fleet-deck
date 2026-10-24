//! Tests of the children, the environments and the staging.
#![allow(clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

use super::*;

#[derive(Default)]
struct Sink(Mutex<Vec<i32>>);

#[async_trait]
impl PgidSink for Sink {
    async fn record(&self, pgid: i32) {
        self.0.lock().expect("sink").push(pgid);
    }
}

fn os(a: &str, b: &str) -> (OsString, OsString) {
    (a.into(), b.into())
}

fn instance() -> ChildEnv {
    let mut env = ChildEnv::empty();
    env.set("PATH", "/usr/bin:/bin").expect("PATH");
    env.set("INSTANCE_ONLY", "x").expect("name");
    env
}

fn station() -> Vec<(OsString, OsString)> {
    vec![
        os("npm_config_prefix", "/evil"),
        os("NPM_CONFIG_GLOBALCONFIG", "/evil"),
        os("BUN_INSTALL_GLOBAL_DIR", "/evil"),
        os("NODE_OPTIONS", "--require /evil.js"),
        os("HOME", "/home/owner"),
        os("SSL_CERT_FILE", "/ca.pem"),
        os("HTTPS_PROXY", "http://proxy:3128"),
        os("NODE_EXTRA_CA_CERTS", "/extra.pem"),
    ]
}

fn names(env: &[(OsString, OsString)]) -> Vec<String> {
    let mut v: Vec<String> = env
        .iter()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn the_npm_argv_is_exact() {
    let got = npm_argv(Path::new("/p/lib/.fd-cli-update/a1"), "0.161.0");
    let r = "/p/lib/.fd-cli-update/a1";
    let want = vec![
        "install".to_owned(),
        "--global".into(),
        format!("--prefix={r}/p"),
        format!("--userconfig={r}/empty.npmrc"),
        format!("--globalconfig={r}/empty.npmrc"),
        format!("--cache={r}/cache"),
        "--registry=https://registry.npmjs.org/".into(),
        "--@openai:registry=https://registry.npmjs.org/".into(),
        "--ignore-scripts".into(),
        "--no-fund".into(),
        "--no-audit".into(),
        "--no-update-notifier".into(),
        "@openai/codex@0.161.0".into(),
    ];
    assert_eq!(got, want);
}

#[test]
fn the_bun_argv_is_exact() {
    assert_eq!(
        bun_argv("0.161.0"),
        [
            "add",
            "--global",
            "--ignore-scripts",
            "--registry=https://registry.npmjs.org/",
            "@openai/codex@0.161.0"
        ]
    );
}

#[test]
fn the_npm_staging_env_holds_only_the_names_of_the_design() {
    let r = Path::new("/m/lib/.fd-cli-update/a1");
    let env = staging_env(r, Path::new("/m/bin"), &instance(), &station(), false, "mk");
    assert_eq!(
        names(&env),
        [
            "FLEETDECK_CLI_UPDATE_ID",
            "HOME",
            "HTTPS_PROXY",
            "LANG",
            "NODE_EXTRA_CA_CERTS",
            "PATH",
            "SSL_CERT_FILE",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME"
        ]
    );
    let get = |k: &str| {
        env.iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.to_string_lossy().into_owned())
    };
    assert_eq!(
        get("HOME").as_deref(),
        Some("/m/lib/.fd-cli-update/a1/home")
    );
    assert_eq!(get("PATH").as_deref(), Some("/m/bin:/usr/bin:/bin"));
    assert_eq!(get("FLEETDECK_CLI_UPDATE_ID").as_deref(), Some("mk"));
}

#[test]
fn the_bun_staging_env_adds_the_four_bun_names() {
    let r = Path::new("/b/install/global/.fd-cli-update/a1");
    let env = staging_env(r, Path::new("/b/bin"), &instance(), &[], true, "mk");
    let got = names(&env);
    for want in [
        "BUN_INSTALL",
        "BUN_INSTALL_BIN",
        "BUN_INSTALL_CACHE_DIR",
        "BUN_INSTALL_GLOBAL_DIR",
    ] {
        assert!(got.iter().any(|n| n == want), "{want} in {got:?}");
    }
    assert!(
        !got.iter()
            .any(|n| n == "NODE_OPTIONS" || n == "INSTANCE_ONLY")
    );
    let dir = env
        .iter()
        .find(|(n, _)| n == "BUN_INSTALL_GLOBAL_DIR")
        .expect("set");
    assert_eq!(
        dir.1,
        OsString::from("/b/install/global/.fd-cli-update/a1/b/install/global")
    );
}

#[test]
fn a_stage_and_a_destination_on_two_devices_are_unsupported() {
    assert!(!same_device(Path::new("/proc"), Path::new("/")).expect("stat"));
    let here = tempfile::tempdir().expect("tempdir");
    assert!(same_device(here.path(), here.path()).expect("stat"));
}

#[tokio::test]
async fn staging_across_two_devices_is_refused_before_a_child_runs() {
    let Ok(shm) = tempfile::tempdir_in("/dev/shm") else {
        return;
    };
    let disk = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir");
    if same_device(shm.path(), disk.path()).unwrap_or(true) {
        return;
    }
    let modules = disk.path().join("lib/node_modules");
    std::fs::create_dir_all(&modules).expect("modules");
    let tool = Tool::Npm {
        prefix: disk.path().to_path_buf(),
        npm: disk.path().join("bin/npm"),
        modules,
        bin_link: disk.path().join("bin/codex"),
    };
    let sink = Sink::default();
    let ctx = Ctx {
        attempt_id: "a1",
        marker: "a1",
        station_dir: disk.path(),
        sink: &sink,
        curl: Path::new("curl"),
    };
    let root = shm.path().join("r");
    let got = stage_codex(&ctx, &tool, &root, "1.0.0", &instance(), &[]).await;
    assert_eq!(got, Err(Failure::Refused(reason::UNSUPPORTED_LAYOUT)));
    assert!(sink.0.lock().expect("sink").is_empty());
}

#[test]
fn the_stage_root_is_next_to_modules() {
    let tool = Tool::Bun {
        bun_install: "/h/.bun".into(),
        bun: "/h/.bun/bin/bun".into(),
        modules: "/h/.bun/install/global/node_modules".into(),
        bin_link: "/h/.bun/bin/codex".into(),
    };
    assert_eq!(
        codex_stage_root(&tool, "a1"),
        Some(PathBuf::from("/h/.bun/install/global/.fd-cli-update/a1"))
    );
    assert_eq!(
        staged_modules(&tool, Path::new("/r")),
        Some(PathBuf::from("/r/b/install/global/node_modules"))
    );
}

#[test]
fn the_download_config_has_the_limits_and_the_output() {
    let c = download_config("https://x/claude", Path::new("/v/.fd-stage-a1"));
    for line in [
        "proto = \"=https\"",
        "no-location",
        "max-time = 600",
        "max-filesize = 536870912",
        "url = \"https://x/claude\"",
        "output = \"/v/.fd-stage-a1\"",
        "create-file-mode = 0600",
    ] {
        assert!(c.lines().any(|l| l == line), "{line} in {c}");
    }
}

#[tokio::test]
async fn a_child_log_is_private_capped_and_the_pgid_is_recorded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sink = Sink::default();
    let ctx = Ctx {
        attempt_id: "a1",
        marker: "stage-test-log",
        station_dir: dir.path(),
        sink: &sink,
        curl: Path::new("curl"),
    };
    let spec = ChildSpec {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            "head -c 400000 /dev/zero | tr '\\0' x; echo err >&2".into(),
        ],
        env: vec![os("PATH", "/usr/bin:/bin")],
        cwd: None,
        stdin: None,
        limit: Duration::from_secs(20),
    };
    assert_eq!(run_child(&ctx, spec).await, Ok(ChildEnd::Exited(0)));
    let log = dir.path().join("cli-update/logs/a1.log");
    let meta = std::fs::metadata(&log).expect("log");
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(meta.len(), LOG_CAP);
    assert_eq!(sink.0.lock().expect("sink").len(), 1);
}

#[tokio::test]
async fn a_child_past_its_limit_is_killed_with_its_group() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sink = Sink::default();
    let ctx = Ctx {
        attempt_id: "a2",
        marker: "stage-test-limit",
        station_dir: dir.path(),
        sink: &sink,
        curl: Path::new("curl"),
    };
    let spec = ChildSpec {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "sleep 60 & wait".into()],
        env: vec![os("PATH", "/usr/bin:/bin")],
        cwd: None,
        stdin: None,
        limit: Duration::from_millis(300),
    };
    assert_eq!(run_child(&ctx, spec).await, Ok(ChildEnd::TimedOut));
}

#[test]
fn only_the_newest_log_files_stay() {
    let dir = tempfile::tempdir().expect("tempdir");
    for i in 0..25 {
        let p = dir.path().join(format!("{i}.log"));
        std::fs::write(&p, "x").expect("write");
        let f = std::fs::File::options().write(true).open(&p).expect("open");
        f.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1000 + i))
            .expect("mtime");
    }
    prune_logs(dir.path(), LOG_KEEP).expect("prune");
    let left = std::fs::read_dir(dir.path()).expect("dir").count();
    assert_eq!(left, LOG_KEEP);
    assert!(!dir.path().join("0.log").exists());
    assert!(dir.path().join("24.log").exists());
}
