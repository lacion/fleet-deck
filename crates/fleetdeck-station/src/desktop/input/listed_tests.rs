//! The registry of listed-window handles (R13 P7b §7): issuance and reuse,
//! the 60 s life, every invalidation, the capacity limits and the hook of a
//! window close.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{Binding, Item, Listed, Seen};
use crate::desktop::events::{Event, Events};

const SIG: &str = "sig_a_1";

fn binding(session: &str) -> Binding {
    Binding {
        deck_fp: "deck".into(),
        grant_id: "grant-1".into(),
        caller_station: "caller".into(),
        caller_session: session.into(),
        binding_id: "binding-1".into(),
        generation: 3,
    }
}

fn open(events: &Events, address: &str) -> u64 {
    events.inject(Event::Open {
        address: address.into(),
        class: "kitty".into(),
    });
    events.incarnation(address).unwrap()
}

fn item(events: &Events, address: &str) -> Item {
    Item {
        address: address.into(),
        class: "kitty".into(),
        incarnation: events.incarnation(address).unwrap(),
    }
}

fn seen<'a>(events: &'a Events, now: Instant) -> Seen<'a> {
    Seen {
        signature: SIG,
        events,
        now,
    }
}

/// An `Events` with the windows `0x1` to `0x<n>` open, a registry hooked to
/// its closes, and the first epoch.
fn world(n: usize) -> (Events, Listed) {
    let events = Events::new();
    let listed = Listed::default();
    let hooked = listed.clone();
    events.on_close(Arc::new(move |a| hooked.forget_address(a)));
    for i in 1..=n {
        open(&events, &format!("0x{i:x}"));
    }
    (events, listed)
}

fn one(listed: &Listed, events: &Events, who: &Binding, address: &str, now: Instant) -> String {
    let items = [item(events, address)];
    let issued = listed.issue(who, SIG, events.epoch(), &items, now).unwrap();
    issued.handles[0].0.clone()
}

#[test]
fn a_handle_is_32_lowercase_hex_characters_and_resolves_for_its_binding() {
    let (events, listed) = world(1);
    let now = Instant::now();
    let id = one(&listed, &events, &binding("s1"), "0x1", now);
    assert_eq!(id.len(), 32);
    assert!(id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    let found = listed
        .resolve(&id, &binding("s1"), &seen(&events, now))
        .unwrap();
    assert_eq!(
        (found.address.as_str(), found.class.as_str()),
        ("0x1", "kitty")
    );
    assert_eq!(found.incarnation, events.incarnation("0x1").unwrap());
}

#[test]
fn an_unexpired_handle_of_the_same_incarnation_is_reused_and_not_extended() {
    let (events, listed) = world(1);
    let t0 = Instant::now();
    let items = [item(&events, "0x1")];
    let first = listed
        .issue(&binding("s1"), SIG, events.epoch(), &items, t0)
        .unwrap();
    assert_eq!(first.handles[0].1, 60_000);
    assert_eq!(first.fresh.len(), 1);
    let later = t0 + Duration::from_secs(20);
    let again = listed
        .issue(&binding("s1"), SIG, events.epoch(), &items, later)
        .unwrap();
    assert_eq!(again.handles[0].0, first.handles[0].0);
    // The life counts from the first issue: 40 s are left, not 60.
    assert_eq!(again.handles[0].1, 40_000);
    assert!(again.fresh.is_empty());
    assert_eq!(listed.len(), 1);
    // Another binding gets its own handle for the same window.
    let other = listed
        .issue(&binding("s2"), SIG, events.epoch(), &items, later)
        .unwrap();
    assert_ne!(other.handles[0].0, first.handles[0].0);
}

#[test]
fn a_handle_expires_after_sixty_seconds() {
    let (events, listed) = world(1);
    let t0 = Instant::now();
    let id = one(&listed, &events, &binding("s1"), "0x1", t0);
    let who = binding("s1");
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    assert!(
        listed
            .resolve(&id, &who, &seen(&events, at(59_999)))
            .is_some()
    );
    assert!(
        listed
            .resolve(&id, &who, &seen(&events, at(60_000)))
            .is_none()
    );
    // A new listing after the expiry issues a new handle.
    let items = [item(&events, "0x1")];
    let again = listed
        .issue(&who, SIG, events.epoch(), &items, at(61_000))
        .unwrap();
    assert_ne!(again.handles[0].0, id);
    assert_eq!(again.handles[0].1, 60_000);
}

#[test]
fn a_close_a_reopen_an_epoch_change_and_a_new_target_each_invalidate() {
    let who = binding("s1");
    // A close (the hook removes the entry).
    let (events, listed) = world(2);
    let now = Instant::now();
    let a = one(&listed, &events, &who, "0x1", now);
    let b = one(&listed, &events, &who, "0x2", now);
    events.inject(Event::Close {
        address: "0x1".into(),
    });
    assert!(listed.resolve(&a, &who, &seen(&events, now)).is_none());
    assert!(listed.resolve(&b, &who, &seen(&events, now)).is_some());
    assert_eq!(listed.len(), 1, "the hook removed the handle of the window");
    // A reopen of the same address: a new incarnation, the old handle stays
    // invalid even though the address is the same.
    let before = events.incarnation("0x2").unwrap();
    events.inject(Event::Open {
        address: "0x2".into(),
        class: "kitty".into(),
    });
    assert!(events.incarnation("0x2").unwrap() > before);
    assert!(listed.resolve(&b, &who, &seen(&events, now)).is_none());
    // Another target identity.
    let (events, listed) = world(1);
    let id = one(&listed, &events, &who, "0x1", now);
    let other = Seen {
        signature: "sig_b_2",
        events: &events,
        now,
    };
    assert!(listed.resolve(&id, &who, &other).is_none());
    assert!(listed.resolve(&id, &who, &seen(&events, now)).is_some());
    // An epoch change (a gap in the stream).
    events.inject_gap();
    assert!(listed.resolve(&id, &who, &seen(&events, now)).is_none());
}

#[test]
fn another_binding_generation_session_or_deck_gets_the_same_none() {
    let (events, listed) = world(1);
    let now = Instant::now();
    let who = binding("s1");
    let id = one(&listed, &events, &who, "0x1", now);
    let mut generation = who.clone();
    generation.generation += 1;
    let mut deck = who.clone();
    deck.deck_fp = "other".into();
    let mut grant = who.clone();
    grant.grant_id = "grant-2".into();
    let mut station = who.clone();
    station.caller_station = "elsewhere".into();
    for foreign in [generation, deck, grant, station, binding("s2")] {
        assert!(
            listed.resolve(&id, &foreign, &seen(&events, now)).is_none(),
            "{foreign:?}"
        );
    }
    // An unknown handle gives the same answer.
    assert!(
        listed
            .resolve(&"0".repeat(32), &who, &seen(&events, now))
            .is_none()
    );
    assert!(listed.resolve(&id, &who, &seen(&events, now)).is_some());
}

#[test]
fn a_changed_class_gets_a_new_handle_and_the_old_one_names_the_old_class() {
    let (events, listed) = world(1);
    let now = Instant::now();
    let who = binding("s1");
    let old = one(&listed, &events, &who, "0x1", now);
    let renamed = Item {
        class: "other".into(),
        ..item(&events, "0x1")
    };
    let issued = listed
        .issue(&who, SIG, events.epoch(), &[renamed], now)
        .unwrap();
    assert_ne!(issued.handles[0].0, old);
    let found = listed.resolve(&old, &who, &seen(&events, now)).unwrap();
    assert_eq!(found.class, "kitty");
}

#[test]
fn sixty_four_handles_for_a_binding_then_busy_and_nothing_is_evicted() {
    let (events, listed) = world(66);
    let now = Instant::now();
    let who = binding("s1");
    let items: Vec<Item> = (1..=64)
        .map(|i| item(&events, &format!("0x{i:x}")))
        .collect();
    let issued = listed
        .issue(&who, SIG, events.epoch(), &items, now)
        .unwrap();
    assert_eq!(issued.handles.len(), 64);
    // One more window of the same binding: busy.
    let extra = [item(&events, "0x41")];
    let e = listed
        .issue(&who, SIG, events.epoch(), &extra, now)
        .unwrap_err();
    assert_eq!(e.code, "busy");
    assert_eq!(listed.len(), 64);
    for (id, _) in &issued.handles {
        assert!(listed.resolve(id, &who, &seen(&events, now)).is_some());
    }
    // A listing that reuses all 64 still works.
    let again = listed
        .issue(&who, SIG, events.epoch(), &items, now)
        .unwrap();
    assert!(again.fresh.is_empty());
    // Another binding is not blocked by the limit of the first.
    one(&listed, &events, &binding("s2"), "0x41", now);
}

#[test]
fn a_listing_that_does_not_fit_issues_nothing() {
    let (events, listed) = world(66);
    let now = Instant::now();
    let who = binding("s1");
    let first: Vec<Item> = (1..=63)
        .map(|i| item(&events, &format!("0x{i:x}")))
        .collect();
    listed
        .issue(&who, SIG, events.epoch(), &first, now)
        .unwrap();
    // Two new windows need two slots; one is left: all or none.
    let two = [item(&events, "0x40"), item(&events, "0x41")];
    let e = listed
        .issue(&who, SIG, events.epoch(), &two, now)
        .unwrap_err();
    assert_eq!(e.code, "busy");
    assert_eq!(listed.len(), 63);
}

#[test]
fn the_station_holds_at_most_1024_handles_and_evicts_none() {
    let (events, listed) = world(64);
    let now = Instant::now();
    let items: Vec<Item> = (1..=64)
        .map(|i| item(&events, &format!("0x{i:x}")))
        .collect();
    let mut first = None;
    for n in 0..16 {
        let who = binding(&format!("s{n}"));
        let issued = listed
            .issue(&who, SIG, events.epoch(), &items, now)
            .unwrap();
        first.get_or_insert((who, issued.handles[0].0.clone()));
    }
    assert_eq!(listed.len(), 1024);
    let e = listed
        .issue(&binding("s16"), SIG, events.epoch(), &items[..1], now)
        .unwrap_err();
    assert_eq!(e.code, "busy");
    assert_eq!(listed.len(), 1024);
    let (who, id) = first.unwrap();
    assert!(listed.resolve(&id, &who, &seen(&events, now)).is_some());
    // Expired entries are no live entries: they free the room.
    let later = now + Duration::from_millis(60_000);
    one(&listed, &events, &binding("s16"), "0x1", later);
}

#[test]
fn a_revoke_removes_the_handles_of_a_failed_listing() {
    let (events, listed) = world(2);
    let now = Instant::now();
    let who = binding("s1");
    let kept = one(&listed, &events, &who, "0x1", now);
    let items = [item(&events, "0x1"), item(&events, "0x2")];
    let issued = listed
        .issue(&who, SIG, events.epoch(), &items, now)
        .unwrap();
    assert_eq!(issued.fresh.len(), 1, "0x1 is reused");
    listed.revoke(&issued.fresh);
    assert_eq!(listed.len(), 1);
    assert!(listed.resolve(&kept, &who, &seen(&events, now)).is_some());
}
