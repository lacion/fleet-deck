//! G12: the pairing commands take the invitation from `--invite-file` (mode
//! 0600), `--stdin` or a hidden prompt, never from argv, and never print it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;

use common::fleetdeck;
use fleetdeck_testkit::TempNode;

const INV: &str =
    "fdinv1:wss://127.0.0.1:7443#00.0123456789abcdef.5ec2e75ec2e75ec2e75ec2e75ec2e75e.1";

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn an_invitation_as_an_argument_is_refused() {
    let node = TempNode::new("inv").unwrap();
    for args in [
        &["station", "join", INV][..],
        &["deck", "add-station", INV][..],
    ] {
        let out = fleetdeck(&node).args(args).output().unwrap();
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(err.contains("never a command-line argument"), "{err}");
        assert!(!err.contains("5ec2e75e"), "{err}");
    }
}

#[test]
fn an_invitation_file_readable_by_others_is_refused() {
    let node = TempNode::new("inv").unwrap();
    let path = node.root().join("inv.txt");
    std::fs::write(&path, INV).unwrap();
    for mode in [0o640, 0o700] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        let out = fleetdeck(&node)
            .args(["station", "join", "--invite-file"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(err.contains("mode 0600"), "{err}");
        assert!(err.contains(&format!("has mode {mode:o}")), "{err}");
        assert!(!err.contains("5ec2e75e"), "{err}");
    }

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let out = fleetdeck(&node)
        .args(["station", "join", "--invite-file"])
        .arg(&path)
        .output()
        .unwrap();
    let err = stderr(&out);
    assert!(
        err.contains("not implemented yet"),
        "the file was accepted: {err}"
    );
    assert!(!err.contains("5ec2e75e"), "{err}");
}

#[test]
fn stdin_works_and_a_missing_terminal_is_explained() {
    let node = TempNode::new("inv").unwrap();
    let mut child = fleetdeck(&node)
        .args(["deck", "add-station", "--stdin"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{INV}\n").as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let err = stderr(&out);
    assert!(err.contains("not implemented yet"), "{err}");
    assert!(!err.contains("5ec2e75e"), "{err}");

    // No flag and no terminal: no prompt that waits forever.
    let out = fleetdeck(&node)
        .args(["station", "join"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(stderr(&out).contains("not a terminal"), "{}", stderr(&out));
}
