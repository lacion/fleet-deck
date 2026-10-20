//! Bridge socket tests (DL-194 T2).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::*;
use crate::coordinator::{BindingState, Caller, Ingress, ToolClass, mint};

struct Accept;

#[async_trait]
impl Forward for Accept {
    async fn call(&self, _: Caller, _: &str, _: Value) -> Result<Value, String> {
        Ok(json!({"state": "accepted"}))
    }
}

pub(crate) struct Rig {
    pub(crate) bindings: Bindings,
    pub(crate) token: String,
    pub(crate) socket: BridgeSocket,
    pub(crate) path: PathBuf,
    pub(crate) dir: tempfile::TempDir,
}

pub(crate) async fn rig() -> Rig {
    let bindings = Bindings::default();
    let (token, digest) = mint().unwrap();
    bindings.insert(
        digest,
        Binding {
            binding_id: "b-1".to_owned(),
            grant_id: "grant-1".to_owned(),
            session_id: "coord-1".into(),
            provider_instance: "claude".to_owned(),
            native_id: Some("native-1".to_owned()),
            generation: 1,
            classes: [ToolClass::Read, ToolClass::Mail].into_iter().collect(),
            state: BindingState::Active,
            control_skill: "SKILL v1".to_owned(),
            kind: fleetdeck_proto::command::GrantKind::Coordinator,
            scope: fleetdeck_proto::command::GrantScope::Lineage,
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.s");
    let socket = BridgeSocket::start(
        &path,
        bindings.clone(),
        Arc::new(Accept),
        None,
        Limits::default(),
    )
    .await
    .unwrap();
    Rig {
        bindings,
        token: token.into_exposed(),
        socket,
        path,
        dir,
    }
}

/// Sends `payload` on one connection and reads until the server closes.
async fn exchange(path: &Path, payload: &[u8]) -> Vec<u8> {
    let mut stream = UnixStream::connect(path).await.unwrap();
    stream.write_all(payload).await.unwrap();
    // Half-close: a payload that stops early is seen as an end of file.
    stream.shutdown().await.unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut out))
        .await
        .unwrap()
        .unwrap();
    out
}

fn list() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
}

fn frame(token: &str, message: &str) -> Vec<u8> {
    format!("AUTH {token}\n{message}\n").into_bytes()
}

/// The body of an HTTP answer of the ingress.
async fn http_body(addr: std::net::SocketAddr, token: &str, body: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        addr.port(),
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    stream.read_to_end(&mut out).await.unwrap();
    let text = String::from_utf8(out).unwrap();
    text.split_once("\r\n\r\n").unwrap().1.to_owned()
}

#[tokio::test]
async fn an_authorized_message_gets_the_answer_of_the_http_ingress_as_one_line() {
    let r = rig().await;
    let message = list().to_string();
    let got = exchange(&r.path, &frame(&r.token, &message)).await;
    let ingress = Ingress::start(r.bindings.clone(), Arc::new(Accept), Limits::default())
        .await
        .unwrap();
    let http = http_body(ingress.addr, &r.token, &message).await;
    let line = String::from_utf8(got).unwrap();
    assert!(line.ends_with('\n'));
    assert_eq!(line.matches('\n').count(), 1);
    let socket: Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(socket, serde_json::from_str::<Value>(&http).unwrap());
    assert!(socket["result"]["tools"].as_array().is_some());
}

#[tokio::test]
async fn a_wrong_or_an_ended_token_gets_err_unauthorized() {
    let r = rig().await;
    let (other, _) = mint().unwrap();
    let message = list().to_string();
    assert_eq!(
        exchange(&r.path, &frame(other.expose(), &message)).await,
        b"ERR unauthorized\n"
    );
    r.bindings
        .update("b-1", |b| b.state = BindingState::Revoked);
    assert_eq!(
        exchange(&r.path, &frame(&r.token, &message)).await,
        b"ERR unauthorized\n"
    );
}

#[tokio::test]
async fn a_notification_gets_end_of_file_without_a_line() {
    let r = rig().await;
    let message = json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string();
    assert!(
        exchange(&r.path, &frame(&r.token, &message))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn a_malformed_message_gets_err_bad_request() {
    let r = rig().await;
    for payload in [
        b"HELLO\nx\n".to_vec(),
        b"AUTH \n{}\n".to_vec(),
        b"AUTH\n{}\n".to_vec(),
        b"AUTH \xff\xfe\n{}\n".to_vec(),
        frame(&r.token, "not json"),
        frame(&r.token, "[]"),
        [b"AUTH ".as_slice(), &[b'a'; 2000], b"\n{}\n"].concat(),
    ] {
        assert_eq!(
            exchange(&r.path, &payload).await,
            b"ERR bad_request\n",
            "{}",
            String::from_utf8_lossy(&payload[..payload.len().min(20)])
        );
    }
    let big = format!(
        "{{\"pad\":\"{}\"}}",
        "a".repeat(Limits::default().body_bytes)
    );
    assert_eq!(
        exchange(&r.path, &frame(&r.token, &big)).await,
        b"ERR bad_request\n"
    );
}

#[tokio::test]
async fn a_connection_that_closes_before_the_message_is_closed_without_a_line() {
    let r = rig().await;
    assert!(exchange(&r.path, b"").await.is_empty());
    assert!(
        exchange(&r.path, format!("AUTH {}\n", r.token).as_bytes())
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn dropping_the_socket_removes_the_file_and_a_stale_file_is_replaced() {
    let r = rig().await;
    assert!(r.path.exists());
    let Rig {
        socket, path, dir, ..
    } = r;
    drop(socket);
    assert!(!path.exists());
    // A stale socket file (no server) is replaced by the next start.
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    assert!(path.exists());
    let again = BridgeSocket::start(
        &path,
        Bindings::default(),
        Arc::new(Accept),
        None,
        Limits::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(again);
    drop(dir);
}

#[test]
fn the_socket_path_prefers_the_runtime_dir_then_the_station_dir_then_the_temporary_dir() {
    let runtime = tempfile::tempdir().unwrap();
    let station = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let first = mcp_socket_path(Some(runtime.path()), station.path(), tmp.path(), "INC").unwrap();
    assert_eq!(first, runtime.path().join("fleetdeck/INC/mcp.s"));
    assert_eq!(
        std::fs::metadata(first.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let second = mcp_socket_path(None, station.path(), tmp.path(), "INC").unwrap();
    assert_eq!(second, station.path().join("run/mcp/mcp.s"));
    assert!(second.parent().unwrap().is_dir());
}

#[test]
fn a_long_station_dir_falls_back_to_the_temporary_dir() {
    let station = tempfile::tempdir().unwrap();
    let long = station.path().join("a".repeat(SUN_PATH_MAX));
    let tmp = tempfile::tempdir().unwrap();
    let path = mcp_socket_path(None, &long, tmp.path(), "INC").unwrap();
    let uid = rustix::process::getuid().as_raw();
    assert_eq!(path, tmp.path().join(format!("fd-{uid}-INC/mcp.s")));
    // Nothing fits: no path.
    let long_tmp = tmp.path().join("b".repeat(SUN_PATH_MAX));
    assert!(mcp_socket_path(None, &long, &long_tmp, "INC").is_none());
}

#[test]
fn an_open_directory_is_refused_and_the_next_candidate_is_used() {
    let runtime = tempfile::tempdir().unwrap();
    let station = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let open = runtime.path().join("fleetdeck/INC");
    std::fs::create_dir_all(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = mcp_socket_path(Some(runtime.path()), station.path(), tmp.path(), "INC").unwrap();
    assert_eq!(path, station.path().join("run/mcp/mcp.s"));
}

#[tokio::test]
async fn a_held_token_gets_err_starting_until_it_is_released() {
    let r = rig().await;
    let message = list().to_string();
    let held = crate::coordinator::digest(&r.token);
    r.bindings.hold([held.clone()]);
    assert_eq!(
        exchange(&r.path, &frame(&r.token, &message)).await,
        b"ERR starting\n"
    );
    let (other, _) = mint().unwrap();
    assert_eq!(
        exchange(&r.path, &frame(other.expose(), &message)).await,
        b"ERR unauthorized\n"
    );
    r.bindings.release([&held]);
    let got = exchange(&r.path, &frame(&r.token, &message)).await;
    assert!(got.starts_with(b"{"), "{got:?}");
}
