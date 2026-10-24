//! Table agreement (GATE-M1 G7): the method table in `fleetdeck-link` must
//! equal "Method table v1 (Revision 2)" in ADR-004 §9.6.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::Path;

use fleetdeck_link::methods::{Callers, METHODS};

#[derive(Debug, PartialEq, Eq)]
struct Row {
    callers: String,
    scope: String,
    feature: Option<String>,
    mutating: bool,
}

fn adr() -> String {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/v2/adr/ADR-004-link-protocol-v1.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn ticked(cell: &str) -> Vec<String> {
    cell.split('`')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s.to_owned())
        .collect()
}

/// Method name → row, parsed from the ADR table.
fn adr_table(text: &str) -> BTreeMap<String, Row> {
    let start = text
        .find("**Method table v1 (Revision 2).**")
        .expect("the ADR names the method table");
    let mut rows = BTreeMap::new();
    let mut seen_table = false;
    for line in text[start..].lines().skip(1) {
        let line = line.trim();
        if !line.starts_with('|') {
            if seen_table {
                break;
            }
            continue;
        }
        seen_table = true;
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells[0] == "Method" || cells[0].starts_with("---") {
            continue;
        }
        assert_eq!(cells.len(), 5, "{line}");
        let feature = ticked(cells[3]).into_iter().next();
        assert!(feature.is_some() || cells[3] == "—", "{line}");
        let mutating = match cells[4].split_whitespace().next() {
            Some("yes") => true,
            Some("no") => false,
            _ => panic!("mutating cell: {line}"),
        };
        for name in ticked(cells[0]) {
            let row = Row {
                callers: cells[1].to_owned(),
                scope: cells[2].to_owned(),
                feature: feature.clone(),
                mutating,
            };
            assert!(rows.insert(name.clone(), row).is_none(), "{name} twice");
        }
    }
    rows
}

fn code_table() -> BTreeMap<String, Row> {
    METHODS
        .iter()
        .map(|s| {
            let callers = match s.callers {
                Callers::DeckToStation => "deck → station",
                Callers::StationToDeck => "station → deck",
                Callers::Either => "either → either",
            };
            let row = Row {
                callers: callers.to_owned(),
                scope: s.scope.name().to_owned(),
                feature: s.feature.map(str::to_owned),
                mutating: s.mutating,
            };
            (s.name.to_owned(), row)
        })
        .collect()
}

#[test]
fn the_code_method_table_equals_the_adr_method_table() {
    let adr = adr_table(&adr());
    let code = code_table();
    // 135 + 2 (BG4-1 execution stop) + 3 (BG2-2 installer repair)
    // + 6 (BG1-1 desktop tools and maintenance) + 1 (MCP P3 `fleet.view`)
    // + 1 (MCP P4b `session.coordinator_set`) + 3 (Fleet Sync FS-3 secret files)
    // + 1 (Fleet Sync `project.secret_browse`) + 2 (B12 workflows)
    // + 1 (B20 `session.usage_recount`) + 1 (B18 `session.set_effort`).
    assert_eq!(adr.len(), 156, "{:?}", adr.keys());
    for (name, row) in &adr {
        assert_eq!(code.get(name), Some(row), "{name}");
    }
    assert_eq!(adr, code);
}

#[test]
fn the_omarchy_admin_methods_need_their_feature_and_scope() {
    let code = code_table();
    for name in ["omarchy.status", "omarchy.act"] {
        let row = code.get(name).unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(row.callers, "deck → station", "{name}");
        assert_eq!(row.scope, "admin", "{name}");
        assert_eq!(row.feature.as_deref(), Some("omarchy_admin.v1"), "{name}");
        assert!(!row.mutating, "{name}");
    }
}

#[test]
fn the_omarchy_repair_reads_need_their_feature_and_scope() {
    let code = code_table();
    for name in [
        "omarchy.install_plan",
        "omarchy.repair_status",
        "omarchy.restore_plan",
    ] {
        let row = code.get(name).unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(row.callers, "deck → station", "{name}");
        assert_eq!(row.scope, "admin", "{name}");
        assert_eq!(row.feature.as_deref(), Some("omarchy_repair.v1"), "{name}");
        assert!(!row.mutating, "{name}");
    }
}

#[test]
fn the_desktop_tool_rows_need_their_features_and_scope() {
    let code = code_table();
    for (name, feature) in [
        ("station.tools", "station_tools.v1"),
        ("station.desktop_tools_plan", "desktop_tool_install.v1"),
        ("maintenance.open", "desktop_tool_install.v1"),
        ("maintenance.list", "desktop_tool_install.v1"),
        ("maintenance.attach", "desktop_tool_install.v1"),
        ("maintenance.resize", "desktop_tool_install.v1"),
    ] {
        let row = code.get(name).unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(row.callers, "deck → station", "{name}");
        assert_eq!(row.scope, "admin", "{name}");
        assert_eq!(row.feature.as_deref(), Some(feature), "{name}");
        assert!(!row.mutating, "{name}");
    }
}

#[test]
fn the_fleet_view_row_needs_its_feature_and_scope() {
    let code = code_table();
    let row = code.get("fleet.view").expect("fleet.view missing");
    assert_eq!(row.callers, "station → deck");
    assert_eq!(row.scope, "admin");
    assert_eq!(row.feature.as_deref(), Some("fleet_view.v1"));
    assert!(!row.mutating);
}

#[test]
fn the_fleet_git_homes_methods_need_their_feature_and_scope() {
    let code = code_table();
    for (name, callers, scope) in [
        ("project.ensure_home", "deck → station", "admin"),
        ("project.home_status", "deck → station", "read"),
        ("fleet_git.artifact_authorize", "station → deck", "admin"),
        ("fleet_git.share_verdict", "deck → station", "read"),
    ] {
        let row = code.get(name).unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(row.callers, callers, "{name}");
        assert_eq!(row.scope, scope, "{name}");
        assert_eq!(row.feature.as_deref(), Some("fleet_git.homes.v1"), "{name}");
        assert!(!row.mutating, "{name}");
    }
}

#[test]
fn the_fleet_git_land_methods_need_their_feature_and_scope() {
    let code = code_table();
    for (name, scope, mutating) in [
        ("project.radar", "read", false),
        ("session.land", "drive", true),
        ("fleet_git.land_status", "read", false),
        ("fleet_git.land_abandon", "drive", false),
    ] {
        let row = code.get(name).unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(row.callers, "deck → station", "{name}");
        assert_eq!(row.scope, scope, "{name}");
        assert_eq!(row.feature.as_deref(), Some("fleet_git.land.v1"), "{name}");
        assert_eq!(row.mutating, mutating, "{name}");
    }
}

#[test]
fn the_fleet_git_branches_methods_need_their_feature_and_scope() {
    let code = code_table();
    for name in ["project.branches", "project.fetch"] {
        let row = code.get(name).unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(row.callers, "deck → station", "{name}");
        assert_eq!(row.scope, "read", "{name}");
        assert_eq!(
            row.feature.as_deref(),
            Some("fleet_git.branches.v1"),
            "{name}"
        );
        assert!(!row.mutating, "{name}");
    }
}

#[test]
fn the_fleet_git_move_methods_need_their_feature_and_scope() {
    let code = code_table();
    for (name, scope, mutating) in [
        ("session.move_out", "admin", true),
        ("session.move_export", "admin", false),
        ("session.move_prepare", "admin", false),
        ("session.move_in", "admin", true),
        ("session.move_finish", "admin", true),
        ("session.move_release", "admin", false),
        ("session.move_abort", "admin", false),
        ("session.move_discard", "admin", false),
        ("session.move_status", "read", false),
        ("session.move_fail", "admin", false),
        ("session.move_undo", "admin", true),
    ] {
        let row = code.get(name).unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(row.callers, "deck → station", "{name}");
        assert_eq!(row.scope, scope, "{name}");
        assert_eq!(row.feature.as_deref(), Some("fleet_git.move.v1"), "{name}");
        assert_eq!(row.mutating, mutating, "{name}");
    }
}

#[test]
fn the_fleet_sync_secret_methods_need_their_feature_and_scope() {
    let code = code_table();
    for name in [
        "project.secret_scan",
        "project.secret_read",
        "project.secret_write",
    ] {
        let row = code.get(name).unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(row.callers, "deck → station", "{name}");
        assert_eq!(row.scope, "admin", "{name}");
        assert_eq!(
            row.feature.as_deref(),
            Some("fleet_git.secrets.v1"),
            "{name}"
        );
        assert!(!row.mutating, "{name}");
    }
}

#[test]
fn the_secret_browse_method_needs_its_feature_and_scope() {
    let code = code_table();
    let row = code
        .get("project.secret_browse")
        .expect("project.secret_browse missing");
    assert_eq!(row.callers, "deck → station");
    assert_eq!(row.scope, "admin");
    assert_eq!(row.feature.as_deref(), Some("fleet_git.secrets_browse.v1"));
    assert!(!row.mutating);
}

#[test]
fn the_methods_are_in_the_table_with_their_feature() {
    let code = code_table();
    for name in ["session.workflows", "session.workflow_agent"] {
        let row = code.get(name).unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(row.callers, "deck → station", "{name}");
        assert_eq!(row.scope, "read", "{name}");
        assert_eq!(row.feature.as_deref(), Some("workflows.v1"), "{name}");
        assert!(!row.mutating, "{name}");
    }
}

#[test]
fn a_changed_adr_row_breaks_the_agreement() {
    let row = "| `coordinator.call` | station → deck | coordinate | `coordinator.v1` | yes |";
    let text = adr();
    assert!(text.contains(row), "the ADR has the coordinator.call row");
    let changed = text.replace(
        row,
        "| `coordinator.call` | station → deck | coordinate | `coordinator.v1` | no |",
    );
    assert_ne!(adr_table(&changed), code_table());
    let moved = text.replace(
        row,
        "| `coordinator.call` | deck → station | coordinate | `coordinator.v1` | yes |",
    );
    assert_ne!(adr_table(&moved), code_table());
}
