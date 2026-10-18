//! The fake desktop of the P5b proofs: the P6 rig (fake `hyprctl`, `pgrep`
//! and the other programs, a Hyprland instance folder), a fake `wf-recorder`
//! behind the `shim-exec` seam, and the files that control the recorder.
//!
//! The station launches the recorder as `<exe> shim-exec --parent <pid>
//! --fsize <n> --stdin-fd3 -- <wf-recorder> <argv>`. The `<exe>` of a test is
//! `shim_rec.sh`: it drops `--parent <pid>`, ignores `--fsize`, moves stdin to
//! FD 3 at `--stdin-fd3` (as the real shim does), makes the child a session
//! and group leader (`setsid`, as the real shim does) and runs the rest.
//!
//! The fake `wf-recorder` writes a fixture MP4 to `/proc/self/fd/3`, then
//! waits. It takes the fixture `fixture-<seconds>.mp4` (the seconds of its
//! `-F fps=30,trim=duration=<seconds>` argument) or else `fixture.mp4`. It
//! traps `SIGINT` to finish and exit 0; the flag file `ignore_int` makes it
//! ignore `SIGINT`, and the flag file `bad_file` makes it write bytes that are
//! not an MP4. The environment of the recorder is cleared, so a switch is a
//! file of the rig folder.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleetdeck_core::ChildEnv;
use fleetdeck_station::desktop::Desktop;

#[path = "../p6_support/fake_desktop.rs"]
mod p6_rig;

pub use p6_rig::Rig;

fn write_script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The shell script of the fake recorder.
fn recorder_script(dir: &Path) -> String {
    let d = dir.display();
    format!(
        "[ -e /proc/self/fd/3 ] || exit 9\n\
         sec=\n\
         for a in \"$@\"; do case \"$a\" in fps=30,trim=duration=*) sec=\"${{a#fps=30,trim=duration=}}\";; esac; done\n\
         {{ for a in \"$@\"; do echo \"$a\"; done; echo --; }} >> {d}/recorder.log\n\
         if [ -f {d}/ignore_int ]; then trap '' INT; else trap 'exit 0' INT; fi\n\
         if [ -f {d}/bad_file ]; then printf 'this is not an mp4 file' >&3; else\n\
           f={d}/fixture-$sec.mp4; [ -f \"$f\" ] || f={d}/fixture.mp4; /bin/cat \"$f\" >&3; fi\n\
         echo $$ > {d}/recorder.pid\n\
         while :; do /bin/sleep 0.05; done"
    )
}

/// The shell script of the `shim-exec` executable of the test.
const SHIM: &str = "[ \"$1\" = shim-exec ] && [ \"$2\" = --parent ] || exit 3\n\
    shift 3\n\
    while [ \"$1\" != -- ]; do\n\
      case \"$1\" in\n\
        --fsize) shift;;\n\
        --stdin-fd3) exec 3<&0 </dev/null;;\n\
        *) exit 4;;\n\
      esac\n\
      shift\n\
    done\n\
    shift\n\
    exec /usr/bin/setsid \"$@\"";

/// The P6 rig with the fake recorder and its shim: the desktop of the rig is
/// replaced by one that launches through `shim_rec.sh`.
pub fn recorder_rig() -> Rig {
    let mut rig = Rig::new();
    let root = rig.dir.path().to_owned();
    write_script(&root.join("bin/wf-recorder"), &recorder_script(&root));
    write_script(&root.join("shim_rec.sh"), SHIM);
    let base = ChildEnv::scrubbed(
        [
            ("PATH", root.join("bin").into_os_string()),
            ("XDG_RUNTIME_DIR", root.join("run").into_os_string()),
            ("HOME", root.join("home").into_os_string()),
            ("XDG_DATA_HOME", root.join("data").into_os_string()),
            ("OMARCHY_PATH", root.join("omarchy").into_os_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v)),
    );
    rig.desktop = Arc::new(Desktop::with_env(base).with_shim_exe(root.join("shim_rec.sh")));
    rig
}

/// The controls of the fake recorder (an extension of the rig).
pub trait RecorderRig {
    /// The file that the recorder writes when it is asked for `seconds`.
    fn fixture(&self, seconds: u32, bytes: &[u8]);
    /// The file that the recorder writes for any other duration.
    fn default_fixture(&self, bytes: &[u8]);
    /// Switches the flag file `name` (`ignore_int`, `bad_file`).
    fn flag(&self, name: &str);
    /// The PID that the recorder wrote, if it ran.
    fn recorder_pid(&self) -> Option<u32>;
    /// How many times the recorder started.
    fn recorder_runs(&self) -> usize;
    /// How many processes of the fake recorder live now (a scan of `/proc`).
    fn live_recorders(&self) -> usize;
}

impl RecorderRig for Rig {
    fn fixture(&self, seconds: u32, bytes: &[u8]) {
        std::fs::write(
            self.dir.path().join(format!("fixture-{seconds}.mp4")),
            bytes,
        )
        .unwrap();
    }

    fn default_fixture(&self, bytes: &[u8]) {
        std::fs::write(self.dir.path().join("fixture.mp4"), bytes).unwrap();
    }

    fn flag(&self, name: &str) {
        std::fs::write(self.dir.path().join(name), "").unwrap();
    }

    fn recorder_pid(&self) -> Option<u32> {
        std::fs::read_to_string(self.dir.path().join("recorder.pid"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    fn recorder_runs(&self) -> usize {
        std::fs::read_to_string(self.dir.path().join("recorder.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| *l == "--")
            .count()
    }

    fn live_recorders(&self) -> usize {
        let program: PathBuf = self.dir.path().join("bin/wf-recorder");
        let program = program.to_string_lossy().into_owned();
        std::fs::read_dir("/proc")
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .bytes()
                    .all(|b| b.is_ascii_digit())
            })
            .filter_map(|e| std::fs::read(e.path().join("cmdline")).ok())
            .filter(|cmd| String::from_utf8_lossy(cmd).contains(&program))
            .count()
    }
}
