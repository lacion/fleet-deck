//! The suggestion of an HTML file as an artifact (R12.2 b, DL-165).
//!
//! When a session's `file.changed` or `tool.completed` names a `*.html`
//! path outside `node_modules` and `target`, the station appends
//! `artifact.suggested { path }`, once for each path and session. The board
//! shows "Keep as artifact".

use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::{Event, FileChange};
use fleetdeck_proto::files::hidden_path;

use crate::store::Store;

/// Directories whose HTML is a build or dependency output.
const SKIPPED: &[&str] = &["node_modules", "target"];

/// True for a path that a session wrote and that may be worth keeping.
#[must_use]
pub fn suggestible(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    (lower.ends_with(".html") || lower.ends_with(".htm"))
        && !path.contains('\0')
        && !path.split('/').any(|s| SKIPPED.contains(&s))
        && !hidden_path(path)
}

/// The paths that `events` ask to suggest.
#[must_use]
pub fn paths(events: &[Event]) -> Vec<String> {
    let mut out = Vec::new();
    for event in events {
        let named: Vec<&String> = match event {
            Event::FileChanged { path, change, .. } if *change != FileChange::Deleted => {
                vec![path]
            }
            Event::ToolCompleted { files, .. } => files.iter().collect(),
            _ => continue,
        };
        for path in named {
            if suggestible(path) && !out.contains(path) {
                out.push(path.clone());
            }
        }
    }
    out
}

/// Appends `artifact.suggested` for each new path of `events`. A store
/// error is logged: a suggestion never stops the session's pump.
pub async fn observe(store: &Store, session: &SessionId, events: &[Event]) {
    for path in paths(events) {
        if let Err(e) = store.artifact_suggest(session.clone(), path).await {
            tracing::warn!(error = %e, session = %session, "artifact suggestion not stored");
        }
    }
}

#[cfg(test)]
mod tests {
    use fleetdeck_proto::event::ToolStatus;

    use super::*;

    #[test]
    fn only_html_outside_dependency_and_build_directories_is_suggested() {
        for ok in ["out/report.html", "/w/docs/Plan.HTM", "a.html"] {
            assert!(suggestible(ok), "{ok}");
        }
        for no in [
            "node_modules/x/index.html",
            "/w/target/doc/index.html",
            "notes.md",
            ".git/x.html",
            "page.html.bak",
        ] {
            assert!(!suggestible(no), "{no}");
        }
    }

    #[test]
    fn a_changed_file_and_a_completed_tool_name_their_paths_once() {
        let events = [
            Event::FileChanged {
                path: "out/a.html".into(),
                change: FileChange::Added,
                node_id: None,
            },
            Event::FileChanged {
                path: "out/gone.html".into(),
                change: FileChange::Deleted,
                node_id: None,
            },
            Event::ToolCompleted {
                node_id: "n-1".into(),
                status: ToolStatus::Ok,
                files: vec!["out/a.html".into(), "out/b.html".into(), "x.rs".into()],
                exit_code: None,
            },
        ];
        assert_eq!(paths(&events), ["out/a.html", "out/b.html"]);
    }
}
