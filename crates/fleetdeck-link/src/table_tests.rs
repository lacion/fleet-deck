use super::*;
use fleetdeck_core::identity::Identity;

fn fp() -> Fingerprint {
    Identity::generate().unwrap().fingerprint()
}

/// Two fingerprints, the lower one first.
fn ordered() -> (Fingerprint, Fingerprint) {
    let (a, b) = (fp(), fp());
    if a < b { (a, b) } else { (b, a) }
}

fn id(epoch: u64, attempt: &str, dialer: Dialer) -> LinkId {
    LinkId {
        epoch,
        attempt_id: attempt.into(),
        dialer,
    }
}

#[test]
fn the_same_dialer_resolves_by_epoch() {
    let (low, high) = ordered();
    for dialer in [Dialer::Local, Dialer::Remote] {
        let (old, new) = (id(3, "a", dialer), id(4, "b", dialer));
        assert!(wins(&low, &high, &new, &old));
        assert!(!wins(&low, &high, &old, &new));
        assert!(!wins(&low, &high, &old, &old));
    }
}

#[test]
fn opposite_dialers_resolve_to_the_link_of_the_lower_fingerprint_on_both_nodes() {
    let (low, high) = ordered();
    // At the low node, its own dial is Local; at the high node it is Remote.
    let at_low = (id(9, "x", Dialer::Local), id(1, "y", Dialer::Remote));
    let at_high = (id(9, "x", Dialer::Remote), id(1, "y", Dialer::Local));
    assert!(wins(&low, &high, &at_low.0, &at_low.1));
    assert!(!wins(&low, &high, &at_low.1, &at_low.0));
    assert!(wins(&high, &low, &at_high.0, &at_high.1));
    assert!(!wins(&high, &low, &at_high.1, &at_high.0));
}

#[test]
fn a_winner_supersedes_the_current_link_and_a_loser_is_refused() {
    let (own, peer) = ordered();
    let table = LinkTable::new(own);
    let first = table
        .insert(peer, id(1, "a", Dialer::Remote), false)
        .unwrap();
    let lost = table
        .insert(peer, id(1, "b", Dialer::Remote), false)
        .unwrap_err();
    assert_eq!(lost.current.attempt_id, "a");
    assert!(first.closing.borrow().is_none());
    let second = table
        .insert(peer, id(2, "c", Dialer::Remote), false)
        .unwrap();
    let closing = first.closing.borrow().clone().unwrap();
    assert_eq!(closing.reason, SUPERSEDED);
    assert!(!first.guard.is_current());
    assert!(second.guard.is_current());
    // The superseded guard does not remove its successor.
    drop(first);
    assert_eq!(table.current(&peer).unwrap().attempt_id, "c");
    drop(second);
    assert!(table.is_empty());
}

#[test]
fn a_current_check_turns_false_when_a_later_link_wins_admission() {
    let (own, peer) = ordered();
    let table = LinkTable::new(own);
    let first = table
        .insert(peer, id(1, "a", Dialer::Remote), false)
        .unwrap();
    let first_check = first.guard.check();
    assert!(first_check.is_current());
    let second = table
        .insert(peer, id(2, "b", Dialer::Remote), false)
        .unwrap();
    let second_check = second.guard.check();
    assert!(!first_check.is_current());
    assert!(!first_check.clone().is_current());
    assert!(second_check.is_current());
}

#[tokio::test]
async fn a_superseded_setup_stops_waiting_for_its_data_channel() {
    let (own, peer) = ordered();
    let table = LinkTable::new(own);
    let first = table
        .insert(peer, id(1, "a", Dialer::Remote), true)
        .unwrap();
    let _second = table
        .insert(peer, id(2, "b", Dialer::Remote), true)
        .unwrap();
    let wait = tokio::time::timeout(std::time::Duration::from_secs(1), first.data.unwrap());
    assert!(wait.await.unwrap().is_err(), "the slot was dropped");
}

#[test]
fn a_data_slot_needs_the_same_epoch_and_attempt_and_is_single_use() {
    let (own, peer) = ordered();
    let table = LinkTable::new(own);
    let _link = table
        .insert(peer, id(5, "att", Dialer::Remote), true)
        .unwrap();
    assert!(table.take_data_slot(&peer, 4, "att").is_none());
    assert!(table.take_data_slot(&peer, 5, "other").is_none());
    assert!(table.take_data_slot(&fp(), 5, "att").is_none());
    assert!(table.take_data_slot(&peer, 5, "att").is_some());
    assert!(table.take_data_slot(&peer, 5, "att").is_none());
    // A link this node dialed never takes an inbound data channel.
    let table = LinkTable::new(own);
    let _mine = table
        .insert(peer, id(5, "att", Dialer::Local), true)
        .unwrap();
    assert!(table.take_data_slot(&peer, 5, "att").is_none());
}

#[test]
fn closing_a_peer_reaches_its_current_link() {
    let (own, peer) = ordered();
    let table = LinkTable::new(own);
    assert!(!table.close_peer(
        &peer,
        Closing {
            reason: "revoked".into(),
            detail: String::new()
        }
    ));
    let link = table
        .insert(peer, id(1, "a", Dialer::Local), false)
        .unwrap();
    assert!(table.close_peer(
        &peer,
        Closing {
            reason: "revoked".into(),
            detail: String::new()
        }
    ));
    assert_eq!(link.closing.borrow().as_ref().unwrap().reason, "revoked");
}
