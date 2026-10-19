//! Tests of the station's connection to a runner.

use std::time::Duration;

use tokio::io::AsyncReadExt as _;
use tokio::net::UnixListener;
use tokio::time::timeout;

use super::files;
use super::frame::{Frame, read_frame, write_frame};
use super::runner_client::connect;
use super::test_support::{ID, POST_OK, RUNNER_TOKEN, STATION_TOKEN, private_dir, spec, start};

const WAIT: Duration = Duration::from_secs(30);

/// A fake runner that answers `HelloOk` with `answer` and then collects
/// every byte that the client sends until it closes.
async fn fake_runner(
    dir: &std::path::Path,
    answer: String,
) -> tokio::task::JoinHandle<(Frame, Vec<u8>)> {
    let listener = UnixListener::bind(files::socket_path(dir, ID)).unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let hello = read_frame(&mut stream).await.unwrap();
        write_frame(&mut stream, &Frame::HelloOk(answer.into_bytes()))
            .await
            .unwrap();
        let mut rest = Vec::new();
        let _ = timeout(WAIT, stream.read_to_end(&mut rest)).await;
        (hello, rest)
    })
}

#[tokio::test]
async fn maintenance_client_a_wrong_runner_token_is_refused_before_any_input_byte() {
    let dir = private_dir();
    let server = fake_runner(dir.path(), "9".repeat(32)).await;
    let refused = connect(dir.path(), &spec(ID)).await;
    assert_eq!(
        refused.err().unwrap().kind(),
        std::io::ErrorKind::InvalidData
    );
    let (hello, rest) = timeout(WAIT, server).await.unwrap().unwrap();
    // The server got the station token and nothing else.
    assert_eq!(hello, Frame::Hello(STATION_TOKEN.into()));
    assert!(rest.is_empty(), "{rest:?}");
}

#[tokio::test]
async fn maintenance_client_a_runner_token_of_the_right_length_that_differs_is_refused() {
    let dir = private_dir();
    let mut near = RUNNER_TOKEN.as_bytes().to_vec();
    near[31] = b'3';
    let server = fake_runner(dir.path(), String::from_utf8(near).unwrap()).await;
    assert!(connect(dir.path(), &spec(ID)).await.is_err());
    let (_, rest) = timeout(WAIT, server).await.unwrap().unwrap();
    assert!(rest.is_empty());
}

#[tokio::test]
async fn maintenance_client_a_missing_socket_is_an_error() {
    let dir = private_dir();
    let refused = connect(dir.path(), &spec(ID)).await;
    assert_eq!(refused.err().unwrap().kind(), std::io::ErrorKind::NotFound);
}

#[tokio::test]
async fn maintenance_client_talks_to_the_real_runner() {
    let run = start("read -r x; echo got-$x", POST_OK);
    let mut client = loop {
        // The runner needs a moment to listen.
        match connect(run.path(), &spec(ID)).await {
            Ok(client) => break client,
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    };
    client.resize(90, 20).await.unwrap();
    client.input(b"x\n").await.unwrap();
    let mut output = Vec::new();
    let code = timeout(WAIT, async {
        loop {
            match client.next().await.unwrap() {
                Frame::Output(bytes) => output.extend(bytes),
                Frame::Exit(code) => return code,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(code, 0);
    assert!(String::from_utf8_lossy(&output).contains("got-x"));
    run.finished().await;
}
