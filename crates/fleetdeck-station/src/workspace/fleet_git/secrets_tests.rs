//! `project.secret_scan`, `project.secret_read` and `project.secret_write`
//! (Fleet Sync FS-3).

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fleetdeck_proto::fleet_git::{GitSnapshot, StationHome};
use fleetdeck_testkit::Det;
use serde_json::{Value, json};

use super::*;
use crate::store::OfflineApprovalPolicy;
use crate::store::git_state::tests::store as new_store;
use crate::workspace::branches::parse_git_version;

const SALT: &str = "abababababababababababababababababababababababababababababababab";
const URL: &str = "https://github.com/acme/app.git";

struct Rig {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
    program: PathBuf,
    store: Store,
    policy: Arc<Policy>,
    dest: String,
}

impl Rig {
    /// `None` (with a printed reason) when the Git of the machine is too old.
    async fn new() -> Option<Self> {
        let out = Command::new("git").arg("--version").output().unwrap();
        if parse_git_version(&String::from_utf8_lossy(&out.stdout)).is_none_or(|v| v < (2, 45)) {
            eprintln!("skipped: Fleet Git needs Git 2.45 or newer");
            return None;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let program = root.join("git-wrapper");
        fs::write(
            &program,
            "#!/bin/sh\nexport GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1\nexec git \"$@\"\n",
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let (home, data) = (root.join("home"), root.join("data"));
        fs::create_dir(&home).unwrap();
        fs::create_dir(&data).unwrap();
        let policy = Policy::new(
            vec![root.clone()],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::new(),
        );
        let rig = Self {
            store: new_store(&Det::new()).await,
            policy: Arc::new(policy),
            dest: remote_identity(URL, LocalRemotes::Allow).unwrap().dest,
            _tmp: tmp,
            home,
            data,
            program,
        };
        rig.git(&["init", "-b", "main"]);
        rig.git(&["remote", "add", "origin", URL]);
        fs::write(
            rig.home.join(".gitignore"),
            ".env\n.env.*\nconfig/*.local\nconfig/.env*\nsecrets*\n*.pem\n",
        )
        .unwrap();
        fs::write(rig.home.join("tracked.txt"), "tracked\n").unwrap();
        rig.git(&["add", "."]);
        // A tracked file that the ignore rules match.
        fs::write(rig.home.join(".env.tracked"), "t\n").unwrap();
        rig.git(&["add", "-f", ".env.tracked"]);
        rig.git(&["commit", "-m", "one"]);
        rig.set_home(&rig.home.clone(), 1).await;
        Some(rig)
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new(&self.program)
            .current_dir(&self.home)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    async fn set_home(&self, path: &Path, generation: u64) {
        let snapshot = GitSnapshot {
            revision: generation,
            projects: Vec::new(),
            homes: vec![StationHome {
                project_id: "p1".to_owned(),
                generation,
                path: path.to_string_lossy().into_owned(),
                sync: true,
            }],
        };
        self.store
            .git_apply_snapshot(snapshot, format!("digest-{generation}"), Vec::new())
            .await
            .unwrap();
    }

    fn options(&self) -> GitOptions {
        GitOptions {
            program: self.program.clone(),
            timeout: Duration::from_secs(60),
            isolated: true,
            confine: None,
        }
    }

    fn write(&self, rel: &str, content: &[u8]) {
        let path = self.home.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
    }

    async fn call_with(
        &self,
        method: &str,
        params: Value,
        data: Option<&Path>,
    ) -> Result<Value, SecretError> {
        secret_call(
            &self.store,
            &self.policy,
            Arc::new(PrivatePaths::default()),
            &self.options(),
            data,
            method,
            params,
        )
        .await
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, SecretError> {
        let data = self.data.clone();
        self.call_with(method, params, Some(&data)).await
    }

    fn base(&self) -> Value {
        json!({
            "project_id": "p1",
            "generation": 1,
            "dest": self.dest,
            "remote_name": "origin",
        })
    }

    async fn scan(&self, paths: &[&str]) -> Vec<SecretFileState> {
        let mut params = self.base();
        params["files"] = paths
            .iter()
            .map(|p| json!({ "path": p, "salt": SALT }))
            .collect();
        let answer = self.call("project.secret_scan", params).await.unwrap();
        serde_json::from_value::<SecretScanAnswer>(answer)
            .unwrap()
            .files
    }

    async fn scan_one(&self, path: &str) -> SecretFileState {
        self.scan(&[path]).await.remove(0)
    }

    async fn read(&self, path: &str, digest: &str) -> Result<Value, SecretError> {
        let mut params = self.base();
        params["path"] = json!(path);
        params["salt"] = json!(SALT);
        params["digest"] = json!(digest);
        self.call("project.secret_read", params).await
    }

    fn write_params(&self, path: &str, content: &[u8], expect: Option<&str>) -> Value {
        let mut params = self.base();
        params["path"] = json!(path);
        params["salt"] = json!(SALT);
        params["digest"] = json!(secret_digest(SALT, content).unwrap());
        params["content_b64"] = json!(STANDARD.encode(content));
        if let Some(d) = expect {
            params["expect"] = json!(d);
        }
        params
    }

    async fn put(
        &self,
        path: &str,
        content: &[u8],
        expect: Option<&str>,
    ) -> Result<bool, SecretError> {
        let params = self.write_params(path, content, expect);
        self.call("project.secret_write", params)
            .await
            .map(|v| v["backup"].as_bool().unwrap())
    }

    fn digest(content: &[u8]) -> String {
        secret_digest(SALT, content).unwrap()
    }

    fn backups(&self, path: &str) -> Vec<PathBuf> {
        let folder = backup_folder(&self.data, "p1", path).unwrap();
        let mut files: Vec<PathBuf> = fs::read_dir(folder)
            .map(|d| d.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default();
        files.sort();
        files
    }

    /// The files of the home that a failed write could leave.
    fn leftovers(&self) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![self.home.clone()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(dir).unwrap().map(Result::unwrap) {
                let (path, name) = (
                    entry.path(),
                    entry.file_name().to_string_lossy().into_owned(),
                );
                if name == ".git" {
                    continue;
                }
                if path.is_dir() && !path.is_symlink() {
                    stack.push(path);
                } else if name.contains(".fleetdeck-") {
                    found.push(name);
                }
            }
        }
        found
    }
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn code_of(e: Result<impl std::fmt::Debug, SecretError>) -> &'static str {
    e.unwrap_err().code
}

#[test]
fn the_digest_is_a_keyed_hash_with_a_checked_salt() {
    let digest = secret_digest(SALT, b"A=1\n").unwrap();
    assert_eq!(digest.len(), 64);
    assert!(decode_salt(&digest).is_some(), "lower-case hex");
    assert_eq!(secret_digest(SALT, b"A=1\n").unwrap(), digest);
    let other = "cd".repeat(32);
    assert_ne!(secret_digest(&other, b"A=1\n").unwrap(), digest);
    assert_ne!(secret_digest(SALT, b"A=2\n").unwrap(), digest);
    for bad in [
        "",
        "ab",
        &"AB".repeat(32),
        &"ab".repeat(31),
        &"zz".repeat(32),
    ] {
        assert_eq!(secret_digest(bad, b"x"), None, "{bad}");
    }
}

#[tokio::test]
async fn a_scan_tells_present_and_missing() {
    let Some(rig) = Rig::new().await else { return };
    rig.write(".env", b"A=1\n");
    let states = rig.scan(&[".env", "config/.env.local", ".env.local"]).await;
    assert_eq!(states.len(), 3);
    assert_eq!(states[0].state, SecretPresence::Present);
    assert_eq!(states[0].digest.as_deref(), Some(&*Rig::digest(b"A=1\n")));
    assert_eq!(states[0].size, Some(4));
    assert!(states[0].mtime_ms.unwrap() > 0);
    assert_eq!(states[0].code, None);
    // A missing parent folder and a missing file are both `missing`.
    assert_eq!(states[1].state, SecretPresence::Missing);
    assert_eq!(states[2].state, SecretPresence::Missing);
    assert_eq!(
        states.iter().map(|s| s.path.as_str()).collect::<Vec<_>>(),
        [".env", "config/.env.local", ".env.local"]
    );
}

#[tokio::test]
async fn a_scan_refuses_a_file_that_git_does_not_ignore() {
    let Some(rig) = Rig::new().await else { return };
    // Tracked, although the ignore rules match.
    let tracked = rig.scan_one(".env.tracked").await;
    assert_eq!(tracked.state, SecretPresence::Refused);
    assert_eq!(tracked.code.as_deref(), Some("not_ignored"));
    assert_eq!(tracked.digest, None);
    // Hidden from members, but no ignore rule matches.
    rig.write("other.key", b"k\n");
    let loose = rig.scan_one("other.key").await;
    assert_eq!(loose.state, SecretPresence::Refused);
    assert_eq!(loose.code.as_deref(), Some("not_ignored"));
    // The same for a file that does not exist.
    let gone = rig.scan_one("never.key").await;
    assert_eq!(gone.code.as_deref(), Some("not_ignored"));
}

#[tokio::test]
async fn a_scan_refuses_links_folders_and_big_files() {
    let Some(rig) = Rig::new().await else { return };
    // A link at the file.
    rig.write("target.txt", b"x");
    symlink(rig.home.join("target.txt"), rig.home.join(".env")).unwrap();
    // A link at a parent folder.
    fs::create_dir(rig.home.join("real")).unwrap();
    fs::write(rig.home.join("real/.env.local"), b"x").unwrap();
    symlink(rig.home.join("real"), rig.home.join("config")).unwrap();
    // A folder at the file.
    fs::create_dir(rig.home.join("secrets.d")).unwrap();
    // A file of 512 KiB + 1.
    rig.write(".env.big", &vec![b'a'; 512 * 1024 + 1]);
    // A file of exactly 512 KiB.
    rig.write(".env.edge", &vec![b'a'; 512 * 1024]);
    let paths = [
        ".env",
        "config/.env.local",
        "secrets.d",
        ".env.big",
        ".env.edge",
    ];
    let states = rig.scan(&paths).await;
    let codes: Vec<Option<&str>> = states.iter().map(|s| s.code.as_deref()).collect();
    assert_eq!(
        codes,
        [
            Some("symlink"),
            Some("symlink"),
            Some("not_regular"),
            Some("too_large"),
            None
        ]
    );
    assert_eq!(states[4].state, SecretPresence::Present);
    assert_eq!(states[4].size, Some(512 * 1024));
}

#[tokio::test]
async fn a_scan_refuses_a_bad_path_before_git() {
    let Some(rig) = Rig::new().await else { return };
    let bad = [
        "../.env",
        ".git/.env",
        "/etc/passwd",
        "README.md",
        "a//.env",
    ];
    for state in rig.scan(&bad).await {
        assert_eq!(state.state, SecretPresence::Refused, "{}", state.path);
        assert_eq!(
            state.code.as_deref(),
            Some("invalid_path"),
            "{}",
            state.path
        );
    }
}

#[tokio::test]
async fn a_hard_linked_file_is_a_private_file() {
    let Some(rig) = Rig::new().await else { return };
    rig.write("real.txt", b"A=1\n");
    fs::hard_link(rig.home.join("real.txt"), rig.home.join(".env")).unwrap();
    let state = rig.scan_one(".env").await;
    assert_eq!(state.state, SecretPresence::Refused);
    assert_eq!(state.code.as_deref(), Some("private_file"));
    let e = rig.read(".env", &Rig::digest(b"A=1\n")).await.unwrap_err();
    assert_eq!(e.code, "private_file");
    // A write that expects a digest puts the file back.
    let e = rig
        .put(".env", b"B=2\n", Some(&Rig::digest(b"A=1\n")))
        .await
        .unwrap_err();
    assert_eq!(e.code, "private_file");
    assert_eq!(fs::read(rig.home.join(".env")).unwrap(), b"A=1\n");
    assert!(rig.leftovers().is_empty(), "{:?}", rig.leftovers());
}

#[tokio::test]
async fn a_read_checks_the_digest() {
    let Some(rig) = Rig::new().await else { return };
    rig.write(".env", b"A=1\n");
    let answer = rig.read(".env", &Rig::digest(b"A=1\n")).await.unwrap();
    let content = STANDARD
        .decode(answer["content_b64"].as_str().unwrap())
        .unwrap();
    assert_eq!(content, b"A=1\n");
    let (a1, a2) = (Rig::digest(b"A=1\n"), Rig::digest(b"A=2\n"));
    for (path, digest, code) in [
        (".env", &a2, "changed_meanwhile"),
        (".env.local", &a1, "missing"),
        ("config/.env.local", &a1, "missing"),
        (".env.tracked", &a1, "not_ignored"),
        ("README.md", &a1, "invalid_path"),
    ] {
        assert_eq!(code_of(rig.read(path, digest).await), code, "{path}");
    }
}

#[tokio::test]
async fn a_write_makes_a_new_file_and_its_folders() {
    let Some(rig) = Rig::new().await else { return };
    let backup = rig.put("deep/er/.env", b"A=1\n", None).await.unwrap();
    assert!(!backup);
    let file = rig.home.join("deep/er/.env");
    assert_eq!(fs::read(&file).unwrap(), b"A=1\n");
    assert_eq!(mode_of(&file), 0o600);
    assert_eq!(mode_of(&rig.home.join("deep")), 0o700);
    assert_eq!(mode_of(&rig.home.join("deep/er")), 0o700);
    assert!(rig.leftovers().is_empty(), "{:?}", rig.leftovers());
    assert!(rig.backups("deep/er/.env").is_empty());
}

#[tokio::test]
async fn a_write_replaces_with_a_backup() {
    let Some(rig) = Rig::new().await else { return };
    rig.write(".env", b"OLD=1\n");
    fs::set_permissions(rig.home.join(".env"), fs::Permissions::from_mode(0o644)).unwrap();
    let backup = rig
        .put(".env", b"NEW=2\n", Some(&Rig::digest(b"OLD=1\n")))
        .await
        .unwrap();
    assert!(backup);
    assert_eq!(fs::read(rig.home.join(".env")).unwrap(), b"NEW=2\n");
    assert_eq!(mode_of(&rig.home.join(".env")), 0o600);
    let kept = rig.backups(".env");
    assert_eq!(kept.len(), 1);
    assert_eq!(fs::read(&kept[0]).unwrap(), b"OLD=1\n");
    assert_eq!(mode_of(&kept[0]), 0o600);
    assert_eq!(mode_of(kept[0].parent().unwrap()), 0o700);
    assert!(rig.leftovers().is_empty(), "{:?}", rig.leftovers());
    // The backup is in the data folder, never in the repository.
    assert!(kept[0].starts_with(&rig.data));
}

#[tokio::test]
async fn a_write_refuses_a_changed_file() {
    let Some(rig) = Rig::new().await else { return };
    rig.write(".env", b"OLD=1\n");
    // A wrong `expect`.
    let e = rig
        .put(".env", b"NEW=2\n", Some(&Rig::digest(b"OTHER\n")))
        .await;
    assert_eq!(code_of(e), "changed_meanwhile");
    assert_eq!(fs::read(rig.home.join(".env")).unwrap(), b"OLD=1\n");
    // `None` with an existing file.
    assert_eq!(
        code_of(rig.put(".env", b"NEW=2\n", None).await),
        "changed_meanwhile"
    );
    assert_eq!(fs::read(rig.home.join(".env")).unwrap(), b"OLD=1\n");
    // `Some` with a missing file.
    let e = rig
        .put(".env.local", b"NEW=2\n", Some(&Rig::digest(b"OLD=1\n")))
        .await;
    assert_eq!(code_of(e), "changed_meanwhile");
    assert!(!rig.home.join(".env.local").exists());
    assert!(rig.leftovers().is_empty(), "{:?}", rig.leftovers());
    assert!(rig.backups(".env").is_empty());
}

#[tokio::test]
async fn a_write_refuses_a_content_that_does_not_match_its_digest() {
    let Some(rig) = Rig::new().await else { return };
    let mut params = rig.write_params(".env", b"A=1\n", None);
    params["content_b64"] = json!(STANDARD.encode(b"A=2\n"));
    let e = rig.call("project.secret_write", params).await.unwrap_err();
    assert_eq!(e.code, "digest_mismatch");
    assert_eq!(e.text, DIGEST_MISMATCH.text);
    assert!(!rig.home.join(".env").exists());
    // Bad base64 and a too large content.
    let mut params = rig.write_params(".env", b"A=1\n", None);
    params["content_b64"] = json!("***");
    let e = rig.call("project.secret_write", params).await;
    assert_eq!(code_of(e), "invalid_params");
    let big = vec![b'a'; 512 * 1024 + 1];
    let e = rig
        .call("project.secret_write", rig.write_params(".env", &big, None))
        .await;
    assert_eq!(code_of(e), "too_large");
    assert!(!rig.home.join(".env").exists());
}

#[tokio::test]
async fn seven_writes_keep_five_backups() {
    let Some(rig) = Rig::new().await else { return };
    rig.write(".env", b"V=0\n");
    for n in 0..7 {
        let old = format!("V={n}\n");
        let new = format!("V={}\n", n + 1);
        let made = rig
            .put(".env", new.as_bytes(), Some(&Rig::digest(old.as_bytes())))
            .await
            .unwrap();
        assert!(made);
    }
    assert_eq!(fs::read(rig.home.join(".env")).unwrap(), b"V=7\n");
    let mut kept: Vec<String> = rig
        .backups(".env")
        .iter()
        .map(|p| fs::read_to_string(p).unwrap())
        .collect();
    kept.sort();
    // The backups hold the 5 newest old contents: V=2 to V=6.
    assert_eq!(kept, ["V=2\n", "V=3\n", "V=4\n", "V=5\n", "V=6\n"]);
}

#[tokio::test]
async fn a_failed_backup_writes_nothing_and_leaves_no_temp_file() {
    let Some(rig) = Rig::new().await else { return };
    rig.write(".env", b"OLD=1\n");
    let params = rig.write_params(".env", b"NEW=2\n", Some(&Rig::digest(b"OLD=1\n")));
    // No data folder: the station cannot keep a copy.
    let e = rig
        .call_with("project.secret_write", params, None)
        .await
        .unwrap_err();
    assert_eq!(e.code, "backup_failed");
    assert_eq!(e.text, BACKUP_FAILED.text);
    assert_eq!(fs::read(rig.home.join(".env")).unwrap(), b"OLD=1\n");
    assert!(rig.leftovers().is_empty(), "{:?}", rig.leftovers());
}

/// A `commit` with `expect` in a plain folder, where an editor makes a new
/// `.env` right after the old one moved aside. Returns the error code, the
/// content of `.env` and the names that the station left in the folder.
fn window(expect: &[u8], data: bool) -> (&'static str, Vec<u8>, Vec<String>, Vec<Vec<u8>>) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    let data_dir = home.join("data");
    fs::write(home.join(".env"), b"OLD=1\n").unwrap();
    let root = open_root(&home).unwrap();
    let private = PrivatePaths::default();
    let job = Job {
        parent: root.as_fd(),
        name: ".env",
        rel: ".env",
        salt: SALT,
        project_id: "p1",
        private: &private,
        data_dir: data.then_some(data_dir.as_path()),
    };
    let e = commit(&job, b"NEW=2\n", Some(&Rig::digest(expect)), || {
        fs::write(home.join(".env"), b"EDITED=3\n").unwrap();
    })
    .unwrap_err();
    let names = fs::read_dir(&home)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".fleetdeck-"))
        .collect();
    let folder = backup_folder(&data_dir, "p1", ".env").unwrap();
    let backups = fs::read_dir(folder)
        .map(|d| d.map(|e| fs::read(e.unwrap().path()).unwrap()).collect())
        .unwrap_or_default();
    (e.code, fs::read(home.join(".env")).unwrap(), names, backups)
}

#[test]
fn an_edit_in_the_window_of_a_replace_is_kept() {
    // The old file is the expected one: its content is in the backup.
    let (code, now, names, backups) = window(b"OLD=1\n", true);
    assert_eq!(
        (code, now.as_slice()),
        ("changed_meanwhile", &b"EDITED=3\n"[..])
    );
    assert_eq!(backups, [b"OLD=1\n".to_vec()]);
    assert!(names.is_empty(), "{names:?}");
    // The old file is not the expected one: the same, from the other branch.
    let (code, now, names, backups) = window(b"OTHER\n", true);
    assert_eq!(
        (code, now.as_slice()),
        ("changed_meanwhile", &b"EDITED=3\n"[..])
    );
    assert_eq!(backups, [b"OLD=1\n".to_vec()]);
    assert!(names.is_empty(), "{names:?}");
}

#[test]
fn a_failed_backup_in_the_window_keeps_the_old_file_aside() {
    // R12 b: no data folder, so no backup. The old file stays aside, the temp
    // file goes, and the edit stays in place.
    let (code, now, names, backups) = window(b"OTHER\n", false);
    assert_eq!(
        (code, now.as_slice()),
        ("changed_meanwhile", &b"EDITED=3\n"[..])
    );
    assert!(backups.is_empty());
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names[0].contains(".fleetdeck-old-"), "{names:?}");
}

#[tokio::test]
async fn a_write_does_not_follow_a_link() {
    let Some(rig) = Rig::new().await else { return };
    fs::create_dir(rig.home.join("real")).unwrap();
    symlink(rig.home.join("real"), rig.home.join("config")).unwrap();
    let e = rig.put("config/.env.local", b"A=1\n", None).await;
    assert_eq!(code_of(e), "symlink");
    assert!(!rig.home.join("real/.env.local").exists());
    // A link at the file name.
    rig.write("target.txt", b"T");
    symlink(rig.home.join("target.txt"), rig.home.join(".env")).unwrap();
    let e = rig.put(".env", b"A=1\n", None).await;
    assert_eq!(code_of(e), "changed_meanwhile");
    assert_eq!(fs::read(rig.home.join("target.txt")).unwrap(), b"T");
    // A file where a folder must be.
    rig.write("flat", b"F");
    let e = rig.put("flat/.env", b"A=1\n", None).await;
    assert_eq!(code_of(e), "symlink");
}

#[tokio::test]
async fn the_home_and_the_repository_are_checked_first() {
    let Some(rig) = Rig::new().await else { return };
    rig.write(".env", b"A=1\n");
    let mut params = rig.base();
    params["files"] = json!([{ "path": ".env", "salt": SALT }]);
    // Another generation.
    let mut stale = params.clone();
    stale["generation"] = json!(2);
    let e = rig.call("project.secret_scan", stale).await.unwrap_err();
    assert_eq!(e.code, "stale_generation");
    assert_eq!(e.text, STALE_GENERATION.text);
    // An unknown project.
    let mut unknown = params.clone();
    unknown["project_id"] = json!("p9");
    let e = rig.call("project.secret_scan", unknown).await.unwrap_err();
    assert_eq!(e.code, "not_a_home");
    // A home outside the allowed roots.
    let outside = tempfile::tempdir().unwrap();
    rig.set_home(outside.path(), 2).await;
    let mut gen2 = params.clone();
    gen2["generation"] = json!(2);
    let e = rig.call("project.secret_scan", gen2).await.unwrap_err();
    assert_eq!(e.code, "not_a_home");
    rig.set_home(&rig.home.clone(), 3).await;
    // Another repository.
    let mut other = params.clone();
    other["generation"] = json!(3);
    other["dest"] = json!("https://github.com:443/acme/other.git");
    let e = rig.call("project.secret_scan", other).await.unwrap_err();
    assert_eq!(e.code, "not_this_repo");
    assert_eq!(e.text, NOT_THIS_REPO.text);
    // A remote that the home does not have.
    let mut remote = params.clone();
    remote["generation"] = json!(3);
    remote["remote_name"] = json!("upstream");
    let e = rig.call("project.secret_scan", remote).await.unwrap_err();
    assert_eq!(e.code, "not_this_repo");
    // A remote name outside the rule.
    let mut bad = params.clone();
    bad["generation"] = json!(3);
    bad["remote_name"] = json!("--upload-pack=x");
    let e = rig.call("project.secret_scan", bad).await.unwrap_err();
    assert_eq!(e.code, "invalid_params");
    // A push URL with two values is not one repository.
    for url in [
        "https://github.com/acme/app.git",
        "https://github.com/acme/o.git",
    ] {
        rig.git(&["remote", "set-url", "--push", "--add", "origin", url]);
    }
    let mut two = params.clone();
    two["generation"] = json!(3);
    let e = rig.call("project.secret_scan", two).await.unwrap_err();
    assert_eq!(e.code, "not_this_repo");
    // The methods that write refuse in the same way.
    let mut write = rig.write_params(".env", b"B=2\n", None);
    write["dest"] = json!("https://github.com:443/acme/other.git");
    write["generation"] = json!(3);
    let e = rig.call("project.secret_write", write).await.unwrap_err();
    assert_eq!(e.code, "not_this_repo");
    assert_eq!(fs::read(rig.home.join(".env")).unwrap(), b"A=1\n");
}

#[tokio::test]
async fn a_params_error_does_not_quote_the_params() {
    let Some(rig) = Rig::new().await else { return };
    for (method, params) in [
        (
            "project.secret_scan",
            json!({ "project_id": "SENTINEL-PARAM", "generation": "SENTINEL-PARAM" }),
        ),
        (
            "project.secret_read",
            json!({ "project_id": "p1", "surprise": "SENTINEL-PARAM" }),
        ),
        (
            "project.secret_write",
            json!({ "content_b64": ["SENTINEL-PARAM"] }),
        ),
        ("project.secret_write", json!("SENTINEL-PARAM")),
    ] {
        let e = rig.call(method, params).await.unwrap_err();
        assert_eq!(e.code, "invalid_params");
        assert_eq!(e.text, "The request does not parse.");
    }
    // A bad salt and too many files.
    let mut params = rig.base();
    params["files"] = json!([{ "path": ".env", "salt": "SENTINEL-PARAM" }]);
    let e = rig.call("project.secret_scan", params).await.unwrap_err();
    assert_eq!(e.text, "The request does not parse.");
    let mut params = rig.base();
    params["files"] = (0..21)
        .map(|n| json!({ "path": format!(".env{n}"), "salt": SALT }))
        .collect();
    let e = rig.call("project.secret_scan", params).await.unwrap_err();
    assert_eq!(e.code, "invalid_params");
}

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buf {
    type Writer = Buf;

    fn make_writer(&'a self) -> Buf {
        self.clone()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn the_logs_hold_no_content_digest_or_salt() {
    let Some(rig) = Rig::new().await else { return };
    let buf = Buf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(buf.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let secret = b"SENTINEL-SECRET-VALUE=1\n";
    rig.put(".env", secret, None).await.unwrap();
    let digest = Rig::digest(secret);
    rig.scan(&[".env", ".env.local"]).await;
    rig.read(".env", &digest).await.unwrap();
    let _ = rig.read(".env", &Rig::digest(b"other")).await;
    rig.put(".env", b"SENTINEL-NEW=2\n", Some(&digest))
        .await
        .unwrap();
    let _ = rig.put(".env", b"SENTINEL-NEW=3\n", Some(&digest)).await;
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("project.secret_write"), "{log}");
    for hidden in [
        "SENTINEL",
        &digest,
        SALT,
        &STANDARD.encode(secret),
        &Rig::digest(b"SENTINEL-NEW=2\n"),
    ] {
        assert!(!log.contains(hidden), "{hidden}");
    }
}

#[test]
fn a_backup_name_sorts_by_its_number_and_its_suffix() {
    assert_eq!(backup_key("1700000000000"), Some((1_700_000_000_000, 0)));
    assert_eq!(backup_key("1700000000000-2"), Some((1_700_000_000_000, 2)));
    for bad in ["", "-1", "1-", "a", "1-a", "1.5", "1-2-3", ".tmp"] {
        assert_eq!(backup_key(bad), None, "{bad}");
    }
    let data = Path::new("/d");
    assert!(backup_folder(data, "abc_DEF-123", ".env").is_some());
    for bad in ["", "a/b", "..", "a b", &"a".repeat(65)] {
        assert_eq!(backup_folder(data, bad, ".env"), None, "{bad}");
    }
}
