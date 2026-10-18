//! `fleetdeck omarchy screensaver-tick`, `screensaver on | off | rebaseline`,
//! the old `fleetdeck desktop screensaver-run` entry, and the restore of
//! `fleetdeck omarchy uninstall` (`omarchy-admin.md` sections 3.4 to 3.7),
//! with the rig of the installer tests and a fake station socket that
//! answers `fleet_summary`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
#![allow(dead_code, reason = "the rig of the installer tests is shared")]

#[path = "omarchy_install/rig.rs"]
mod rig;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

use serde_json::{Value, json};

use rig::{Rig, err, out};

const OLD_BACKUP: &str = "fleetdeck: an old screensaver backup exists and differs from the current file; the screensaver stays off.";
const NOT_INSTALLED: &str = "Install the Omarchy integration first.";

fn answer() -> String {
    json!({
        "status": "fleet_summary", "seq": 1, "link": "up", "held_s": 0,
        "summary": {
            "v": 1, "at": "2026-10-18T12:04:00Z", "board": null, "deck": "up",
            "counts": {"working": 1, "idle": 0, "needs_you": 3, "blocked": 0, "lost": 0},
            "needs_you": [], "usage": null, "stations": []
        }
    })
    .to_string()
}

fn fake_station(rig: &Rig) {
    let listener = UnixListener::bind(rig.root().join("data/station/station.sock")).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut line = String::new();
            if BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap_or(0)
                == 0
            {
                continue;
            }
            let _ = writeln!(stream, "{}", answer());
        }
    });
}

fn branding(rig: &Rig) -> PathBuf {
    rig.omarchy_dir().join("branding")
}

fn file(rig: &Rig) -> PathBuf {
    branding(rig).join("screensaver.txt")
}

fn backup(rig: &Rig) -> PathBuf {
    branding(rig).join("screensaver.txt.fleetdeck-backup")
}

fn original(rig: &Rig, text: &str) {
    std::fs::create_dir_all(branding(rig)).unwrap();
    std::fs::write(file(rig), text).unwrap();
}

fn code(rig: &Rig, args: &[&str]) -> (i32, String, String) {
    let o = rig.run(args);
    (o.status.code().unwrap(), out(&o), err(&o))
}

fn tick(rig: &Rig) -> (i32, String, String) {
    code(rig, &["omarchy", "screensaver-tick"])
}

fn install_on(rig: &Rig) {
    let (c, o, e) = code(rig, &["omarchy", "install", "--yes", "--screensaver"]);
    assert_eq!(c, 0, "{o}{e}");
}

fn status(rig: &Rig) -> Value {
    let o = rig.run(&["omarchy", "status", "--json"]);
    serde_json::from_str(out(&o).trim()).unwrap()
}

fn text(path: PathBuf) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn omarchy_screensaver_run_prints_its_line_and_exits_0() {
    let rig = Rig::new();
    let (c, o, e) = code(&rig, &["desktop", "screensaver-run"]);
    assert_eq!(
        (c, o.as_str()),
        (0, "The screensaver now runs in the station service.\n"),
        "{e}"
    );
    // Also with an installed record and the switch on: it writes nothing.
    install_on(&rig);
    let (c, o, _) = code(&rig, &["desktop", "screensaver-run"]);
    assert_eq!(
        (c, o.as_str()),
        (0, "The screensaver now runs in the station service.\n")
    );
    assert!(!file(&rig).exists(), "the old entry is no writer");
}

#[test]
fn omarchy_screensaver_tick_without_a_record_or_when_off_exits_0_and_writes_nothing() {
    let rig = Rig::new();
    let (c, o, e) = tick(&rig);
    assert_eq!((c, o.as_str(), e.as_str()), (0, "", ""));
    assert_eq!(rig.install().status.code(), Some(0));
    let (c, _, e) = tick(&rig);
    assert_eq!(c, 0, "{e}");
    assert!(
        !file(&rig).exists(),
        "a tick with the switch off writes nothing"
    );
}

#[test]
fn omarchy_screensaver_tick_writes_the_summary_and_uninstall_restores_the_original() {
    let rig = Rig::new();
    install_on(&rig);
    assert_eq!(rig.record()["screensaver"]["enabled"], true);
    original(&rig, "original\n");
    fake_station(&rig);

    let (c, _, e) = tick(&rig);
    assert_eq!(c, 0, "{e}");
    let written = text(file(&rig));
    assert!(written.starts_with("FLEET DECK\n"), "{written}");
    assert!(written.contains("\nneed you\n"), "{written}");
    assert!(
        written.contains("working 1   idle 0   blocked 0   lost 0\n"),
        "{written}"
    );
    assert_eq!(text(backup(&rig)), "original\n");
    let record = rig.record();
    assert_eq!(record["screensaver"]["had_original"], true);
    assert_eq!(
        record["screensaver"]["backup_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(
        record["screensaver"]["last_written_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );

    let (c, o, e) = code(&rig, &["omarchy", "uninstall"]);
    assert_eq!(c, 0, "{o}{e}");
    assert_eq!(text(file(&rig)), "original\n");
    assert_eq!(text(backup(&rig)), "original\n", "the backup is kept");
    assert!(
        o.contains(&format!(
            "The original screensaver is kept in {}.",
            backup(&rig).display()
        )),
        "{o}"
    );
    assert!(!rig.fd_dir().join("omarchy-install.json").exists());
}

#[test]
fn omarchy_screensaver_uninstall_keeps_a_file_that_the_user_changed() {
    let rig = Rig::new();
    install_on(&rig);
    let (c, _, e) = tick(&rig);
    assert_eq!(c, 0, "{e}");
    assert_eq!(rig.record()["screensaver"]["had_original"], false);
    std::fs::write(file(&rig), "mine now\n").unwrap();
    let (c, o, e) = code(&rig, &["omarchy", "uninstall"]);
    assert_eq!(c, 0, "{o}{e}");
    assert!(
        o.contains(&format!("kept (changed): {}", file(&rig).display())),
        "{o}"
    );
    assert_eq!(text(file(&rig)), "mine now\n");
}

#[test]
fn omarchy_screensaver_off_and_on_through_the_cli() {
    let rig = Rig::new();
    assert_eq!(rig.install().status.code(), Some(0));
    original(&rig, "original\n");
    let (c, _, e) = code(&rig, &["omarchy", "screensaver", "on"]);
    assert_eq!(c, 0, "{e}");
    assert_eq!(rig.record()["screensaver"]["enabled"], true);
    assert_eq!(status(&rig)["screensaver"]["enabled"], true);
    assert_eq!(status(&rig)["screensaver"]["file_restored"], Value::Null);
    let (c, _, e) = tick(&rig);
    assert_eq!(c, 0, "{e}");
    let first = text(file(&rig));
    assert!(
        first.starts_with("Fleet Deck: no data") || first.starts_with("FLEET"),
        "{first}"
    );

    let (c, o, e) = code(&rig, &["omarchy", "screensaver", "off"]);
    assert_eq!(c, 0, "{o}{e}");
    assert!(
        o.contains(&format!(
            "The original screensaver is kept in {}.",
            backup(&rig).display()
        )),
        "{o}"
    );
    assert_eq!(text(file(&rig)), "original\n");
    assert_eq!(text(backup(&rig)), "original\n");
    let v = status(&rig);
    assert_eq!(
        v["screensaver"],
        json!({"enabled": false, "file_restored": true, "blocked": null})
    );

    // A tick with the switch off writes nothing.
    let (c, _, _) = tick(&rig);
    assert_eq!(c, 0);
    assert_eq!(text(file(&rig)), "original\n");

    // `on` again: the next tick writes the same rendering again.
    let (c, _, e) = code(&rig, &["omarchy", "screensaver", "on"]);
    assert_eq!(c, 0, "{e}");
    let (c, _, e) = tick(&rig);
    assert_eq!(c, 0, "{e}");
    assert_eq!(text(file(&rig)), first);
    assert_eq!(text(backup(&rig)), "original\n");
    assert_eq!(status(&rig)["screensaver"]["file_restored"], Value::Null);
}

#[test]
fn omarchy_screensaver_off_without_an_original_removes_the_file() {
    let rig = Rig::new();
    install_on(&rig);
    let (c, _, e) = tick(&rig);
    assert_eq!(c, 0, "{e}");
    assert!(file(&rig).exists());
    let (c, o, e) = code(&rig, &["omarchy", "screensaver", "off"]);
    assert_eq!(c, 0, "{o}{e}");
    assert!(!file(&rig).exists());
    assert_eq!(rig.record()["screensaver"]["restored"], "removed");
    assert_eq!(status(&rig)["screensaver"]["file_restored"], true);
}

#[test]
fn omarchy_screensaver_commands_without_a_record_say_install_first() {
    let rig = Rig::new();
    for action in ["on", "off", "rebaseline"] {
        let (c, o, e) = code(&rig, &["omarchy", "screensaver", action]);
        assert_eq!((c, o.as_str()), (1, ""), "{action}: {e}");
        assert!(e.contains(NOT_INSTALLED), "{action}: {e}");
    }
    // A lock file without a record gives the same answer.
    std::fs::create_dir_all(rig.fd_dir()).unwrap();
    std::fs::write(rig.fd_dir().join("omarchy.lock"), "").unwrap();
    let (c, _, e) = code(&rig, &["omarchy", "screensaver", "on"]);
    assert_eq!(c, 1);
    assert!(e.contains(NOT_INSTALLED), "{e}");
}

#[test]
fn omarchy_screensaver_a_tick_with_the_lock_held_writes_nothing() {
    let rig = Rig::new();
    install_on(&rig);
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .open(rig.fd_dir().join("omarchy.lock"))
        .unwrap();
    lock.lock().unwrap();
    let (c, o, e) = tick(&rig);
    assert_eq!((c, o.as_str(), e.as_str()), (0, "", ""));
    assert!(!file(&rig).exists());
    drop(lock);
    let (c, _, e) = tick(&rig);
    assert_eq!(c, 0, "{e}");
    assert!(file(&rig).exists());
}

#[test]
fn omarchy_screensaver_a_refused_first_write_is_blocked_and_rebaseline_repairs_it() {
    let rig = Rig::new();
    install_on(&rig);
    original(&rig, "first original\n");
    let (c, _, e) = tick(&rig);
    assert_eq!(c, 0, "{e}");
    assert_eq!(text(backup(&rig)), "first original\n");
    let (c, _, e) = code(&rig, &["omarchy", "screensaver", "off"]);
    assert_eq!(c, 0, "{e}");

    // The user sets a new screensaver; the kept backup now differs.
    std::fs::write(file(&rig), "second original\n").unwrap();
    let (c, _, e) = code(&rig, &["omarchy", "screensaver", "on"]);
    assert_eq!(c, 0, "{e}");
    let (c, _, e) = tick(&rig);
    assert_eq!(c, 0, "a refusal still exits 0");
    assert!(e.contains(OLD_BACKUP), "{e}");
    assert_eq!(text(file(&rig)), "second original\n");
    assert_eq!(text(backup(&rig)), "first original\n");
    assert_eq!(rig.record()["screensaver"]["first_write_refused"], true);
    assert_eq!(
        status(&rig)["screensaver"],
        json!({"enabled": true, "file_restored": null, "blocked": "kept_original_differs"})
    );
    let o = rig.run(&["omarchy", "status"]);
    assert!(
        out(&o).contains("screensaver blocked: kept_original_differs"),
        "{}",
        out(&o)
    );

    let (c, o, e) = code(&rig, &["omarchy", "screensaver", "rebaseline"]);
    assert_eq!(c, 0, "{o}{e}");
    assert!(o.starts_with("The previous original is kept in "), "{o}");
    let kept = PathBuf::from(
        o.trim()
            .trim_start_matches("The previous original is kept in ")
            .trim_end_matches('.'),
    );
    assert!(
        kept.to_string_lossy()
            .contains("screensaver.txt.fleetdeck-backup."),
        "{o}"
    );
    assert_eq!(
        text(kept),
        "first original\n",
        "the old original is never deleted"
    );
    assert!(!backup(&rig).exists());
    assert_eq!(rig.record()["screensaver"]["first_write_refused"], false);
    assert_eq!(rig.record()["screensaver"]["enabled"], true);

    let (c, _, e) = tick(&rig);
    assert_eq!(c, 0, "{e}");
    assert_eq!(text(backup(&rig)), "second original\n");
    assert!(text(file(&rig)).starts_with("Fleet Deck") || text(file(&rig)).starts_with("FLEET"));
    assert_eq!(status(&rig)["screensaver"]["blocked"], Value::Null);
}

#[test]
fn omarchy_screensaver_a_failed_restore_keeps_the_record_and_uninstall_stays_incomplete() {
    let rig = Rig::new();
    install_on(&rig);
    original(&rig, "original\n");
    let (c, _, e) = tick(&rig);
    assert_eq!(c, 0, "{e}");
    let written = text(file(&rig));
    // A link in place of the file: the restore cannot read it.
    std::fs::remove_file(file(&rig)).unwrap();
    let victim = rig.root().join("victim");
    std::fs::write(&victim, "keep").unwrap();
    std::os::unix::fs::symlink(&victim, file(&rig)).unwrap();
    let before = rig.record()["screensaver"].clone();

    let (c, _, e) = code(&rig, &["omarchy", "screensaver", "off"]);
    assert_eq!(c, 1);
    assert!(
        e.contains("fleetdeck: the screensaver could not be restored:"),
        "{e}"
    );
    assert_eq!(rig.record()["screensaver"], before);
    assert_eq!(text(victim.clone()), "keep");

    // Uninstall: the files go, the record stays, exit 1.
    let (c, o, e) = code(&rig, &["omarchy", "uninstall"]);
    assert_eq!(c, 1, "{o}{e}");
    assert!(
        e.contains("fleetdeck: the screensaver could not be restored:"),
        "{e}"
    );
    assert!(rig.fd_dir().join("omarchy-install.json").exists());
    assert_eq!(rig.record()["screensaver"], before);

    // The file is ours again: a second `off` restores.
    std::fs::remove_file(file(&rig)).unwrap();
    std::fs::write(file(&rig), written).unwrap();
    let (c, o, e) = code(&rig, &["omarchy", "screensaver", "off"]);
    assert_eq!(c, 0, "{o}{e}");
    assert_eq!(text(file(&rig)), "original\n");
}
