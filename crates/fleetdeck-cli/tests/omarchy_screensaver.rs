//! `fleetdeck desktop screensaver-run` and the restore of `fleetdeck omarchy
//! uninstall` (R13 P4 task G, `omarchy-p4.md` §3.8), with the rig of the
//! installer tests and a fake station socket that answers `fleet_summary`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
#![allow(dead_code, reason = "the rig of the installer tests is shared")]

#[path = "omarchy_install/rig.rs"]
mod rig;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use serde_json::json;

use rig::{Rig, err, out};

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

#[test]
fn omarchy_screensaver_run_exits_0_at_once_without_a_record_or_when_off() {
    let rig = Rig::new();
    let o = rig.run(&["desktop", "screensaver-run"]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    assert_eq!(rig.install().status.code(), Some(0));
    let o = rig.run(&["desktop", "screensaver-run"]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    assert!(
        !rig.omarchy_dir().join("branding/screensaver.txt").exists(),
        "a job with the switch off writes nothing"
    );
}

#[test]
fn omarchy_screensaver_writes_the_summary_and_uninstall_restores_the_original() {
    let rig = Rig::new();
    let o = rig.run(&["omarchy", "install", "--yes", "--screensaver"]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    assert_eq!(rig.record()["screensaver"]["enabled"], true);
    let branding = rig.omarchy_dir().join("branding");
    std::fs::create_dir_all(&branding).unwrap();
    let file = branding.join("screensaver.txt");
    std::fs::write(&file, "original\n").unwrap();
    fake_station(&rig);

    let mut job = rig
        .command(&["desktop", "screensaver-run"])
        .spawn()
        .unwrap();
    let started = Instant::now();
    let text = loop {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        if text != "original\n"
            && !text.is_empty()
            && !rig.record()["screensaver"]["last_written_sha256"].is_null()
        {
            break text;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "no write");
        std::thread::sleep(Duration::from_millis(50));
    };
    job.kill().unwrap();
    job.wait().unwrap();
    assert!(text.starts_with("FLEET DECK\n"), "{text}");
    assert!(text.contains("\nneed you\n"), "{text}");
    assert!(
        text.contains("working 1   idle 0   blocked 0   lost 0\n"),
        "{text}"
    );
    assert_eq!(
        std::fs::read_to_string(branding.join("screensaver.txt.fleetdeck-backup")).unwrap(),
        "original\n"
    );
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

    let o = rig.run(&["omarchy", "uninstall"]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "original\n");
    assert!(!branding.join("screensaver.txt.fleetdeck-backup").exists());
    assert!(!rig.fd_dir().join("omarchy-install.json").exists());
}

#[test]
fn omarchy_screensaver_uninstall_keeps_a_file_that_the_user_changed() {
    let rig = Rig::new();
    let o = rig.run(&["omarchy", "install", "--yes", "--screensaver"]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    fake_station(&rig);
    let branding = rig.omarchy_dir().join("branding");
    let file = branding.join("screensaver.txt");
    let mut job = rig
        .command(&["desktop", "screensaver-run"])
        .spawn()
        .unwrap();
    let started = Instant::now();
    while !file.is_file() || rig.record()["screensaver"]["last_written_sha256"].is_null() {
        assert!(started.elapsed() < Duration::from_secs(20), "no write");
        std::thread::sleep(Duration::from_millis(50));
    }
    job.kill().unwrap();
    job.wait().unwrap();
    assert_eq!(rig.record()["screensaver"]["had_original"], false);
    std::fs::write(&file, "mine now\n").unwrap();
    let o = rig.run(&["omarchy", "uninstall"]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    assert!(
        out(&o).contains(&format!("kept (changed): {}", file.display())),
        "{}",
        out(&o)
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine now\n");
}
