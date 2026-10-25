//! Helpers of the history tests: a rig with a store, a provider home, the
//! snapshot directory and builders for transcript records.

use std::collections::BTreeMap;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::{
    HistoryState, HistoryUnavailable, SessionHistory, SessionHistoryQuery,
};
use fleetdeck_proto::native::{
    NativePreview, NativePreviewQuery, NativeSession, NativeSessionList, NativeSessionsQuery,
    TranscriptPin,
};
use fleetdeck_testkit::Det;

use super::browse::{Budget, native_sessions};
use super::browse_preview::native_preview;
use super::locate::encode_cwd;
use super::trust::{Mounts, supported_fs};
use super::{History, HistoryError, Limits, index, locate};
use crate::manager::Policy;
use crate::manager::policy::SetupEnvs;
use crate::store::{NewSession, OfflineApprovalPolicy, Store, StoreConfig};
use crate::workspace::PrivatePaths;

pub(super) const NATIVE: &str = "11111111-2222-3333-4444-555555555555";

pub(super) fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/transcripts")
        .join(name)
}

/// A store, a provider home and a snapshot directory in a temp directory.
pub(super) struct Rig {
    pub _tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub store: Store,
    pub history: History,
    pub private: Arc<PrivatePaths>,
    pub policy: Policy,
    pub kind: ProviderKind,
}

impl Rig {
    pub(super) async fn new(kind: ProviderKind) -> Self {
        Self::with_limits(kind, Limits::default()).await
    }

    pub(super) async fn with_limits(kind: ProviderKind, limits: Limits) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let history = History::with_limits(limits);
        history.set_dir(root.join("data/history"));
        let det = Det::new();
        let store = Store::open_in_memory_with_history(
            det.clock(),
            det.ids(),
            StoreConfig::default(),
            history.clone(),
        )
        .await
        .unwrap();
        store.recover(Vec::new()).await.unwrap();
        let mut env = ChildEnv::empty();
        env.set("HOME", &root).unwrap();
        let mut envs = crate::manager::policy::SetupEnvs::new();
        envs.insert("claude".into(), env.clone());
        envs.insert("codex".into(), env);
        let policy = Policy::new(
            Vec::new(),
            4,
            OfflineApprovalPolicy::Wait,
            [
                ("claude".into(), ProviderKind::Claude),
                ("codex".into(), ProviderKind::Codex),
            ]
            .into_iter()
            .collect(),
        )
        .with_setup_envs(envs);
        Self {
            _tmp: tmp,
            root,
            store,
            history,
            private: Arc::new(PrivatePaths::default()),
            policy,
            kind,
        }
    }

    /// The provider home that the policy environment resolves to.
    pub(super) fn home(&self) -> PathBuf {
        self.root.join(match self.kind {
            ProviderKind::Codex => ".codex",
            _ => ".claude",
        })
    }

    /// The working directory of the sessions of the rig.
    pub(super) fn cwd(&self) -> String {
        self.root
            .join("work/project")
            .to_string_lossy()
            .into_owned()
    }

    /// Creates a session row with its facts.
    pub(super) async fn session(&self, id: &str, native: Option<&str>) -> SessionId {
        let (provider, instance) = match self.kind {
            ProviderKind::Codex => (ProviderKind::Codex, "codex"),
            _ => (ProviderKind::Claude, "claude"),
        };
        self.store
            .create_session(NewSession {
                session_id: id.into(),
                provider,
                provider_instance: instance.into(),
                cwd: self.cwd(),
                worktree: None,
                branch: None,
                mode: None,
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        if let Some(native) = native {
            self.store
                .exec_for_tests(format!(
                    "UPDATE sessions SET native_session_id = '{native}' WHERE session_id = '{id}'"
                ))
                .await;
        }
        id.into()
    }

    /// Adds the first effect of a session (`start` or `resume` import).
    pub(super) async fn first_effect(&self, id: &str, kind: &str, import: bool) {
        let payload = format!("{{\"kind\":\"{kind}\",\"import\":{import}}}");
        self.store
            .exec_for_tests(format!(
                "INSERT INTO effects (effect_id, session_id, kind, payload, state, created_at, updated_at)
                 VALUES ('e-{id}', '{id}', '{kind}', '{payload}', 'confirmed', 1, 1)"
            ))
            .await;
    }

    /// Adds a process row that started at `at` (Unix ms).
    pub(super) async fn process(&self, id: &str, at: i64) {
        self.store
            .exec_for_tests(format!(
                "INSERT INTO processes (process_incarnation, session_id, state, started_at)
                 VALUES ('p-{id}', '{id}', 'dead', {at})"
            ))
            .await;
    }

    /// Writes a provider transcript for the session of the rig and returns
    /// its path.
    pub(super) fn write(&self, native: &str, lines: &[String]) -> PathBuf {
        let path = self.transcript_path(native);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_private(&path, &join(lines));
        path
    }

    pub(super) fn transcript_path(&self, native: &str) -> PathBuf {
        match self.kind {
            ProviderKind::Codex => self
                .home()
                .join("sessions/2026/10/14")
                .join(format!("rollout-2026-10-14T10-00-00-{native}.jsonl")),
            _ => self
                .home()
                .join("projects")
                .join(locate::encode_cwd(&self.cwd()))
                .join(format!("{native}.jsonl")),
        }
    }

    pub(super) async fn ask(
        &self,
        id: &str,
        before: Option<&str>,
        limit: Option<u32>,
        member: bool,
    ) -> Result<SessionHistory, super::HistoryError> {
        super::answer(
            &self.store,
            &self.policy,
            &self.private,
            SessionHistoryQuery {
                session_id: id.into(),
                before: before.map(str::to_owned),
                limit,
                member,
            },
        )
        .await
    }
}

pub(super) fn join(lines: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in lines {
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }
    out
}

pub(super) fn write_private(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

/// A Claude user record.
pub(super) fn claude_user(n: u32, text: &str) -> String {
    serde_json::json!({
        "type": "user", "uuid": format!("u-{n}"), "sessionId": NATIVE,
        "timestamp": format!("2026-10-14T10:00:{:02}.000Z", n % 60),
        "message": {"role": "user", "content": text},
    })
    .to_string()
}

/// A Claude assistant record with one text block.
pub(super) fn claude_assistant(n: u32, text: &str) -> String {
    serde_json::json!({
        "type": "assistant", "uuid": format!("a-{n}"), "sessionId": NATIVE,
        "timestamp": format!("2026-10-14T10:00:{:02}.000Z", n % 60),
        "message": {"role": "assistant", "content": [{"type": "text", "text": text}]},
    })
    .to_string()
}

/// A Codex session_meta record.
pub(super) fn codex_meta(native: &str) -> String {
    serde_json::json!({
        "type": "session_meta", "timestamp": "2026-10-14T10:00:00.000Z",
        "payload": {"id": native, "cwd": "/w"},
    })
    .to_string()
}

pub(super) fn codex_message(n: u32, role: &str, text: &str) -> String {
    let block = if role == "user" {
        "input_text"
    } else {
        "output_text"
    };
    serde_json::json!({
        "type": "response_item", "timestamp": format!("2026-10-14T10:00:{:02}.000Z", n % 60),
        "payload": {"type": "message", "id": format!("m-{n}"), "role": role,
                    "content": [{"type": block, "text": text}]},
    })
    .to_string()
}

/// All items of a snapshot file, oldest first, through the production index
/// and page code.
pub(super) fn items_of(kind: ProviderKind, bytes: &[u8]) -> (SessionHistory, index::Index) {
    use std::io::Write;
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    let sha = index::hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref());
    let limits = Limits {
        answer_max: usize::MAX / 2,
        ..Limits::default()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let idx = index::build(
        &file,
        kind,
        bytes.len() as u64,
        &sha,
        &limits,
        deadline,
        None,
    )
    .unwrap();
    let query = index::PageQuery {
        kind,
        hash16: &sha[..16],
        before: None,
        limit: usize::MAX / 2,
        member: false,
    };
    let page = index::page(&file, &idx, &query, &limits).unwrap();
    (page, idx)
}

// ----------------------------------------------------- native browser (B7)

pub(super) const ID_A: &str = "aaaaaaaa-1111-2222-3333-444444444444";
pub(super) const ID_B: &str = "bbbbbbbb-1111-2222-3333-444444444444";
pub(super) const ID_C: &str = "cccccccc-1111-2222-3333-444444444444";
pub(super) const CODEX_ID: &str = "01a13f28-e384-7ed1-aee0-423548cce681";

pub(super) struct B {
    pub rig: Rig,
    pub policy: Arc<Policy>,
    pub cwd: String,
}

pub(super) fn env_of(root: &Path, config: Option<&str>) -> ChildEnv {
    let mut env = ChildEnv::empty();
    env.set("HOME", root).unwrap();
    if let Some(config) = config {
        env.set("CLAUDE_CONFIG_DIR", config).unwrap();
    }
    env
}

pub(super) fn policy_of(root: &Path, extra: &[(&str, Option<&str>)]) -> Arc<Policy> {
    let mut envs = SetupEnvs::new();
    let mut instances = BTreeMap::new();
    envs.insert("claude".into(), env_of(root, None));
    envs.insert("codex".into(), env_of(root, None));
    instances.insert("claude".into(), ProviderKind::Claude);
    instances.insert("codex".into(), ProviderKind::Codex);
    for (name, config) in extra {
        envs.insert((*name).into(), env_of(root, *config));
        instances.insert((*name).into(), ProviderKind::Claude);
    }
    Arc::new(
        Policy::new(
            vec![root.join("work")],
            4,
            OfflineApprovalPolicy::Wait,
            instances,
        )
        .with_setup_envs(envs),
    )
}

/// A rig with a work directory, or `None` (printed) when the temp
/// directory is not on a supported filesystem and verifiable mount.
pub(super) async fn rig(limits: Limits, extra: &[(&str, Option<&str>)]) -> Option<B> {
    let rig = Rig::with_limits(ProviderKind::Claude, limits).await;
    let dir = std::fs::File::open(&rig.root).unwrap();
    let dev = dir.metadata().unwrap().dev();
    if !supported_fs(&dir) || !Mounts::load().verifiable(dev, &rig.root) {
        eprintln!("skipped: the temp directory is not on a supported filesystem and mount");
        return None;
    }
    let cwd = rig.root.join("work/project");
    std::fs::create_dir_all(&cwd).unwrap();
    let policy = policy_of(&rig.root, extra);
    Some(B {
        cwd: cwd.to_string_lossy().into_owned(),
        policy,
        rig,
    })
}

pub(super) fn claude_rec(id: &str, cwd: &str, n: u32, text: &str) -> String {
    serde_json::json!({
        "type": "user", "uuid": format!("u-{n}"), "sessionId": id, "cwd": cwd,
        "timestamp": format!("2026-10-14T10:00:{:02}.000Z", n % 60),
        "message": {"role": "user", "content": text},
    })
    .to_string()
}

pub(super) fn title_rec(id: &str, title: &str) -> String {
    serde_json::json!({"type": "ai-title", "aiTitle": title, "sessionId": id}).to_string()
}

pub(super) fn codex_meta_rec(id: &str, cwd: &str, parent: Option<&str>) -> String {
    serde_json::json!({
        "type": "session_meta", "timestamp": "2026-10-14T10:00:00.000Z",
        "payload": {"id": id, "cwd": cwd, "parent_thread_id": parent},
    })
    .to_string()
}

pub(super) fn turn_rec(cwd: &str) -> String {
    serde_json::json!({
        "type": "turn_context", "timestamp": "2026-10-14T10:00:01.000Z",
        "payload": {"turn_id": "t1", "cwd": cwd},
    })
    .to_string()
}

impl B {
    pub(super) fn home(&self) -> PathBuf {
        self.rig.root.join(".claude")
    }

    pub(super) fn codex_home(&self) -> PathBuf {
        self.rig.root.join(".codex")
    }

    pub(super) fn claude_path(&self, cwd: &str, id: &str) -> PathBuf {
        self.home()
            .join("projects")
            .join(encode_cwd(cwd))
            .join(format!("{id}.jsonl"))
    }

    pub(super) fn put_claude(&self, id: &str, lines: &[String]) -> PathBuf {
        let path = self.claude_path(&self.cwd, id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_private(&path, &join(lines));
        path
    }

    pub(super) fn codex_path(&self, id: &str) -> PathBuf {
        self.codex_home()
            .join("sessions/2026/10/14")
            .join(format!("rollout-2026-10-14T10-00-00-{id}.jsonl"))
    }

    pub(super) fn put_codex(&self, id: &str, lines: &[String]) -> PathBuf {
        let path = self.codex_path(id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_private(&path, &join(lines));
        path
    }

    pub(super) fn good_claude(&self, id: &str) -> PathBuf {
        self.put_claude(
            id,
            &[
                title_rec(id, "A title"),
                claude_rec(id, &self.cwd, 1, "the first prompt"),
                claude_rec(id, &self.cwd, 2, "the second prompt"),
            ],
        )
    }

    pub(super) async fn list(&self) -> NativeSessionList {
        self.list_with(q(), Budget::default()).await.unwrap()
    }

    pub(super) async fn list_with(
        &self,
        query: NativeSessionsQuery,
        budget: Budget,
    ) -> Result<NativeSessionList, HistoryError> {
        native_sessions(
            &self.rig.store,
            &self.policy,
            &self.rig.private,
            query,
            budget,
        )
        .await
    }
}

pub(super) fn entry<'a>(list: &'a NativeSessionList, id: &str) -> &'a NativeSession {
    list.sessions
        .iter()
        .find(|s| s.native_id == id)
        .unwrap_or_else(|| panic!("{id} is not in the list"))
}

pub(super) fn pin_of(path: &Path, home: &Path) -> TranscriptPin {
    let meta = std::fs::metadata(path).unwrap();
    TranscriptPin {
        home: home.to_string_lossy().into_owned(),
        dev: meta.dev(),
        ino: meta.ino(),
    }
}

pub(super) fn q() -> NativeSessionsQuery {
    NativeSessionsQuery {
        provider_instance: None,
        limit: None,
    }
}

pub(super) fn query_of(instance: &str, id: &str, pin: TranscriptPin) -> NativePreviewQuery {
    NativePreviewQuery {
        provider_instance: instance.into(),
        native_id: id.to_owned(),
        pin,
    }
}

impl B {
    pub(super) async fn preview(
        &self,
        query: NativePreviewQuery,
    ) -> Result<NativePreview, HistoryError> {
        native_preview(
            &self.rig.store,
            &self.policy,
            &self.rig.private,
            query,
            Budget::default(),
        )
        .await
    }
}

pub(super) fn unavailable(p: &NativePreview, reason: HistoryUnavailable) {
    assert_eq!(p.state, HistoryState::Unavailable { reason }, "{p:?}");
    assert!(p.items.is_empty());
}
