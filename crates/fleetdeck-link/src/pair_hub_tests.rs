use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};

use fleetdeck_core::identity::Identity;

use super::*;

/// An in-memory approver: the completion records, and a switch that makes
/// every commit fail.
#[derive(Debug, Default)]
struct Memory {
    done: Mutex<HashMap<String, Completion>>,
    fail: Mutex<Option<CommitError>>,
}

#[async_trait::async_trait]
impl PairApprover for Memory {
    async fn commit(&self, c: &Completion) -> Result<(), CommitError> {
        if let Some(e) = self.fail.lock().unwrap().clone() {
            return Err(e);
        }
        self.done
            .lock()
            .unwrap()
            .insert(c.request_id.clone(), c.clone());
        Ok(())
    }

    async fn completion(&self, id: &str) -> Result<Option<Completion>, String> {
        Ok(self.done.lock().unwrap().get(id).cloned())
    }
}

fn key() -> PeerKey {
    Identity::generate().unwrap().public_key()
}

fn ip(last: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(100, 64, 0, last))
}

fn request(name: &str) -> CanonicalRequest {
    CanonicalRequest::generate(name, [Scope::Read].into()).unwrap()
}

fn hub_with(cfg: HubConfig) -> (Arc<PairRequests>, Arc<Memory>, Arc<PinSet>) {
    let memory = Arc::new(Memory::default());
    let pins = PinSet::new();
    let hub = PairRequests::new(
        key(),
        [Scope::Admin].into(),
        Arc::clone(&pins),
        Arc::clone(&memory) as Arc<dyn PairApprover>,
        cfg,
    );
    (hub, memory, pins)
}

fn hub() -> (Arc<PairRequests>, Arc<Memory>, Arc<PinSet>) {
    hub_with(HubConfig::default())
}

/// A hub with an open window (one tailnet listener is counted).
async fn open(cfg: HubConfig) -> (Arc<PairRequests>, Arc<Memory>, Arc<PinSet>, ListenerGuard) {
    let (hub, memory, pins) = hub_with(cfg);
    let guard = hub.add_listener("100.64.0.1:7421".parse().unwrap());
    hub.open_window().await.unwrap();
    (hub, memory, pins, guard)
}

fn digest_of(view: &PendingView) -> [u8; 32] {
    crate::pairing::unhex::<32>(&view.digest).unwrap()
}

#[tokio::test]
async fn the_window_opens_only_with_a_tailnet_listener_and_has_a_generation() {
    let (hub, ..) = hub();
    assert_eq!(hub.open_window().await, Err(NoTailnetListener));
    assert!(!hub.is_open());
    let guard = hub.add_listener("100.64.0.1:7421".parse().unwrap());
    let first = hub.open_window().await.unwrap();
    assert!(first.open && first.deadline_ms.is_some());
    assert!(hub.is_open());
    let second = hub.open_window().await.unwrap();
    assert_eq!(second.generation, first.generation + 1);
    assert!(hub.close_window().await);
    let closed = hub.window();
    assert!(!closed.open && closed.deadline_ms.is_none());
    assert_eq!(closed.generation, second.generation);
    drop(guard);
    assert_eq!(hub.tailnet_listeners(), 0);
}

#[tokio::test]
async fn a_request_is_refused_while_the_window_is_closed_or_the_key_is_paired() {
    let (hub, ..) = hub();
    let _guard = hub.add_listener("100.64.0.1:7421".parse().unwrap());
    let r = hub.admit(request("a"), key(), ip(1), false).await;
    assert_eq!(r.unwrap_err(), Refusal::WindowClosed);
    hub.open_window().await.unwrap();
    let r = hub.admit(request("a"), key(), ip(1), true).await;
    assert_eq!(r.unwrap_err(), Refusal::AlreadyPaired);
    hub.close_window().await;
    let r = hub.admit(request("a"), key(), ip(1), false).await;
    assert_eq!(r.unwrap_err(), Refusal::WindowClosed);
}

#[tokio::test]
async fn the_limits_are_one_per_key_two_per_source_and_sixteen_in_all() {
    let (hub, _m, _p, _g) = open(HubConfig::default()).await;
    let k = key();
    let a = hub.admit(request("a"), k, ip(1), false).await.unwrap();
    // A second request of the same key waits for none: busy.
    let again = hub.admit(request("a2"), k, ip(1), false).await;
    assert_eq!(again.unwrap_err(), Refusal::Busy);
    // The same source: 2 pending, the third is busy.
    let b = hub.admit(request("b"), key(), ip(1), false).await.unwrap();
    let third = hub.admit(request("c"), key(), ip(1), false).await;
    assert_eq!(third.unwrap_err(), Refusal::Busy);
    // Another source still gets a place; 16 in all.
    let mut held = vec![a, b];
    for n in 2..=15 {
        held.push(hub.admit(request("x"), key(), ip(n), false).await.unwrap());
    }
    assert_eq!(held.len(), 16);
    let full = hub.admit(request("x"), key(), ip(99), false).await;
    assert_eq!(full.unwrap_err(), Refusal::Busy, "the 17th is refused, none is evicted");
    assert_eq!(hub.list().await.len(), 16);
}

#[tokio::test]
async fn an_ipv6_source_counts_by_its_64_prefix() {
    let (hub, _m, _p, _g) = open(HubConfig::default()).await;
    let a: IpAddr = "fd7a:115c:a1e0::1".parse().unwrap();
    let b: IpAddr = "fd7a:115c:a1e0::2".parse().unwrap();
    let _x = hub.admit(request("a"), key(), a, false).await.unwrap();
    let _y = hub.admit(request("b"), key(), b, false).await.unwrap();
    let z = hub.admit(request("c"), key(), a, false).await;
    assert_eq!(z.unwrap_err(), Refusal::Busy);
}

#[tokio::test(start_paused = true)]
async fn thirty_new_requests_a_minute_and_then_busy() {
    let cfg = HubConfig {
        max_per_source: 100,
        max_pending: 100,
        ..HubConfig::default()
    };
    let (hub, _m, _p, _g) = open(cfg).await;
    for n in 0..30 {
        let a = hub.admit(request("x"), key(), ip(n), false).await.unwrap();
        // The request ends; the global rate counts it anyway.
        assert!(hub.finish(&a.id).await);
    }
    let r = hub.admit(request("x"), key(), ip(40), false).await;
    assert_eq!(r.unwrap_err(), Refusal::Busy);
    tokio::time::advance(RATE_WINDOW).await;
    assert!(hub.admit(request("x"), key(), ip(40), false).await.is_ok());
}

#[tokio::test]
async fn a_request_id_of_another_key_or_another_request_is_a_conflict() {
    let (hub, memory, _p, _g) = open(HubConfig::default()).await;
    let k = key();
    let req = request("a");
    let a = hub.admit(req.clone(), k, ip(1), false).await.unwrap();
    // Another key, same id.
    let other = hub.admit(req.clone(), key(), ip(2), false).await;
    assert_eq!(other.unwrap_err(), Refusal::RequestIdConflict);
    // The same key, same id, another content.
    let mut changed = req.clone();
    changed.name = "b".into();
    let again = hub.admit(changed, k, ip(1), false).await;
    assert_eq!(again.unwrap_err(), Refusal::RequestIdConflict);
    // The same key and request again while pending: busy (the old one stays).
    let dup = hub.admit(req.clone(), k, ip(1), false).await;
    assert_eq!(dup.unwrap_err(), Refusal::Busy);
    assert!(hub.finish(&a.id).await);
    // A stored approval holds its id for ever.
    let id = req.id_hex();
    memory.done.lock().unwrap().insert(
        id.clone(),
        Completion {
            request_id: id,
            station_key: k,
            deck_key: hub.deck_key(),
            request_digest: req.digest(),
            nonce: req.nonce,
            requested_name: req.name.clone(),
            offered_scopes: req.offered_scopes.clone(),
            granted_scopes: [Scope::Admin].into(),
            window_generation: 1,
            approved_at: 1,
        },
    );
    let other = hub.admit(req.clone(), key(), ip(3), false).await;
    assert_eq!(other.unwrap_err(), Refusal::RequestIdConflict);
    let same = hub.admit(req, k, ip(1), false).await;
    assert_eq!(same.unwrap_err(), Refusal::AlreadyPaired);
}

#[tokio::test]
async fn approve_commits_pins_and_wakes_the_connection() {
    let (hub, memory, pins, _g) = open(HubConfig::default()).await;
    let k = key();
    let a = hub.admit(request("laptop"), k, ip(1), false).await.unwrap();
    let view = hub.list().await.remove(0);
    assert_eq!(view.requested_name, "laptop");
    assert_eq!(view.station_fingerprint, k.fingerprint().to_string());
    let approved = hub
        .approve(&a.id, &k.fingerprint(), &digest_of(&view))
        .await
        .unwrap();
    assert_eq!(approved.station, k);
    assert_eq!(a.decision.await.unwrap(), Decision::Approved);
    assert_eq!(pins.role_of(&k), Some(Role::Station));
    let done = memory.done.lock().unwrap().get(&a.id).cloned().unwrap();
    assert_eq!(done.requested_name, "laptop");
    assert_eq!(done.granted_scopes, [Scope::Admin].into());
    assert_eq!(done.offered_scopes, [Scope::Read].into());
    assert_eq!(done.window_generation, hub.window().generation);
    assert!(hub.list().await.is_empty());
}

#[tokio::test]
async fn approve_refuses_a_wrong_key_a_wrong_digest_and_writes_nothing() {
    let (hub, memory, pins, _g) = open(HubConfig::default()).await;
    let k = key();
    let a = hub.admit(request("laptop"), k, ip(1), false).await.unwrap();
    let digest = digest_of(&hub.list().await[0]);
    // Key substitution: the owner approved another fingerprint.
    let err = hub
        .approve(&a.id, &key().fingerprint(), &digest)
        .await
        .unwrap_err();
    assert_eq!(err, ApproveError::FingerprintMismatch);
    // A stale digest (the owner saw another record).
    let err = hub
        .approve(&a.id, &k.fingerprint(), &[7u8; 32])
        .await
        .unwrap_err();
    assert_eq!(err, ApproveError::DigestMismatch);
    let err = hub
        .approve("00000000000000000000000000000000", &k.fingerprint(), &digest)
        .await
        .unwrap_err();
    assert_eq!(err, ApproveError::UnknownRequest);
    assert!(memory.done.lock().unwrap().is_empty());
    assert!(pins.is_empty());
    // The request still waits and can be approved with the right values.
    assert!(hub.approve(&a.id, &k.fingerprint(), &digest).await.is_ok());
}

#[tokio::test]
async fn a_failed_commit_leaves_the_request_pending_and_pins_nothing() {
    let (hub, memory, pins, _g) = open(HubConfig::default()).await;
    let k = key();
    let a = hub.admit(request("laptop"), k, ip(1), false).await.unwrap();
    let digest = digest_of(&hub.list().await[0]);
    for (set, expect) in [
        (CommitError::AlreadyPaired, ApproveError::AlreadyPaired),
        (
            CommitError::NameTaken("laptop".into()),
            ApproveError::NameTaken("laptop".into()),
        ),
        (CommitError::Store("disk".into()), ApproveError::Store("disk".into())),
    ] {
        *memory.fail.lock().unwrap() = Some(set);
        let err = hub.approve(&a.id, &k.fingerprint(), &digest).await.unwrap_err();
        assert_eq!(err, expect);
        assert!(pins.is_empty());
        assert_eq!(hub.list().await.len(), 1, "the owner can still deny it");
    }
}

#[tokio::test]
async fn approval_is_refused_after_a_close_and_after_a_close_and_reopen() {
    let (hub, memory, _p, _g) = open(HubConfig::default()).await;
    let k = key();
    let a = hub.admit(request("a"), k, ip(1), false).await.unwrap();
    let digest = digest_of(&hub.list().await[0]);
    let first = hub.window().generation;
    // Close: the request closes with `window_closed`; approval is refused.
    hub.close_window().await;
    assert_eq!(a.decision.await.unwrap(), Decision::Closed("window_closed"));
    let err = hub.approve(&a.id, &k.fingerprint(), &digest).await.unwrap_err();
    assert_eq!(err, ApproveError::WindowClosed);
    // Close and reopen: a new generation; the old request is never approvable.
    let b = hub.admit(request("b"), key(), ip(2), false).await;
    assert_eq!(b.unwrap_err(), Refusal::WindowClosed);
    hub.open_window().await.unwrap();
    assert_eq!(hub.window().generation, first + 1);
    let err = hub.approve(&a.id, &k.fingerprint(), &digest).await.unwrap_err();
    assert_eq!(err, ApproveError::WindowClosed);
    assert!(memory.done.lock().unwrap().is_empty());
    // A reopen while a request waits closes it too.
    let c = hub.admit(request("c"), key(), ip(3), false).await.unwrap();
    hub.open_window().await.unwrap();
    assert_eq!(c.decision.await.unwrap(), Decision::Closed("window_closed"));
}

#[tokio::test(start_paused = true)]
async fn a_request_expires_at_the_window_deadline_and_a_late_timer_does_not_approve() {
    let cfg = HubConfig {
        window_ttl: Duration::from_secs(60),
        request_ttl: Duration::from_secs(600),
        ..HubConfig::default()
    };
    let (hub, memory, _p, _g) = open(cfg).await;
    tokio::time::advance(Duration::from_secs(40)).await;
    let k = key();
    let a = hub.admit(request("a"), k, ip(1), false).await.unwrap();
    // The expiry is the window deadline (20 s away), not 10 minutes.
    assert_eq!(a.expires, hub.window_deadline());
    let digest = digest_of(&hub.list().await[0]);
    tokio::time::advance(Duration::from_secs(21)).await;
    // The connection's own timer has not run, but the approval is refused.
    let err = hub.approve(&a.id, &k.fingerprint(), &digest).await.unwrap_err();
    assert!(
        matches!(err, ApproveError::UnknownRequest | ApproveError::WindowClosed),
        "{err:?}"
    );
    assert!(memory.done.lock().unwrap().is_empty());
    assert!(hub.list().await.is_empty());
}

#[tokio::test]
async fn deny_closes_with_denied_and_starts_a_cooldown_of_that_key() {
    let (hub, _m, _p, _g) = open(HubConfig::default()).await;
    let k = key();
    let a = hub.admit(request("a"), k, ip(1), false).await.unwrap();
    assert_eq!(hub.deny(&a.id).await.unwrap(), k);
    assert_eq!(a.decision.await.unwrap(), Decision::Closed("denied"));
    assert_eq!(hub.deny(&a.id).await.unwrap_err(), ApproveError::UnknownRequest);
    let again = hub.admit(request("b"), k, ip(2), false).await;
    assert_eq!(again.unwrap_err(), Refusal::Denied);
    // Another key is not affected.
    assert!(hub.admit(request("c"), key(), ip(2), false).await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn the_cooldown_ends_after_ten_minutes() {
    let cfg = HubConfig {
        window_ttl: Duration::from_secs(3600),
        request_ttl: Duration::from_secs(600),
        ..HubConfig::default()
    };
    let (hub, _m, _p, _g) = open(cfg).await;
    let k = key();
    let a = hub.admit(request("a"), k, ip(1), false).await.unwrap();
    hub.deny(&a.id).await.unwrap();
    tokio::time::advance(COOLDOWN - Duration::from_secs(1)).await;
    let r = hub.admit(request("b"), k, ip(1), false).await;
    assert_eq!(r.unwrap_err(), Refusal::Denied);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(hub.admit(request("b"), k, ip(1), false).await.is_ok());
}

#[tokio::test]
async fn a_dropped_connection_cancels_the_request_before_an_approval() {
    let (hub, memory, _p, _g) = open(HubConfig::default()).await;
    let k = key();
    let a = hub.admit(request("a"), k, ip(1), false).await.unwrap();
    let digest = digest_of(&hub.list().await[0]);
    // The connection task is gone: its receiver drops.
    drop(a.decision);
    let err = hub.approve(&a.id, &k.fingerprint(), &digest).await.unwrap_err();
    assert_eq!(err, ApproveError::UnknownRequest);
    assert!(memory.done.lock().unwrap().is_empty());
    // And a finish after the approval reports that a decision came first.
    let b = hub.admit(request("b"), key(), ip(2), false).await.unwrap();
    let digest = digest_of(&hub.list().await[0]);
    let kb = hub.list().await[0].station_fingerprint.parse().unwrap();
    hub.approve(&b.id, &kb, &digest).await.unwrap();
    assert!(!hub.finish(&b.id).await);
}

#[tokio::test]
async fn cancel_key_and_cancel_all_end_waiting_requests_with_cancelled() {
    let (hub, _m, _p, _g) = open(HubConfig::default()).await;
    let k = key();
    let a = hub.admit(request("a"), k, ip(1), false).await.unwrap();
    let b = hub.admit(request("b"), key(), ip(2), false).await.unwrap();
    hub.cancel_key(&k).await;
    assert_eq!(a.decision.await.unwrap(), Decision::Closed("cancelled"));
    assert_eq!(hub.list().await.len(), 1);
    hub.cancel_all().await;
    assert_eq!(b.decision.await.unwrap(), Decision::Closed("cancelled"));
    assert!(hub.list().await.is_empty());
}

#[tokio::test]
async fn the_device_list_finds_a_pending_request_by_source_ip() {
    let (hub, _m, _p, _g) = open(HubConfig::default()).await;
    let _a = hub.admit(request("a"), key(), ip(5), false).await.unwrap();
    assert!(hub.has_request_from(ip(5)).await);
    assert!(!hub.has_request_from(ip(6)).await);
}
