//! Tests of the real effects (P8 T3b) that need no running station: the
//! station config, the directory rules, the release files and the mapping of
//! the answers of the station.
//!
//! The write of `station.toml` checks every ancestor of the path from `/`
//! (no group or other write bit), so a directory below `/tmp` fails it. The
//! tests that write use a private place below the source tree or `$HOME`
//! (`private_base`); when there is none they print a note and stop.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use fleetdeck_proto::link::StationProvider;
use fleetdeck_station::local_fingerprint::BuildIdentity;

use super::*;

const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// A directory (0700) whose ancestors pass the rule of the writers: no
/// link, owned by root or this user, no group or other write bit.
fn private_base() -> Option<tempfile::TempDir> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let places = [Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))), home];
    places.into_iter().flatten().find_map(|place| {
        let place = place.canonicalize().ok()?;
        let dir = tempfile::Builder::new()
            .prefix("provision-real-")
            .tempdir_in(&place)
            .ok()?;
        chmod(dir.path(), 0o700);
        let me = fs::metadata(dir.path()).unwrap().uid();
        for ancestor in dir.path().ancestors().skip(1) {
            let meta = fs::symlink_metadata(ancestor).unwrap();
            let ok =
                meta.is_dir() && (meta.uid() == 0 || meta.uid() == me) && meta.mode() & 0o022 == 0;
            if !ok {
                eprintln!("no private place: {} fails the rule", ancestor.display());
                return None;
            }
        }
        Some(dir)
    })
}

fn dirs_in(base: &Path) -> (NodeDirs, Env) {
    let dirs = NodeDirs {
        config: base.join("config"),
        data: base.join("data"),
    };
    let env = Env {
        home: Some(base.join("home")),
        ..Env::default()
    };
    (dirs, env)
}

fn put_config(dirs: &NodeDirs, text: &str) {
    fs::create_dir_all(&dirs.config).unwrap();
    let path = dirs.station_config();
    fs::write(&path, text).unwrap();
    chmod(&path, 0o600);
}

// ---- station.toml ----

#[test]
fn the_station_config_states() {
    let Some(base) = private_base() else { return };
    let (dirs, env) = dirs_in(base.path());
    assert_eq!(config_state(&dirs, &env), ConfigState::Absent);
    put_config(&dirs, "name = \"laptop\"\n");
    assert_eq!(
        config_state(&dirs, &env),
        ConfigState::Named("laptop".to_owned())
    );
    put_config(&dirs, "max_sessions = 4\n");
    assert_eq!(config_state(&dirs, &env), ConfigState::Unnamed);
    put_config(&dirs, "max_sessions = 0\n");
    assert_eq!(config_state(&dirs, &env), ConfigState::Invalid);
    put_config(&dirs, "this is not toml\n");
    assert_eq!(config_state(&dirs, &env), ConfigState::Invalid);
}

#[test]
fn a_config_that_others_can_write_is_invalid() {
    let Some(base) = private_base() else { return };
    let (dirs, env) = dirs_in(base.path());
    put_config(&dirs, "name = \"laptop\"\n");
    chmod(&dirs.station_config(), 0o666);
    assert_eq!(config_state(&dirs, &env), ConfigState::Invalid);
}

#[test]
fn a_new_station_config_is_written_with_the_name() {
    let Some(base) = private_base() else { return };
    let (dirs, env) = dirs_in(base.path());
    // The config directory does not exist: it is created with mode 0700.
    create_config(&dirs, "laptop").unwrap();
    let path = dirs.station_config();
    assert_eq!(fs::read_to_string(&path).unwrap(), "name = \"laptop\"\n");
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(&dirs.config).unwrap().mode() & 0o777, 0o700);
    assert_eq!(
        config_state(&dirs, &env),
        ConfigState::Named("laptop".to_owned())
    );
}

#[test]
fn a_station_config_is_never_replaced() {
    let Some(base) = private_base() else { return };
    let (dirs, _) = dirs_in(base.path());
    put_config(&dirs, "max_sessions = 4\n");
    assert!(create_config(&dirs, "laptop").is_err());
    assert_eq!(
        fs::read_to_string(dirs.station_config()).unwrap(),
        "max_sessions = 4\n"
    );
    // A dangling link counts as an existing file.
    fs::remove_file(dirs.station_config()).unwrap();
    symlink(base.path().join("nowhere"), dirs.station_config()).unwrap();
    assert!(create_config(&dirs, "laptop").is_err());
    assert!(!base.path().join("nowhere").exists());
}

#[test]
fn a_config_directory_of_mode_0755_is_used_and_not_changed() {
    let Some(base) = private_base() else { return };
    let (dirs, env) = dirs_in(base.path());
    fs::create_dir_all(&dirs.config).unwrap();
    chmod(&dirs.config, 0o755);
    create_config(&dirs, "laptop").unwrap();
    assert_eq!(fs::metadata(&dirs.config).unwrap().mode() & 0o777, 0o755);
    assert_eq!(
        config_state(&dirs, &env),
        ConfigState::Named("laptop".to_owned())
    );
}

#[test]
fn a_config_directory_that_group_can_write_is_refused() {
    let Some(base) = private_base() else { return };
    let (dirs, _) = dirs_in(base.path());
    fs::create_dir_all(&dirs.config).unwrap();
    chmod(&dirs.config, 0o775);
    assert!(create_config(&dirs, "laptop").is_err());
    assert!(!dirs.station_config().exists());
}

// ---- the directory rules ----

#[test]
fn a_missing_directory_is_created_with_mode_0700_with_its_parents() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a/b/station");
    ensure_private_dir(&dir).unwrap();
    for part in [dir.as_path(), dir.parent().unwrap()] {
        assert_eq!(fs::metadata(part).unwrap().mode() & 0o777, 0o700);
    }
    // Again: an existing directory passes.
    ensure_private_dir(&dir).unwrap();
}

#[test]
fn an_existing_directory_is_checked_and_never_changed() {
    let root = tempfile::tempdir().unwrap();
    let ok = root.path().join("ok");
    fs::create_dir(&ok).unwrap();
    chmod(&ok, 0o755);
    ensure_private_dir(&ok).unwrap();
    assert_eq!(fs::metadata(&ok).unwrap().mode() & 0o777, 0o755);
    for mode in [0o775, 0o757] {
        let bad = root.path().join(format!("bad{mode:o}"));
        fs::create_dir(&bad).unwrap();
        chmod(&bad, mode);
        assert!(ensure_private_dir(&bad).is_err(), "{mode:o}");
        assert_eq!(fs::metadata(&bad).unwrap().mode() & 0o777, mode);
    }
    // A link to a good directory, and a file.
    let link = root.path().join("link");
    symlink(&ok, &link).unwrap();
    assert!(ensure_private_dir(&link).is_err());
    let file = root.path().join("file");
    fs::write(&file, b"x").unwrap();
    assert!(ensure_private_dir(&file).is_err());
}

// ---- the files of the release prerequisite ----

#[test]
fn a_root_file_has_its_bytes_owner_and_mode() {
    let root = tempfile::tempdir().unwrap();
    chmod(root.path(), 0o750);
    let path = root.path().join("manifest.json");
    fs::write(&path, b"{}").unwrap();
    chmod(&path, 0o444);
    let me = fs::metadata(&path).unwrap().uid();
    let file = read_root_file(&path).unwrap();
    assert_eq!(file.bytes, b"{}");
    assert_eq!(file.uid, me);
    assert_eq!(file.mode, 0o444);
    assert!(!file.symlink);
    assert_eq!(file.parent_uid, me);
    assert_eq!(file.parent_mode, 0o750);
}

#[test]
fn a_root_file_that_is_a_link_is_flagged() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    fs::write(&real, b"data").unwrap();
    let link = root.path().join("link");
    symlink(&real, &link).unwrap();
    let file = read_root_file(&link).unwrap();
    assert!(file.symlink);
    assert_eq!(file.bytes, b"data");
}

#[test]
fn a_root_file_over_64_kib_a_missing_file_and_a_directory_are_errors() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("big");
    fs::write(&path, vec![b'a'; 64 * 1024]).unwrap();
    assert_eq!(read_root_file(&path).unwrap().bytes.len(), 64 * 1024);
    fs::write(&path, vec![b'a'; 64 * 1024 + 1]).unwrap();
    assert!(read_root_file(&path).is_err());
    assert!(read_root_file(&root.path().join("missing")).is_err());
    assert!(read_root_file(root.path()).is_err());
}

// ---- the answers of the station ----

fn good(features: &[&str], fingerprint: &str, paired: Option<&str>) -> LocalResponse {
    LocalResponse::Fingerprint {
        features: features.iter().map(|f| (*f).to_owned()).collect(),
        fingerprint: fingerprint.to_owned(),
        paired_deck: paired.map(str::to_owned),
        link_up: true,
        build: BuildIdentity {
            version: "2.0.0".to_owned(),
            commit: "abc1234".to_owned(),
            release: true,
        },
        binary_sha256: FP.to_owned(),
        providers: Some(Vec::<StationProvider>::new()),
    }
}

#[test]
fn a_good_response_becomes_the_view() {
    let view = view_of(good(&[FEATURE], FP, Some(FP))).unwrap();
    assert_eq!(view.fingerprint, FP);
    assert_eq!(view.paired_deck.as_deref(), Some(FP));
    assert!(view.link_up);
    assert_eq!(
        view.build,
        Build {
            version: "2.0.0".to_owned(),
            commit: "abc1234".to_owned(),
            release: true
        }
    );
    assert_eq!(view.binary_sha256, FP);
    assert_eq!(view.providers, Some(Vec::new()));
    assert!(view_of(good(&["other", FEATURE], FP, None)).is_ok());
}

#[test]
fn every_other_response_is_invalid() {
    assert_eq!(view_of(good(&[], FP, None)), Err(CallError::Invalid));
    assert_eq!(
        view_of(good(&["station_provision.v2"], FP, None)),
        Err(CallError::Invalid)
    );
    for bad in [FP.to_uppercase(), FP[..63].to_owned(), String::new()] {
        assert_eq!(
            view_of(good(&[FEATURE], &bad, None)),
            Err(CallError::Invalid)
        );
        assert_eq!(
            view_of(good(&[FEATURE], FP, Some(&bad))),
            Err(CallError::Invalid)
        );
    }
    let mut empty_hash = good(&[FEATURE], FP, None);
    if let LocalResponse::Fingerprint { binary_sha256, .. } = &mut empty_hash {
        binary_sha256.clear();
    }
    assert_eq!(view_of(empty_hash), Err(CallError::Invalid));
    for other in [
        LocalResponse::Error {
            message: "bad request".to_owned(),
        },
        LocalResponse::ProvisionError {
            code: "unavailable".to_owned(),
        },
        LocalResponse::Sessions { sessions: vec![] },
    ] {
        assert_eq!(view_of(other), Err(CallError::Invalid));
    }
}

#[tokio::test]
async fn no_station_socket_is_unavailable() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("station.sock");
    assert_eq!(
        ask_fingerprint(&path, false, Some(FP)).await,
        Err(CallError::Unavailable)
    );
    assert_eq!(
        ask_fingerprint(&path, true, Some(FP)).await,
        Err(CallError::Unavailable)
    );
}

#[test]
fn the_call_waits_are_two_and_thirty_five_seconds() {
    assert_eq!(CALL_WAIT, Duration::from_secs(2));
    assert_eq!(PROBE_CALL_WAIT, Duration::from_secs(35));
    assert_eq!(JOIN_WAIT, Duration::from_secs(690));
    assert_eq!(OMARCHY_LIMIT, Duration::from_secs(60));
}

#[test]
fn the_join_answers_map_to_outcomes() {
    let code = |c: &str| JoinOutcome::Code(c.to_owned());
    assert_eq!(
        join_outcome(LocalResponse::TailnetPaired {
            device: "deck".to_owned(),
            deck: FP.to_owned()
        }),
        JoinOutcome::Paired
    );
    assert_eq!(
        join_outcome(LocalResponse::TailnetRefused {
            code: "window_closed".to_owned(),
            detail: "text that is never read".to_owned()
        }),
        code("window_closed")
    );
    assert_eq!(
        join_outcome(LocalResponse::TailnetObserved {
            fingerprint: FP.to_owned()
        }),
        code("invalid_request")
    );
    assert_eq!(
        join_outcome(LocalResponse::Error {
            message: "x".to_owned()
        }),
        code("failed")
    );
    assert_eq!(
        join_outcome(LocalResponse::Sessions { sessions: vec![] }),
        code("unsupported")
    );
}

#[tokio::test]
async fn a_join_without_a_station_socket_is_failed_and_a_bad_pin_is_invalid() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("station.sock");
    let code = |c: &str| JoinOutcome::Code(c.to_owned());
    assert_eq!(
        ask_join(
            &path,
            "deck.example.ts.net".to_owned(),
            7421,
            FP,
            "laptop".to_owned(),
            Some(FP)
        )
        .await,
        code("failed")
    );
    assert_eq!(
        ask_join(
            &path,
            "deck.example.ts.net".to_owned(),
            7421,
            "xyz",
            "laptop".to_owned(),
            Some(FP)
        )
        .await,
        code("invalid_request")
    );
}

// ---- the process behind the socket ----

/// A `/proc` that answers from a script.
struct FakeProc {
    exe: io::Result<String>,
    /// The start times, one per call; the last repeats.
    starts: Mutex<Vec<io::Result<u64>>>,
}

fn kind(e: &io::Result<String>) -> io::Result<String> {
    match e {
        Ok(text) => Ok(text.clone()),
        Err(e) => Err(io::Error::new(e.kind(), "scripted")),
    }
}

impl FakeProc {
    fn new(exe: &str, starts: &[u64]) -> Self {
        Self {
            exe: Ok(exe.to_owned()),
            starts: Mutex::new(starts.iter().map(|s| Ok(*s)).collect()),
        }
    }
}

impl ProcReader for FakeProc {
    fn exe_sha256(&self, _pid: u32) -> io::Result<String> {
        kind(&self.exe)
    }

    fn start_time(&self, _pid: u32) -> io::Result<u64> {
        let mut starts = self.starts.lock().unwrap();
        if starts.len() > 1 {
            return starts.remove(0);
        }
        match starts.first() {
            Some(Ok(start)) => Ok(*start),
            _ => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }
}

const TRUSTED: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OTHER: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

#[test]
fn the_start_time_is_field_22_counted_from_the_last_bracket() {
    let stat = "123 (name with ) and (brackets) S 1 123 123 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 987654 1000 2 18446744073709551615";
    assert_eq!(parse_start_time(stat), Some(987_654));
    assert_eq!(parse_start_time("1 (x) S 1"), None);
    assert_eq!(parse_start_time("no bracket"), None);
    assert_eq!(parse_start_time(&stat.replace("987654", "abc")), None);
    // The real `/proc` of this process has a start time.
    assert!(RealProc.start_time(std::process::id()).is_ok());
}

#[test]
fn the_peer_check_accepts_only_the_trusted_process_of_the_same_user() {
    let proc = FakeProc::new(TRUSTED, &[7]);
    let cred = |uid, pid| Ok::<_, io::Error>((uid, pid));
    let peer = verify_peer(&proc, 1000, Some(TRUSTED), cred(1000, Some(42)));
    assert_eq!(peer, Some(Peer { pid: 42, start: 7 }));
    // Another executable.
    let other = FakeProc::new(OTHER, &[7]);
    assert!(verify_peer(&other, 1000, Some(TRUSTED), cred(1000, Some(42))).is_none());
    // Another user.
    assert!(verify_peer(&proc, 1000, Some(TRUSTED), cred(1001, Some(42))).is_none());
    // No process id.
    assert!(verify_peer(&proc, 1000, Some(TRUSTED), cred(1000, None)).is_none());
    // No credentials.
    let none = Err(io::Error::from(io::ErrorKind::Unsupported));
    assert!(verify_peer(&proc, 1000, Some(TRUSTED), none).is_none());
    // No trusted hash.
    assert!(verify_peer(&proc, 1000, None, cred(1000, Some(42))).is_none());
    // The executable or the start time cannot be read.
    let unreadable = FakeProc {
        exe: Err(io::Error::from(io::ErrorKind::PermissionDenied)),
        starts: Mutex::new(vec![Ok(7)]),
    };
    assert!(verify_peer(&unreadable, 1000, Some(TRUSTED), cred(1000, Some(42))).is_none());
    let no_start = FakeProc {
        exe: Ok(TRUSTED.to_owned()),
        starts: Mutex::new(vec![Err(io::Error::from(io::ErrorKind::NotFound))]),
    };
    assert!(verify_peer(&no_start, 1000, Some(TRUSTED), cred(1000, Some(42))).is_none());
}

#[test]
fn a_changed_start_time_is_not_the_same_process() {
    let peer = Peer { pid: 42, start: 7 };
    assert!(still_peer(&FakeProc::new(TRUSTED, &[7]), peer));
    assert!(!still_peer(&FakeProc::new(TRUSTED, &[8]), peer));
    let gone = FakeProc {
        exe: Ok(TRUSTED.to_owned()),
        starts: Mutex::new(vec![Err(io::Error::from(io::ErrorKind::NotFound))]),
    };
    assert!(!still_peer(&gone, peer));
}

/// A station on `path` that answers one request with `response`.
async fn serve_one(path: &Path, response: LocalResponse) -> tokio::task::JoinHandle<()> {
    let listener = fleetdeck_core::localsock::bind(path).await.unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(read);
        let _ = fleetdeck_core::localsock::read_line(&mut reader).await;
        let _ = fleetdeck_core::localsock::send(&mut write, &response).await;
    })
}

fn euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

#[tokio::test]
async fn a_fingerprint_call_needs_the_verified_process_before_and_after_the_answer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("station.sock");
    let server = serve_one(&path, good(&[FEATURE], FP, None)).await;
    let proc = FakeProc::new(TRUSTED, &[7]);
    let view = ask_fingerprint_with(&path, false, Some(TRUSTED), euid(), &proc)
        .await
        .unwrap();
    assert_eq!(view.fingerprint, FP);
    server.await.unwrap();

    // A different hash: the request is never sent (the answer is not read).
    let path = dir.path().join("hash.sock");
    let server = serve_one(&path, good(&[FEATURE], FP, None)).await;
    let proc = FakeProc::new(OTHER, &[7]);
    assert_eq!(
        ask_fingerprint_with(&path, false, Some(TRUSTED), euid(), &proc).await,
        Err(CallError::Invalid)
    );
    server.abort();

    // Another user.
    let path = dir.path().join("uid.sock");
    let server = serve_one(&path, good(&[FEATURE], FP, None)).await;
    let proc = FakeProc::new(TRUSTED, &[7]);
    assert_eq!(
        ask_fingerprint_with(&path, false, Some(TRUSTED), euid() + 1, &proc).await,
        Err(CallError::Invalid)
    );
    server.abort();

    // No trusted hash.
    let path = dir.path().join("none.sock");
    let server = serve_one(&path, good(&[FEATURE], FP, None)).await;
    assert_eq!(
        ask_fingerprint_with(&path, false, None, euid(), &proc).await,
        Err(CallError::Invalid)
    );
    server.abort();

    // The process changed while it answered.
    let path = dir.path().join("start.sock");
    let server = serve_one(&path, good(&[FEATURE], FP, None)).await;
    let proc = FakeProc::new(TRUSTED, &[7, 8]);
    assert_eq!(
        ask_fingerprint_with(&path, false, Some(TRUSTED), euid(), &proc).await,
        Err(CallError::Invalid)
    );
    server.await.unwrap();
}

async fn join(path: &Path, trusted: Option<&str>, uid: u32, proc: &FakeProc) -> JoinOutcome {
    ask_join_with(
        path,
        "deck.example.ts.net".to_owned(),
        7421,
        (FP, "laptop".to_owned()),
        (trusted, uid, proc),
    )
    .await
}

#[tokio::test]
async fn a_join_needs_the_verified_process_and_gives_build_mismatch_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let code = |c: &str| JoinOutcome::Code(c.to_owned());
    let paired = || LocalResponse::TailnetPaired {
        device: "deck".to_owned(),
        deck: FP.to_owned(),
    };
    let path = dir.path().join("ok.sock");
    let server = serve_one(&path, paired()).await;
    let proc = FakeProc::new(TRUSTED, &[7]);
    assert_eq!(
        join(&path, Some(TRUSTED), euid(), &proc).await,
        JoinOutcome::Paired
    );
    server.await.unwrap();

    let path = dir.path().join("hash.sock");
    let server = serve_one(&path, paired()).await;
    let proc = FakeProc::new(OTHER, &[7]);
    assert_eq!(
        join(&path, Some(TRUSTED), euid(), &proc).await,
        code("build_mismatch")
    );
    server.abort();

    let path = dir.path().join("uid.sock");
    let server = serve_one(&path, paired()).await;
    let proc = FakeProc::new(TRUSTED, &[7]);
    assert_eq!(
        join(&path, Some(TRUSTED), euid() + 1, &proc).await,
        code("build_mismatch")
    );
    server.abort();

    let path = dir.path().join("none.sock");
    let server = serve_one(&path, paired()).await;
    assert_eq!(
        join(&path, None, euid(), &proc).await,
        code("build_mismatch")
    );
    server.abort();

    let path = dir.path().join("start.sock");
    let server = serve_one(&path, paired()).await;
    let proc = FakeProc::new(TRUSTED, &[7, 9]);
    assert_eq!(
        join(&path, Some(TRUSTED), euid(), &proc).await,
        code("build_mismatch")
    );
    server.await.unwrap();
}
