//! The read permits (R13 P6 §5.7): the id, the hash, the five checks, the
//! single use and the slots.

use std::time::{Duration, Instant};

use fleetdeck_link::desktop_authority::read_hash;

use super::*;

const NODE: &str = "node-fingerprint-of-the-target";
const EPOCH: &str = "0123456789abcdef0123456789abcdef";

fn ctx() -> CoordinatorContext {
    CoordinatorContext {
        grant_id: "g1".into(),
        binding_id: "b1".into(),
        generation: 3,
        caller_station: StationId::from("caller"),
        caller_session: "coord-1".into(),
        caller_label: "coord-1".into(),
    }
}

fn registry() -> Permits {
    Permits::new(Arc::new(Slots::default()))
}

fn register(permits: &Permits) -> Registered {
    permits
        .register(
            ctx(),
            (StationId::from("target"), NODE.into(), EPOCH.into()),
            OpClass::Desktop,
            ProtectedRead::Info,
            Instant::now() + Duration::from_secs(60),
        )
        .expect("a slot")
}

struct Got {
    station: StationId,
    node: String,
    epoch: Option<String>,
    read: ProtectedRead,
    hash: String,
}

impl Got {
    fn of(reg: &Registered) -> Self {
        Self {
            station: StationId::from("target"),
            node: NODE.into(),
            epoch: Some(EPOCH.into()),
            read: ProtectedRead::Info,
            hash: reg.read_hash.clone(),
        }
    }

    fn arrival(&self) -> Arrival<'_> {
        Arrival {
            station: &self.station,
            station_node: &self.node,
            epoch: self.epoch.as_deref(),
            read: &self.read,
            hash: &self.hash,
        }
    }
}

#[test]
fn a_permit_has_a_random_id_of_32_hex_characters_and_the_contract_hash() {
    let permits = registry();
    let one = register(&permits);
    let two = register(&permits);
    for id in [&one.read_id, &two.read_id] {
        assert_eq!(id.len(), 32);
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
    }
    assert_ne!(one.read_id, two.read_id);
    let want = read_hash(&ctx(), EPOCH, &ProtectedRead::Info, &one.read_id, NODE);
    assert_eq!(one.read_hash, want);
    assert_eq!(permits.len(), 2);
}

#[test]
fn the_exact_arrival_passes_all_five_checks() {
    let permits = registry();
    let reg = register(&permits);
    let permit = permits.take(&reg.read_id).expect("the permit");
    let got = Got::of(&reg);
    assert!(permit.matches(&got.arrival(), &reg.read_id, Instant::now()));
}

#[test]
fn a_permit_is_single_use_and_the_first_check_takes_it() {
    let permits = registry();
    let reg = register(&permits);
    assert!(permits.take(&reg.read_id).is_some());
    assert!(permits.take(&reg.read_id).is_none(), "a second use");
    assert_eq!(permits.len(), 0);
    assert!(permits.take("ffffffffffffffffffffffffffffffff").is_none());
}

#[test]
fn each_check_refuses_on_its_own() {
    let permits = registry();
    let reg = register(&permits);
    let permit = permits.take(&reg.read_id).expect("the permit");
    let ok = Got::of(&reg);
    assert!(permit.matches(&ok.arrival(), &reg.read_id, Instant::now()));
    // 1. Expired.
    let late = Instant::now() + Duration::from_secs(61);
    assert!(!permit.matches(&ok.arrival(), &reg.read_id, late));
    // 2. Another station, or another node of the station.
    let mut got = Got::of(&reg);
    got.station = StationId::from("other");
    assert!(!permit.matches(&got.arrival(), &reg.read_id, Instant::now()));
    let mut got = Got::of(&reg);
    got.node = "another-node".into();
    assert!(!permit.matches(&got.arrival(), &reg.read_id, Instant::now()));
    // 3. Another connection, or none.
    let mut got = Got::of(&reg);
    got.epoch = Some("fedcba9876543210fedcba9876543210".into());
    assert!(!permit.matches(&got.arrival(), &reg.read_id, Instant::now()));
    got.epoch = None;
    assert!(!permit.matches(&got.arrival(), &reg.read_id, Instant::now()));
    // 4. Another read object.
    let mut got = Got::of(&reg);
    got.read = ProtectedRead::ActionStatus {
        operation_id: "op".into(),
    };
    assert!(!permit.matches(&got.arrival(), &reg.read_id, Instant::now()));
    // 5. Another hash, and another read id (the hash covers it).
    let mut got = Got::of(&reg);
    got.hash = "0".repeat(64);
    assert!(!permit.matches(&got.arrival(), &reg.read_id, Instant::now()));
    assert!(!permit.matches(
        &ok.arrival(),
        "00000000000000000000000000000000",
        Instant::now()
    ));
}

#[test]
fn at_most_64_permits_live_and_a_removed_permit_frees_its_slot() {
    let permits = registry();
    let regs: Vec<Registered> = (0..64).map(|_| register(&permits)).collect();
    let refused = permits.register(
        ctx(),
        (StationId::from("target"), NODE.into(), EPOCH.into()),
        OpClass::Desktop,
        ProtectedRead::Info,
        Instant::now() + Duration::from_secs(1),
    );
    assert_eq!(refused.err(), Some("busy"));
    permits.remove(&regs[0].read_id);
    let again = register(&permits);
    assert_eq!(again.read_id.len(), 32);
}

#[test]
fn the_guard_removes_the_permit_when_it_is_dropped() {
    let permits = registry();
    let reg = register(&permits);
    {
        let _guard = PermitGuard {
            permits: &permits,
            read_id: reg.read_id.clone(),
        };
        assert_eq!(permits.len(), 1);
    }
    assert_eq!(permits.len(), 0);
}
