//! The station reads of the desktop tools (`board-gaps.md` §2.1, §2.3, §2.4,
//! §2.9, BG-1): `station.tools` and `station.desktop_tools_plan`, and the
//! plans that the maintenance channel (BG1-3) reads.
//!
//! No value from the wire reaches an argv, an environment or a path. The
//! children are fixed (`probe.rs`), the packages come from a compiled
//! allowlist (`mapping.rs`), and the answers hold parsed facts only.

use std::time::{SystemTime, UNIX_EPOCH};

use fleetdeck_core::ChildEnv;
use fleetdeck_core::identity::Fingerprint;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::omarchy_admin::code as request_code;
use serde::Serialize;
use serde_json::Value;

mod etc_check;
mod facts;
pub mod mapping;
mod plan;
mod probe;
mod store;
mod transaction;

#[cfg(test)]
mod call_tests;
#[cfg(test)]
mod facts_desktop_tests;
#[cfg(test)]
mod facts_tests;
#[cfg(test)]
mod mapping_tests;
#[cfg(test)]
mod plan_refusal_tests;
#[cfg(test)]
mod plan_tests;
#[cfg(test)]
mod probe_tests;
#[cfg(test)]
mod recheck_tests;
#[cfg(test)]
mod test_support;

#[allow(unused_imports)] // Used by the maintenance channel (BG1-3).
pub(crate) use plan::recheck;
#[allow(unused_imports)] // Used by the maintenance channel (BG1-3).
pub(crate) use store::{StoredPlan, plan, remove};

const TEXT_INVALID: &str = "The request is not valid.";

/// Unix milliseconds now.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn wire(code: &str, text: &str) -> Outcome {
    Outcome::Err(WireError::new(code, text))
}

fn answer(value: impl Serialize) -> Outcome {
    match serde_json::to_value(value) {
        Ok(v) => Outcome::Ok(v),
        Err(_) => wire("internal", "The answer could not be built."),
    }
}

/// The link handler of `station.tools` and `station.desktop_tools_plan`.
/// `peer` is the fingerprint of the deck link of the request: a plan is
/// bound to it. The params must be `null` or `{}`.
pub async fn call(method: &str, params: Value, peer: Fingerprint) -> Outcome {
    if !matches!(method, "station.tools" | "station.desktop_tools_plan") {
        return Outcome::Err(WireError::new(
            crate::link::NOT_IMPLEMENTED,
            format!("`{method}` is not served by this station build"),
        ));
    }
    if !crate::omarchy_admin::is_empty_params(&params) {
        return wire(request_code::INVALID_REQUEST, TEXT_INVALID);
    }
    let base = ChildEnv::from_process();
    let found = crate::desktop::detect::detect(&base).await;
    if method == "station.tools" {
        let tools = facts::collect(&probe::RealExec, &base, found.as_ref(), probe::TOTAL_LIMIT);
        return answer(tools.await);
    }
    let plan = plan::build(
        &probe::RealExec,
        &etc_check::EtcRoot::system(),
        &store::STORE,
        &base,
        found.as_ref(),
        peer,
        now_ms(),
    )
    .await;
    match plan {
        Some(plan) => answer(plan),
        None => wire("internal", "The answer could not be built."),
    }
}
