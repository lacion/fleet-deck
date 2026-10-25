//! The model input that the station builds from a session's own events
//! (R10.1, R10.2, DL-167).

use fleetdeck_proto::event::EventBody;
use fleetdeck_proto::{Event, EventEnvelope};

/// The timeline lines of a digest input.
pub const DIGEST_LINES: usize = 20;
/// The longest line, in characters.
pub const LINE_CHARS: usize = 300;
/// The longest first prompt of a title input, in characters.
pub const PROMPT_CHARS: usize = 1000;

/// `text` on one line, cut at `max` characters.
#[must_use]
pub fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The line of one event, if it has one.
fn line(event: &Event) -> Option<String> {
    Some(match event {
        Event::RunStarted { input_summary, .. } => {
            format!("Prompt: {}", one_line(input_summary, LINE_CHARS))
        }
        Event::MessageCompleted {
            text,
            parent_node_id: None,
            ..
        } => format!("Agent: {}", one_line(text, LINE_CHARS)),
        Event::ToolStarted {
            title,
            input_summary,
            ..
        } => format!(
            "Tool: {}",
            one_line(&format!("{title} {input_summary}"), LINE_CHARS)
        ),
        Event::FileChanged { path, change, .. } => format!(
            "File {}: {}",
            format!("{change:?}").to_lowercase(),
            one_line(path, LINE_CHARS)
        ),
        Event::Error { code, message, .. } => {
            format!("Error {code}: {}", one_line(message, LINE_CHARS))
        }
        _ => return None,
    })
}

/// The last [`DIGEST_LINES`] lines of `events`, oldest first, one per line.
#[must_use]
pub fn digest_input(events: &[EventEnvelope]) -> String {
    let lines: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.event {
            EventBody::Known(event) => line(event),
            EventBody::Unknown { .. } => None,
        })
        .collect();
    let from = lines.len().saturating_sub(DIGEST_LINES);
    lines[from..].join("\n")
}

#[cfg(test)]
mod tests {
    use fleetdeck_proto::SessionId;
    use fleetdeck_proto::event::{FileChange, RunOrigin, ToolKind};

    use super::*;

    fn envelope(seq: u64, event: Event) -> EventEnvelope {
        EventEnvelope {
            station_seq: seq,
            session_id: SessionId::from("s"),
            session_seq: seq,
            at: 0,
            proto_version: 1,
            raw_ref: None,
            event: EventBody::Known(event),
        }
    }

    #[test]
    fn one_line_flattens_and_cuts() {
        assert_eq!(one_line("a  b\n\nc", 10), "a b c");
        let cut = one_line(&"x".repeat(50), 10);
        assert_eq!(cut.chars().count(), 10);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn the_digest_input_keeps_the_last_twenty_lines_in_order() {
        let mut events = vec![envelope(
            1,
            Event::RunStarted {
                run_id: "r".into(),
                input_summary: "fix the bug".into(),
                origin: RunOrigin::Board,
                author: None,
                input_id: None,
                input_text: None,
            },
        )];
        for n in 0..25 {
            events.push(envelope(
                n + 2,
                Event::ToolStarted {
                    node_id: format!("n{n}").into(),
                    parent_node_id: None,
                    tool_kind: ToolKind::Shell,
                    title: "Bash".into(),
                    input_summary: format!("step {n}"),
                    files: Vec::new(),
                    provider_refs: Default::default(),
                },
            ));
        }
        events.push(envelope(
            40,
            Event::FileChanged {
                path: "src/lib.rs".into(),
                change: FileChange::Modified,
                node_id: None,
            },
        ));
        let text = digest_input(&events);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), DIGEST_LINES);
        assert_eq!(lines[0], "Tool: Bash step 6");
        assert_eq!(lines[DIGEST_LINES - 1], "File modified: src/lib.rs");
        assert!(!text.contains("fix the bug"), "the oldest line was cut");
    }

    #[test]
    fn a_subagent_message_makes_no_line() {
        let message = |parent: Option<&str>| {
            envelope(
                1,
                Event::MessageCompleted {
                    turn_id: "t".into(),
                    parent_node_id: parent.map(Into::into),
                    text: "done".into(),
                },
            )
        };
        assert_eq!(digest_input(&[message(None)]), "Agent: done");
        assert_eq!(digest_input(&[message(Some("n"))]), "");
    }
}
