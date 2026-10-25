//! G1 (design `fleet-gaps.md` §1): a start outside the allowed roots is
//! refused with `outside_roots` and a text that names the folder.

use fleetdeck_proto::RejectCode;
use fleetdeck_proto::command::{ReceiptState, StopMode};

use super::policy_limits::outside_roots_text;
use super::{Station, plain_start, spec};

#[test]
fn the_outside_roots_text_names_the_station() {
    assert_eq!(
        outside_roots_text("/x", "box"),
        "The folder /x is outside the allowed roots of station box."
    );
    assert_eq!(
        outside_roots_text("/x", ""),
        "The folder /x is outside the allowed roots of this station."
    );
}

#[tokio::test]
async fn a_start_outside_the_allowed_roots_is_refused_with_outside_roots_and_its_text() {
    let mut st = Station::new(1).await;
    let outside = st
        .root
        .path()
        .join("elsewhere")
        .to_string_lossy()
        .into_owned();
    let d = st.send(plain_start(spec(&outside, "claude"), None)).await;
    assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
    let reject = d.receipt.reject.unwrap();
    assert_eq!(reject.code, RejectCode::OutsideRoots);
    assert_eq!(reject.message, outside_roots_text(&outside, ""));
    st.manager.shutdown(StopMode::Kill).await;
}
