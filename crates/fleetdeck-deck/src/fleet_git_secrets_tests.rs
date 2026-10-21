//! `decide()` (Fleet Sync FS-3, revision 2 R5): the table of the design §4.

use super::*;

fn st(id: &str) -> StationId {
    StationId::from(id.to_owned())
}

fn present(digest: &str, size: u64, mtime_ms: i64) -> SecretFileState {
    SecretFileState {
        path: ".env".into(),
        state: SecretPresence::Present,
        code: None,
        digest: Some(digest.into()),
        size: Some(size),
        mtime_ms: Some(mtime_ms),
    }
}

fn missing() -> SecretFileState {
    SecretFileState {
        path: ".env".into(),
        state: SecretPresence::Missing,
        code: None,
        digest: None,
        size: None,
        mtime_ms: None,
    }
}

fn refused() -> SecretFileState {
    SecretFileState {
        path: ".env".into(),
        state: SecretPresence::Refused,
        code: Some("not_ignored".into()),
        digest: None,
        size: None,
        mtime_ms: None,
    }
}

fn row(id: &str, position: u32, seen: Option<&str>, state: SecretFileState) -> Scanned {
    (st(id), position, seen.map(str::to_owned), state)
}

fn winner(d: &Decision) -> (&str, &str, bool) {
    let w = d.winner.as_ref().expect("a winner");
    (w.digest.as_str(), w.source.as_str(), w.conflict)
}

fn targets(d: &Decision) -> Vec<(&str, Option<&str>)> {
    d.targets
        .iter()
        .map(|(s, e)| (s.as_str(), e.as_deref()))
        .collect()
}

#[test]
fn all_missing_gives_no_winner_and_no_target() {
    let d = decide(
        None,
        &[row("a", 0, None, missing()), row("b", 1, None, missing())],
    );
    assert_eq!(d.winner, None);
    assert!(d.targets.is_empty());
    assert_eq!(decide(None, &[]).winner, None);
}

#[test]
fn a_missing_station_is_a_target_that_expects_nothing() {
    let d = decide(
        None,
        &[
            row("a", 0, None, present("d1", 5, 10)),
            row("b", 1, None, missing()),
        ],
    );
    assert_eq!(winner(&d), ("d1", "a", false));
    assert_eq!(targets(&d), [("b", None)]);
}

#[test]
fn one_distinct_digest_wins_and_needs_no_write() {
    let d = decide(
        None,
        &[
            row("a", 0, Some("old"), present("d1", 5, 10)),
            row("b", 1, None, present("d1", 5, 20)),
        ],
    );
    assert_eq!(winner(&d), ("d1", "b", false));
    assert!(d.targets.is_empty());
}

#[test]
fn one_changed_copy_wins_against_a_newer_unchanged_copy() {
    // `a` changed (seen d0, now d1, older); `b` is unchanged and newer.
    let d = decide(
        None,
        &[
            row("a", 0, Some("d0"), present("d1", 5, 10)),
            row("b", 1, Some("d0"), present("d0", 5, 99)),
        ],
    );
    assert_eq!(winner(&d), ("d1", "a", false));
    assert_eq!(targets(&d), [("b", Some("d0"))]);
}

#[test]
fn two_changed_copies_give_the_newest_and_a_conflict() {
    let d = decide(
        None,
        &[
            row("a", 0, Some("d0"), present("d1", 5, 10)),
            row("b", 1, Some("d0"), present("d2", 5, 20)),
            row("c", 2, Some("d0"), present("d0", 5, 99)),
        ],
    );
    assert_eq!(winner(&d), ("d2", "b", true));
    assert_eq!(targets(&d), [("a", Some("d1")), ("c", Some("d0"))]);
}

#[test]
fn an_empty_file_never_wins_against_a_non_empty_one() {
    let d = decide(
        None,
        &[
            row("a", 0, None, present("empty", 0, 99)),
            row("b", 1, None, present("full", 5, 10)),
        ],
    );
    assert_eq!(winner(&d), ("full", "b", false));
    // The empty file is a target.
    assert_eq!(targets(&d), [("a", Some("empty"))]);
    // Only empty files: the empty digest can win.
    let d = decide(
        None,
        &[
            row("a", 0, None, present("empty", 0, 5)),
            row("b", 1, None, missing()),
        ],
    );
    assert_eq!(winner(&d), ("empty", "a", false));
}

#[test]
fn without_a_seen_digest_the_newest_wins_with_a_conflict() {
    let d = decide(
        None,
        &[
            row("a", 0, None, present("d1", 5, 10)),
            row("b", 1, None, present("d2", 5, 20)),
        ],
    );
    assert_eq!(winner(&d), ("d2", "b", true));
    assert_eq!(targets(&d), [("a", Some("d1"))]);
}

#[test]
fn a_tie_goes_to_the_lowest_position() {
    let d = decide(
        None,
        &[
            row("b", 1, None, present("d2", 5, 20)),
            row("a", 0, None, present("d1", 5, 20)),
        ],
    );
    assert_eq!(winner(&d), ("d1", "a", true));
    // The same tie for the source of one digest.
    let d = decide(
        None,
        &[
            row("b", 1, None, present("d1", 5, 20)),
            row("a", 0, None, present("d1", 5, 20)),
            row("c", 2, None, missing()),
        ],
    );
    assert_eq!(winner(&d), ("d1", "a", false));
}

#[test]
fn a_refused_station_is_not_a_target_and_never_reverts_a_newer_value() {
    // `c` was refused during the last run, so its seen digest is old. It
    // comes back with the old copy: it is not changed, it is a target.
    let d = decide(
        None,
        &[
            row("a", 0, Some("d2"), present("d2", 5, 50)),
            row("b", 1, Some("d2"), present("d2", 5, 60)),
            row("c", 2, Some("d1"), present("d1", 5, 5)),
        ],
    );
    assert_eq!(winner(&d), ("d2", "b", false));
    assert_eq!(targets(&d), [("c", Some("d1"))]);
    // A refused state is neither a candidate nor a target.
    let d = decide(
        None,
        &[
            row("a", 0, None, present("d1", 5, 10)),
            row("b", 1, None, refused()),
        ],
    );
    assert_eq!(winner(&d), ("d1", "a", false));
    assert!(d.targets.is_empty());
}

#[test]
fn a_new_home_with_another_copy_gets_the_settled_value() {
    // `a` and `b` are settled on d2; the new home `c` has another copy and no
    // seen digest, so it is not changed.
    let d = decide(
        None,
        &[
            row("a", 0, Some("d2"), present("d2", 5, 50)),
            row("b", 1, Some("d2"), present("d2", 5, 60)),
            row("c", 2, None, present("d9", 5, 999)),
        ],
    );
    assert_eq!(winner(&d), ("d2", "b", false));
    assert_eq!(targets(&d), [("c", Some("d9"))]);
}

#[test]
fn a_copy_that_the_relay_wrote_never_counts_as_changed() {
    // The relay wrote d2 to `b` (seen d2). A copy equal to its seen digest is
    // settled, never changed, so the older stale copy `c` does not win.
    let d = decide(
        None,
        &[
            row("a", 0, Some("d2"), present("d2", 5, 10)),
            row("b", 1, Some("d2"), present("d2", 5, 20)),
            row("c", 2, Some("d1"), present("d1", 5, 5)),
        ],
    );
    assert_eq!(winner(&d), ("d2", "b", false));
}

#[test]
fn an_edit_after_the_read_with_one_station_offline_gives_the_newest_edit() {
    // The review's example: `a` was the source of d1 and was edited after
    // (d3); `b` (seen d1) holds d1; `c` is offline and not in the list.
    let d = decide(
        None,
        &[
            row("a", 0, Some("d1"), present("d3", 5, 100)),
            row("b", 1, Some("d1"), present("d1", 5, 50)),
        ],
    );
    assert_eq!(winner(&d), ("d3", "a", false));
    assert_eq!(targets(&d), [("b", Some("d1"))]);
}

#[test]
fn the_targets_are_in_position_order() {
    let d = decide(
        None,
        &[
            row("z", 2, None, missing()),
            row("a", 0, None, present("d1", 5, 10)),
            row("m", 1, None, present("d0", 5, 1)),
        ],
    );
    assert_eq!(targets(&d), [("m", Some("d0")), ("z", None)]);
}

#[test]
fn a_present_state_without_a_digest_is_ignored() {
    let broken = SecretFileState {
        digest: None,
        ..present("x", 1, 1)
    };
    let d = decide(
        None,
        &[row("a", 0, None, broken), row("b", 1, None, missing())],
    );
    assert_eq!(d.winner, None);
}

fn losers(d: &Decision) -> Vec<(&str, &str)> {
    d.losers
        .iter()
        .map(|(s, digest)| (s.as_str(), digest.as_str()))
        .collect()
}

#[test]
fn the_changed_copies_that_lose_are_the_losers() {
    // Both homes saw d0; the first became d1 (time 10), the second d2 (time 20).
    let d = decide(
        Some("d0"),
        &[
            row("a", 0, Some("d0"), present("d1", 5, 10)),
            row("b", 1, Some("d0"), present("d2", 5, 20)),
        ],
    );
    assert_eq!(winner(&d), ("d2", "b", true));
    assert_eq!(targets(&d), [("a", Some("d1"))]);
    assert_eq!(losers(&d), [("a", "d1")]);
    // A copy that did not change is a target, never a loser.
    let d = decide(
        None,
        &[
            row("a", 0, Some("d0"), present("d1", 5, 10)),
            row("b", 1, Some("d0"), present("d0", 5, 99)),
        ],
    );
    assert_eq!(winner(&d), ("d1", "a", false));
    assert!(d.losers.is_empty());
    // No `changed` rule, no loser.
    let d = decide(
        None,
        &[
            row("a", 0, Some("d2"), present("d2", 5, 50)),
            row("b", 1, Some("d1"), present("d1", 5, 60)),
        ],
    );
    assert!(d.losers.is_empty());
}

#[test]
fn a_failed_conflict_write_gives_the_same_winner_on_the_retry() {
    // The finding's case: both homes saw d0, then became d1 (time 10) and d2
    // (time 20). d2 wins; the write to the first home fails.
    let first = decide(
        Some("d0"),
        &[
            row("a", 0, Some("d0"), present("d1", 5, 10)),
            row("b", 1, Some("d0"), present("d2", 5, 20)),
        ],
    );
    assert_eq!(winner(&first), ("d2", "b", true));
    // After the run: the winner is accepted, the winner home has seen d2 and
    // the loser home that was not written has seen its own copy d1.
    let retry = decide(
        Some("d2"),
        &[
            row("a", 0, Some("d1"), present("d1", 5, 10)),
            row("b", 1, Some("d2"), present("d2", 5, 20)),
        ],
    );
    assert_eq!(winner(&retry), ("d2", "b", false));
    assert_eq!(targets(&retry), [("a", Some("d1"))]);
    // Before the fix the first home kept the seen digest d0: its copy d1 was
    // the only changed one and it won over the newer d2.
    let broken = decide(
        None,
        &[
            row("a", 0, Some("d0"), present("d1", 5, 10)),
            row("b", 1, Some("d2"), present("d2", 5, 20)),
        ],
    );
    assert_eq!(winner(&broken), ("d1", "a", false));
}

#[test]
fn the_accepted_digest_beats_a_newer_settled_copy() {
    // `a` changed to d1 (time 10) and won; the write to `b` failed. `b` is
    // settled on the old d0 with a newer time: the accepted d1 still wins.
    let states = [
        row("a", 0, Some("d1"), present("d1", 5, 10)),
        row("b", 1, Some("d0"), present("d0", 5, 99)),
    ];
    let d = decide(Some("d1"), &states);
    assert_eq!(winner(&d), ("d1", "a", false));
    assert_eq!(targets(&d), [("b", Some("d0"))]);
    // Without an accepted digest the settled rule picks the newest copy.
    assert_eq!(winner(&decide(None, &states)), ("d0", "b", false));
    // An accepted digest that no copy has changes nothing.
    assert_eq!(winner(&decide(Some("gone"), &states)), ("d0", "b", false));
}

#[test]
fn the_newest_copy_with_the_accepted_digest_is_the_source() {
    let d = decide(
        Some("d1"),
        &[
            row("a", 0, Some("d1"), present("d1", 5, 10)),
            row("b", 1, Some("d1"), present("d1", 5, 30)),
            row("c", 2, Some("d0"), present("d0", 5, 99)),
        ],
    );
    assert_eq!(winner(&d), ("d1", "b", false));
}
