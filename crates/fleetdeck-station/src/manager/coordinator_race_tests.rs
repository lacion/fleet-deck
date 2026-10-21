//! MCP P4b PRE-PR R1 (station): a `session.coordinator_set` cannot slip
//! between the record read of a process start and the installation of its
//! binding, and a renewal of a grant with a record changes memory only after
//! its commit.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, GrantKind, GrantScope, RETRY_WINDOW_MS, ReceiptState,
    SessionCommand,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::{CommandId, Event, EventBody};
use fleetdeck_providers::host::ProcessSpec;
use fleetdeck_providers::host::shim::host::{AckMode, ShimHost, ShimHostConfig};
use fleetdeck_testkit::Det;
use tokio::sync::Notify;

use super::epochs_tests::{GRANT, Link, grant, session_spec, verified};
use super::start_tools;
use crate::coordinator::{Binding, BindingState, Bindings, BridgeEndpoint, ToolClass, mint};
use crate::manager::coordinator_spec_tests::coordinator_spec;
use crate::manager::shims::{LaunchError, launch_shim_for};
use crate::manager::{Adapters, Holders, Policy, SessionManager};
use crate::store::session_grants::{GrantRecord, RecordState};
use crate::store::shim_bindings::{BindingRow, RowState};
use crate::store::{
    Caller, Decided, Dispatch, NewSession, OfflineApprovalPolicy, ReadResult, Store, StoreConfig,
};

const FAR: i64 = 4_102_444_800_000;
const BINDING: &str = "s1:grant-1";
const PAUSE: Duration = Duration::from_millis(150);

struct Rig {
    det: Det,
    manager: Arc<SessionManager>,
    bindings: Bindings,
    deck: Caller,
    next: u32,
    tmp: tempfile::TempDir,
}

impl Rig {
    /// A station with the session `s1`, a deck link that negotiated
    /// `coordinator_live.v1`, and the bridge of the shim host.
    async fn new() -> Self {
        let det = Det::new();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        store
            .create_session(NewSession {
                session_id: "s1".into(),
                provider: ProviderKind::Claude,
                provider_instance: "claude".into(),
                cwd: "/w".to_owned(),
                worktree: None,
                branch: None,
                mode: None,
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let bindings = Bindings::default().with_handoff(verified);
        bindings.record_version(ProviderKind::Claude, "2.1.296".to_owned());
        bindings.set_endpoint("http://127.0.0.1:41234/mcp".to_owned());
        bindings.set_bridge(BridgeEndpoint {
            program: "/station/bin/current".into(),
            socket: "/run/fleetdeck/mcp.s".into(),
        });
        bindings.set_deck(1, Arc::new(Link { live: true }));
        let policy = Policy::new(
            vec![tmp.path().join("projects")],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
        );
        let manager = SessionManager::start_with_holders(
            store,
            policy,
            Holders {
                coordinator: bindings.clone(),
                ..Holders::default()
            },
            Adapters::new(),
            det.ids(),
            Arc::new(crate::workspace::PrivatePaths::default()),
        )
        .await
        .unwrap();
        let key = Identity::generate().unwrap().public_key();
        manager
            .store()
            .pin_peer(crate::store::peers::PairedKey {
                key,
                role: fleetdeck_link::tls::Role::Deck,
                scopes: [Scope::Read, Scope::Drive].into_iter().collect(),
                dial_url: None,
            })
            .await
            .unwrap();
        Self {
            det,
            manager: Arc::new(manager),
            bindings,
            deck: Caller::peer(key.fingerprint(), Scope::Drive),
            next: 0,
            tmp,
        }
    }

    fn store(&self) -> &Store {
        self.manager.store()
    }

    fn envelope(&mut self, command: SessionCommand) -> CommandEnvelope {
        self.next += 1;
        CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("race-{}", self.next)),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Board,
            command,
        }
    }

    /// Writes the record of `s1` as a start would.
    async fn put(&mut self, generation: u32) {
        let record = GrantRecord {
            session_id: "s1".into(),
            grant_id: GRANT.to_owned(),
            seq: 1,
            spec: Some(coordinator_spec()),
            generation,
            state: RecordState::Attached,
            ended_reason: None,
            updated_at: 1,
        };
        let envelope = self.envelope(SessionCommand::CoordinatorRevoke {
            session_id: "s1".into(),
            grant_id: "x".to_owned(),
        });
        self.store()
            .decide(envelope, Caller::local(), move |d| {
                d.session_grant_put(&record)?;
                Ok(crate::manager::decide::result(&"s1".into(), None))
            })
            .await
            .unwrap();
    }

    async fn record(&self) -> GrantRecord {
        self.store()
            .session_grant("s1".into())
            .await
            .unwrap()
            .unwrap()
    }

    /// The submit of a give of the grant with the classes `read` and `mail`
    /// (sequence 2), as a task.
    fn wide_set(&self) -> tokio::task::JoinHandle<Decided> {
        let mut spec = coordinator_spec();
        spec.classes = vec!["read".to_owned(), "mail".to_owned()];
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from("race-set"),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Board,
            command: SessionCommand::CoordinatorSet {
                session_id: "s1".into(),
                seq: 2,
                grant_id: GRANT.to_owned(),
                spec: Some(Box::new(spec)),
            },
        };
        let (manager, deck) = (Arc::clone(&self.manager), self.deck.clone());
        tokio::spawn(async move { manager.submit(envelope, deck).await.unwrap() })
    }

    async fn renew(&mut self, generation: u32) -> Result<Decided, crate::store::StoreError> {
        let envelope = self.envelope(SessionCommand::CoordinatorRenew {
            session_id: "s1".into(),
            grant_id: GRANT.to_owned(),
            generation,
        });
        self.manager.submit(envelope, self.deck.clone()).await
    }

    async fn row(&self, digest: &str) -> BindingRow {
        self.store()
            .shim_bindings_load()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.digest == digest)
            .unwrap()
    }

    async fn bound(&self) -> Vec<String> {
        let ReadResult::Events { events, .. } = self.store().read_after(0, 1000).await.unwrap()
        else {
            panic!("no events");
        };
        events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(Event::CoordinatorBound {
                    binding_id,
                    generation,
                    ..
                }) => Some(format!("bound:{binding_id}@{generation}")),
                _ => None,
            })
            .collect()
    }
}

fn accepted(d: &Decided) -> Option<u64> {
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    d.receipt.result.clone().unwrap().seq
}

/// Finding 2: on both hosts, a set during a process start waits and then
/// applies to the new binding in place.
#[tokio::test]
async fn a_set_during_a_process_start_reaches_the_new_binding() {
    direct_host().await;
    shim_host().await;
}

/// The direct host: the start holds the lock of the session from its record
/// read until its binding is in memory.
async fn direct_host() {
    let mut rig = Rig::new().await;
    rig.put(0).await;
    let spec = session_spec("/w", Some(grant(FAR)));
    let mut tools = start_tools(
        rig.store(),
        &rig.bindings,
        ProviderKind::Claude,
        &"s1".into(),
        &spec,
        false,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(tools.launch.is_some() && tools.bridge.is_none());
    assert!(tools.lock.is_some(), "the start holds the session lock");
    let before = rig.bindings.get(BINDING).unwrap();
    assert!(!before.classes.contains(&ToolClass::Mail));

    let set = rig.wide_set();
    tokio::time::sleep(PAUSE).await;
    assert!(!set.is_finished(), "the set waits for the start");
    assert_eq!(rig.record().await.seq, 1, "the set decided nothing yet");

    // The direct host drops the lock when the binding is in memory.
    drop(tools.lock.take());
    assert_eq!(accepted(&set.await.unwrap()), Some(2));
    let after = rig.bindings.get(BINDING).unwrap();
    assert!(
        after.classes.contains(&ToolClass::Mail),
        "{:?}",
        after.classes
    );
    assert_eq!(after.generation, 1, "the start generation stays");
    assert_eq!(rig.record().await.seq, 2);
}

/// The shim host: the launch holds the lock until the row and the binding are
/// stored, and drops it right after.
async fn shim_host() {
    let mut rig = Rig::new().await;
    rig.put(0).await;
    let spec = session_spec("/w", Some(grant(FAR)));
    let tools = start_tools(
        rig.store(),
        &rig.bindings,
        ProviderKind::Claude,
        &"s1".into(),
        &spec,
        true,
        None,
        None,
    )
    .await
    .unwrap();
    let mut start = tools.bridge.expect("a bridge start");
    let mut tools_lock = tools.lock;
    start.lock = tools_lock.take();
    assert!(start.lock.is_some());
    let digest = start.digest.clone();

    let exe = rig.tmp.path().join("fleetdeck-exe");
    std::fs::write(&exe, b"not a real shim").unwrap();
    // The launch hashes the provider program before its final gate.
    let exe_for_hash = rig.tmp.path().join("claude-exe");
    std::fs::write(&exe_for_hash, b"not a real provider").unwrap();
    std::fs::create_dir(rig.tmp.path().join("station")).unwrap();
    let mut config = ShimHostConfig::new(
        exe,
        rig.tmp.path().join("station"),
        rig.store().incarnation().as_str().to_owned(),
    );
    config.runtime_dir = None;
    config.tmp_dir = rig.tmp.path().to_owned();
    let host = ShimHost::new(config);
    let process = ProcessSpec {
        program: exe_for_hash,
        args: Vec::new(),
        cwd: rig.tmp.path().to_owned(),
        env: ChildEnv::scrubbed([("PATH".into(), "/usr/bin:/bin".into())]),
        launch: None,
    };

    let (calls, entered, release, stored, proceed) = (
        AtomicUsize::new(0),
        Notify::new(),
        Notify::new(),
        Notify::new(),
        Notify::new(),
    );
    // The early gate runs before the row is stored; the final gate after.
    let gate = || {
        let n = calls.fetch_add(1, Ordering::SeqCst);
        let (entered, release, stored, proceed) = (&entered, &release, &stored, &proceed);
        Box::pin(async move {
            if n == 0 {
                entered.notify_one();
                release.notified().await;
                None
            } else {
                stored.notify_one();
                proceed.notified().await;
                Some(Dispatch::NotDispatched("the test ends the launch".into()))
            }
        }) as Pin<Box<dyn Future<Output = Option<Dispatch>> + Send + '_>>
    };
    let session = "s1".into();
    let launch = launch_shim_for(
        rig.store(),
        &host,
        &session,
        &process,
        AckMode::Manual,
        None,
        &gate,
        &rig.bindings,
        Some(start),
    );
    let drive = async {
        entered.notified().await;
        assert!(
            rig.bindings.get(BINDING).is_none(),
            "memory holds nothing yet"
        );
        let set = rig.wide_set();
        tokio::time::sleep(PAUSE).await;
        assert!(!set.is_finished(), "the set waits for the launch");
        release.notify_one();
        // The launch stored the row and the binding and dropped the lock.
        stored.notified().await;
        let decided = tokio::time::timeout(Duration::from_secs(15), set)
            .await
            .expect("the set finishes once the binding is installed")
            .unwrap();
        assert_eq!(accepted(&decided), Some(2));
        let memory = rig.bindings.get_digest(&digest).unwrap();
        assert!(
            memory.classes.contains(&ToolClass::Mail),
            "{:?}",
            memory.classes
        );
        let row = rig.row(&digest).await;
        assert_eq!(row.to_binding().unwrap().classes, memory.classes);
        proceed.notify_one();
    };
    let (launched, ()) = tokio::join!(launch, drive);
    assert!(matches!(launched, Err(LaunchError::Refused(_))));
}

fn binding(state: BindingState, generation: u32) -> Binding {
    Binding {
        binding_id: BINDING.to_owned(),
        grant_id: GRANT.to_owned(),
        session_id: "s1".into(),
        provider_instance: "claude".to_owned(),
        native_id: Some("n1".to_owned()),
        generation,
        classes: std::iter::once(ToolClass::Own).collect(),
        scope: GrantScope::Lineage,
        state,
        control_skill: String::new(),
        kind: GrantKind::Coordinator,
    }
}

async fn fail_record_updates(rig: &Rig, fail: bool) {
    let sql = if fail {
        "CREATE TRIGGER fail_record_update BEFORE UPDATE ON session_grants
         BEGIN SELECT RAISE(ABORT, 'disk full'); END;"
    } else {
        "DROP TRIGGER fail_record_update;"
    };
    rig.store()
        .db()
        .call(move |conn| {
            conn.execute_batch(sql)?;
            Ok(())
        })
        .await
        .unwrap();
}

/// Finding 3: the decision of a renewal fails after the row rewrite and the
/// event, before its commit; memory and the projection stay as they were. A
/// successful renewal then moves memory after the commit.
#[tokio::test]
async fn a_failed_renewal_leaves_memory_and_the_projection_unchanged() {
    for shim in [false, true] {
        for state in [BindingState::Active, BindingState::Expired] {
            let mut rig = Rig::new().await;
            rig.put(1).await;
            let b = binding(state, 1);
            let (_, digest) = mint().unwrap();
            rig.bindings.insert(digest.clone(), b.clone());
            if shim {
                let mut row = BindingRow::from_binding(
                    digest.clone(),
                    "shim1".to_owned(),
                    &b,
                    Some(coordinator_spec().expires_at),
                    1,
                );
                row.state = RowState::Active;
                rig.store().shim_binding_insert(row).await.unwrap();
            }
            let records = vec![rig.record().await];
            let projected = rig.bindings.projection_with(&records);

            fail_record_updates(&rig, true).await;
            assert!(rig.renew(2).await.is_err(), "the decision fails");
            let got = rig.bindings.get(BINDING).unwrap();
            assert_eq!((got.state, got.generation), (state, 1), "shim: {shim}");
            assert_eq!(rig.bindings.projection_with(&records), projected);
            assert_eq!(rig.record().await.generation, 1);
            assert!(rig.bound().await.is_empty(), "the event rolled back");
            if shim {
                assert_eq!(rig.row(&digest).await.generation, 1, "the row rolled back");
            }

            // The retry commits; memory follows the commit.
            fail_record_updates(&rig, false).await;
            let decided = rig.renew(2).await.unwrap();
            assert_eq!(decided.receipt.state, ReceiptState::Accepted);
            let got = rig.bindings.get(BINDING).unwrap();
            assert_eq!((got.state, got.generation), (BindingState::Active, 2));
            assert_eq!(rig.record().await.generation, 2);
            assert_eq!(rig.bound().await, [format!("bound:{BINDING}@2")]);
            if shim {
                assert_eq!(rig.row(&digest).await.generation, 2);
            }
        }
    }
}
