//! Unit tests of the wake pieces that need no store. The task itself is in
//! `tests/desktop_wake.rs`.

use super::scan::repo_ok;
use super::{Limits, last_component};

#[test]
fn the_default_limits_are_the_values_of_the_design() {
    let l = Limits::default();
    assert_eq!(
        (l.window_ms, l.skew_ms, l.cooldown_ms, l.workers, l.queue),
        (60_000, 5_000, 30_000, 4, 32)
    );
}

#[test]
fn the_repository_basename_is_the_last_component_of_any_separator() {
    assert_eq!(last_component("/home/me/work/repo"), "repo");
    assert_eq!(last_component("/home/me/work/repo/"), "repo");
    assert_eq!(last_component("C:\\work\\repo"), "repo");
    assert_eq!(last_component(""), "");
}

#[test]
fn a_repository_filter_needs_an_exact_basename() {
    assert!(repo_ok(None, None));
    assert!(repo_ok(None, Some("app")));
    assert!(repo_ok(Some("app"), Some("app")));
    assert!(!repo_ok(Some("app"), Some("App")));
    assert!(!repo_ok(Some("app"), Some("app2")));
    assert!(!repo_ok(Some("app"), None));
}
