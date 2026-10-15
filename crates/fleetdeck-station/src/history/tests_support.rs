//! Helpers of the history tests: a rig with a store, a provider home, the
//! snapshot directory and builders for transcript records.

use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::{SessionHistory, SessionHistoryQuery};
use fleetdeck_testkit::Det;

use super::{History, Limits, index, locate};
use crate::manager::Policy;
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
