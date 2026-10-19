//! R13 P7 §3.2 and R13 P7b §3.1, §4: the station-internal `input_enabled`,
//! `click_enabled` and `input_any` classes of a binding come only from the
//! spec fields, the classes `desktop`, `capture` and `input`, and the features
//! that the deck link negotiated at the start.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use fleetdeck_proto::CommandId;
use fleetdeck_proto::command::CoordinatorSpec;
use fleetdeck_proto::coordinator_spec::InputScope;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use serde_json::Value;

use super::{DeckFeatures, ToolClass, grant_classes};
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

fn features(input_mcp: bool, click_mcp: bool, input_any: bool) -> DeckFeatures {
    DeckFeatures {
        input_mcp,
        click_mcp,
        input_any,
    }
}

/// Every input feature on the link.
const FULL: DeckFeatures = DeckFeatures {
    input_mcp: true,
    click_mcp: true,
    input_any: true,
};

fn derived(
    classes: &[&str],
    allow: bool,
    scope: Option<InputScope>,
    click: bool,
    f: DeckFeatures,
) -> BTreeSet<ToolClass> {
    let mut s = spec(classes, allow, scope);
    s.allow_click = click;
    grant_classes(&s, f).unwrap()
}

fn enabled(classes: &[&str], allow: bool, scope: Option<InputScope>, mcp: bool) -> bool {
    derived(classes, allow, scope, false, features(mcp, false, false))
        .contains(&ToolClass::InputEnabled)
}

const ALL: [&str; 3] = ["desktop", "capture", "input"];
const OWN: Option<InputScope> = Some(InputScope::OwnWindows);
const ANY: Option<InputScope> = Some(InputScope::AnyWindow);

/// The three internal classes of a derivation, as `[input, click, any]`.
fn internal(classes: &BTreeSet<ToolClass>) -> [bool; 3] {
    [
        classes.contains(&ToolClass::InputEnabled),
        classes.contains(&ToolClass::ClickEnabled),
        classes.contains(&ToolClass::InputAny),
    ]
}

#[test]
fn the_class_needs_every_condition() {
    assert!(enabled(&ALL, true, OWN, true));
    // The public class itself is the same with or without the allowance.
    let classes = derived(&ALL, true, OWN, false, features(true, false, false));
    assert!(classes.contains(&ToolClass::Input));
    // Each missing condition gives no `input_enabled`.
    assert!(!enabled(&ALL, false, OWN, true), "no allow_input");
    assert!(!enabled(&ALL, true, None, true), "no scope");
    assert!(!enabled(&ALL, true, OWN, false), "no feature");
    assert!(
        !enabled(&["capture", "input"], true, OWN, true),
        "no desktop"
    );
    assert!(
        !enabled(&["desktop", "input"], true, OWN, true),
        "no capture"
    );
    assert!(
        !enabled(&["desktop", "capture"], true, OWN, true),
        "no input"
    );
    assert!(!enabled(&[], true, OWN, true), "no classes");
    // Without the opt-in the public class `input` is still a class, but a
    // start never adds the internal class.
    let classes = derived(&ALL, false, None, false, FULL);
    assert!(classes.contains(&ToolClass::Input));
    assert_eq!(internal(&classes), [false; 3]);
}

#[test]
fn the_own_scope_gives_the_click_class_only_with_the_allowance_and_the_feature() {
    // [input, click, any]
    for (click, click_mcp, expected) in [
        (false, false, [true, false, false]),
        (false, true, [true, false, false]),
        (true, false, [true, false, false]),
        (true, true, [true, true, false]),
    ] {
        let classes = derived(&ALL, true, OWN, click, features(true, click_mcp, false));
        assert_eq!(internal(&classes), expected, "{click} {click_mcp}");
        // `input_any` on the link never gives `input_any` to an own scope.
        let classes = derived(&ALL, true, OWN, click, features(true, click_mcp, true));
        assert_eq!(internal(&classes), expected, "{click} {click_mcp} any");
    }
}

#[test]
fn the_any_scope_needs_its_feature_and_is_never_reduced_to_the_own_scope() {
    // With `desktop_input_any.v1`: input, `input_any` and the click rules.
    let classes = derived(&ALL, true, ANY, false, features(true, false, true));
    assert_eq!(internal(&classes), [true, false, true]);
    let classes = derived(&ALL, true, ANY, true, features(true, true, true));
    assert_eq!(internal(&classes), [true, true, true]);
    let classes = derived(&ALL, true, ANY, true, features(true, false, true));
    assert_eq!(internal(&classes), [true, false, true]);
    // Without it no input class at all, whatever the other flags are.
    for (click, click_mcp) in [(false, false), (false, true), (true, false), (true, true)] {
        let classes = derived(&ALL, true, ANY, click, features(true, click_mcp, false));
        assert_eq!(internal(&classes), [false; 3], "{click} {click_mcp}");
        assert!(classes.contains(&ToolClass::Input));
    }
    // Without `desktop_input_mcp.v1` nothing, even with the other two.
    let classes = derived(&ALL, true, ANY, true, features(false, true, true));
    assert_eq!(internal(&classes), [false; 3]);
}

#[test]
fn the_click_and_any_classes_need_the_three_explicit_classes_and_the_allowance() {
    for classes in [
        vec!["capture", "input"],
        vec!["desktop", "input"],
        vec!["desktop", "capture"],
        vec![],
    ] {
        for scope in [OWN, ANY] {
            let got = derived(&classes, true, scope, true, FULL);
            assert_eq!(internal(&got), [false; 3], "{classes:?} {scope:?}");
        }
    }
    for scope in [OWN, ANY] {
        let got = derived(&ALL, false, scope, true, FULL);
        assert_eq!(internal(&got), [false; 3], "{scope:?}");
    }
    // The click allowance alone (no scope) gives nothing.
    let got = derived(&ALL, true, None, true, FULL);
    assert_eq!(internal(&got), [false; 3]);
}

#[test]
fn a_spec_that_names_an_internal_class_is_refused_as_unknown() {
    for name in ["input_enabled", "click_enabled", "input_any"] {
        for (allow, f) in [(false, DeckFeatures::default()), (true, FULL)] {
            let err = grant_classes(&spec(&["desktop", "capture", "input", name], allow, OWN), f)
                .unwrap_err();
            assert_eq!(err, format!("unknown grant class `{name}`"));
        }
    }
}

struct Link {
    input: bool,
    click: bool,
    any: bool,
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
        (self.input && name == feature::DESKTOP_INPUT_MCP)
            || (self.click && name == feature::DESKTOP_CLICK_MCP)
            || (self.any && name == feature::DESKTOP_INPUT_ANY)
    }
}

#[test]
fn the_start_reads_the_features_of_the_current_deck_link() {
    let bindings = Bindings::default();
    // No deck link: no feature.
    assert_eq!(DeckFeatures::of(&bindings), DeckFeatures::default());
    bindings.set_deck(
        1,
        Arc::new(Link {
            input: false,
            click: false,
            any: false,
        }),
    );
    assert_eq!(DeckFeatures::of(&bindings), DeckFeatures::default());
    bindings.set_deck(
        2,
        Arc::new(Link {
            input: true,
            click: false,
            any: false,
        }),
    );
    assert_eq!(DeckFeatures::of(&bindings), features(true, false, false));
    bindings.set_deck(
        3,
        Arc::new(Link {
            input: true,
            click: true,
            any: true,
        }),
    );
    assert_eq!(DeckFeatures::of(&bindings), FULL);
    bindings.set_deck(
        4,
        Arc::new(Link {
            input: false,
            click: true,
            any: false,
        }),
    );
    assert_eq!(DeckFeatures::of(&bindings), features(false, true, false));
    bindings.clear_deck(4);
    assert_eq!(DeckFeatures::of(&bindings), DeckFeatures::default());
}
