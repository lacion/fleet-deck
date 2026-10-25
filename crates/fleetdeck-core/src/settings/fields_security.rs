//! The field entries of the security options (B27 §3.2, §3.4, §3.5): the grant
//! limits with "no limit", the bypass switch of worker grants and the default
//! mode of a station. `fields.rs` appends these lists to its tables.

use fleetdeck_proto::settings::Apply::Live;
use fleetdeck_proto::settings::FieldKind as K;
use fleetdeck_proto::settings::FieldMeta;

use super::fields::field;
use crate::config::codex_modes;

/// The security fields of `deck.toml`.
pub(super) fn deck_security_fields() -> Vec<FieldMeta> {
    [
        field(
            "grant_limits.worker_grants",
            "Most worker grants",
            K::Limit,
            Live,
            "The most worker grants in effect at the same time, on all stations. Default 200. No limit lets every session that you start get a worker grant.",
        )
        .range(1, 100_000)
        .danger(),
        field(
            "grant_limits.spawn_cap",
            "Highest session limit of a grant",
            K::Limit,
            Live,
            "The highest session limit that you can give a grant on the board. Default 50.",
        )
        .range(1, 100_000)
        .danger(),
        field(
            "grant_limits.worker_spawn_cap",
            "Highest session limit of a worker grant",
            K::Limit,
            Live,
            "The highest value of Worker grant session limit. Default 10.",
        )
        .range(1, 100_000)
        .danger(),
        field(
            "grant_limits.idle_hours",
            "Longest idle time of a grant (hours)",
            K::Limit,
            Live,
            "The longest time without use that a grant can have. Default 24. No limit lets a grant stay in effect without use until its lifetime ends.",
        )
        .range(1, 168)
        .danger(),
        field(
            "grant_limits.answer_cap",
            "Highest answer limit of a grant",
            K::Limit,
            Live,
            "The highest number of approvals and questions that one grant can answer. Default 5000.",
        )
        .range(1, 100_000)
        .danger(),
        field(
            "worker_grant.allow_bypass",
            "Allow bypass mode for workers",
            K::Bool,
            Live,
            "Lets Worker grant Claude mode and Worker grant Codex mode be the bypass mode. A worker can then start a session that runs every tool without asking, only on a station that allows bypass permissions and only while the worker itself runs in a bypass mode.",
        )
        .danger(),
    ]
    .into_iter()
    .map(|f| f.0)
    .collect()
}

/// The security fields of `station.toml`.
pub(super) fn station_security_fields() -> Vec<FieldMeta> {
    let codex: Vec<String> = codex_modes();
    let codex: Vec<&str> = codex.iter().map(String::as_str).collect();
    [
        field(
            "default_mode.claude",
            "Default Claude mode",
            K::Enum,
            Live,
            "The mode of a new Claude session when the start gives no mode: from the board, a schedule or a worker. Default: default (asks before changes). A start from issue or pull request text keeps a restrictive mode.",
        )
        .options(&[
            "default",
            "plan",
            "acceptEdits",
            "dontAsk",
            "auto",
            "bypassPermissions",
        ])
        .danger(),
        field(
            "default_mode.codex",
            "Default Codex mode",
            K::Enum,
            Live,
            "The mode of a new Codex session when the start gives no mode: from the board, a schedule or a worker. Default: untrusted/workspace-write. A start from issue or pull request text keeps a restrictive mode.",
        )
        .options(&codex)
        .danger(),
    ]
    .into_iter()
    .map(|f| f.0)
    .collect()
}
