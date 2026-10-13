//! G-C (GATE-M1 G14): the proto types agree with the link contract of
//! ADR-004 §9 "Revision 2 (M1 gate)", and PLAN §8 points to it. Phase 3
//! (P3-2) adds the full table-agreement test against the method table of
//! `fleetdeck-link`; this test keeps the document and the proto in step
//! until then.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::path::PathBuf;

use fleetdeck_proto::command::SessionCommand;
use fleetdeck_proto::link::ControlFrame;
use fleetdeck_proto::mutation;
use fleetdeck_proto::version::feature;
use serde_json::{Value, json};

fn doc(path: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::read_to_string(root.join(path)).unwrap()
}

fn adr() -> String {
    doc("docs/v2/adr/ADR-004-link-protocol-v1.md")
}

/// The text of one Markdown section, up to the next heading of the same
/// or a higher level.
fn section<'a>(text: &'a str, heading: &str) -> &'a str {
    let level = heading.chars().take_while(|c| *c == '#').count();
    let start = text
        .find(heading)
        .unwrap_or_else(|| panic!("no section `{heading}`"));
    let rest = &text[start + heading.len()..];
    let mut end = rest.len();
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if offset > 0 && (1..=level).contains(&hashes) && line[hashes..].starts_with(' ') {
            end = offset;
            break;
        }
        offset += line.len();
    }
    &rest[..end]
}

/// One row of the §9.6 method table.
#[derive(Debug)]
struct Row {
    methods: Vec<String>,
    direction: String,
    scope: String,
    feature: String,
    mutating: bool,
}

fn method_table() -> Vec<Row> {
    let adr = adr();
    let text = section(&adr, "### 9.6 Roles, directions and method table (G6, G7)");
    text.lines()
        .filter(|l| l.starts_with("| `"))
        .map(|l| {
            let cells: Vec<&str> = l.trim_matches('|').split('|').map(str::trim).collect();
            assert_eq!(cells.len(), 5, "{l}");
            Row {
                methods: cells[0]
                    .split(',')
                    .map(|m| m.trim().trim_matches('`').to_owned())
                    .collect(),
                direction: cells[1].to_owned(),
                scope: cells[2].to_owned(),
                feature: cells[3].trim_matches('`').to_owned(),
                mutating: cells[4].starts_with("yes"),
            }
        })
        .collect()
}

fn row<'a>(table: &'a [Row], method: &str) -> &'a Row {
    table
        .iter()
        .find(|r| r.methods.iter().any(|m| m == method))
        .unwrap_or_else(|| panic!("`{method}` is not in ADR-004 §9.6"))
}

/// One sample of every `SessionCommand` variant. The match below has no
/// wildcard, so a new variant fails to compile until it has a sample.
fn commands() -> Vec<SessionCommand> {
    let s = "01J0000000000000000000000S";
    let samples = [
        json!({"op": "start", "spec": {"provider_instance": "claude", "cwd": "/w",
            "mode": null, "model": null, "title": null}, "prompt": null}),
        json!({"op": "resume", "session_id": s}),
        json!({"op": "prompt", "session_id": s, "input": {"text": "hi"}}),
        json!({"op": "steer", "session_id": s, "input": {"text": "hi"}}),
        json!({"op": "follow_up", "session_id": s, "input": {"text": "hi"}}),
        json!({"op": "interrupt", "session_id": s}),
        json!({"op": "answer", "session_id": s, "request_id": "req-1",
            "decision": {"option_id": "allow", "updated_input": null, "text": null}}),
        json!({"op": "set_mode", "session_id": s, "mode": "plan"}),
        json!({"op": "set_model", "session_id": s, "model": "haiku"}),
        json!({"op": "fork", "session_id": s}),
        json!({"op": "stop", "session_id": s, "how": "graceful"}),
        json!({"op": "revert", "session_id": s, "turn_id": "turn-1"}),
        json!({"op": "rename", "session_id": s, "title": "docs"}),
        json!({"op": "resolve_revert", "session_id": s, "revert_id": "rv-1", "resolution": "retry_restore"}),
        json!({"op": "coordinator_renew", "session_id": s, "grant_id": "g-1", "generation": 2}),
        json!({"op": "coordinator_revoke", "session_id": s, "grant_id": "g-1"}),
        json!({"op": "worktree_trash", "session_id": s}),
        json!({"op": "worktree_restore", "session_id": s}),
        json!({"op": "worktree_purge", "session_id": s, "confirm_name": "app--fd-x--01j000000000"}),
        json!({"op": "worktree_release", "session_id": s}),
    ];
    let commands: Vec<SessionCommand> = samples
        .into_iter()
        .map(|v| serde_json::from_value(v).unwrap())
        .collect();
    for c in &commands {
        match c {
            SessionCommand::Start { .. }
            | SessionCommand::Resume { .. }
            | SessionCommand::Import { .. }
            | SessionCommand::Prompt { .. }
            | SessionCommand::Steer { .. }
            | SessionCommand::FollowUp { .. }
            | SessionCommand::Interrupt { .. }
            | SessionCommand::Answer { .. }
            | SessionCommand::SetMode { .. }
            | SessionCommand::SetModel { .. }
            | SessionCommand::Fork { .. }
            | SessionCommand::Stop { .. }
            | SessionCommand::Revert { .. }
            | SessionCommand::Rename { .. }
            | SessionCommand::ResolveRevert { .. }
            | SessionCommand::SetAutoContinue { .. }
            | SessionCommand::CoordinatorRenew { .. }
            | SessionCommand::CoordinatorRevoke { .. }
            | SessionCommand::WorktreeTrash { .. }
            | SessionCommand::WorktreeRestore { .. }
            | SessionCommand::WorktreePurge { .. }
            | SessionCommand::WorktreeRelease { .. } => {}
        }
    }
    commands
}

#[test]
fn every_session_command_is_a_mutating_deck_to_station_method_of_the_adr_table() {
    let table = method_table();
    let commands = commands();
    assert_eq!(commands.len(), 20);
    for c in &commands {
        let r = row(&table, c.method());
        assert!(r.mutating, "`{}` must be mutating", c.method());
        assert_eq!(r.direction, "deck → station", "{}", c.method());
        let scope = if c.method() == "approval.answer" {
            "approve"
        } else if c.method().starts_with("session.worktree_") {
            "admin"
        } else {
            "drive"
        };
        assert_eq!(r.scope, scope, "{}", c.method());
    }
}

#[test]
fn the_adr_table_names_known_features_and_scopes_and_makes_coordinator_calls_mutating() {
    let table = method_table();
    assert!(table.len() >= 12, "{table:?}");
    for r in &table {
        assert!(
            r.feature == "—" || feature::ALL.contains(&r.feature.as_str()),
            "unknown feature `{}`",
            r.feature
        );
        assert!(
            ["read", "drive", "approve", "admin", "coordinate"].contains(&r.scope.as_str()),
            "unknown scope `{}`",
            r.scope
        );
        assert!(
            ["deck → station", "station → deck", "either → either"].contains(&r.direction.as_str()),
            "bad direction `{}`",
            r.direction
        );
    }
    // G7: intent-changing coordinator operations are mutating, reads are not.
    let call = row(&table, "coordinator.call");
    assert!(call.mutating);
    assert_eq!(
        (call.direction.as_str(), call.scope.as_str()),
        ("station → deck", "coordinate")
    );
    assert!(!row(&table, "coordinator.query").mutating);
    // G6: snapshots need their feature; reads never mutate.
    assert_eq!(row(&table, "session.snapshot").feature, feature::SNAPSHOTS);
    assert_eq!(row(&table, "artifact.open").feature, feature::DATA_STREAMS);
    for r in table.iter().filter(|r| r.scope == "read") {
        assert!(!r.mutating, "{:?}", r.methods);
    }
}

/// One row of the §9.3 envelope table.
#[derive(Debug)]
struct EnvelopeRow {
    methods: Vec<String>,
    params: String,
    payload: String,
    holder: String,
}

fn envelope_table() -> Vec<EnvelopeRow> {
    let adr = adr();
    let text = section(
        &adr,
        "### 9.3 Command envelopes, incarnation and deadline (G3)",
    );
    text.lines()
        .filter(|l| l.starts_with("| `"))
        .map(|l| {
            let cells: Vec<&str> = l.trim_matches('|').split('|').map(str::trim).collect();
            assert_eq!(cells.len(), 4, "{l}");
            EnvelopeRow {
                methods: cells[0]
                    .split(',')
                    .map(|m| m.trim().trim_matches('`').to_owned())
                    .collect(),
                params: cells[1].trim_matches('`').to_owned(),
                payload: cells[2].trim_matches('`').to_owned(),
                holder: cells[3].to_owned(),
            }
        })
        .collect()
}

#[test]
fn the_adr_envelope_table_equals_the_code_table() {
    let doc = envelope_table();
    assert_eq!(doc.len(), mutation::ENVELOPES.len(), "{doc:?}");
    for (row, code) in doc.iter().zip(mutation::ENVELOPES) {
        let methods: Vec<&str> = row.methods.iter().map(String::as_str).collect();
        assert_eq!(methods, code.methods, "{row:?}");
        assert_eq!(row.params, code.params, "{row:?}");
        assert_eq!(row.payload, code.payload, "{row:?}");
        assert_eq!(row.holder, code.holder.name(), "{row:?}");
    }
}

#[test]
fn every_mutating_method_of_the_method_table_has_exactly_one_envelope() {
    let table = method_table();
    let mut mutating = Vec::new();
    for r in &table {
        for m in &r.methods {
            assert_eq!(
                mutation::envelope_of(m).is_some(),
                r.mutating,
                "`{m}`: an envelope exists only for a mutating method"
            );
            if r.mutating {
                mutating.push(m.clone());
            }
        }
    }
    let mut enveloped: Vec<String> = mutation::ENVELOPES
        .iter()
        .flat_map(|e| e.methods.iter().map(|m| (*m).to_owned()))
        .collect();
    mutating.sort();
    enveloped.sort();
    assert_eq!(mutating, enveloped);
    // The holder agrees with the receiver role of the method table.
    for kind in mutation::ENVELOPES {
        for m in kind.methods {
            let receiver = row(&table, m)
                .direction
                .rsplit(" → ")
                .next()
                .map(str::to_owned);
            let expected = match kind.holder {
                mutation::Holder::Station => "station",
                mutation::Holder::Deck => "deck",
                mutation::Holder::Receiver => "either",
            };
            assert_eq!(receiver.as_deref(), Some(expected), "{m}");
        }
    }
}

#[test]
fn every_control_frame_type_is_in_the_adr_frame_list_with_its_incarnation() {
    let adr = adr();
    let frames = section(
        &adr,
        "### 9.4 Incarnation on every stream frame and snapshot batches (G4)",
    );
    let inc = "01J0000000000000000000000I";
    let samples: Vec<Value> = vec![
        json!({"type": "request", "id": 1, "command_id": null, "method": "station.info", "params": {}}),
        json!({"type": "response", "id": 1, "ok": {}}),
        json!({"type": "ack", "incarnation": inc, "station_seq": 3}),
        json!({"type": "snapshot_begin", "incarnation": inc, "watermark": 3, "session_count": 0}),
        json!({"type": "snapshot_end", "incarnation": inc, "watermark": 3}),
        json!({"type": "ping", "nonce": 1}),
        json!({"type": "pong", "nonce": 1}),
        json!({"type": "close", "reason": "superseded", "detail": ""}),
    ];
    let mut names = Vec::new();
    for v in samples {
        let frame: ControlFrame = serde_json::from_value(v).unwrap();
        names.push(frame.type_name());
    }
    names.extend(["event", "snapshot_session"]);
    for name in names {
        let line = frames
            .lines()
            .find(|l| l.split_whitespace().next() == Some(name))
            .unwrap_or_else(|| panic!("`{name}` is not in ADR-004 §9.4"));
        if [
            "event",
            "ack",
            "snapshot_begin",
            "snapshot_session",
            "snapshot_end",
        ]
        .contains(&name)
        {
            assert!(line.contains("incarnation"), "{line}");
        }
    }
}

#[test]
fn plan_section_8_points_to_revision_2_and_the_decisions_log_has_its_entry() {
    let plan = doc("docs/v2/PLAN.md");
    let link = section(&plan, "## 8. Link protocol");
    assert!(
        link.contains("Revision 2 (M1 gate"),
        "PLAN §8 has no Revision 2 note"
    );
    assert!(link.contains("ADR-004 §9"));
    for g in ["G1", "G2", "G3", "G4", "G5", "G6", "G7"] {
        assert!(
            link.contains(&format!("({g}")) || link.contains(&format!(", {g}")),
            "PLAN §8 does not name {g}"
        );
    }
    let adr = adr();
    for g in ["(G1)", "(G2, G6)", "(G3)", "(G4)", "(G5)", "(G6, G7)"] {
        assert!(adr.contains(g), "ADR-004 §9 has no section for {g}");
    }
    let log = doc("docs/v2/DECISIONS-LOG.md");
    let entry = section(&log, "## DL-040");
    assert!(entry.contains("GATE-M1"), "DL-040 must point to GATE-M1");
}
