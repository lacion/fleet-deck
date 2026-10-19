//! Cancellation of `Control::request` (MCP P1 §3.3 step 4, test 19).

use super::*;

/// Waits until `check` holds; fails after a real-time limit.
async fn until(what: &str, check: impl Fn() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timeout: {what}");
}

#[tokio::test]
async fn a_dropped_link_request_before_the_queue_frees_its_slot_and_a_late_reply_after_the_queue_keeps_the_link()
 {
    // Part 1: a request that waits for a queue slot. The raw peer never
    // reads and the socket buffer is tiny, so the outbound queue stays full.
    let config = ControlConfig {
        queue: 2,
        write_limit: Duration::from_secs(600),
        idle_limit: Duration::from_secs(600),
        ..ControlConfig::default()
    };
    let (s, mut raw) = with_raw(Role::Deck, feature::ALL, 256, config).await;
    let filler = {
        let c = s.control.clone();
        tokio::spawn(async move {
            let mut n = 0;
            while c.send(ack(n)).await.is_ok() {
                n += 1;
            }
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!filler.is_finished(), "the filler waits for queue capacity");
    let before = s.control.open_requests();
    let waiting = {
        let c = s.control.clone();
        tokio::spawn(async move { c.request("station.info", None, json!({})).await })
    };
    until("the request reserved its slot", || {
        s.control.open_requests() == before + 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!waiting.is_finished(), "the request waits in enqueue");
    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    assert_eq!(s.control.open_requests(), before);
    assert_eq!(s.control.outstanding(), 0);
    // The dropped request never reached the peer.
    filler.abort();
    let mut frames = 0;
    while let Ok(Some(frame)) =
        tokio::time::timeout(Duration::from_millis(200), recv_raw(&mut raw)).await
    {
        assert!(
            !matches!(frame, ControlFrame::Request { .. }),
            "a dropped request was sent: {frame:?}"
        );
        frames += 1;
    }
    assert!(frames > 0, "the peer read no frame");
    assert!(s.control.ended().is_none());

    // Part 2: a request that was queued and sent, dropped while it awaits
    // its reply. The late reply must not close the link.
    let (s, mut raw) = with_raw(Role::Deck, feature::ALL, 1 << 20, ControlConfig::default()).await;
    let before = s.control.open_requests();
    let sent = {
        let c = s.control.clone();
        tokio::spawn(async move { c.request("station.info", None, json!({})).await })
    };
    let id = match recv_raw(&mut raw).await {
        Some(ControlFrame::Request { id, .. }) => id,
        other => panic!("expected a request, got {other:?}"),
    };
    sent.abort();
    assert!(sent.await.unwrap_err().is_cancelled());
    // After the queue the entry stays until the reply arrives.
    assert_eq!(s.control.open_requests(), before + 1);
    send_raw(
        &mut raw,
        &ControlFrame::Response {
            id,
            outcome: Outcome::Ok(json!("late")),
        },
    )
    .await;
    until("the late reply freed the slot", || {
        s.control.open_requests() == before
    })
    .await;
    assert!(s.control.ended().is_none(), "{:?}", s.control.ended());
    // The link still works.
    let next = {
        let c = s.control.clone();
        tokio::spawn(async move { c.request("station.info", None, json!({})).await })
    };
    let id = match recv_raw(&mut raw).await {
        Some(ControlFrame::Request { id, .. }) => id,
        other => panic!("expected a request, got {other:?}"),
    };
    send_raw(
        &mut raw,
        &ControlFrame::Response {
            id,
            outcome: Outcome::Ok(json!("ok")),
        },
    )
    .await;
    assert_eq!(next.await.unwrap().unwrap(), Outcome::Ok(json!("ok")));
    assert_eq!(s.control.open_requests(), before);
}
