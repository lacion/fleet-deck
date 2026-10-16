//! Detection with a temp `XDG_RUNTIME_DIR` (R13 §6.1).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, SystemTime};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::desktop::DesktopKind;
use tokio::net::UnixListener;

use super::detect::detect;

struct Fx {
    dir: tempfile::TempDir,
}

impl Fx {
    fn new(instances_json: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("run")).unwrap();
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        let script = dir.path().join("bin/hyprctl");
        std::fs::write(
            &script,
            format!("#!/bin/sh\n/bin/cat <<'EOF'\n{instances_json}\nEOF\n"),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }

    fn env(&self, runtime: bool, omarchy: &Path) -> ChildEnv {
        let mut vars = vec![
            ("PATH", self.dir.path().join("bin").into_os_string()),
            ("OMARCHY_PATH", omarchy.as_os_str().to_owned()),
        ];
        if runtime {
            vars.push((
                "XDG_RUNTIME_DIR",
                self.dir.path().join("run").into_os_string(),
            ));
        }
        ChildEnv::scrubbed(vars.into_iter().map(|(k, v)| (k.into(), v)))
    }

    fn none(&self) -> std::path::PathBuf {
        self.dir.path().join("no-omarchy")
    }

    /// A live instance folder; the listener must stay alive.
    fn live(&self, signature: &str) -> UnixListener {
        let inst = self.dir.path().join("run/hypr").join(signature);
        std::fs::create_dir_all(&inst).unwrap();
        UnixListener::bind(inst.join(".socket.sock")).unwrap()
    }

    /// An instance folder whose socket refuses a connect.
    fn stale(&self, signature: &str) {
        drop(self.live(signature));
    }

    fn age(&self, signature: &str, secs_ago: u64) {
        let dir = std::fs::File::open(self.dir.path().join("run/hypr").join(signature)).unwrap();
        dir.set_modified(SystemTime::now() - Duration::from_secs(secs_ago))
            .unwrap();
    }
}

fn instances(names: &[&str]) -> String {
    let items: Vec<String> = names
        .iter()
        .map(|n| format!(r#"{{"instance":"{n}","wl_socket":"wayland-{n}"}}"#))
        .collect();
    format!("[{}]", items.join(","))
}

#[tokio::test]
async fn without_a_runtime_dir_or_an_instance_there_is_no_desktop() {
    let fx = Fx::new(&instances(&["a"]));
    assert!(detect(&fx.env(false, &fx.none())).await.is_none());
    assert!(detect(&fx.env(true, &fx.none())).await.is_none());
    std::fs::create_dir_all(fx.dir.path().join("run/hypr")).unwrap();
    assert!(detect(&fx.env(true, &fx.none())).await.is_none());
}

#[tokio::test]
async fn one_live_socket_is_a_hyprland_desktop_with_its_wayland_socket() {
    let fx = Fx::new(&instances(&["a"]));
    let _live = fx.live("a");
    let found = detect(&fx.env(true, &fx.none())).await.unwrap();
    assert_eq!(found.signature, "a");
    assert_eq!(found.wayland, "wayland-a");
    assert_eq!(found.kind, DesktopKind::Hyprland);
    assert_eq!(found.omarchy, None);
    assert_eq!(found.runtime_dir, fx.dir.path().join("run"));
}

#[tokio::test]
async fn two_live_instances_give_the_newest_and_a_stale_socket_is_ignored() {
    let fx = Fx::new(&instances(&["old", "new", "dead"]));
    let _old = fx.live("old");
    let _new = fx.live("new");
    fx.stale("dead");
    fx.age("old", 3600);
    fx.age("new", 10);
    fx.age("dead", 0);
    let found = detect(&fx.env(true, &fx.none())).await.unwrap();
    assert_eq!(found.signature, "new");
    assert_eq!(found.wayland, "wayland-new");
}

#[tokio::test]
async fn a_stale_socket_alone_is_no_desktop() {
    let fx = Fx::new(&instances(&["dead"]));
    fx.stale("dead");
    assert!(detect(&fx.env(true, &fx.none())).await.is_none());
}

#[tokio::test]
async fn an_instance_that_hyprctl_does_not_name_is_no_desktop() {
    let fx = Fx::new(&instances(&["other"]));
    let _live = fx.live("a");
    assert!(detect(&fx.env(true, &fx.none())).await.is_none());
    let fx = Fx::new("not json");
    let _live = fx.live("a");
    assert!(detect(&fx.env(true, &fx.none())).await.is_none());
}

#[tokio::test]
async fn omarchy_needs_an_executable_bin_omarchy_and_reads_its_version() {
    let fx = Fx::new(&instances(&["a"]));
    let _live = fx.live("a");
    let root = fx.dir.path().join("omarchy");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("version"), " 3.2.1 \n").unwrap();
    // A file that is not executable is not Omarchy.
    std::fs::write(root.join("bin/omarchy"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(
        root.join("bin/omarchy"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let found = detect(&fx.env(true, &root)).await.unwrap();
    assert_eq!(found.kind, DesktopKind::Hyprland);
    std::fs::set_permissions(
        root.join("bin/omarchy"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let found = detect(&fx.env(true, &root)).await.unwrap();
    assert_eq!(found.kind, DesktopKind::Omarchy);
    assert_eq!(found.omarchy_version.as_deref(), Some("3.2.1"));
    assert_eq!(found.omarchy.as_deref(), Some(root.as_path()));
}

#[tokio::test]
async fn the_desktop_environment_names_the_instance_and_prefers_the_omarchy_bin() {
    let fx = Fx::new(&instances(&["a"]));
    let _live = fx.live("a");
    let root = fx.dir.path().join("omarchy");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/omarchy"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(
        root.join("bin/omarchy"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let base = fx.env(true, &root);
    let found = detect(&base).await.unwrap();
    let env = super::env::desktop_env(&base, &found).unwrap();
    let get = |n: &str| env.get(n).unwrap().to_string_lossy().into_owned();
    assert_eq!(get("HYPRLAND_INSTANCE_SIGNATURE"), "a");
    assert_eq!(get("WAYLAND_DISPLAY"), "wayland-a");
    assert_eq!(
        get("XDG_RUNTIME_DIR"),
        fx.dir.path().join("run").to_string_lossy()
    );
    let path = get("PATH");
    assert!(
        path.starts_with(&format!("{}:", root.join("bin").display())),
        "{path}"
    );
    assert!(path.ends_with(&fx.dir.path().join("bin").display().to_string()));
    // The station's own environment does not change.
    assert!(base.get("WAYLAND_DISPLAY").is_none());
    // Hyprland alone: the PATH stays as it is.
    let plain = fx.env(true, &fx.none());
    let found = detect(&plain).await.unwrap();
    let env = super::env::desktop_env(&plain, &found).unwrap();
    assert_eq!(env.get("PATH"), plain.get("PATH"));
}
