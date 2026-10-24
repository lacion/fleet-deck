use super::ring::{RING_BYTES, Ring};

#[test]
fn maintenance_ring_keeps_everything_below_its_size() {
    let mut ring = Ring::default();
    ring.push(b"abc");
    ring.push(b"def");
    assert_eq!(ring.snapshot(), b"abcdef");
}

#[test]
fn maintenance_ring_drops_the_oldest_bytes_when_full() {
    let mut ring = Ring::with_capacity(8);
    ring.push(b"abcdef");
    ring.push(b"ghij");
    assert_eq!(ring.snapshot(), b"cdefghij");
}

#[test]
fn maintenance_ring_keeps_the_tail_of_a_push_that_is_larger_than_it() {
    let mut ring = Ring::with_capacity(4);
    ring.push(b"xy");
    ring.push(b"0123456789");
    assert_eq!(ring.snapshot(), b"6789");
}

#[test]
fn maintenance_ring_has_the_size_of_the_design() {
    assert_eq!(RING_BYTES, 262_144);
    let mut ring = Ring::default();
    ring.push(&vec![7_u8; RING_BYTES + 10]);
    assert_eq!(ring.snapshot().len(), RING_BYTES);
}
