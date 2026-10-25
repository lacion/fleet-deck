use std::time::Duration;

use fleetdeck_proto::SessionId;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

use super::*;

#[test]
fn the_open_and_close_events_are_parsed() {
    let open = |a: &str, c: &str| Event::Open {
        address: a.to_owned(),
        class: c.to_owned(),
    };
    assert_eq!(
        parse_event("openwindow>>5A3E8A40,1,fleetdeck-wall-ab,Title, with, commas"),
        Some(open("0x5a3e8a40", "fleetdeck-wall-ab"))
    );
    assert_eq!(
        parse_event("openwindowv2>>0x5a3e8a40,2,kitty,~"),
        Some(open("0x5a3e8a40", "kitty"))
    );
    assert_eq!(
        parse_event("closewindow>>5a3e8a40"),
        Some(Event::Close {
            address: "0x5a3e8a40".into()
        })
    );
    for garbage in [
        "",
        "garbage",
        ">>",
        "openwindow>>",
        "openwindow>>zz,1,c,t",
        "openwindow>>5a,1,c",
        "openwindow>>12345678901234567,1,c,t",
        "closewindow>>",
        "closewindow>>xyz",
        "workspace>>1",
        "activewindow>>a,b",
    ] {
        assert_eq!(parse_event(garbage), None, "{garbage:?}");
    }
}

#[tokio::test]
async fn the_line_reader_is_bounded_and_skips_a_long_line() {
    let mut data = Vec::new();
    data.extend_from_slice(b"first\n");
    data.extend(std::iter::repeat_n(b'x', MAX_LINE + 10));
    data.extend_from_slice(b"\nsecond\n");
    data.extend(std::iter::repeat_n(b'y', MAX_LINE));
    data.extend_from_slice(b"\npartial");
    let mut reader = BufReader::with_capacity(7, &data[..]);
    let mut buf = Vec::new();
    let mut got = Vec::new();
    while let Some(line) = next_line(&mut reader, &mut buf).await.unwrap() {
        got.push((line, buf.len()));
    }
    assert_eq!(
        got,
        vec![
            (Line::Text, 5),
            (Line::TooLong, 0),
            (Line::Text, 6),
            (Line::Text, MAX_LINE),
        ]
    );
}

fn template(class: &str, kind: Kind, display: Option<&str>) -> Record {
    Record {
        signature: String::new(),
        address: String::new(),
        class: class.to_owned(),
        kind,
        display_id: display.map(str::to_owned),
        monitor: Some("DP-1".into()),
        opener: Some(SessionId::new("01HZZZZZZZZZZZZZZZZZZZZZZZ")),
        launch_id: "launch".into(),
    }
}

async fn socket(dir: &Path) -> UnixListener {
    let inst = dir.join("hypr/sig");
    std::fs::create_dir_all(&inst).unwrap();
    UnixListener::bind(inst.join(".socket2.sock")).unwrap()
}

#[tokio::test]
async fn a_window_with_a_pending_class_becomes_a_record_and_close_deletes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = socket(tmp.path()).await;
    let events = Events::new();
    events.ensure(tmp.path(), "sig");
    let (mut peer, _) = listener.accept().await.unwrap();
    assert!(events.wait_connected(Duration::from_secs(2)).await);
    let rx = events
        .register(template("fleetdeck-wall-1", Kind::Wall, Some("d1")))
        .unwrap();
    // A window of another class is not ours.
    peer.write_all(b"openwindow>>aa,1,firefox,t\n")
        .await
        .unwrap();
    peer.write_all(b"openwindow>>bb,1,fleetdeck-wall-1,t\n")
        .await
        .unwrap();
    assert_eq!(rx.await.unwrap(), "0xbb");
    assert!(events.record("0xaa").is_none());
    let record = events.record("0xbb").unwrap();
    assert_eq!(record.signature, "sig");
    assert_eq!(record.display_id.as_deref(), Some("d1"));
    assert_eq!(events.walls()[0].address, "0xbb");
    // A second window of the same class is not a launch window.
    peer.write_all(b"openwindow>>cc,1,fleetdeck-wall-1,t\n")
        .await
        .unwrap();
    peer.write_all(b"closewindow>>bb\n").await.unwrap();
    assert!(events.wait_gone("0xbb", Duration::from_secs(2)).await);
    assert!(events.record("0xcc").is_none());
}

#[tokio::test]
async fn two_walls_for_one_display_conflict_also_while_pending() {
    let tmp = tempfile::tempdir().unwrap();
    let _listener = socket(tmp.path()).await;
    let events = Events::new();
    events.ensure(tmp.path(), "sig");
    let _rx = events
        .register(template("c1", Kind::Wall, Some("d1")))
        .unwrap();
    let e = events
        .register(template("c2", Kind::Wall, Some("d1")))
        .unwrap_err();
    assert_eq!(e.code, "conflict");
    events
        .register(template("c3", Kind::Wall, Some("d2")))
        .unwrap();
    events
        .register(template("c4", Kind::Preview, None))
        .unwrap();
    events.cancel("c1");
    events
        .register(template("c5", Kind::Wall, Some("d1")))
        .unwrap();
}

#[tokio::test]
async fn an_eof_clears_all_records_and_the_task_reconnects() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = socket(tmp.path()).await;
    let events = Events::new();
    events.ensure(tmp.path(), "sig");
    let (mut peer, _) = listener.accept().await.unwrap();
    assert!(events.wait_connected(Duration::from_secs(2)).await);
    let rx = events
        .register(template("c1", Kind::Preview, None))
        .unwrap();
    peer.write_all(b"openwindow>>aa,1,c1,t\n").await.unwrap();
    rx.await.unwrap();
    assert!(events.record("0xaa").is_some());
    drop(peer);
    for _ in 0..100 {
        if events.record("0xaa").is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(events.record("0xaa").is_none(), "an EOF clears all records");
    // The task connects again after its pause.
    let (_again, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("a reconnect")
        .unwrap();
    assert!(events.wait_connected(Duration::from_secs(2)).await);
}

#[tokio::test]
async fn a_new_signature_clears_all_records() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = socket(tmp.path()).await;
    let events = Events::new();
    events.ensure(tmp.path(), "sig");
    let (mut peer, _) = listener.accept().await.unwrap();
    assert!(events.wait_connected(Duration::from_secs(2)).await);
    let rx = events
        .register(template("c1", Kind::Preview, None))
        .unwrap();
    peer.write_all(b"openwindow>>aa,1,c1,t\n").await.unwrap();
    rx.await.unwrap();
    events.ensure(tmp.path(), "other");
    assert!(events.record("0xaa").is_none());
}

#[tokio::test]
async fn a_long_event_line_does_not_stop_the_stream() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = socket(tmp.path()).await;
    let events = Events::new();
    events.ensure(tmp.path(), "sig");
    let (mut peer, _) = listener.accept().await.unwrap();
    assert!(events.wait_connected(Duration::from_secs(2)).await);
    let rx = events
        .register(template("c1", Kind::Preview, None))
        .unwrap();
    let mut long = b"activewindow>>".to_vec();
    long.extend(std::iter::repeat_n(b'z', MAX_LINE * 2));
    long.push(b'\n');
    peer.write_all(&long).await.unwrap();
    peer.write_all(b"openwindow>>aa,1,c1,t\n").await.unwrap();
    assert_eq!(rx.await.unwrap(), "0xaa");
}
