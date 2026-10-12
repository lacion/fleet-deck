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
    assert_eq!(adr.len(), 36, "{:?}", adr.keys());
    for (name, row) in &adr {
        assert_eq!(code.get(name), Some(row), "{name}");
    }
    assert_eq!(adr, code);
}

#[test]
fn a_changed_adr_row_breaks_the_agreement() {
    let row = "| `coordinator.call` | station → deck | coordinate | — | yes |";
    let text = adr();
    assert!(text.contains(row), "the ADR has the coordinator.call row");
    let changed = text.replace(
        row,
        "| `coordinator.call` | station → deck | coordinate | — | no |",
    );
    assert_ne!(adr_table(&changed), code_table());
    let moved = text.replace(
        row,
        "| `coordinator.call` | deck → station | coordinate | — | yes |",
    );
    assert_ne!(adr_table(&moved), code_table());
}
