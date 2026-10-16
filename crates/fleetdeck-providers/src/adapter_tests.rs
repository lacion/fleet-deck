//! Tests of the adapter contract: typed degradations (L5), the event sink,
//! and the workspace resolution (trap 8).

use async_trait::async_trait;
use fleetdeck_proto::command::{ApprovalDecision, StopMode, UserInput};
use fleetdeck_proto::event::{ResolvedBy, RunOrigin};
use fleetdeck_proto::{Capabilities, Event, InputId, Operation, RequestId};

use super::*;

/// A session with the minimal capabilities: every optional operation must
/// fail with its typed degradation, and the session must not reach the
/// provider for it.
struct Minimal {
    caps: Capabilities,
    sent: Vec<&'static str>,
}

#[async_trait]
impl LiveSession for Minimal {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    async fn prompt(&mut self, _: HostInput) -> Result<()> {
        self.sent.push("prompt");
        Ok(())
    }
    async fn steer(&mut self, _: HostInput) -> Result<()> {
        require(&self.caps, Operation::Steer)?;
        self.sent.push("steer");
        Ok(())
    }
    async fn follow_up(&mut self, _: HostInput) -> Result<()> {
        self.sent.push("follow_up");
        Ok(())
    }
    async fn interrupt(&mut self, _: Option<ControlId>) -> Result<()> {
        require(&self.caps, Operation::Interrupt)?;
        self.sent.push("interrupt");
        Ok(())
    }
    async fn answer(&mut self, _: Answer) -> Result<()> {
        require(&self.caps, Operation::Answer)?;
        self.sent.push("answer");
        Ok(())
    }
    async fn set_mode(&mut self, _: PermissionMode) -> Result<()> {
        require(&self.caps, Operation::SetMode)?;
        self.sent.push("set_mode");
        Ok(())
    }
    async fn set_model(&mut self, _: ModelId) -> Result<()> {
        require(&self.caps, Operation::SetModel)?;
        self.sent.push("set_model");
        Ok(())
    }
    async fn fork(&mut self) -> Result<NativeSessionId> {
        require(&self.caps, Operation::Fork)?;
        Ok(NativeSessionId("x".to_owned()))
    }
    async fn stop(&mut self, _: StopMode) -> Result<()> {
        self.sent.push("stop");
        Ok(())
    }
}

fn host_input() -> HostInput {
    HostInput {
        id: InputId::new("01JZ8Y3K9W4V7T2Q5R6S8N0M1P"),
        origin: RunOrigin::Local,
        input: UserInput::new("x".to_owned()),
        context: Vec::new(),
    }
}

fn unsupported(r: Result<()>) -> Operation {
    match r {
        Err(ProviderError::Unsupported(u)) => {
            assert_eq!(u.degradation, u.op.degradation());
            u.op
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[tokio::test]
async fn unsupported_operations_return_a_typed_degradation() {
    let mut s = Minimal {
        caps: Capabilities::minimal(),
        sent: Vec::new(),
    };
    assert_eq!(unsupported(s.steer(host_input()).await), Operation::Steer);
    assert_eq!(unsupported(s.interrupt(None).await), Operation::Interrupt);
    let answer = Answer {
        request: RequestId::new("req-1"),
        decision: ApprovalDecision {
            answered_by: None,
            option_id: "allow".to_owned(),
            updated_input: None,
            text: None,
        },
        by: ResolvedBy::Local,
    };
    assert_eq!(unsupported(s.answer(answer).await), Operation::Answer);
    assert_eq!(
        unsupported(s.set_mode(PermissionMode("plan".to_owned())).await),
        Operation::SetMode
    );
    assert_eq!(
        unsupported(s.set_model(ModelId("m".to_owned())).await),
        Operation::SetModel
    );
    assert_eq!(unsupported(s.fork().await.map(|_| ())), Operation::Fork);
    s.prompt(host_input()).await.unwrap();
    s.follow_up(host_input()).await.unwrap();
    s.stop(StopMode::Graceful).await.unwrap();
    assert_eq!(s.sent, ["prompt", "follow_up", "stop"]);
}

#[tokio::test]
async fn the_event_sink_keeps_order_and_reports_a_closed_station() {
    let (sink, mut rx) = EventSink::channel(0);
    let events = [
        Event::ModeChanged {
            mode: "plan".to_owned(),
        },
        Event::ModelChanged {
            model: "default".to_owned(),
        },
    ];
    let send = {
        let sink = sink.clone();
        let events = events.clone();
        tokio::spawn(async move {
            for e in events {
                sink.emit(e).await.unwrap();
            }
        })
    };
    assert_eq!(rx.recv().await.unwrap().event, events[0]);
    assert_eq!(rx.recv().await.unwrap().event, events[1]);
    send.await.unwrap();
    drop(rx);
    assert!(matches!(
        sink.emit(events[0].clone()).await,
        Err(ProviderError::SinkClosed)
    ));
}

#[test]
fn trap_8_a_symlinked_workspace_resolves_to_its_real_directory() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let expected = std::fs::canonicalize(&real).unwrap();
    assert_eq!(canonical_workspace(&link).unwrap(), expected);
    assert_eq!(
        canonical_workspace(&link.join("..").join("link")).unwrap(),
        expected
    );
    let file = dir.path().join("f");
    std::fs::write(&file, "x").unwrap();
    assert!(matches!(
        canonical_workspace(&file),
        Err(ProviderError::Workspace { .. })
    ));
    assert!(matches!(
        canonical_workspace(&dir.path().join("missing")),
        Err(ProviderError::Workspace { .. })
    ));
    let dangling = dir.path().join("dangling");
    std::os::unix::fs::symlink(dir.path().join("gone"), &dangling).unwrap();
    assert!(canonical_workspace(&dangling).is_err());
}
