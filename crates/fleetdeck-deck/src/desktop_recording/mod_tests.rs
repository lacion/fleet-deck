use super::*;

#[test]
fn the_waits_are_the_seconds_plus_35_and_40() {
    let o = RecordOptions::default();
    assert_eq!(o.wait(1), Duration::from_secs(36));
    assert_eq!(o.wait(120), Duration::from_secs(155));
    assert_eq!(o.handler(1), Duration::from_secs(41));
    assert_eq!(o.handler(120), Duration::from_secs(160));
    assert_eq!(o.stop_wait, Duration::from_secs(5));
    assert_eq!(o.stop_handler, Duration::from_secs(10));
}

#[test]
fn only_a_plain_id_goes_into_an_audit_row() {
    assert!(plain_id("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
    assert!(plain_id("a-b_c"));
    for bad in ["", "a b", "/etc/passwd", "a=b", &"x".repeat(65)] {
        assert!(!plain_id(bad), "{bad}");
    }
}
