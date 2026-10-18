//! Tests of the Omarchy service: the rig (a fake executable as the program),
//! the argv and the environment of each action, the order of the checks and
//! the single action slot.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::childenv::ChildEnv;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::omarchy_admin::{ActOutcome, OmarchyActResult, OmarchyStatus};
use serde_json::{Value, json};

use super::{Config, Limits, OmarchyAdmin, SessionEnv};

/// A status JSON of a clean, current install.
pub(super) const STATUS_OK: &str = r#"{"installed":true,"files":[{"path":"a","state":"ok"},{"path":"b","state":"ok"},{"path":"c","state":"changed"}],"plugin_enabled":true,"summary":"on","hyprland_line":"ok","record_version":2,"outdated":false,"screensaver":{"enabled":true,"file_restored":null,"blocked":null}}"#;

/// The same install with an outdated integration.
pub(super) const STATUS_OUTDATED: &str = r#"{"installed":true,"files":[],"plugin_enabled":true,"summary":"on","hyprland_line":"ok","record_version":1,"outdated":true,"screensaver":null}"#;

/// One run of the fake program: its argv and its environment.
#[derive(Debug)]
pub(super) struct Run {
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// A temp folder with a fake Omarchy, a fake program and a service on them.
pub(super) struct Rig {
    pub dir: tempfile::TempDir,
    pub admin: Arc<OmarchyAdmin>,
}

impl Rig {
    /// `body` is the shell text that the fake runs for every command except
    /// `omarchy status --json`.
    pub fn new(body: &str) -> Self {
        Self::with(body, |_| {})
    }

    pub fn with(body: &str, tweak: impl FnOnce(&mut Config)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for sub in ["omarchy/bin", "rec", "station", "config/fleetdeck"] {
            fs::create_dir_all(root.join(sub)).unwrap();
        }
        let bin = root.join("omarchy/bin/omarchy");
        fs::write(&bin, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(root.join("omarchy/version"), "3.1\n").unwrap();
        fs::write(root.join("rec/status.json"), STATUS_OK).unwrap();
        let program = root.join("fake.sh");
        fs::write(&program, script(&root.join("rec"), body)).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = Config {
            program,
            data_dir: Some(root.join("data")),
            station_dir: root.join("station"),
            config_home: Some(root.join("config")),
            env: vec![
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("KEEP".into(), "yes".into()),
            ],
            omarchy_env: ChildEnv::scrubbed([(
                OsString::from("OMARCHY_PATH"),
                root.join("omarchy").into_os_string(),
            )]),
            session: SessionEnv::Fixed(vec![
                ("XDG_RUNTIME_DIR", "/run/user/fake".into()),
                ("WAYLAND_DISPLAY", "wayland-fake".into()),
                ("HYPRLAND_INSTANCE_SIGNATURE", "sig-fake".into()),
            ]),
            limits: Limits {
                install: Duration::from_secs(10),
                uninstall: Duration::from_secs(10),
                screensaver: Duration::from_secs(10),
                status: Duration::from_secs(10),
                tick: Duration::from_secs(10),
                retry: Duration::from_secs(10),
            },
        };
        tweak(&mut cfg);
        Self {
            dir,
            admin: Arc::new(OmarchyAdmin::new(cfg)),
        }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    pub fn set_status(&self, json: &str) {
        fs::write(self.path("rec/status.json"), json).unwrap();
    }

    /// Writes the provisioning state file.
    pub fn write_state(&self, phase: &str, error: Option<&str>) {
        let text = json!({
            "version": 1, "config_sha256": "x", "station_fingerprint": null,
            "phase": phase, "error": error,
        });
        fs::write(self.path("station/provision-state.json"), text.to_string()).unwrap();
    }

    /// Every run of the fake so far, oldest first.
    pub fn runs(&self) -> Vec<Run> {
        runs(&self.path("rec"))
    }

    /// The runs without the `omarchy status --json` runs.
    pub fn action_runs(&self) -> Vec<Run> {
        self.runs()
            .into_iter()
            .filter(|r| {
                !r.args
                    .ends_with(&["status".to_owned(), "--json".to_owned()])
            })
            .collect()
    }
}

/// The text of the fake program. Every run is logged to `rec/log`.
fn script(rec: &Path, body: &str) -> String {
    format!(
        r#"#!/bin/sh
REC='{rec}'
{{
  echo RUN
  for a in "$@"; do echo "A:$a"; done
  env | sort | sed 's/^/E:/'
}} >> "$REC/log"
case "$*" in
  *"omarchy status --json"*) cat "$REC/status.json"; exit "$(cat "$REC/status.exit" 2>/dev/null || echo 0)";;
esac
{body}
"#,
        rec = rec.display()
    )
}

fn runs(rec: &Path) -> Vec<Run> {
    let Ok(log) = fs::read_to_string(rec.join("log")) else {
        return Vec::new();
    };
    let mut out: Vec<Run> = Vec::new();
    for line in log.lines() {
        if line == "RUN" {
            out.push(Run {
                args: Vec::new(),
                env: BTreeMap::new(),
            });
        } else if let Some(arg) = line.strip_prefix("A:") {
            out.last_mut().unwrap().args.push(arg.to_owned());
        } else if let Some(var) = line.strip_prefix("E:")
            && let Some((k, v)) = var.split_once('=')
        {
            out.last_mut()
                .unwrap()
                .env
                .insert(k.to_owned(), v.to_owned());
        }
    }
    out
}

pub(super) async fn act(admin: &Arc<OmarchyAdmin>, action: &str, elevated: bool) -> Outcome {
    admin
        .act_call(json!({"action": action, "elevated": elevated}))
        .await
}

/// The `omarchy.act` answer of an `ok` outcome.
pub(super) fn result(outcome: Outcome) -> OmarchyActResult {
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).unwrap(),
        Outcome::Err(e) => panic!("wire error: {e}"),
    }
}

/// The `omarchy.status` answer of an `ok` outcome.
pub(super) fn status(outcome: Outcome) -> OmarchyStatus {
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).unwrap(),
        Outcome::Err(e) => panic!("wire error: {e}"),
    }
}

/// The code and the text of an `err` outcome.
pub(super) fn wire_error(outcome: Outcome) -> (String, String) {
    match outcome {
        Outcome::Err(e) => (e.code, e.message),
        Outcome::Ok(v) => panic!("unexpected answer: {v}"),
    }
}

/// Waits until `check` is true, at most 5 s.
pub(super) async fn until(mut check: impl FnMut() -> bool) {
    for _ in 0..250 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the condition did not become true");
}

fn tail(run: &Run, skip_data_dir: bool) -> Vec<&str> {
    let skip = if skip_data_dir { 2 } else { 0 };
    run.args.iter().skip(skip).map(String::as_str).collect()
}

#[tokio::test]
async fn omarchy_each_action_runs_its_fixed_argv_with_the_data_dir() {
    let cases: [(&str, &[&str]); 5] = [
        ("install", &["omarchy", "install", "--yes"]),
        ("uninstall", &["omarchy", "uninstall"]),
        ("screensaver_on", &["omarchy", "screensaver", "on"]),
        ("screensaver_off", &["omarchy", "screensaver", "off"]),
        (
            "screensaver_rebaseline",
            &["omarchy", "screensaver", "rebaseline"],
        ),
    ];
    for (action, expected) in cases {
        let rig = Rig::new("exit 0");
        let got = result(act(&rig.admin, action, true).await);
        assert_eq!(got.outcome, ActOutcome::Done, "{action}");
        assert_eq!(got.code, None, "{action}");
        assert_eq!(got.message, None, "{action}");
        assert_eq!(got.status.running, None, "{action}");
        let runs = rig.action_runs();
        assert_eq!(runs.len(), 1, "{action}: {runs:?}");
        let data = rig.path("data").to_string_lossy().into_owned();
        assert_eq!(runs[0].args[0], "--data-dir", "{action}");
        assert_eq!(runs[0].args[1], data, "{action}");
        assert_eq!(tail(&runs[0], true), expected, "{action}");
    }
}

#[tokio::test]
async fn omarchy_a_station_without_a_data_dir_passes_no_flag() {
    let rig = Rig::with("exit 0", |cfg| cfg.data_dir = None);
    result(act(&rig.admin, "install", true).await);
    let runs = rig.runs();
    assert!(!runs.is_empty());
    for run in &runs {
        assert_eq!(run.args[0], "omarchy", "{run:?}");
        assert!(!run.args.contains(&"--data-dir".to_owned()), "{run:?}");
    }
}

#[tokio::test]
async fn omarchy_the_child_gets_the_process_environment_and_three_session_variables() {
    let rig = Rig::new("exit 0");
    result(act(&rig.admin, "install", true).await);
    for run in rig.runs() {
        // The shell adds PWD, SHLVL and `_`; they are not ours.
        let env: BTreeMap<&str, &str> = run
            .env
            .iter()
            .filter(|(k, _)| !["PWD", "SHLVL", "_", "OLDPWD"].contains(&k.as_str()))
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let want: BTreeMap<&str, &str> = BTreeMap::from([
            ("PATH", "/usr/bin:/bin"),
            ("KEEP", "yes"),
            ("XDG_RUNTIME_DIR", "/run/user/fake"),
            ("WAYLAND_DISPLAY", "wayland-fake"),
            ("HYPRLAND_INSTANCE_SIGNATURE", "sig-fake"),
        ]);
        assert_eq!(env, want, "{run:?}");
    }
}

#[tokio::test]
async fn omarchy_no_session_variable_is_added_without_a_found_desktop() {
    let rig = Rig::with("exit 0", |cfg| cfg.session = SessionEnv::Fixed(Vec::new()));
    result(act(&rig.admin, "install", true).await);
    for run in rig.runs() {
        assert!(!run.env.contains_key("WAYLAND_DISPLAY"), "{run:?}");
        assert_eq!(
            run.env.get("PATH").map(String::as_str),
            Some("/usr/bin:/bin")
        );
    }
}

#[tokio::test]
async fn omarchy_elevated_false_is_refused_before_anything_runs() {
    let rig = Rig::new("exit 0");
    let (code, text) = wire_error(act(&rig.admin, "install", false).await);
    assert_eq!(code, "elevation_required");
    assert_eq!(text, "This action needs the owner's elevation.");
    assert!(rig.runs().is_empty());
    assert_eq!(rig.admin.running(), None);
}

#[tokio::test]
async fn omarchy_bad_params_are_invalid_requests_with_a_fixed_text() {
    let rig = Rig::new("exit 0");
    for params in [
        json!({"action": "install", "elevated": true, "extra": 1}),
        json!({"action": "reboot", "elevated": true}),
        json!({"action": "install"}),
        json!({"elevated": true}),
        json!("install"),
        Value::Null,
    ] {
        let (code, text) = wire_error(rig.admin.act_call(params.clone()).await);
        assert_eq!(code, "invalid_request", "{params}");
        assert_eq!(text, "The request is not valid.", "{params}");
    }
    for params in [json!({"x": 1}), json!([]), json!(1), json!("a")] {
        let (code, _) = wire_error(rig.admin.status_call(params.clone()).await);
        assert_eq!(code, "invalid_request", "{params}");
    }
    assert!(rig.runs().is_empty());
}

#[tokio::test]
async fn omarchy_status_takes_null_or_an_empty_object() {
    let rig = Rig::new("exit 0");
    for params in [Value::Null, json!({})] {
        let got = status(rig.admin.status_call(params).await);
        assert!(got.omarchy.present);
        assert_eq!(got.omarchy.version.as_deref(), Some("3.1"));
        let install = got.install.unwrap();
        assert!(install.installed && install.plugin_enabled && install.hyprland_line);
        assert_eq!(
            (
                install.files_ok,
                install.files_changed,
                install.files_missing
            ),
            (2, 1, 0)
        );
        assert!(!install.outdated);
        assert!(got.screensaver.unwrap().enabled);
        assert_eq!(got.provision, None);
        assert_eq!(got.running, None);
    }
    let runs = rig.runs();
    assert_eq!(runs.len(), 2);
    assert_eq!(tail(&runs[0], true), ["omarchy", "status", "--json"]);
}

#[tokio::test]
async fn omarchy_a_missing_omarchy_refuses_every_action_but_the_retry() {
    let rig = Rig::with("exit 0", |cfg| cfg.omarchy_env = ChildEnv::empty());
    for action in [
        "install",
        "uninstall",
        "screensaver_on",
        "screensaver_off",
        "screensaver_rebaseline",
    ] {
        let (code, text) = wire_error(act(&rig.admin, action, true).await);
        assert_eq!(code, "omarchy_missing", "{action}");
        assert_eq!(text, "Omarchy was not found on this station.");
    }
    assert!(rig.runs().is_empty());
    let got = status(rig.admin.status_call(Value::Null).await);
    assert!(!got.omarchy.present);
    assert_eq!(got.omarchy.version, None);
    assert_eq!((got.install, got.screensaver), (None, None));
    assert!(rig.runs().is_empty(), "no status child without Omarchy");
}
