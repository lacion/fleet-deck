//! The idle time of a link (B22, design `last-seen.md` §3.2).

use super::*;

#[tokio::test]
async fn since_last_frame_grows_without_frames_and_resets_on_a_frame() {
    let config = ControlConfig {
        heartbeat: Duration::from_secs(600),
        idle_limit: Duration::from_secs(600),
        ..ControlConfig::default()
    };
    let (s, mut raw) = with_raw(Role::Deck, feature::ALL, 1 << 20, config).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        s.control.since_last_frame() >= Duration::from_millis(250),
        "the idle time grows while no frame comes"
    );
    send_raw(&mut raw, &ControlFrame::Ping { nonce: 1 }).await;
    for _ in 0..200 {
        if s.control.since_last_frame() < Duration::from_millis(100) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("a frame from the peer did not reset the idle time");
}

#[tokio::test]
async fn an_abrupt_disconnect_does_not_count_as_a_frame() {
    let config = ControlConfig {
        heartbeat: Duration::from_secs(600),
        idle_limit: Duration::from_secs(600),
        ..ControlConfig::default()
    };
    let (s, raw) = with_raw(Role::Deck, feature::ALL, 1 << 20, config).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(raw);
    // The read loop sees the end of the socket and stops.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        s.control.since_last_frame() >= Duration::from_millis(450),
        "the end of the socket reset the idle time: {:?}",
        s.control.since_last_frame()
    );
}
