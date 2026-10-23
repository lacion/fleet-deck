//! The rig of the usage recount tests: a session manager on an in-memory
//! store, a provider home in a temp directory, and builders for sessions and
//! provider records.

use std::collections::BTreeMap;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::capability::Capabilities;
use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand,
};
use fleetdeck_proto::event::{ProviderKind, ProviderRefs, Usage, UsageSource};
use fleetdeck_proto::{CommandId, Event, EventBody, RejectCode, SessionId};
use fleetdeck_testkit::Det;

use crate::history::locate::encode_cwd;
use crate::manager::decide::result;
use crate::manager::{Adapters, Policy, SessionManager};
use crate::store::transcripts::FirstEffect;
use crate::store::{
    Caller, Decided, NewEvent, NewSession, OfflineApprovalPolicy, ReadResult, Store, StoreConfig,
};
use crate::workspace::PrivatePaths;

pub(super) const NATIVE: &str = "11111111-2222-3333-4444-555555555555";
pub(super) const OTHER: &str = "66666666-7777-8888-9999-aaaaaaaaaaaa";
pub(super) const CWD: &str = "/work/project";

pub(super) struct Fx {
    _tmp: tempfile::TempDir,
    pub(super) root: PathBuf,
    _det: Det,
    pub(super) manager: Arc<SessionManager>,
    next: u32,
}

/// The old count that every session of the rig starts with.
pub(super) fn old_count() -> Usage {
    Usage {
        input_tokens: Some(500),
        output_tokens: Some(50),
        cost_usd: Some(1.25),
        ..Usage::default()
    }
}

pub(super) fn instance_of(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Codex => "codex",
        _ => "claude",
    }
}

pub(super) fn started(kind: ProviderKind, native: Option<&str>) -> NewEvent {
    let mut refs = ProviderRefs::new();
    if let Some(native) = native {
        let key = if kind == ProviderKind::Codex {
            "thread_id"
        } else {
            "session_id"
        };
        refs.insert(key.to_owned(), serde_json::Value::from(native));
    }
    Event::SessionStarted {
        provider: kind,
        provider_instance: instance_of(kind).into(),
        provider_version: None,
        model: (kind == ProviderKind::Codex).then(|| "gpt-5".to_owned()),
        cwd: CWD.to_owned(),
        mode: None,
        capabilities: Box::new(Capabilities::minimal()),
        resumed: None,
        provider_refs: refs,
        title: None,
        branch: None,
        repo: None,
    }
    .into()
}

pub(super) fn ended() -> NewEvent {
    Event::SessionEnded {
        reason: "process_exit".to_owned(),
        exit_code: Some(0),
    }
    .into()
}

pub(super) fn usage_event(usage: Usage) -> NewEvent {
    Event::UsageUpdated {
        usage,
        rate_limits: Vec::new(),
    }
    .into()
}

pub(super) fn write_private(path: &Path, text: &str) {
    use std::io::Write;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

pub(super) fn claude_user() -> String {
    format!("{{\"type\":\"user\",\"sessionId\":\"{NATIVE}\"}}\n")
}

pub(super) fn cost_state(input: u64, cost: f64) -> String {
    format!(
        "{{\"type\":\"cost-state\",\"sessionId\":\"{NATIVE}\",\"totalCostUSD\":{cost},\
         \"modelUsage\":{{\"claude-haiku-5-5\":{{\"inputTokens\":{input},\"outputTokens\":2,\
         \"cacheReadInputTokens\":3,\"cacheCreationInputTokens\":4,\"costUSD\":{cost}}}}}}}\n"
    )
}

pub(super) fn codex_meta() -> String {
    format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{NATIVE}\",\"cwd\":\"/w\"}}}}\n")
}

pub(super) fn codex_context(model: &str) -> String {
    format!("{{\"type\":\"turn_context\",\"payload\":{{\"model\":\"{model}\"}}}}\n")
}

pub(super) fn codex_count(input: u64, cached: u64, output: u64) -> String {
    format!(
        "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"info\":\
         {{\"total_token_usage\":{{\"input_tokens\":{input},\"cached_input_tokens\":{cached},\
         \"output_tokens\":{output}}}}}}}}}\n"
    )
}

impl Fx {
    pub(super) async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let det = Det::new();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        let mut env = ChildEnv::empty();
        env.set("HOME", &root).unwrap();
        let policy = Policy::new(
            Vec::new(),
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([
                ("claude".into(), ProviderKind::Claude),
                ("codex".into(), ProviderKind::Codex),
            ]),
        )
        .with_setup_envs(BTreeMap::from([
            ("claude".into(), env.clone()),
            ("codex".into(), env),
        ]));
        // The manager starts before any session: its recovery ends the
        // sessions that have no process.
        let manager = SessionManager::start(
            store,
            policy,
            Adapters::new(),
            det.ids(),
            Arc::new(PrivatePaths::default()),
        )
        .await
        .unwrap();
        Self {
            _tmp: tmp,
            root,
            _det: det,
            manager: Arc::new(manager),
            next: 0,
        }
    }

    pub(super) fn store(&self) -> &Store {
        self.manager.store()
    }

    pub(super) fn claude_file(&self) -> PathBuf {
        self.root
            .join(".claude/projects")
            .join(encode_cwd(CWD))
            .join(format!("{NATIVE}.jsonl"))
    }

    pub(super) fn codex_file(&self) -> PathBuf {
        self.root
            .join(".codex/sessions/2026/10/14")
            .join(format!("rollout-2026-10-14T10-00-00-{NATIVE}.jsonl"))
    }

    /// An ended session with an old count and a confirmed first `start`
    /// effect.
    pub(super) async fn session(
        &self,
        id: &str,
        kind: ProviderKind,
        native: Option<&str>,
    ) -> SessionId {
        let session = self.bare_session(id, kind, native).await;
        self.effect(id, "start", false, "confirmed").await;
        session
    }

    /// An ended session with an old count and no effect row.
    pub(super) async fn bare_session(
        &self,
        id: &str,
        kind: ProviderKind,
        native: Option<&str>,
    ) -> SessionId {
        self.store()
            .create_session(NewSession {
                session_id: id.into(),
                provider: kind,
                provider_instance: instance_of(kind).into(),
                cwd: CWD.to_owned(),
                worktree: None,
                branch: None,
                mode: None,
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        self.store()
            .append(
                id.into(),
                vec![started(kind, native), usage_event(old_count()), ended()],
            )
            .await
            .unwrap();
        id.into()
    }

    pub(super) async fn effect(&self, id: &str, kind: &str, import: bool, state: &str) {
        let payload = format!("{{\"kind\":\"{kind}\",\"import\":{import}}}");
        self.next_sql(format!(
            "INSERT INTO effects (effect_id, session_id, kind, payload, state, created_at, updated_at)
             VALUES ('e-{id}-{kind}-{state}-{import}', '{id}', '{kind}', '{payload}', '{state}', 1, 1)"
        ))
        .await;
    }

    pub(super) async fn next_sql(&self, sql: String) {
        self.store().exec_for_tests(sql).await;
    }

    pub(super) async fn append(&self, id: &str, event: NewEvent) {
        self.store().append(id.into(), vec![event]).await.unwrap();
    }

    pub(super) fn envelope(&mut self, session: &str) -> CommandEnvelope {
        self.next += 1;
        CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("recount-{}", self.next)),
            target_incarnation: None,
            retry_until: self.store().now_ms() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Board,
            command: SessionCommand::UsageRecount {
                session_id: session.into(),
            },
        }
    }

    pub(super) async fn recount(&mut self, session: &str) -> Decided {
        let envelope = self.envelope(session);
        self.manager
            .submit(envelope, Caller::local())
            .await
            .unwrap()
    }

    /// `RecountDb.origin` of a session, read in a command transaction.
    pub(super) async fn recount_origin(&mut self, id: &str) -> FirstEffect {
        let slot = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&slot);
        let session: SessionId = id.into();
        let envelope = self.envelope(id);
        self.store()
            .decide(envelope, Caller::local(), move |d| {
                let db = d.recount_db(&session, ProviderKind::Claude, None)?;
                *seen.lock().unwrap() = Some(db.origin);
                Ok(result(&session, None))
            })
            .await
            .unwrap();
        let origin = slot.lock().unwrap().take();
        origin.expect("the decision ran")
    }

    pub(super) async fn all_events(&self) -> Vec<Event> {
        match self.store().read_after(0, 100_000).await.unwrap() {
            ReadResult::Events { events, .. } => events
                .into_iter()
                .filter_map(|e| match e.event {
                    EventBody::Known(event) => Some(event),
                    EventBody::Unknown { .. } => None,
                })
                .collect(),
            ReadResult::SnapshotRequired { .. } => panic!("snapshot required"),
        }
    }

    pub(super) async fn sets(&self) -> Vec<Event> {
        self.all_events()
            .await
            .into_iter()
            .filter(|e| matches!(e, Event::UsageSet { .. }))
            .collect()
    }

    /// The decision is a refusal with `code`, and nothing was recounted.
    pub(super) async fn refused(&mut self, session: &str, class: RejectCode, code: &str) -> String {
        let decided = self.recount(session).await;
        assert_eq!(
            decided.receipt.state,
            ReceiptState::Rejected,
            "{:?}",
            decided.receipt
        );
        let reject = decided.receipt.reject.unwrap();
        assert_eq!(reject.code, class, "{}", reject.message);
        assert!(
            reject.message.starts_with(&format!("{code}: ")),
            "{}",
            reject.message
        );
        assert!(self.sets().await.is_empty());
        reject.message
    }

    pub(super) async fn accepted(&mut self, session: &str) -> Event {
        let decided = self.recount(session).await;
        assert_eq!(
            decided.receipt.state,
            ReceiptState::Accepted,
            "{:?}",
            decided.receipt.reject
        );
        let mut sets = self.sets().await;
        assert_eq!(sets.len(), 1);
        sets.remove(0)
    }
}

/// The parts of a `usage.set` event.
pub(super) fn set_parts(event: Event) -> (Usage, Usage, UsageSource, CommandId) {
    match event {
        Event::UsageSet {
            usage,
            previous,
            source,
            command_id,
        } => (usage, *previous, source, command_id),
        other => panic!("{other:?}"),
    }
}
