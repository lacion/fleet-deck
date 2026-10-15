#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing_subscriber::fmt::MakeWriter;

use super::*;

/// A fake `tailscale` program: a shell script with mode 0755.
fn fake(dir: &Path, name: &str, body: &str) -> TailscaleCli {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    TailscaleCli::new(path)
}

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(20)
}

/// `cert()` again while a parallel `fork` still holds the new script open
/// for writing (`ETXTBSY`, which is `Failed`).
async fn cert_of(cli: &TailscaleCli) -> Result<Vec<u8>, CertError> {
    let mut last = cli.cert("deck.tail1.ts.net", soon()).await;
    for _ in 0..20 {
        if last != Err(CertError::Failed) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        last = cli.cert("deck.tail1.ts.net", soon()).await;
    }
    last
}

/// A log writer that keeps every byte.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Captured {
    type Writer = Captured;

    fn make_writer(&'a self) -> Captured {
        self.clone()
    }
}

#[tokio::test]
async fn a_fake_program_gives_its_stdout_and_gets_the_fixed_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let args = dir.path().join("args");
    let cli = fake(
        dir.path(),
        "tailscale",
        &format!(
            "echo \"$@\" > '{}'\necho '-----BEGIN CERTIFICATE-----'",
            args.display()
        ),
    );
    let out = cert_of(&cli).await.unwrap();
    assert_eq!(out, b"-----BEGIN CERTIFICATE-----\n");
    assert_eq!(
        fs::read_to_string(&args).unwrap(),
        "cert --cert-file - --key-file - --min-validity 720h deck.tail1.ts.net\n"
    );
}

#[tokio::test]
async fn a_failed_run_logs_and_returns_no_output() {
    let dir = tempfile::tempdir().unwrap();
    let cli = fake(
        dir.path(),
        "tailscale",
        "echo '-----BEGIN PRIVATE KEY----- DUMMY' >&2\necho DUMMY\nexit 1",
    );
    let logs = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(logs.clone())
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    let err = cli.cert("deck.tail1.ts.net", soon()).await.unwrap_err();
    drop(guard);
    assert_eq!(err, CertError::Failed);
    assert!(!err.to_string().contains("DUMMY"));
    let text = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
    assert!(!text.contains("DUMMY"), "{text}");
}

#[tokio::test]
async fn stderr_maps_to_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let https = fake(
        dir.path(),
        "https",
        "echo 'HTTPS certificates are not enabled' >&2\nexit 1",
    );
    assert_eq!(cert_of(&https).await.unwrap_err(), CertError::HttpsDisabled);
    let out = fake(dir.path(), "out", "echo 'Logged out.' >&2\nexit 1");
    assert_eq!(cert_of(&out).await.unwrap_err(), CertError::NotLoggedIn);
}

#[test]
fn the_stderr_substrings_and_the_codes_are_fixed() {
    let cases: [(&[u8], CertError, &str); 8] = [
        (
            b"TLS cert does not support x",
            CertError::HttpsDisabled,
            "https_disabled",
        ),
        (
            b"https is not enabled",
            CertError::HttpsDisabled,
            "https_disabled",
        ),
        (b"NeedsLogin", CertError::NotLoggedIn, "not_logged_in"),
        (
            b"Needs login to continue",
            CertError::NotLoggedIn,
            "not_logged_in",
        ),
        (
            b"you are not logged in",
            CertError::NotLoggedIn,
            "not_logged_in",
        ),
        (b"https only", CertError::Failed, "failed"),
        (b"enable something", CertError::Failed, "failed"),
        (b"", CertError::Failed, "failed"),
    ];
    for (stderr, want, code) in cases {
        let got = map_failure(stderr);
        assert_eq!(got, want, "{}", String::from_utf8_lossy(stderr));
        assert_eq!(got.code(), code);
    }
    let codes = [
        (CertError::Timeout, "timeout"),
        (CertError::NotInstalled, "not_installed"),
        (CertError::TooLarge, "too_large"),
    ];
    for (e, code) in codes {
        assert_eq!(e.code(), code);
    }
}

#[tokio::test]
async fn a_slow_program_times_out_and_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let pid = dir.path().join("pid");
    let cli = fake(
        dir.path(),
        "tailscale",
        &format!("echo $$ > '{}'\nexec sleep 30", pid.display()),
    );
    let started = std::time::Instant::now();
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut err = cli.cert("deck.tail1.ts.net", deadline).await.unwrap_err();
    for _ in 0..20 {
        if err != CertError::Failed {
            break;
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        err = cli.cert("deck.tail1.ts.net", deadline).await.unwrap_err();
    }
    assert_eq!(err, CertError::Timeout);
    assert!(started.elapsed() < Duration::from_secs(10));
    let pid: i32 = fs::read_to_string(&pid).unwrap().trim().parse().unwrap();
    assert!(fs::metadata(format!("/proc/{pid}")).is_err(), "child {pid}");
}

#[tokio::test]
async fn a_missing_program_is_not_installed() {
    let dir = tempfile::tempdir().unwrap();
    let cli = TailscaleCli::new(dir.path().join("nope"));
    assert_eq!(
        cli.cert("deck.tail1.ts.net", soon()).await.unwrap_err(),
        CertError::NotInstalled
    );
}

#[tokio::test]
async fn too_much_stdout_or_stderr_is_too_large() {
    let dir = tempfile::tempdir().unwrap();
    let big = fake(dir.path(), "big", "head -c 71680 /dev/zero");
    assert_eq!(cert_of(&big).await.unwrap_err(), CertError::TooLarge);
    let noisy = fake(dir.path(), "noisy", "head -c 20000 /dev/zero >&2\necho x");
    assert_eq!(cert_of(&noisy).await.unwrap_err(), CertError::TooLarge);
    let ok = fake(dir.path(), "ok", "head -c 65536 /dev/zero");
    assert_eq!(cert_of(&ok).await.unwrap().len(), 65536);
}
