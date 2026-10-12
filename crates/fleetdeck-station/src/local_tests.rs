//! The local socket protocol (M2 P2-7). The CLI end-to-end test is
//! `crates/fleetdeck-cli/tests/station_local.rs`.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use fleetdeck_proto::command::{ReceiptState, StopMode, UserInput};
use fleetdeck_proto::{RejectCode, SessionId};
use fleetdeck_testkit::Det;
use tokio::io::AsyncWriteExt;

use super::*;
use crate::manager::{Adapters, Policy};
use crate::store::{OfflineApprovalPolicy, Store, StoreConfig};

struct Server {
    dir: tempfile::TempDir,
    path: PathBuf,
    shutdown: watch::Sender<bool>,
    manager: Arc<SessionManager>,
    task: tokio::task::JoinHandle<()>,
}

async fn server() -> Server {
    let det = Det::new();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let policy = Policy::new(
        vec![dir.path().to_owned()],
        2,
        OfflineApprovalPolicy::Wait,
        BTreeMap::new(),
    );
    let manager = Arc::new(
        SessionManager::start(store, policy, Adapters::new(), det.ids())
            .await
            .unwrap(),
    );
    let path = socket_path(dir.path());
    let listener = bind(&path).await.unwrap();
    let (shutdown, rx) = watch::channel(false);
    let task = tokio::spawn(serve(listener, Arc::clone(&manager), None, rx));
    Server {
        dir,
        path,
        shutdown,
        manager,
        task,
    }
}

impl Server {
    async fn stop(self) {
        self.shutdown.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .unwrap()
            .unwrap();
        self.manager.shutdown(StopMode::Kill).await;
        drop(self.dir);
    }
}

#[tokio::test]
async fn the_socket_is_owner_only_and_a_second_station_cannot_take_it() {
    let s = server().await;
    let mode = std::fs::metadata(&s.path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert!(matches!(bind(&s.path).await, Err(LocalError::InUse(_))));
    s.stop().await;
}

#[tokio::test]
async fn a_stale_socket_file_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_path(dir.path());
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    assert!(
        path.exists(),
        "the stale file stays after its listener closed"
    );
    let listener = bind(&path).await.unwrap();
    drop(listener);
}

#[tokio::test]
async fn requests_get_one_response_each_on_the_same_connection() {
    let s = server().await;
    let mut client = LocalClient::connect(&s.path).await.unwrap();
    assert_eq!(
        client.call(&LocalRequest::Sessions).await.unwrap(),
        LocalResponse::Sessions {
            sessions: Vec::new()
        }
    );
    // A command goes through the decision: a stored, rejected receipt.
    let response = client
        .call(&LocalRequest::Command {
            command_id: Some("local-1".into()),
            command: SessionCommand::Prompt {
                session_id: SessionId::from("nope"),
                input: UserInput::new("hi"),
            },
        })
        .await
        .unwrap();
    let LocalResponse::Receipt { receipt } = response else {
        panic!("{response:?}");
    };
    assert_eq!(receipt.state, ReceiptState::Rejected);
    assert_eq!(receipt.reject.unwrap().code, RejectCode::UnknownSession);
    assert_eq!(
        s.manager
            .store()
            .receipt("local-1".into())
            .await
            .unwrap()
            .unwrap()
            .state,
        ReceiptState::Rejected
    );
    // An answer to a request the station does not know.
    let response = client
        .call(&LocalRequest::Approve {
            command_id: None,
            request_id: "req-x".into(),
            decision: ApprovalDecision {
                option_id: "allow".into(),
                updated_input: None,
                text: None,
            },
        })
        .await
        .unwrap();
    assert!(
        matches!(&response, LocalResponse::Error { message } if message.contains("req-x")),
        "{response:?}"
    );
    s.stop().await;
}

#[tokio::test]
async fn a_bad_line_gets_an_error_and_an_oversized_line_closes_the_connection() {
    let s = server().await;
    let stream = UnixStream::connect(&s.path).await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    write.write_all(b"{\"op\":\"launch\"}\n").await.unwrap();
    let line = read_line(&mut reader).await.unwrap().unwrap();
    let response: LocalResponse = serde_json::from_slice(&line).unwrap();
    assert!(
        matches!(response, LocalResponse::Error { .. }),
        "{response:?}"
    );

    let big = vec![b'x'; MAX_LINE_BYTES + 10];
    write.write_all(&big).await.unwrap();
    write.write_all(b"\n").await.unwrap();
    let line = read_line(&mut reader).await.unwrap().unwrap();
    let response: LocalResponse = serde_json::from_slice(&line).unwrap();
    assert!(
        matches!(&response, LocalResponse::Error { message } if message.contains("longer")),
        "{response:?}"
    );
    assert_eq!(read_line(&mut reader).await.unwrap(), None, "closed");
    s.stop().await;
}

#[test]
fn the_wire_shapes_are_stable() {
    let request = LocalRequest::Approve {
        command_id: None,
        request_id: "r1".into(),
        decision: ApprovalDecision {
            option_id: "deny".into(),
            updated_input: None,
            text: None,
        },
    };
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["op"], "approve");
    assert_eq!(json["request_id"], "r1");
    assert_eq!(
        serde_json::to_value(LocalRequest::Sessions).unwrap(),
        serde_json::json!({"op": "sessions"})
    );
    let error = LocalResponse::Error {
        message: "x".into(),
    };
    assert_eq!(serde_json::to_value(&error).unwrap()["status"], "error");
}
