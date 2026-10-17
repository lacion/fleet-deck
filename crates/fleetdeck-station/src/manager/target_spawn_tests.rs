//! DL-165 (resume): the spawn of a start or a resume with the handoff of
//! the session's own tools. A recording adapter shows which adapter method
//! the station calls; a captured subscriber shows the info lines.

#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fleetdeck_core::logging::{LogFormat, env_filter, subscriber};
use fleetdeck_proto::command::{SessionSpec, StopMode};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{Capabilities, SessionId};
use fleetdeck_providers::adapter::{NativeSessionId, ProviderProbe, Result};
use fleetdeck_providers::coordinator::CoordinatorLaunch;
use fleetdeck_providers::{
    Answer, ControlId, EventSink, HostInput, LiveSession, ModelId, PermissionMode, ProviderAdapter,
    ProviderError,
};

use super::target_spawn::Spawn;
use crate::coordinator::{BindingState, Bindings};
use crate::manager::coordinator_start::{self, NoTools};

const URL: &str = "http://127.0.0.1:41234/mcp";

fn verified(_: ProviderKind, _: Option<&str>) -> std::result::Result<(), &'static str> {
    Ok(())
}

struct Stub(Capabilities);

#[async_trait]
impl LiveSession for Stub {
    fn capabilities(&self) -> &Capabilities {
        &self.0
    }
    async fn prompt(&mut self, _: HostInput) -> Result<()> {
        Ok(())
    }
    async fn steer(&mut self, _: HostInput) -> Result<()> {
        Ok(())
    }
    async fn follow_up(&mut self, _: HostInput) -> Result<()> {
        Ok(())
    }
    async fn interrupt(&mut self, _: Option<ControlId>) -> Result<()> {
        Ok(())
    }
    async fn answer(&mut self, _: Answer) -> Result<()> {
        Ok(())
    }
    async fn set_mode(&mut self, _: PermissionMode) -> Result<()> {
        Ok(())
    }
    async fn set_model(&mut self, _: ModelId) -> Result<()> {
        Ok(())
    }
    async fn fork(&mut self) -> Result<NativeSessionId> {
        Err(ProviderError::InvalidInput("fork".to_owned()))
    }
    async fn stop(&mut self, _: StopMode) -> Result<()> {
        Ok(())
    }
}

/// Records each adapter call; the coordinated calls refuse when `refuse`.
struct Recording {
    calls: Mutex<Vec<&'static str>>,
    refuse: bool,
}

impl Recording {
    fn new(refuse: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            refuse,
        })
    }
    fn note(&self, call: &'static str) -> Result<Box<dyn LiveSession>> {
        self.calls.lock().unwrap().push(call);
        if self.refuse && call.ends_with("_coordinated") {
            return Err(ProviderError::CoordinatorHandoff("handoff_unverified"));
        }
        Ok(Box::new(Stub(Capabilities::minimal())))
    }
    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl ProviderAdapter for Recording {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Claude
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::minimal()
    }
    async fn probe(&self) -> Result<ProviderProbe> {
        Err(ProviderError::InvalidInput("probe".to_owned()))
    }
    async fn start(&self, _: SessionSpec, _: EventSink) -> Result<Box<dyn LiveSession>> {
        self.note("start")
    }
    async fn start_coordinated(
        &self,
        _: SessionSpec,
        _: EventSink,
        _: CoordinatorLaunch,
    ) -> Result<Box<dyn LiveSession>> {
        self.note("start_coordinated")
    }
    async fn resume(
        &self,
        _: &NativeSessionId,
        _: SessionSpec,
        _: EventSink,
    ) -> Result<Box<dyn LiveSession>> {
        self.note("resume")
    }
    async fn resume_coordinated(
        &self,
        _: &NativeSessionId,
        _: SessionSpec,
        _: EventSink,
        _: CoordinatorLaunch,
    ) -> Result<Box<dyn LiveSession>> {
        self.note("resume_coordinated")
    }
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

fn spec() -> SessionSpec {
    SessionSpec {
        provider_instance: "claude".into(),
        cwd: "/w".to_owned(),
        mode: None,
        model: None,
        title: None,
        coordinator: None,
        launch: None,
    }
}

struct Run {
    calls: Vec<&'static str>,
    ok: bool,
    log: String,
    state: BindingState,
}

/// Runs one spawn under a captured subscriber.
async fn run(resume: Option<&str>, refuse: bool, fleet_only: bool) -> Run {
    let buf = Buf::default();
    let sub = subscriber(
        LogFormat::Text,
        env_filter("info").unwrap(),
        {
            let buf = buf.clone();
            move || buf.clone()
        },
        false,
    );
    let _guard = tracing::subscriber::set_default(sub);
    let bindings = Bindings::default().with_handoff(verified);
    bindings.set_endpoint(URL.to_owned());
    let session: SessionId = "s-1".into();
    let spec = spec();
    let launch =
        coordinator_start::prepare_fleet(&bindings, ProviderKind::Claude, &session, &spec, false)
            .ok();
    let adapter = Recording::new(refuse);
    let dynamic: Arc<dyn ProviderAdapter> = adapter.clone();
    let (sink, _rx) = EventSink::channel(4);
    let result = Spawn {
        bindings: &bindings,
        session_id: &session,
        spec: &spec,
        resume,
        launch,
        fleet_only,
    }
    .run(&dynamic, sink)
    .await;
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(!log.contains("fdc1_"), "no credential in a log line");
    Run {
        calls: adapter.calls(),
        ok: result.is_ok(),
        log,
        state: bindings.get("s-1:fleet").unwrap().state,
    }
}

#[tokio::test]
async fn a_resumed_fleet_only_session_gets_resume_coordinated() {
    let r = run(Some("native-1"), false, true).await;
    assert_eq!(r.calls, ["resume_coordinated"]);
    assert!(r.ok);
    assert_eq!(r.state, BindingState::Pending);
    assert!(r.log.contains("session tools attached"), "{}", r.log);
    assert!(r.log.contains("session=s-1") && r.log.contains("provider=Claude"));
}

#[tokio::test]
async fn a_refused_resume_handoff_falls_back_to_the_plain_resume_and_revokes_the_binding() {
    let r = run(Some("native-1"), true, true).await;
    assert_eq!(r.calls, ["resume_coordinated", "resume"]);
    assert!(r.ok);
    assert_eq!(r.state, BindingState::Revoked);
    assert!(
        r.log
            .contains("no session tools: the provider refused the handoff"),
        "{}",
        r.log
    );
    assert!(!r.log.contains("session tools attached"));
}

#[tokio::test]
async fn a_resume_of_a_grant_session_keeps_the_plain_resume() {
    let r = run(Some("native-1"), false, false).await;
    assert_eq!(r.calls, ["resume"]);
    assert!(r.ok);
}

#[tokio::test]
async fn a_start_keeps_start_coordinated_and_its_fallback() {
    let r = run(None, false, true).await;
    assert_eq!(r.calls, ["start_coordinated"]);
    assert!(r.log.contains("session tools attached"));
    let r = run(None, true, true).await;
    assert_eq!(r.calls, ["start_coordinated", "start"]);
    assert!(r.ok);
    assert_eq!(r.state, BindingState::Revoked);
    // A grant session never starts without its handoff.
    let r = run(None, true, false).await;
    assert_eq!(r.calls, ["start_coordinated"]);
    assert!(!r.ok);
}

#[test]
fn the_no_tools_line_names_the_reason() {
    let buf = Buf::default();
    let sub = subscriber(
        LogFormat::Text,
        env_filter("info").unwrap(),
        {
            let buf = buf.clone();
            move || buf.clone()
        },
        false,
    );
    tracing::subscriber::with_default(sub, || {
        let session: SessionId = "s-9".into();
        for why in [
            NoTools::Durable,
            NoTools::Unverified("1.0.0, 1.0.1".to_owned()),
            NoTools::NoEndpoint,
        ] {
            coordinator_start::log_no_tools(&session, ProviderKind::Codex, &why);
        }
    });
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("no session tools: durable host"), "{log}");
    assert!(log.contains(
        "no session tools: provider version not verified for the handoff (1.0.0, 1.0.1)"
    ));
    assert!(log.contains("no session tools: no tool endpoint"));
    assert!(log.contains("session=s-9") && log.contains("provider=Codex"));
}
