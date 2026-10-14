//! Tests of the login limits and the connection admission (FDV2-001,
//! FDV2-002): the windows, the reservation of running attempts, the flows
//! that do not block each other, and the verification permits.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::http::admission::{
    Admission, BOARD_MAX_CONNECTIONS, BOARD_MAX_PER_IP, FlowLimiter, HASH_PERMITS, LoginLimits,
    source_key,
};

fn ip(last: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(10, 0, 0, last))
}

#[test]
fn five_failures_of_one_ip_lock_it_until_the_window_passes() {
    let l = FlowLimiter::default();
    for t in 0..5 {
        assert_eq!(l.locked(ip(1), t * 1000), None);
        l.begin(ip(1), t * 1000).unwrap().failed(t * 1000);
    }
    assert_eq!(l.locked(ip(1), 5000), Some(56));
    assert_eq!(l.locked(ip(1), 59_999), Some(1));
    assert!(l.begin(ip(1), 5000).is_err());
    // The first failure leaves the window.
    assert_eq!(l.locked(ip(1), 60_000), None);
    assert!(l.begin(ip(1), 60_000).is_ok());
}

#[test]
fn a_second_ip_is_not_locked_by_the_first() {
    let l = FlowLimiter::default();
    for _ in 0..5 {
        l.begin(ip(1), 0).unwrap().failed(0);
    }
    assert!(l.begin(ip(1), 1).is_err());
    l.begin(ip(2), 1).unwrap().succeeded();
}

#[test]
fn the_global_window_locks_its_flow_only() {
    let limits = LoginLimits::default();
    // 10 sources with 5 failures each: 50 failures in the password flow.
    for source in 1..=10 {
        for _ in 0..5 {
            limits.password.begin(ip(source), 0).unwrap().failed(0);
        }
    }
    assert_eq!(limits.password.locked(ip(100), 1), Some(60));
    assert!(limits.password.begin(ip(100), 1).is_err());
    // The other flows are open to the same source.
    limits.login_link.begin(ip(100), 1).unwrap().succeeded();
    limits.member_join.begin(ip(100), 1).unwrap().succeeded();
    // The window passes.
    assert!(limits.password.begin(ip(100), 60_000).is_ok());
}

#[test]
fn running_attempts_count_until_they_end() {
    let l = FlowLimiter::default();
    let mut running: Vec<_> = (0..5).map(|_| l.begin(ip(1), 0).unwrap()).collect();
    // Five running attempts fill the window: a sixth never starts.
    assert!(l.begin(ip(1), 0).is_err());
    // A success frees its place.
    running.pop().unwrap().succeeded();
    // A released attempt (nothing was checked) frees its place too.
    let sixth = l.begin(ip(1), 0).unwrap();
    assert!(l.begin(ip(1), 0).is_err());
    sixth.release();
    let again = l.begin(ip(1), 0).unwrap();
    // A failure keeps its place, and moves to the time of the failure.
    again.failed(30_000);
    assert_eq!(l.counts(ip(1), 30_000), (5, 5));
    drop(running);
    // The four dropped attempts stay failures from their start.
    assert_eq!(l.locked(ip(1), 30_000), Some(31));
    assert_eq!(l.locked(ip(1), 60_000), None);
}

#[test]
fn an_attempt_that_is_dropped_counts_as_a_failure() {
    let l = FlowLimiter::default();
    for _ in 0..5 {
        drop(l.begin(ip(1), 0).unwrap());
    }
    assert_eq!(l.counts(ip(1), 1), (5, 5));
    assert_eq!(l.locked(ip(1), 1), Some(60));
}

#[tokio::test]
async fn a_cancelled_attempt_future_counts_as_a_failure() {
    let l = FlowLimiter::default();
    let pending = async {
        let attempt = l.begin(ip(1), 0).unwrap();
        std::future::pending::<()>().await;
        attempt.succeeded();
    };
    let cancelled = tokio::time::timeout(Duration::from_millis(20), pending).await;
    assert!(cancelled.is_err());
    assert_eq!(l.counts(ip(1), 1), (1, 1));
}

#[test]
fn a_source_without_records_leaves_the_map() {
    let l = FlowLimiter::default();
    l.begin(ip(1), 0).unwrap().failed(0);
    l.begin(ip(2), 0).unwrap().succeeded();
    assert_eq!(l.keys(), 1);
    assert_eq!(l.locked(ip(1), 60_000), None);
    assert_eq!(l.keys(), 0);
}

#[tokio::test]
async fn two_permits_cap_the_verifications_and_a_third_times_out() {
    let limits = LoginLimits::default();
    let a = limits.hash_permit().await.unwrap();
    let b = limits.hash_permit().await.unwrap();
    assert_eq!(limits.verifications_started.load(Ordering::SeqCst), 2);
    assert_eq!(
        limits.verifications_peak.load(Ordering::SeqCst),
        HASH_PERMITS
    );
    // The third waits 2 s and gives up; it is not a started verification.
    assert!(limits.hash_permit().await.is_none());
    assert_eq!(limits.verifications_started.load(Ordering::SeqCst), 2);
    drop(a);
    let c = limits.hash_permit().await.unwrap();
    assert_eq!(limits.verifications_started.load(Ordering::SeqCst), 3);
    assert_eq!(
        limits.verifications_peak.load(Ordering::SeqCst),
        HASH_PERMITS
    );
    drop((b, c));
}

#[test]
fn a_source_is_an_ipv4_address_or_an_ipv6_prefix() {
    let mapped: IpAddr = "::ffff:192.168.1.7".parse().unwrap();
    assert_eq!(source_key(mapped), "192.168.1.7".parse::<IpAddr>().unwrap());
    let a: IpAddr = "fd7a:115c:a1e0:1:2:3:4:5".parse().unwrap();
    let b: IpAddr = "fd7a:115c:a1e0:1:ffff::9".parse().unwrap();
    let other: IpAddr = "fd7a:115c:a1e0:2::1".parse().unwrap();
    assert_eq!(source_key(a), source_key(b));
    assert_ne!(source_key(a), source_key(other));
    assert_eq!(
        source_key(a),
        IpAddr::V6(Ipv6Addr::new(0xfd7a, 0x115c, 0xa1e0, 1, 0, 0, 0, 0))
    );
    // One /64 shares the limit of a login flow.
    let l = FlowLimiter::default();
    for _ in 0..5 {
        l.begin(a, 0).unwrap().failed(0);
    }
    assert!(l.begin(b, 1).is_err());
    assert!(l.begin(other, 1).is_ok());
}

#[tokio::test]
async fn a_source_holds_32_connections_and_all_sources_256() {
    let admission = Admission::default();
    let mut held: Vec<_> = (0..BOARD_MAX_PER_IP)
        .map(|_| admission.try_admit(ip(1)).unwrap())
        .collect();
    assert!(admission.try_admit(ip(1)).is_none());
    let second = admission.try_admit(ip(2)).unwrap();
    assert_eq!(admission.admitted(), BOARD_MAX_PER_IP + 1);
    drop(second);
    // A freed place is usable again.
    held.pop();
    held.push(admission.try_admit(ip(1)).unwrap());
    drop(held);
    assert_eq!(admission.admitted(), 0);
    // The global cap: 8 sources of 32 connections.
    let all: Vec<_> = (0..8u8)
        .flat_map(|source| (0..BOARD_MAX_PER_IP).map(move |_| source))
        .map(|source| admission.try_admit(ip(10 + source)).unwrap())
        .collect();
    assert_eq!(all.len(), BOARD_MAX_CONNECTIONS);
    assert!(admission.try_admit(ip(99)).is_none());
    drop(all);
    assert_eq!(admission.admitted(), 0);
    assert!(admission.try_admit(ip(99)).is_some());
}
