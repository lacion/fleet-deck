//! DL-177: `allow_bypass_permissions` decides whether the adapters offer the
//! mode that skips every permission check.

use std::sync::Arc;

use fleetdeck_providers::host::{DirectChildHost, SessionHost};

use super::policy_hub::test_config;
use super::*;

const CLAUDE_BYPASS: &str = "bypassPermissions";
const CODEX_FULL_ACCESS: &str = "never/danger-full-access";

fn modes(extra: &str) -> (Vec<String>, Vec<String>) {
    let tmp = tempfile::tempdir().unwrap();
    let config = test_config(tmp.path(), extra);
    let host: Arc<dyn SessionHost> = Arc::new(DirectChildHost::new());
    let adapters = adapters_from_config(&config, &host, None).unwrap();
    let of = |kind: fleetdeck_proto::event::ProviderKind| {
        adapters
            .values()
            .find(|a| a.kind() == kind)
            .unwrap()
            .capabilities()
            .modes
    };
    (
        of(fleetdeck_proto::event::ProviderKind::Claude),
        of(fleetdeck_proto::event::ProviderKind::Codex),
    )
}

#[test]
fn the_adapters_offer_the_bypass_modes_only_when_the_station_allows_them() {
    let (claude, codex) = modes("");
    assert!(!claude.iter().any(|m| m == CLAUDE_BYPASS));
    assert!(!codex.iter().any(|m| m == CODEX_FULL_ACCESS));
    let (claude, codex) = modes("allow_bypass_permissions = false\n");
    assert!(!claude.iter().any(|m| m == CLAUDE_BYPASS));
    assert!(!codex.iter().any(|m| m == CODEX_FULL_ACCESS));
    let (claude, codex) = modes("allow_bypass_permissions = true\n");
    assert!(claude.iter().any(|m| m == CLAUDE_BYPASS));
    assert!(codex.iter().any(|m| m == CODEX_FULL_ACCESS));
}
