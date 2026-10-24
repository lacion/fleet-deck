//! The field entries of `[provider_updates]` (B28, design `cli-update.md`
//! section 3.10). `fields.rs` appends this list to the station table.

use fleetdeck_proto::settings::Apply::{Live, Restart};
use fleetdeck_proto::settings::FieldKind as K;
use fleetdeck_proto::settings::FieldMeta;

use super::fields::field;

/// The `[provider_updates]` fields of `station.toml`.
pub(super) fn provider_update_fields() -> Vec<FieldMeta> {
    [
        field(
            "provider_updates.enabled",
            "Update Claude and Codex at night",
            K::Bool,
            Restart,
            "The station installs new Claude and Codex versions in the night window, only from the vendor, and goes back to the old version when a check fails. Claude sessions that the station starts then no longer update Claude by themselves. Takes effect after a station restart.",
        )
        .danger(),
        field(
            "provider_updates.window_start",
            "Night window start",
            K::Text,
            Live,
            "The start of the update window, HH:MM in the time zone below.",
        ),
        field(
            "provider_updates.window_hours",
            "Night window length (hours)",
            K::Int,
            Live,
            "How long the station tries; it waits while sessions run.",
        )
        .range(1, 12),
        field(
            "provider_updates.timezone",
            "Window time zone",
            K::Text,
            Live,
            "local (the clock of this machine), UTC, or an offset like +02:00.",
        ),
        field(
            "provider_updates.claude_channel",
            "Claude channel",
            K::Enum,
            Live,
            "stable: a version about one week old that skips releases with major regressions. latest: every release. Codex has one channel.",
        )
        .options(&["stable", "latest"]),
        field(
            "provider_updates.claude_pin",
            "Claude version pin",
            K::Text,
            Live,
            "An exact version, for example 2.1.305. The station installs it and stays on it, also when it is older. Leave it empty to follow the channel.",
        )
        .danger(),
        field(
            "provider_updates.codex_pin",
            "Codex version pin",
            K::Text,
            Live,
            "An exact version, for example 0.163.0. The station installs it and stays on it, also when it is older. Leave it empty to follow the latest release.",
        )
        .danger(),
        field(
            "provider_updates.min_age_hours",
            "Wait for new versions (hours)",
            K::Int,
            Live,
            "A new version installs only after it has been public this long, so that a bad release is found first. A pin does not wait.",
        )
        .range(0, 336)
        .danger(),
    ]
    .into_iter()
    .map(|f| f.0)
    .collect()
}
