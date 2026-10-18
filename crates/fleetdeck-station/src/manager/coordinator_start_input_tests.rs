//! R13 P7 §3.2: the station-internal `input_enabled` class of a binding comes
//! only from `allow_input` with the scope `own_windows`, the classes
//! `desktop`, `capture` and `input`, and `desktop_input_mcp.v1` on the deck
//! link at the start.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use async_trait::async_trait;
use fleetdeck_proto::CommandId;
use fleetdeck_proto::command::CoordinatorSpec;
use fleetdeck_proto::coordinator_spec::InputScope;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use serde_json::Value;

use super::{ToolClass, deck_input_mcp, grant_classes};
use crate::coordinator::{Bindings, Transport};
use crate::manager::coordinator_spec_tests::coordinator_spec;

fn spec(classes: &[&str], allow: bool, scope: Option<InputScope>) -> CoordinatorSpec {
    CoordinatorSpec {
        classes: classes.iter().map(|c| (*c).to_owned()).collect(),
        allow_input: allow,
        input_scope: scope,
        ..coordinator_spec()
    }
}

fn enabled(classes: &[&str], allow: bool, scope: Option<InputScope>, mcp: bool) -> bool {
    grant_classes(&spec(classes, allow, scope), mcp)
        .unwrap()
        .contains(&ToolClass::InputEnabled)
}

const ALL: [&str; 3] = ["desktop", "capture", "input"];

#[test]
fn the_class_needs_every_condition() {
    let own = Some(InputScope::OwnWindows);
    assert!(enabled(&ALL, true, own, true));
    // The public class itself is the same with or without the allowance.
    let classes = grant_classes(&spec(&ALL, true, own), true).unwrap();
    assert!(classes.contains(&ToolClass::Input));
    // Each missing condition gives no `input_enabled`.
    assert!(!enabled(&ALL, false, own, true), "no allow_input");
    assert!(!enabled(&ALL, true, None, true), "no scope");
    assert!(!enabled(&ALL, true, own, false), "no feature");
    assert!(
        !enabled(&["capture", "input"], true, own, true),
        "no desktop"
    );
    assert!(
        !enabled(&["desktop", "input"], true, own, true),
        "no capture"
    );
    assert!(
        !enabled(&["desktop", "capture"], true, own, true),
        "no input"
    );
    assert!(!enabled(&[], true, own, true), "no classes");
    // Without the opt-in the public class `input` is still a class, but a
    // start never adds the internal class.
    let classes = grant_classes(&spec(&ALL, false, None), true).unwrap();
    assert!(classes.contains(&ToolClass::Input));
    assert!(!classes.contains(&ToolClass::InputEnabled));
}

#[test]
fn a_spec_that_names_the_internal_class_is_refused_as_unknown() {
    for (allow, mcp) in [(false, false), (true, true)] {
        let err = grant_classes(
            &spec(
                &["desktop", "capture", "input", "input_enabled"],
                allow,
                Some(InputScope::OwnWindows),
            ),
            mcp,
        )
        .unwrap_err();
        assert_eq!(err, "unknown grant class `input_enabled`");
    }
}

struct Link {
    input: bool,
}

#[async_trait]
impl Transport for Link {
    async fn request(
        &self,
        _method: &str,
        _command_id: Option<&CommandId>,
        _params: Value,
    ) -> Result<Outcome, String> {
        Err("not used".to_owned())
    }

    fn has_feature(&self, name: &str) -> bool {
        self.input && name == feature::DESKTOP_INPUT_MCP
    }
}

#[test]
fn the_start_reads_the_feature_of_the_current_deck_link() {
    let bindings = Bindings::default();
    // No deck link: no feature.
    assert!(!deck_input_mcp(&bindings));
    bindings.set_deck(1, Arc::new(Link { input: false }));
    assert!(!deck_input_mcp(&bindings));
    bindings.set_deck(2, Arc::new(Link { input: true }));
    assert!(deck_input_mcp(&bindings));
    bindings.clear_deck(2);
    assert!(!deck_input_mcp(&bindings));
}
