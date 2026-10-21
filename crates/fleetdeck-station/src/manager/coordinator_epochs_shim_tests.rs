//! MCP P4b §11 test 7, the shim host: the rows of the shim bindings through
//! the output transaction of the store, with the tools of the start.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, CoordinatorSpec, RETRY_WINDOW_MS, SessionCommand, SessionSpec,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{CommandId, Event, SessionId};
use fleetdeck_providers::host::shim::wire::ProviderIdentity;
use fleetdeck_testkit::Det;

use super::super::{plan, start_tools};
use super::{GRANT, Link, URL, grant, lifecycle, session_spec, verified};
use crate::coordinator::{BindingState, Bindings};
use crate::manager::coordinator_command::set::record_start;
use crate::store::session_grants::{GrantRecord, RecordState};
use crate::store::shim_bindings::{BindingRow, MemorySink, RowState};
use crate::store::{
    Caller, NewSession, NewShim, OfflineApprovalPolicy, OutputCommit, OutputMeta, ReadResult,
    ShimState, Store, StoreConfig,
};

struct Shim {
    det: Det,
    store: Store,
    bindings: Bindings,
    next: u32,
}

impl Shim {
    async fn new(live: bool) -> Self {
        let det = Det::new();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        store.recover(Vec::new()).await.unwrap();
        let bindings = Bindings::default().with_handoff(verified);
        bindings.record_version(ProviderKind::Claude, "2.1.296".to_owned());
        bindings.set_endpoint(URL.to_owned());
        bindings.set_bridge(crate::coordinator::BridgeEndpoint {
            program: "/station/bin/current".into(),
            socket: "/run/fleetdeck/mcp.s".into(),
        });
        bindings.set_deck(1, Arc::new(Link { live }));
        store.set_binding_sink(Arc::new(MemorySink(bindings.clone())));
        Self {
            det,
            store,
            bindings,
            next: 0,
        }
    }

    async fn session(&self, id: &str) -> SessionId {
        self.store
            .create_session(NewSession {
                session_id: id.into(),
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
        id.into()
    }

    /// The start decision's record call, in a decide transaction.
    async fn record_start(&self, session: &SessionId, spec: &SessionSpec) -> Option<u64> {
        let (bindings, id, spec) = (self.bindings.clone(), session.clone(), spec.clone());
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("rec-{session}")),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Board,
            command: SessionCommand::CoordinatorRevoke {
                session_id: session.clone(),
                grant_id: "x".to_owned(),
            },
        };
        let seen = Arc::new(std::sync::Mutex::new(None));
        let out = Arc::clone(&seen);
        self.store
            .decide(envelope, Caller::local(), move |d| {
                *out.lock().unwrap() = Some(record_start(d, &bindings, &id, &spec)?);
                Ok(crate::manager::decide::result(&id, None))
            })
            .await
            .unwrap();
        seen.lock().unwrap().take().unwrap()
    }

    async fn record(&self, session: &SessionId) -> Option<GrantRecord> {
        self.store.session_grant(session.clone()).await.unwrap()
    }

    async fn lifecycle(&self, session: &SessionId) -> Vec<String> {
        let ReadResult::Events { events, .. } = self.store.read_after(0, 10_000).await.unwrap()
        else {
            panic!("snapshot required");
        };
        lifecycle(&events, session)
    }

    /// One start or resume under a new shim: the tools, the stored row, the
    /// running shim. Returns the shim id and the generation of the row.
    async fn launch(
        &mut self,
        session: &SessionId,
        spec: &SessionSpec,
        resume: Option<&str>,
    ) -> (String, u32, bool) {
        let tools = start_tools(
            &self.store,
            &self.bindings,
            ProviderKind::Claude,
            session,
            spec,
            true,
            resume,
            None,
        )
        .await
        .unwrap();
        let record_start = tools.record_start;
        let start = tools.bridge.expect("a bridge start");
        let generation = start.binding.generation;
        self.next += 1;
        let shim = format!("shim-{}", self.next);
        self.store
            .shim_starting(NewShim {
                shim_id: shim.clone(),
                session_id: session.clone(),
                socket_path: PathBuf::from(format!("/run/x/{shim}.sock")),
                launch_boot_id: None,
                native_target: resume.map(str::to_owned),
            })
            .await
            .unwrap();
        self.store
            .shim_running(
                shim.clone(),
                ProviderIdentity {
                    pid: 1,
                    start_time: 2,
                    boot_id: "boot".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            self.store.shim_next_generation(shim.clone()).await.unwrap(),
            Some(1)
        );
        let (secrets, stored) = crate::manager::coordinator_start::store_tools(
            &self.store,
            &self.bindings,
            &shim,
            Some(start),
        )
        .await;
        assert!(secrets.is_some() && stored.is_some(), "the row was stored");
        (shim, generation, record_start)
    }

    /// One output batch of the shim: `started` (a native id) and `exited`.
    async fn output(&self, shim: &str, cursor: u64, native: Option<&str>, exited: bool) {
        let events = native
            .map(|n| {
                let mut refs = fleetdeck_proto::event::ProviderRefs::new();
                refs.insert("session_id".to_owned(), n.into());
                crate::store::NewEvent::from(Event::SessionStarted {
                    provider: ProviderKind::Claude,
                    provider_instance: "claude".into(),
                    provider_version: None,
                    model: None,
                    cwd: "/w".to_owned(),
                    mode: None,
                    capabilities: Box::new(fleetdeck_proto::capability::Capabilities::minimal()),
                    resumed: None,
                    provider_refs: refs,
                    title: None,
                    branch: None,
                    repo: None,
                })
            })
            .into_iter()
            .collect();
        let out = self
            .store
            .shim_commit_output_with(
                shim.to_owned(),
                1,
                cursor,
                events,
                OutputMeta {
                    exited,
                    ..OutputMeta::default()
                },
            )
            .await
            .unwrap();
        assert!(matches!(out, OutputCommit::Committed(_)), "{out:?}");
        if exited {
            self.store
                .shim_end(shim.to_owned(), ShimState::Exited, "exited".to_owned(), 1)
                .await
                .unwrap();
        }
    }

    async fn rows(&self) -> Vec<BindingRow> {
        self.store.shim_bindings_load().await.unwrap()
    }
}

pub(super) async fn shim_with_a_record() {
    let mut st = Shim::new(true).await;
    let s = st.session("s1").await;
    let with_grant = session_spec("/w", Some(grant(i64::MAX / 4)));
    let without = session_spec("/w", None);
    assert_eq!(st.record_start(&s, &with_grant).await, Some(1));
    let id = format!("{s}:{GRANT}");

    // The start: generation 1, bound at 1.
    let (shim, generation, record) = st.launch(&s, &with_grant, None).await;
    assert!(record);
    assert_eq!(generation, 1);
    assert_eq!(st.record(&s).await.unwrap().generation, 1);
    assert_eq!(st.rows().await[0].generation, 1);
    st.output(&shim, 1, Some("n1"), false).await;
    assert_eq!(st.bindings.get(&id).unwrap().generation, 1);
    assert_eq!(st.lifecycle(&s).await, [format!("bound:{id}@1")]);
    // The process end writes no `coordinator.ended`; the record stays.
    st.output(&shim, 2, None, true).await;
    assert_eq!(st.rows().await[0].state, RowState::Ended);
    assert_eq!(st.bindings.get(&id).unwrap().state, BindingState::Revoked);
    assert_eq!(st.lifecycle(&s).await, [format!("bound:{id}@1")]);
    assert_eq!(st.record(&s).await.unwrap().state, RecordState::Attached);

    // A resume (hibernation) and a crash resume: generations 2 and 3, with a
    // new credential each time and the expected native id on the row.
    let mut digests = vec![st.rows().await[0].digest.clone()];
    for generation in [2u32, 3] {
        let (shim, got, record) = st.launch(&s, &without, Some("n1")).await;
        assert!(record);
        assert_eq!(got, generation);
        let rows = st.rows().await;
        let row = rows.iter().find(|r| r.shim_id == shim).unwrap();
        assert_eq!(
            (row.state, row.native_id.as_deref(), row.generation),
            (RowState::Pending, Some("n1"), generation)
        );
        assert!(!digests.contains(&row.digest), "a new credential");
        digests.push(row.digest.clone());
        st.output(&shim, 1, Some("n1"), false).await;
        let b = st.bindings.get(&id).unwrap();
        assert_eq!((b.state, b.generation), (BindingState::Active, generation));
        st.output(&shim, 2, None, true).await;
    }
    assert_eq!(
        st.lifecycle(&s).await,
        [
            format!("bound:{id}@1"),
            format!("bound:{id}@2"),
            format!("bound:{id}@3")
        ]
    );
    assert_eq!(st.record(&s).await.unwrap().generation, 3);

    // A resume that reports another native id: the row ends, the record ends.
    let (shim, got, _) = st.launch(&s, &without, Some("n1")).await;
    assert_eq!(got, 4);
    st.output(&shim, 1, Some("n9"), false).await;
    assert_eq!(st.bindings.get(&id).unwrap().state, BindingState::Revoked);
    let record = st.record(&s).await.unwrap();
    assert_eq!(record.state, RecordState::Ended);
    assert_eq!(record.ended_reason.as_deref(), Some("identity_changed"));
    assert_eq!(
        st.lifecycle(&s).await.last().unwrap(),
        &format!("ended:{id}@4:identity_changed")
    );
}

pub(super) async fn shim_expired_and_without_the_feature() {
    // A record past its expiry ends `expired`; the resume gets the fleet
    // tools through the bridge.
    let mut st = Shim::new(true).await;
    let s = st.session("s1").await;
    let soon = i64::try_from(st.det.now_ms()).unwrap() + 60_000;
    assert_eq!(
        st.record_start(&s, &session_spec("/w", Some(grant(soon))))
            .await,
        Some(1)
    );
    st.det.advance(Duration::from_secs(120));
    let (_, generation, record) = st.launch(&s, &session_spec("/w", None), Some("n1")).await;
    assert!(!record);
    assert_eq!(generation, 0, "the fleet binding has no generation");
    let rec = st.record(&s).await.unwrap();
    assert_eq!(rec.state, RecordState::Ended);
    assert_eq!(rec.ended_reason.as_deref(), Some("expired"));
    assert_eq!(
        st.lifecycle(&s).await,
        [format!("ended:{s}:{GRANT}@0:expired")]
    );
    assert_eq!(st.rows().await[0].grant_id, "", "a fleet-only row");

    // No `coordinator_live.v1`: no record, the rules of today.
    let mut st = Shim::new(false).await;
    let s = st.session("s1").await;
    let with_grant = session_spec("/w", Some(grant(i64::MAX / 4)));
    assert_eq!(st.record_start(&s, &with_grant).await, None);
    assert!(st.record(&s).await.is_none());
    let (shim, generation, record) = st.launch(&s, &with_grant, None).await;
    assert!(!record);
    assert_eq!(generation, 0);
    st.output(&shim, 1, Some("n1"), false).await;
    st.output(&shim, 2, None, true).await;
    assert_eq!(
        st.lifecycle(&s).await,
        [
            format!("bound:{s}:{GRANT}@1"),
            format!("ended:{s}:{GRANT}@1:process_exit")
        ]
    );
    let (shim, _, record) = st.launch(&s, &session_spec("/w", None), Some("n1")).await;
    assert!(!record);
    let rows = st.rows().await;
    let row = rows.iter().find(|r| r.shim_id == shim).unwrap();
    assert_eq!(row.grant_id, "", "fleet tools");
}

/// `plan` alone: no record, an ended record, another grant, a worker.
pub(super) async fn plan_rules() {
    let st = Shim::new(true).await;
    let s = st.session("s1").await;
    let with_grant = session_spec("/w", Some(grant(i64::MAX / 4)));
    // No record: the rules of today.
    assert!(
        plan(&st.store, &s, &with_grant, None)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(st.record_start(&s, &with_grant).await, Some(1));
    // Another grant on a start, and a record start of the same grant.
    let other = session_spec(
        "/w",
        Some(CoordinatorSpec {
            grant_id: "grant-2".to_owned(),
            ..grant(i64::MAX / 4)
        }),
    );
    assert!(plan(&st.store, &s, &other, None).await.unwrap().is_none());
    let (effective, record) = plan(&st.store, &s, &with_grant, None)
        .await
        .unwrap()
        .expect("a record start");
    assert_eq!(effective.coordinator.unwrap().grant_id, GRANT);
    assert_eq!((record.generation, record.expected_native), (1, None));
    // A resume of an ended record: the rules of today.
    assert!(
        st.store
            .session_grant_end(s.clone(), "revoked".to_owned())
            .await
            .unwrap()
    );
    assert!(
        plan(&st.store, &s, &session_spec("/w", None), Some("n1"))
            .await
            .unwrap()
            .is_none()
    );
    // A worker grant never gets a record.
    let s2 = st.session("s2").await;
    let worker = session_spec(
        "/w",
        Some(CoordinatorSpec {
            kind: fleetdeck_proto::command::GrantKind::Worker,
            ..grant(i64::MAX / 4)
        }),
    );
    assert_eq!(st.record_start(&s2, &worker).await, None);
    assert!(st.record(&s2).await.is_none());
}
