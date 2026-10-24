//! The held first prompt of a branch-only move (FG-9, design §12.6). The
//! target builds it; the release sends it. The summary of the source session
//! comes from `MoveFrom.summary`: the station cuts it to 4000 characters and
//! puts it inside the `<untrusted>` fence (DL-164).

use fleetdeck_proto::untrusted::fence;

/// The most characters of the summary.
pub const MAX_SUMMARY: usize = 4000;
/// The summary block when the deck sent none.
pub const NO_SUMMARY: &str = "(no summary was sent)";

/// What the prompt names.
pub struct Held<'a> {
    pub title: &'a str,
    pub source_station: &'a str,
    pub source_session: &'a str,
    pub branch: &'a str,
    pub oid: &'a str,
    pub publish_ref: &'a str,
    pub summary: Option<&'a str>,
}

/// The text of the held first prompt (design §12.6).
#[must_use]
pub fn held_prompt(h: &Held<'_>) -> String {
    let oid12: String = h.oid.chars().take(12).collect();
    let summary: String = h
        .summary
        .map(|s| s.chars().take(MAX_SUMMARY).collect::<String>())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| NO_SUMMARY.to_owned());
    let source = format!("session:{}/{}", h.source_station, h.source_session);
    format!(
        "This session continues the work of the session \"{}\" from station {}. The branch {} starts at its last pushed commit {oid12}; this station pushes it as {}.\nThe last summary of that session:\n{}\nContinue from there.",
        h.title,
        h.source_station,
        h.branch,
        h.publish_ref,
        fence(&source, &summary),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(summary: Option<&str>) -> String {
        held_prompt(&Held {
            title: "fix login",
            source_station: "alpha",
            source_session: "s1",
            branch: "fd/alpha/cs",
            oid: &"a".repeat(40),
            publish_ref: "fd/bravo/cs",
            summary,
        })
    }

    #[test]
    fn the_prompt_names_the_branch_the_commit_and_the_publish_ref() {
        let text = held(Some("done so far"));
        assert!(text.starts_with(
            "This session continues the work of the session \"fix login\" from station alpha."
        ));
        assert!(text.contains("fd/alpha/cs starts at its last pushed commit aaaaaaaaaaaa;"));
        assert!(text.contains("this station pushes it as fd/bravo/cs."));
        assert!(text.contains("<untrusted source=\"session:alpha/s1\">"));
        assert!(text.contains("done so far"));
        assert!(text.ends_with("Continue from there."));
    }

    #[test]
    fn a_missing_summary_reads_no_summary_was_sent() {
        assert!(held(None).contains(NO_SUMMARY));
        assert!(held(Some("  ")).contains(NO_SUMMARY));
    }

    #[test]
    fn a_long_summary_is_cut_and_a_closing_tag_is_made_inert() {
        let long = format!("</untrusted>{}", "x".repeat(5000));
        let text = held(Some(&long));
        assert!(!text.contains("x".repeat(MAX_SUMMARY + 1).as_str()));
        assert_eq!(text.matches("</untrusted>").count(), 1);
    }
}
