//! The provider layouts of a move (FG-9, ADR-010 §8 tests 1 to 4, 6 to 10,
//! 14): export from the source home, stage, import into the target home.

use std::fs;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{TranscriptManifest, code};

use super::{MoveSupport, export_files, import_files, staged_name, support};
use crate::history::Limits;
use crate::history::locate::{self, Trust, encode_cwd};
use crate::store::move_runs::ImportedFile;
use crate::workspace::PrivatePaths;
use crate::workspace::fleet_git::move_export::{Exported, MoveFileError};

pub(in crate::workspace::fleet_git) const NATIVE: &str = "11111111-2222-3333-4444-555555555555";
pub(in crate::workspace::fleet_git) const STAMP: &str = "2026-10-17T10-11-12";
pub(in crate::workspace::fleet_git) const CWD_A: &str = "/work/a/repo";
pub(in crate::workspace::fleet_git) const CWD_B: &str = "/work/b/repo";

pub(in crate::workspace::fleet_git) struct Rig {
    pub(in crate::workspace::fleet_git) _dir: tempfile::TempDir,
    pub(in crate::workspace::fleet_git) root: PathBuf,
}

pub(in crate::workspace::fleet_git) fn claude_line(native: &str, n: u32) -> String {
    format!("{{\"sessionId\":\"{native}\",\"type\":\"user\",\"n\":{n}}}\n")
}

pub(in crate::workspace::fleet_git) fn codex_line(native: &str) -> String {
    format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{native}\"}}}}\n")
}

fn my_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

impl Rig {
    pub(in crate::workspace::fleet_git) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        Self { _dir: dir, root }
    }

    pub(in crate::workspace::fleet_git) fn src(&self) -> PathBuf {
        self.root.join("src-home")
    }

    pub(in crate::workspace::fleet_git) fn dst(&self) -> PathBuf {
        self.root.join("dst-home")
    }

    pub(in crate::workspace::fleet_git) fn staged(&self) -> PathBuf {
        self.root.join("staged")
    }

    /// A private file (mode 0600) with its folders.
    pub(in crate::workspace::fleet_git) fn write(&self, path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all_at(bytes, 0).unwrap();
    }

    pub(in crate::workspace::fleet_git) fn claude_main(&self) -> PathBuf {
        self.src()
            .join("projects")
            .join(encode_cwd(CWD_A))
            .join(format!("{NATIVE}.jsonl"))
    }

    pub(in crate::workspace::fleet_git) fn claude_sub(&self, name: &str) -> PathBuf {
        self.src()
            .join("projects")
            .join(encode_cwd(CWD_A))
            .join(NATIVE)
            .join("subagents")
            .join(name)
    }

    pub(in crate::workspace::fleet_git) fn codex_rollout(&self) -> PathBuf {
        self.src()
            .join("sessions/2026/10/17")
            .join(format!("rollout-{STAMP}-{NATIVE}.jsonl"))
    }

    pub(in crate::workspace::fleet_git) fn export(
        &self,
        kind: ProviderKind,
    ) -> Result<Exported, MoveFileError> {
        let (private, limits) = (PrivatePaths::default(), Limits::default());
        let trust = Trust {
            private: &private,
            uid: my_uid(),
            limits: &limits,
            deadline: Instant::now() + Duration::from_secs(30),
        };
        export_files(&self.src(), kind, CWD_A, NATIVE, &trust)
    }

    /// Copies the open files into the staging folder under their staged names.
    pub(in crate::workspace::fleet_git) fn stage(&self, exported: &Exported) {
        fs::create_dir_all(self.staged()).unwrap();
        for (index, open) in exported.files.iter().enumerate() {
            let mut bytes = vec![0_u8; usize::try_from(open.entry.size).unwrap()];
            open.file.read_exact_at(&mut bytes, 0).unwrap();
            self.write(&self.staged().join(staged_name(index)), &bytes);
        }
    }

    pub(in crate::workspace::fleet_git) fn import(
        &self,
        kind: ProviderKind,
        manifest: &TranscriptManifest,
    ) -> Result<Vec<ImportedFile>, MoveFileError> {
        import_files(
            &self.dst(),
            kind,
            CWD_B,
            manifest,
            &self.staged(),
            &mut |_| Ok(()),
        )
    }

    /// Every file and folder below `dir`, relative, sorted.
    pub(in crate::workspace::fleet_git) fn tree(dir: &Path) -> Vec<String> {
        fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
            let Ok(read) = fs::read_dir(dir) else { return };
            for entry in read {
                let path = entry.unwrap().path();
                out.push(path.strip_prefix(base).unwrap().display().to_string());
                if path.is_dir() {
                    walk(base, &path, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }
}

fn trust_of<'a>(private: &'a PrivatePaths, limits: &'a Limits) -> Trust<'a> {
    Trust {
        private,
        uid: my_uid(),
        limits,
        deadline: Instant::now() + Duration::from_secs(30),
    }
}

/// A change of a manifest.
type Change = fn(&mut TranscriptManifest);

fn claude_rig() -> (Rig, Exported) {
    let rig = Rig::new();
    let body = format!("{}{}", claude_line(NATIVE, 1), claude_line(NATIVE, 2));
    rig.write(&rig.claude_main(), body.as_bytes());
    let exported = rig.export(ProviderKind::Claude).unwrap();
    (rig, exported)
}

#[test]
fn support_is_native_for_both_providers_and_branch_only_for_an_unknown_one() {
    assert_eq!(support(ProviderKind::Claude), MoveSupport::Native);
    assert_eq!(support(ProviderKind::Codex), MoveSupport::Native);
    assert_eq!(support(ProviderKind::Unknown), MoveSupport::BranchOnly);
    assert!(Rig::new().export(ProviderKind::Unknown).is_err());
}

#[test]
fn a_moved_claude_transcript_is_written_under_the_project_directory_of_the_target_worktree() {
    let (rig, exported) = claude_rig();
    rig.stage(&exported);
    let placed = rig
        .import(ProviderKind::Claude, &exported.manifest)
        .unwrap();
    let target = rig
        .dst()
        .join("projects")
        .join(encode_cwd(CWD_B))
        .join(format!("{NATIVE}.jsonl"));
    assert_eq!(
        fs::read(&target).unwrap(),
        fs::read(rig.claude_main()).unwrap()
    );
    assert_eq!(placed.len(), 1);
    assert_eq!(placed[0].path, target.display().to_string());
    // The directory of the source folder is not used.
    assert!(!rig.dst().join("projects").join(encode_cwd(CWD_A)).exists());
}

#[test]
fn a_moved_codex_rollout_keeps_its_relative_path_below_sessions() {
    let rig = Rig::new();
    rig.write(&rig.codex_rollout(), codex_line(NATIVE).as_bytes());
    let exported = rig.export(ProviderKind::Codex).unwrap();
    assert_eq!(exported.manifest.files.len(), 1);
    assert_eq!(exported.manifest.files[0].key, "rollout");
    rig.stage(&exported);
    rig.import(ProviderKind::Codex, &exported.manifest).unwrap();
    let target = rig
        .dst()
        .join("sessions/2026/10/17")
        .join(format!("rollout-{STAMP}-{NATIVE}.jsonl"));
    assert_eq!(
        fs::read(&target).unwrap(),
        fs::read(rig.codex_rollout()).unwrap()
    );
}

#[test]
fn an_archived_codex_rollout_is_not_movable() {
    let rig = Rig::new();
    let archived = rig
        .src()
        .join("archived_sessions")
        .join(format!("rollout-{STAMP}-{NATIVE}.jsonl"));
    rig.write(&archived, codex_line(NATIVE).as_bytes());
    let err = rig.export(ProviderKind::Codex).unwrap_err();
    assert_eq!(err, MoveFileError::not_found());
}

#[test]
fn the_target_never_uses_a_path_from_the_source_and_refuses_bad_names_and_dot_dot() {
    let (rig, exported) = claude_rig();
    rig.stage(&exported);
    let good = format!("projects/x/{NATIVE}.jsonl");
    let bad_rels = [
        format!("../x/{NATIVE}.jsonl"),
        format!("projects/../{NATIVE}.jsonl"),
        format!("/etc/{NATIVE}.jsonl"),
        format!("projects//{NATIVE}.jsonl"),
        "projects/x/other.jsonl".to_owned(),
        format!("projects/x\\{NATIVE}.jsonl"),
        format!("projects/x/{NATIVE}.jsonl\0"),
        String::new(),
    ];
    for rel in bad_rels {
        let mut manifest = exported.manifest.clone();
        manifest.files[0].rel = rel.clone();
        let err = rig.import(ProviderKind::Claude, &manifest).unwrap_err();
        assert_eq!(err.code, code::MOVE_RESUME_FAILED, "{rel:?}");
    }
    // An unknown key and a bad subagent id are refused too.
    let mut manifest = exported.manifest.clone();
    manifest.files[0].key = "weird".to_owned();
    assert_eq!(
        rig.import(ProviderKind::Claude, &manifest)
            .unwrap_err()
            .code,
        code::MOVE_RESUME_FAILED
    );
    // The source `rel` is not used as the target path: a good name with any
    // folders lands in the folder that the target computes.
    let mut manifest = exported.manifest.clone();
    manifest.files[0].rel = good;
    rig.import(ProviderKind::Claude, &manifest).unwrap();
    assert!(!rig.dst().join("projects/x").exists());
    assert!(
        rig.dst()
            .join("projects")
            .join(encode_cwd(CWD_B))
            .join(format!("{NATIVE}.jsonl"))
            .is_file()
    );
}

#[test]
fn a_refused_manifest_creates_nothing_in_the_provider_home() {
    let (rig, exported) = claude_rig();
    rig.stage(&exported);
    let mut manifest = exported.manifest.clone();
    manifest.files[0].rel = "../x".to_owned();
    assert!(rig.import(ProviderKind::Claude, &manifest).is_err());
    assert!(!rig.dst().exists());
    // Another provider, a wrong id, no file and too many files.
    let mut other = exported.manifest.clone();
    other.provider = ProviderKind::Codex;
    assert!(rig.import(ProviderKind::Claude, &other).is_err());
    let mut bad_id = exported.manifest.clone();
    bad_id.native_session_id = "../etc".to_owned();
    assert!(rig.import(ProviderKind::Claude, &bad_id).is_err());
    let mut empty = exported.manifest.clone();
    empty.files.clear();
    empty.total = 0;
    assert!(rig.import(ProviderKind::Claude, &empty).is_err());
    let mut twice = exported.manifest.clone();
    twice.files.push(twice.files[0].clone());
    twice.total *= 2;
    assert!(rig.import(ProviderKind::Claude, &twice).is_err());
    assert!(!rig.dst().exists());
}

#[test]
fn a_transfer_with_a_wrong_size_or_sha256_is_refused_and_leaves_no_file() {
    let (rig, exported) = claude_rig();
    rig.stage(&exported);
    let cases: [(&str, Change); 4] = [
        ("size", |m| {
            m.files[0].size += 1;
            m.total += 1;
        }),
        ("sha", |m| m.files[0].sha256 = "a".repeat(64)),
        ("bad hex", |m| m.files[0].sha256 = "Z".repeat(64)),
        ("total", |m| m.total += 1),
    ];
    for (name, change) in cases {
        let mut manifest = exported.manifest.clone();
        change(&mut manifest);
        let err = rig.import(ProviderKind::Claude, &manifest).unwrap_err();
        assert_eq!(err.code, code::MOVE_CHECKSUM, "{name}");
        assert_eq!(
            err.message,
            "The conversation files did not arrive complete. Try again."
        );
        assert!(!rig.dst().exists(), "{name}");
    }
    // A staged file that is shorter than the manifest says.
    let staged = rig.staged().join(staged_name(0));
    fs::OpenOptions::new()
        .write(true)
        .open(&staged)
        .unwrap()
        .set_len(3)
        .unwrap();
    let err = rig
        .import(ProviderKind::Claude, &exported.manifest)
        .unwrap_err();
    assert_eq!(err.code, code::MOVE_CHECKSUM);
    assert!(!rig.dst().exists());
}

#[test]
fn optional_subagent_files_are_dropped_first_when_the_total_is_over_the_cap() {
    let (rig, _) = claude_rig();
    // Under the cap: the subagent files go with the main file, by name.
    rig.write(&rig.claude_sub("agent-b2.jsonl"), b"{\"a\":2}\n");
    rig.write(&rig.claude_sub("agent-a1.jsonl"), b"{\"a\":1}\n");
    rig.write(&rig.claude_sub("agent-a1.meta.json"), b"{}\n");
    rig.write(&rig.claude_sub("notes.txt"), b"not a subagent file\n");
    let kept = rig.export(ProviderKind::Claude).unwrap();
    let keys: Vec<&str> = kept.manifest.files.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(
        keys,
        ["main", "subagent:a1", "subagent_meta:a1", "subagent:b2"]
    );
    assert_eq!(
        kept.manifest.total,
        kept.manifest.files.iter().map(|f| f.size).sum::<u64>()
    );
    // Over the cap: a large subagent file drops all optional files.
    let big = rig.claude_sub("agent-big.jsonl");
    rig.write(&big, b"{}\n");
    fs::OpenOptions::new()
        .write(true)
        .open(&big)
        .unwrap()
        .set_len(64 * 1024 * 1024)
        .unwrap();
    let dropped = rig.export(ProviderKind::Claude).unwrap();
    assert_eq!(dropped.manifest.files.len(), 1);
    assert_eq!(dropped.manifest.files[0].key, "main");
}

#[test]
fn the_subagent_files_are_placed_below_the_session_folder_of_the_target_project() {
    let (rig, _) = claude_rig();
    rig.write(&rig.claude_sub("agent-a1.jsonl"), b"{\"a\":1}\n");
    rig.write(&rig.claude_sub("agent-a1.meta.json"), b"{}\n");
    let exported = rig.export(ProviderKind::Claude).unwrap();
    rig.stage(&exported);
    let placed = rig
        .import(ProviderKind::Claude, &exported.manifest)
        .unwrap();
    assert_eq!(placed.len(), 3);
    let dir = rig
        .dst()
        .join("projects")
        .join(encode_cwd(CWD_B))
        .join(NATIVE)
        .join("subagents");
    assert!(dir.join("agent-a1.jsonl").is_file());
    assert!(dir.join("agent-a1.meta.json").is_file());
    // A bad subagent id is refused.
    let mut manifest = exported.manifest.clone();
    manifest.files[1].key = "subagent:../x".to_owned();
    assert_eq!(
        rig.import(ProviderKind::Claude, &manifest)
            .unwrap_err()
            .code,
        code::MOVE_RESUME_FAILED
    );
}

#[test]
fn the_target_refuses_to_overwrite_an_existing_transcript() {
    let (rig, exported) = claude_rig();
    rig.stage(&exported);
    let target = rig
        .dst()
        .join("projects")
        .join(encode_cwd(CWD_B))
        .join(format!("{NATIVE}.jsonl"));
    rig.write(&target, b"old conversation\n");
    let before = Rig::tree(&rig.dst());
    let err = rig
        .import(ProviderKind::Claude, &exported.manifest)
        .unwrap_err();
    assert_eq!(err.code, code::MOVE_RESUME_FAILED);
    assert!(err.message.contains("exists already"), "{err}");
    assert_eq!(fs::read(&target).unwrap(), b"old conversation\n");
    assert_eq!(Rig::tree(&rig.dst()), before, "no other file appeared");
    // A link in the place of the file counts as existing too.
    fs::remove_file(&target).unwrap();
    std::os::unix::fs::symlink("/nonexistent", &target).unwrap();
    assert!(
        rig.import(ProviderKind::Claude, &exported.manifest)
            .is_err()
    );
}

#[test]
fn moved_files_have_mode_0600_and_new_directories_0700() {
    let (rig, exported) = claude_rig();
    rig.stage(&exported);
    rig.import(ProviderKind::Claude, &exported.manifest)
        .unwrap();
    let project = rig.dst().join("projects").join(encode_cwd(CWD_B));
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&rig.dst()), 0o700);
    assert_eq!(mode(&rig.dst().join("projects")), 0o700);
    assert_eq!(mode(&project), 0o700);
    assert_eq!(mode(&project.join(format!("{NATIVE}.jsonl"))), 0o600);
    // One name, one link: the temporary name is gone.
    let file = fs::metadata(project.join(format!("{NATIVE}.jsonl"))).unwrap();
    assert_eq!(file.nlink(), 1);
    assert_eq!(Rig::tree(&project), [format!("{NATIVE}.jsonl")]);
}

#[test]
fn a_transcript_whose_embedded_id_differs_from_the_native_id_is_refused() {
    let (rig, exported) = claude_rig();
    // The staged main file names another conversation, with a matching hash.
    let body = claude_line("another-conversation", 1);
    fs::create_dir_all(rig.staged()).unwrap();
    rig.write(&rig.staged().join(staged_name(0)), body.as_bytes());
    let mut manifest = exported.manifest.clone();
    manifest.files[0].size = body.len() as u64;
    manifest.total = body.len() as u64;
    manifest.files[0].sha256 = crate::workspace::fleet_git::move_export::hex(
        ring::digest::digest(&ring::digest::SHA256, body.as_bytes()).as_ref(),
    );
    let err = rig.import(ProviderKind::Claude, &manifest).unwrap_err();
    assert_eq!(err.code, code::MOVE_RESUME_FAILED);
    assert!(err.message.contains("another conversation"), "{err}");
    assert!(!rig.dst().exists());
}

#[test]
fn the_import_records_each_file_before_it_gets_its_final_name() {
    let (rig, exported) = claude_rig();
    rig.write(&rig.claude_sub("agent-a1.jsonl"), b"{\"a\":1}\n");
    let exported2 = rig.export(ProviderKind::Claude).unwrap();
    drop(exported);
    rig.stage(&exported2);
    let mut seen: Vec<ImportedFile> = Vec::new();
    let placed = import_files(
        &rig.dst(),
        ProviderKind::Claude,
        CWD_B,
        &exported2.manifest,
        &rig.staged(),
        &mut |file| {
            assert!(
                fs::symlink_metadata(&file.path).is_err(),
                "the final name does not exist yet"
            );
            seen.push(file.clone());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(seen, placed);
    for file in &placed {
        let meta = fs::metadata(&file.path).unwrap();
        assert_eq!((meta.dev(), meta.ino()), (file.dev, file.ino));
    }
}

#[test]
fn a_failing_record_stops_the_import_and_leaves_no_temporary_file() {
    let (rig, exported) = claude_rig();
    rig.stage(&exported);
    let err = import_files(
        &rig.dst(),
        ProviderKind::Claude,
        CWD_B,
        &exported.manifest,
        &rig.staged(),
        &mut |_| Err("the record failed".to_owned()),
    )
    .unwrap_err();
    assert_eq!(err.code, "internal");
    let project = rig.dst().join("projects").join(encode_cwd(CWD_B));
    assert!(Rig::tree(&project).is_empty(), "{:?}", Rig::tree(&project));
}

#[test]
fn the_history_reader_finds_a_moved_session_on_the_target() {
    let (private, limits) = (PrivatePaths::default(), Limits::default());
    // Claude: the project folder of the target worktree.
    let (rig, exported) = claude_rig();
    rig.stage(&exported);
    rig.import(ProviderKind::Claude, &exported.manifest)
        .unwrap();
    let opened = locate::locate(
        &rig.dst(),
        ProviderKind::Claude,
        CWD_B,
        NATIVE,
        &trust_of(&private, &limits),
    )
    .unwrap();
    assert_eq!(opened.size, exported.manifest.total);
    // Codex: found by its id below `sessions/`.
    let rig = Rig::new();
    rig.write(&rig.codex_rollout(), codex_line(NATIVE).as_bytes());
    let exported = rig.export(ProviderKind::Codex).unwrap();
    rig.stage(&exported);
    rig.import(ProviderKind::Codex, &exported.manifest).unwrap();
    let opened = locate::locate(
        &rig.dst(),
        ProviderKind::Codex,
        CWD_B,
        NATIVE,
        &trust_of(&private, &limits),
    )
    .unwrap();
    assert_eq!(opened.size, exported.manifest.total);
}

#[test]
fn a_codex_resume_after_a_move_always_sends_the_target_cwd() {
    use fleetdeck_providers::codex::{
        ClientInfo, CodexCore, CoreConfig, Output, SessionSpec, capabilities,
    };
    let mut core = CodexCore::new(CoreConfig {
        provider_instance: "codex".into(),
        cwd: CWD_B.to_owned(),
        capabilities: capabilities(false),
        resumed: Some(true),
        client: ClientInfo::fleetdeck(),
        request_models: false,
    });
    let spec = SessionSpec {
        cwd: Some(CWD_B.to_owned()),
        resume: Some(NATIVE.to_owned()),
        ..SessionSpec::default()
    };
    let frames: Vec<serde_json::Value> = core
        .open_thread(&spec)
        .unwrap()
        .into_iter()
        .filter_map(|o| match o {
            Output::Write(v) => Some(v),
            _ => None,
        })
        .collect();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["method"], "thread/resume");
    assert_eq!(frames[0]["params"]["threadId"], NATIVE);
    assert_eq!(frames[0]["params"]["cwd"], CWD_B);
}
