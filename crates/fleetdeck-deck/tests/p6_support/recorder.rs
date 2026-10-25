//! The fake `wf-recorder` of the end-to-end proofs of recordings, put into the
//! folder of a P6 rig: a `wf-recorder` that logs its start to `calls.log`,
//! writes a clip to FD 3 and waits, and the `shim-exec` executable that moves
//! stdin to FD 3 and runs the rest.
//!
//! The file `<root>/hold` is a switch: while it exists the recorder writes
//! nothing, and when it is removed the recorder writes the fixture. A test
//! uses it to act "during the recording". The recorder ends by itself when
//! the folder of the rig is gone, so a failed test leaves no process.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

#[path = "../p5b_support/mp4.rs"]
mod mp4;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Puts the fake recorder, its shim and the fixture clip (3440x1440, one
/// second) into the rig folder `root`.
pub fn add_recorder(root: &Path) {
    let write = |path: std::path::PathBuf, body: String| {
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let d = root.display();
    write(
        root.join("bin/wf-recorder"),
        format!(
            "[ -e /proc/self/fd/3 ] || exit 9\n\
             {{ echo \"== wf-recorder\"; for a in \"$@\"; do echo \"$a\"; done; }} >> {d}/calls.log\n\
             trap 'exit 0' INT\n\
             while [ -f {d}/hold ]; do /bin/sleep 0.05; done\n\
             /bin/cat {d}/fixture.mp4 >&3\n\
             : > {d}/written\n\
             while [ -e {d}/fixture.mp4 ]; do /bin/sleep 0.05; done"
        ),
    );
    write(
        root.join("shim.sh"),
        "[ \"$1\" = shim-exec ] && [ \"$2\" = --parent ] || exit 3\n\
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
         exec /usr/bin/setsid \"$@\""
            .to_owned(),
    );
    std::fs::write(root.join("fixture.mp4"), mp4::clip(3440, 1440, 1)).unwrap();
}

/// Makes the recorder wait: it writes nothing until [`release`].
pub fn hold(root: &Path) {
    std::fs::write(root.join("hold"), "").unwrap();
}

/// Lets a waiting recorder write the fixture. The recorder leaves the file
/// `written` behind when the clip is on FD 3.
pub fn release(root: &Path) {
    let _ = std::fs::remove_file(root.join("hold"));
}

/// Waits until the recorder wrote the fixture (the file `written`).
pub async fn written(root: &Path) {
    let path = root.join("written");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !path.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the recorder wrote no clip"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// How many times the fake recorder started (its lines in `calls.log`).
pub fn runs(root: &Path) -> usize {
    std::fs::read_to_string(root.join("calls.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| *l == "== wf-recorder")
        .count()
}

/// How many processes of the fake recorder live now (a scan of `/proc`).
pub fn live(root: &Path) -> usize {
    let program = root.join("bin/wf-recorder").to_string_lossy().into_owned();
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

/// The bytes of the fixture clip.
pub fn fixture(root: &Path) -> Vec<u8> {
    std::fs::read(root.join("fixture.mp4")).unwrap()
}
