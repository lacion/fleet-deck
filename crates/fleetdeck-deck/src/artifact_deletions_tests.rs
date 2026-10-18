//! Unit tests of the order of the deletion tries.

use std::collections::HashSet;

use super::order;
use crate::store::artifact_tombstones::TombstoneDue;

fn pending() -> Vec<(String, TombstoneDue)> {
    vec![
        ("a-due".to_owned(), TombstoneDue::Due),
        ("b-link".to_owned(), TombstoneDue::OnLink),
        ("c-wait".to_owned(), TombstoneDue::Waiting),
        ("d-link".to_owned(), TombstoneDue::OnLink),
    ]
}

#[test]
fn a_new_link_tries_the_rows_that_wait_for_a_link_first_and_then_the_due_rows() {
    let ids = order(pending(), &HashSet::new());
    assert_eq!(ids, ["b-link", "d-link", "a-due"]);
}

#[test]
fn a_row_that_was_tried_on_this_link_waits_for_the_next_link() {
    let tried: HashSet<String> = ["b-link".to_owned()].into();
    let ids = order(pending(), &tried);
    assert_eq!(ids, ["d-link", "a-due"]);
}

#[test]
fn a_row_that_waits_for_its_time_is_not_tried() {
    let only = vec![("c-wait".to_owned(), TombstoneDue::Waiting)];
    assert!(order(only, &HashSet::new()).is_empty());
}
