//! Real-process tests of the two-step shim launch (DL-194 T1): the shim
//! proves its executable (`hello`) before the station sends the session
//! token (`SecretsFrame`); the token reaches only the provider's
//! environment. The test process plays the station.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fleetdeck_core::Secret;
use fleetdeck_providers::host::shim::client::{ClientError, launch_with};
use fleetdeck_providers::host::shim::exe_hash::sha256_file;
use fleetdeck_providers::host::shim::journal::Record;
use fleetdeck_providers::host::shim::launch::LaunchReply;

mod shim_rig;
use shim_rig::*;

const TOKEN: &str = "tok-shimcred-t1-0123456789abcdef";
const TOKEN_VAR: &str = "FLEETDECK_COORDINATOR_TOKEN";

/// A provider that writes its token variable to `$OUT`, then waits.
const SHOW_TOKEN: &str = r#"printf '%s' "${FLEETDECK_COORDINATOR_TOKEN-unset}" > "$OUT"; sleep 30"#;

fn secrets_rig(script: &str) -> Rig {
    Rig::with(script, |spec| {
        spec.secrets = true;
        let out = Path::new(std::str::from_utf8(&spec.cwd).unwrap()).join("out");
        spec.env
            .push((b"OUT".to_vec(), out.as_os_str().as_encoded_bytes().to_vec()));
    })
}

fn out_path(rig: &Rig) -> PathBuf {
    rig.tmp.path().join("out")
}

fn expected_hash() -> String {
    sha256_file(&exe()).unwrap()
}

async fn wait_for_file(path: &Path) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if let Ok(bytes) = std::fs::read(path)
            && !bytes.is_empty()
        {
            return bytes;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} was not written",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The pids of the processes whose command line holds `shim_id`.
fn pids_of_shim(shim_id: &str) -> Vec<u32> {
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if let Ok(cmdline) = std::fs::read(entry.path().join("cmdline"))
            && contains(&cmdline, shim_id.as_bytes())
        {
            pids.push(pid);
        }
    }
    pids
}

/// True when a file under `dir` holds the token. It fails when `dir` holds
/// no file, so an empty scan never passes.
fn leaks_in(dir: &Path) -> bool {
    fn walk(dir: &Path, token_found: &mut bool, files: &mut usize) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                walk(&entry.path(), token_found, files);
            } else if kind.is_file() {
                *files += 1;
                if contains(&std::fs::read(entry.path()).unwrap(), TOKEN.as_bytes()) {
                    *token_found = true;
                }
            }
        }
    }
    let (mut found, mut files) = (false, 0);
    walk(dir, &mut found, &mut files);
    assert!(files > 0, "no file under {}", dir.display());
    found
}

fn script_exe(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A fake shim: it records all its stdin in `recorded` and, after 0.3 s
/// (the spec frame is recorded by then), prints a `hello` with `hash`.
fn fake_recording_shim(rig: &Rig, recorded: &Path, hash: &str) -> PathBuf {
    let body = format!(
        "exec 3<&0\n/bin/cat <&3 > '{}' &\n/bin/sleep 0.3\nprintf '%s\\n' '{{\"reply\":\"hello\",\"exe_sha256\":\"{hash}\",\"protocol\":2}}'\nwait",
        recorded.display(),
    );
    script_exe(rig.tmp.path(), "fake-recording", &body)
}

#[tokio::test]
async fn the_token_reaches_the_provider_environment_after_a_correct_hello() {
    let rig = secrets_rig(SHOW_TOKEN);
    let token = Secret::new(TOKEN.to_owned());
    let launched = launch_with(
        &exe(),
        &rig.spec,
        &rig.key,
        WAIT,
        Some((&expected_hash(), &token)),
    )
    .await
    .unwrap();
    assert!(
        matches!(launched.reply, LaunchReply::Ready { .. }),
        "{:?}",
        launched.reply
    );
    assert_eq!(wait_for_file(&out_path(&rig)).await, TOKEN.as_bytes());
    end(&rig, launched, 1).await;
}

#[tokio::test]
async fn a_wrong_expected_hash_is_untrusted_and_no_provider_runs() {
    let rig = Rig::with(r#"echo ran > "$MARK"; sleep 30"#, |spec| {
        spec.secrets = true;
        let mark = Path::new(std::str::from_utf8(&spec.cwd).unwrap()).join("mark");
        spec.env.push((
            b"MARK".to_vec(),
            mark.as_os_str().as_encoded_bytes().to_vec(),
        ));
    });
    let token = Secret::new(TOKEN.to_owned());
    let wrong = "0".repeat(64);
    let error = launch_with(&exe(), &rig.spec, &rig.key, WAIT, Some((&wrong, &token)))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ClientError::Untrusted(r) if r == "wrong executable"),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "the shim could not be verified: wrong executable"
    );
    for pid in pids_of_shim(&rig.spec.shim_id) {
        assert!(!alive(pid), "the shim {pid} still runs");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!rig.tmp.path().join("mark").exists(), "the provider ran");
    let records = rig.files.records().unwrap();
    assert!(!records.contains(&Record::SpawnAttempted), "{records:?}");
}

#[tokio::test]
async fn a_fake_shim_that_prints_nothing_is_untrusted_after_the_wait() {
    let rig = secrets_rig(SHOW_TOKEN);
    let fake = script_exe(rig.tmp.path(), "fake-silent", "exec /bin/sleep 30");
    let token = Secret::new(TOKEN.to_owned());
    let error = launch_with(
        &fake,
        &rig.spec,
        &rig.key,
        Duration::from_millis(500),
        Some(("abc", &token)),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&error, ClientError::Untrusted(r) if r == "timeout"),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_fake_shim_with_a_wrong_hash_never_receives_the_token() {
    let rig = secrets_rig(SHOW_TOKEN);
    let recorded = rig.tmp.path().join("stdin.rec");
    let fake = fake_recording_shim(&rig, &recorded, "deadbeef");
    let token = Secret::new(TOKEN.to_owned());
    let error = launch_with(&fake, &rig.spec, &rig.key, WAIT, Some(("abc", &token)))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ClientError::Untrusted(r) if r == "wrong executable"),
        "{error:?}"
    );
    // The spec frame arrived (it names the shim); the token never did.
    let bytes = wait_for_file(&recorded).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let bytes = std::fs::read(&recorded).unwrap_or(bytes);
    assert!(contains(&bytes, rig.spec.shim_id.as_bytes()));
    assert!(!contains(&bytes, TOKEN.as_bytes()));
}

#[tokio::test]
async fn the_recording_fake_shim_sees_the_token_after_a_correct_hello() {
    // The positive control of the test above: the same recorder holds the
    // token when the hash is right, so its silence there means something.
    let rig = secrets_rig(SHOW_TOKEN);
    let recorded = rig.tmp.path().join("stdin.rec");
    let fake = fake_recording_shim(&rig, &recorded, "abc");
    let token = Secret::new(TOKEN.to_owned());
    let result = launch_with(&fake, &rig.spec, &rig.key, WAIT, Some(("abc", &token))).await;
    // The fake never answers `ready`.
    assert!(result.is_err());
    let bytes = std::fs::read(&recorded).unwrap();
    assert!(contains(&bytes, TOKEN.as_bytes()));
}

#[tokio::test]
async fn the_token_is_in_no_file_no_spec_and_no_argument() {
    let rig = secrets_rig(SHOW_TOKEN);
    let token = Secret::new(TOKEN.to_owned());
    let launched = launch_with(
        &exe(),
        &rig.spec,
        &rig.key,
        WAIT,
        Some((&expected_hash(), &token)),
    )
    .await
    .unwrap();
    assert_eq!(wait_for_file(&out_path(&rig)).await, TOKEN.as_bytes());
    // The provider's own output file holds the token by design; every
    // other file of the shim directory must not.
    assert!(!leaks_in(&rig.spec.shims_dir));
    let spec_bytes = serde_json::to_vec(&rig.spec).unwrap();
    assert!(!contains(&spec_bytes, TOKEN.as_bytes()));
    assert!(
        !rig.spec
            .env
            .iter()
            .any(|(k, v)| k == TOKEN_VAR.as_bytes() || contains(v, TOKEN.as_bytes()))
    );
    assert!(!rig.spec.args.iter().any(|a| contains(a, TOKEN.as_bytes())));
    // The scan finds a token that a test copy plants.
    let planted = tempfile::tempdir().unwrap();
    std::fs::write(planted.path().join("journal"), format!("x{TOKEN}y")).unwrap();
    assert!(leaks_in(planted.path()));
    end(&rig, launched, 1).await;
}

#[tokio::test]
async fn without_secrets_the_one_frame_exchange_gives_the_provider_no_token() {
    let rig = Rig::with(SHOW_TOKEN, |spec| {
        let out = Path::new(std::str::from_utf8(&spec.cwd).unwrap()).join("out");
        spec.env
            .push((b"OUT".to_vec(), out.as_os_str().as_encoded_bytes().to_vec()));
    });
    assert!(!rig.spec.secrets);
    let launched = launch_with(&exe(), &rig.spec, &rig.key, WAIT, None)
        .await
        .unwrap();
    assert!(matches!(launched.reply, LaunchReply::Ready { .. }));
    assert_eq!(wait_for_file(&out_path(&rig)).await, b"unset");
    end(&rig, launched, 1).await;
}

#[tokio::test]
async fn secrets_and_the_spec_flag_must_agree_before_anything_starts() {
    let token = Secret::new(TOKEN.to_owned());
    let off = Rig::new("sleep 30");
    let error = launch_with(&exe(), &off.spec, &off.key, WAIT, Some(("abc", &token)))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ClientError::Protocol(m) if m == "secrets and spec.secrets differ"),
        "{error:?}"
    );
    let on = secrets_rig("sleep 30");
    let error = launch_with(&exe(), &on.spec, &on.key, WAIT, None)
        .await
        .unwrap_err();
    assert!(matches!(&error, ClientError::Protocol(_)), "{error:?}");
    assert!(pids_of_shim(&off.spec.shim_id).is_empty());
    assert!(pids_of_shim(&on.spec.shim_id).is_empty());
}
