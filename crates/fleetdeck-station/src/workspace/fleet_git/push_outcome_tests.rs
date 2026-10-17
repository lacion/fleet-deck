use fleetdeck_proto::fleet_git::code;

use super::*;

const REF: &str = "fd/alpha/falcon";

fn line(flag: char, reference: &str, summary: &str) -> String {
    format!("To file:///x.git\n{flag}\trefs/heads/x:refs/heads/{reference}\t{summary}\nDone\n")
}

#[test]
fn the_porcelain_flag_of_the_exact_ref_classifies_the_push() {
    for (flag, summary) in [
        (' ', "abc..def"),
        ('+', "abc...def (forced update)"),
        ('*', "[new branch]"),
        ('=', "[up to date]"),
    ] {
        let text = line(flag, REF, summary);
        assert_eq!(
            push_outcome(0, &text, false, REF),
            PushOutcome::Applied,
            "{flag:?}"
        );
        // The same line with a failing exit code is not a proof.
        assert_eq!(
            push_outcome(1, &text, false, REF),
            PushOutcome::Unknown,
            "{flag:?}"
        );
    }
    for summary in [
        "[rejected] (stale info)",
        "[rejected] (fetch first)",
        "[remote rejected] (hook declined)",
    ] {
        let text = line('!', REF, summary);
        assert_eq!(
            push_outcome(1, &text, false, REF),
            PushOutcome::Rejected(code::PUSH_REJECTED)
        );
    }
}

#[test]
fn every_other_push_result_is_unknown() {
    let applied = line('*', REF, "[new branch]");
    // A timeout beats a line.
    assert_eq!(push_outcome(0, &applied, true, REF), PushOutcome::Unknown);
    // A transport or authentication error.
    assert_eq!(push_outcome(128, "", false, REF), PushOutcome::Unknown);
    // A signal.
    assert_eq!(push_outcome(-1, "", false, REF), PushOutcome::Unknown);
    // Exit 0 without the line of the ref, or a line for another ref.
    assert_eq!(push_outcome(0, "Done\n", false, REF), PushOutcome::Unknown);
    let other = line('*', "fd/alpha/other", "[new branch]");
    assert_eq!(push_outcome(0, &other, false, REF), PushOutcome::Unknown);
    // A delete flag is not a push of the commit.
    assert_eq!(
        push_outcome(0, &line('-', REF, "[deleted]"), false, REF),
        PushOutcome::Unknown
    );
}
