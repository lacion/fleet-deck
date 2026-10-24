//! The follow-up text of a review (DL-179, design 3.1). The text goes to the
//! agent and into the session timeline, which members and guests may read:
//! an excerpt of a file that the member deny list hides is never put in it
//! (design 2.3), and no excerpt at all when the deny list cannot be
//! established (fail closed).

use fleetdeck_proto::member_deny::MemberDeny;
use fleetdeck_proto::review::{MAX_REVIEW_TEXT_BYTES, ReviewSide};

/// The lines of an excerpt that the follow-up shows.
pub const EXCERPT_LINES: usize = 6;

/// How a comment enters the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextComment {
    pub short_id: String,
    /// The short id of the top comment, for a reply.
    pub reply_to: Option<String>,
    pub path: String,
    pub other_path: Option<String>,
    pub side: ReviewSide,
    pub line_start: u32,
    pub line_end: u32,
    pub anchor_text: String,
    pub imported: bool,
    pub body: String,
}

/// The review text is over [`MAX_REVIEW_TEXT_BYTES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLarge;

/// What the writer is: the header says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sender<'a> {
    Owner,
    Member(&'a str),
}

/// The language word of a fence: the extension of the file when it is short
/// and alphanumeric, else `text`.
fn lang(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    match file.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && !ext.is_empty()
                && ext.len() <= 10
                && ext.bytes().all(|b| b.is_ascii_alphanumeric()) =>
        {
            ext.to_ascii_lowercase()
        }
        _ => "text".to_owned(),
    }
}

/// A run of backticks one longer than the longest run in `text` (at least
/// three).
fn fence(text: &str) -> String {
    let (mut longest, mut run) = (0usize, 0usize);
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

/// The first [`EXCERPT_LINES`] lines of `anchor`, then `… (<n> more lines)`.
fn excerpt(anchor: &str) -> String {
    let lines: Vec<&str> = anchor.split('\n').collect();
    let mut shown: Vec<String> = lines
        .iter()
        .take(EXCERPT_LINES)
        .map(|l| (*l).to_owned())
        .collect();
    if lines.len() > EXCERPT_LINES {
        shown.push(format!("… ({} more lines)", lines.len() - EXCERPT_LINES));
    }
    shown.join("\n")
}

/// How the excerpt of one comment is shown.
fn excerpt_block(c: &TextComment, deny: Option<&MemberDeny>) -> String {
    if c.imported {
        return "(no excerpt: imported note)".to_owned();
    }
    let Some(deny) = deny else {
        return "(excerpt hidden)".to_owned();
    };
    let protected = deny.denies(&c.path) || c.other_path.as_deref().is_some_and(|p| deny.denies(p));
    if protected {
        return "(excerpt hidden: protected file)".to_owned();
    }
    let body = excerpt(&c.anchor_text);
    let f = fence(&body);
    format!("{f}{}\n{body}\n{f}", lang(&c.path))
}

/// The follow-up text of the review `short_id` of `sender` with `comments`,
/// in this order. `deny` is the member deny list of the station; `None` when
/// it cannot be established.
///
/// # Errors
/// [`TooLarge`] when the text has more than [`MAX_REVIEW_TEXT_BYTES`] bytes.
pub fn build(
    short_id: &str,
    sender: &Sender<'_>,
    comments: &[TextComment],
    deny: Option<&MemberDeny>,
) -> Result<String, TooLarge> {
    let from = match sender {
        Sender::Owner => "the owner".to_owned(),
        Sender::Member(name) => format!("member {name}"),
    };
    let noun = if comments.len() == 1 {
        "comment"
    } else {
        "comments"
    };
    let mut out = format!(
        "Review {short_id} from {from} ({} {noun}). Reply to each comment by its id.",
        comments.len()
    );
    for c in comments {
        out.push_str("\n\n");
        match &c.reply_to {
            Some(top) => out.push_str(&format!("[{}] reply to [{top}]\n{}", c.short_id, c.body)),
            None => {
                let lines = if c.line_end == c.line_start {
                    c.line_start.to_string()
                } else {
                    format!("{}-{}", c.line_start, c.line_end)
                };
                out.push_str(&format!(
                    "[{}] {} line {lines} ({} side)\n{}\n{}",
                    c.short_id,
                    c.path,
                    c.side.as_str(),
                    excerpt_block(c, deny),
                    c.body
                ));
            }
        }
        if out.len() > MAX_REVIEW_TEXT_BYTES {
            return Err(TooLarge);
        }
    }
    if out.len() > MAX_REVIEW_TEXT_BYTES {
        return Err(TooLarge);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(id: &str, path: &str, anchor: &str) -> TextComment {
        TextComment {
            short_id: id.to_owned(),
            reply_to: None,
            path: path.to_owned(),
            other_path: None,
            side: ReviewSide::New,
            line_start: 3,
            line_end: 3,
            anchor_text: anchor.to_owned(),
            imported: false,
            body: "fix this".to_owned(),
        }
    }

    #[test]
    fn the_header_names_the_sender_and_a_comment_has_its_excerpt() {
        let deny = MemberDeny::default();
        let text = build(
            "r-abc234",
            &Sender::Owner,
            &[comment("c-aaaaaa", "src/lib.rs", "let x = 1;")],
            Some(&deny),
        )
        .unwrap();
        assert_eq!(
            text,
            "Review r-abc234 from the owner (1 comment). Reply to each comment by its id.\n\n\
             [c-aaaaaa] src/lib.rs line 3 (new side)\n```rs\nlet x = 1;\n```\nfix this"
        );
        let member = build("r-abc234", &Sender::Member("Ann"), &[], Some(&deny)).unwrap();
        assert!(member.starts_with("Review r-abc234 from member Ann (0 comments)."));
        assert!(!member.contains("owner"));
    }

    #[test]
    fn the_fence_is_longer_than_any_backtick_run_in_the_excerpt() {
        let c = comment("c-aaaaaa", "a.md", "```\nx\n````");
        let text = build(
            "r-aaaaaa",
            &Sender::Owner,
            &[c],
            Some(&MemberDeny::default()),
        )
        .unwrap();
        assert!(text.contains("`````md\n```\nx\n````\n`````\n"), "{text}");
        assert_eq!(fence("a``b"), "```");
        assert_eq!(fence("````"), "`````");
    }

    #[test]
    fn a_long_excerpt_shows_six_lines_then_a_count() {
        let anchor = (1..=9)
            .map(|n| format!("l{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(excerpt(&anchor), "l1\nl2\nl3\nl4\nl5\nl6\n… (3 more lines)");
        assert_eq!(excerpt("a\nb"), "a\nb");
    }

    #[test]
    fn a_protected_path_an_unknown_deny_list_and_an_import_hide_the_excerpt() {
        let deny = MemberDeny::default();
        let secret = comment("c-aaaaaa", "app/.env", "TOKEN=1");
        let mut renamed = comment("c-bbbbbb", "ok.txt", "TOKEN=1");
        renamed.other_path = Some("deploy/id_rsa".to_owned());
        let mut imported = comment("c-cccccc", "a.rs", "");
        imported.imported = true;
        let text = build(
            "r-aaaaaa",
            &Sender::Owner,
            &[secret.clone(), renamed, imported],
            Some(&deny),
        )
        .unwrap();
        assert!(!text.contains("TOKEN"), "{text}");
        assert_eq!(text.matches("(excerpt hidden: protected file)").count(), 2);
        assert!(text.contains("(no excerpt: imported note)"));
        // The path and the lines stay.
        assert!(text.contains("[c-aaaaaa] app/.env line 3 (new side)"));
        let unknown = build("r-aaaaaa", &Sender::Owner, &[secret], None).unwrap();
        assert!(!unknown.contains("TOKEN"), "{unknown}");
        assert!(unknown.contains("(excerpt hidden)"));
    }

    #[test]
    fn a_reply_names_its_top_comment_and_a_range_shows_both_lines() {
        let mut reply = comment("c-bbbbbb", "a.rs", "x");
        reply.reply_to = Some("c-aaaaaa".to_owned());
        let mut range = comment("c-aaaaaa", "a.rs", "x\ny");
        range.line_end = 4;
        range.side = ReviewSide::Old;
        let text = build(
            "r-aaaaaa",
            &Sender::Owner,
            &[range, reply],
            Some(&MemberDeny::default()),
        )
        .unwrap();
        assert!(
            text.contains("[c-aaaaaa] a.rs line 3-4 (old side)"),
            "{text}"
        );
        assert!(
            text.ends_with("[c-bbbbbb] reply to [c-aaaaaa]\nfix this"),
            "{text}"
        );
    }

    #[test]
    fn a_text_over_the_limit_is_refused() {
        let mut c = comment("c-aaaaaa", "a.rs", "x");
        c.body = "b".repeat(8000);
        let many = vec![c; 9];
        assert_eq!(
            build(
                "r-aaaaaa",
                &Sender::Owner,
                &many,
                Some(&MemberDeny::default())
            ),
            Err(TooLarge)
        );
    }
}
