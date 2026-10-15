//! The suggestion of an HTML file as an artifact (R12.2 b, DL-165).
//!
//! When a session's `file.changed` or `tool.completed` names a `*.html`
//! path outside `node_modules` and `target`, the station appends
//! `artifact.suggested { path }`, once for each path and session. The board
//! shows "Keep as artifact". A path over 1024 bytes is not suggested, and a
//! session has at most 200 suggestions (NV-02).

use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::{Event, FileChange};
use fleetdeck_proto::files::hidden_path;

use crate::store::Store;

/// Directories whose HTML is a build or dependency output.
const SKIPPED: &[&str] = &["node_modules", "target"];

/// The longest path that is suggested, in bytes (NV-02).
pub const MAX_SUGGESTION_PATH_BYTES: usize = 1024;

/// True for a path that a session wrote and that may be worth keeping.
#[must_use]
pub fn suggestible(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    path.len() <= MAX_SUGGESTION_PATH_BYTES
        && (lower.ends_with(".html") || lower.ends_with(".htm"))
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
    use fleetdeck_proto::EventBody;
    use fleetdeck_proto::event::{ProviderKind, ToolStatus};
    use fleetdeck_testkit::Det;

    use super::*;
    use crate::store::{NewSession, OfflineApprovalPolicy, ReadResult, StoreConfig};

    async fn store_with(sessions: &[&str]) -> Store {
        let det = Det::new();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        store.recover(Vec::new()).await.unwrap();
        for id in sessions {
            store
                .create_session(NewSession {
                    session_id: (*id).into(),
                    provider: ProviderKind::Claude,
                    provider_instance: "claude".into(),
                    cwd: "/w".into(),
                    worktree: None,
                    branch: None,
                    mode: None,
                    title: None,
                    offline_approval_policy: OfflineApprovalPolicy::Wait,
                })
                .await
                .unwrap();
        }
        store
    }

    async fn suggested(store: &Store) -> Vec<String> {
        match store.read_after(0, 10_000).await.unwrap() {
            ReadResult::Events { events, .. } => events
                .into_iter()
                .filter_map(|e| match e.event {
                    EventBody::Known(Event::ArtifactSuggested { path }) => Some(path),
                    _ => None,
                })
                .collect(),
            ReadResult::SnapshotRequired { .. } => panic!("snapshot"),
        }
    }

    #[tokio::test]
    async fn a_session_keeps_at_most_200_suggestions_and_another_session_is_not_affected() {
        let store = store_with(&["s1", "s2"]).await;
        for n in 0..200 {
            let made = store
                .artifact_suggest("s1".into(), format!("out/{n}.html"))
                .await
                .unwrap();
            assert!(made, "{n}");
        }
        let made = store
            .artifact_suggest("s1".into(), "out/200.html".to_owned())
            .await
            .unwrap();
        assert!(!made, "the 201st path of one session");
        let made = store
            .artifact_suggest("s2".into(), "out/first.html".to_owned())
            .await
            .unwrap();
        assert!(made, "another session");
        let events = suggested(&store).await;
        assert_eq!(events.len(), 201);
        assert!(!events.contains(&"out/200.html".to_owned()));
        assert_eq!(events.last().map(String::as_str), Some("out/first.html"));
    }

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
    fn a_path_over_the_byte_limit_is_not_suggested() {
        let name = ".html";
        let at_limit = format!(
            "{}{name}",
            "a".repeat(MAX_SUGGESTION_PATH_BYTES - name.len())
        );
        assert_eq!(at_limit.len(), 1024);
        assert!(suggestible(&at_limit));
        let over = format!(
            "{}{name}",
            "a".repeat(MAX_SUGGESTION_PATH_BYTES + 1 - name.len())
        );
        assert_eq!(over.len(), 1025);
        assert!(!suggestible(&over));
        let events = [Event::FileChanged {
            path: over,
            change: FileChange::Added,
            node_id: None,
        }];
        assert!(paths(&events).is_empty());
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
                provider_refs: serde_json::Map::new(),
            },
        ];
        assert_eq!(paths(&events), ["out/a.html", "out/b.html"]);
    }
}
