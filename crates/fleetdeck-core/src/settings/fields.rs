//! The field tables: one entry for each key that the config parsers accept
//! (design 1.1). The tests check that the keys equal the parser keys.

use fleetdeck_proto::settings::{Apply, FieldKind, FieldMeta};

use crate::config::MAX_HIBERNATE_AFTER_MIN;

/// The builder of one entry.
struct F(FieldMeta);

fn field(key: &str, label: &str, kind: FieldKind, apply: Apply, help: &str) -> F {
    F(FieldMeta {
        key: key.to_owned(),
        label: label.to_owned(),
        help: help.to_owned(),
        kind,
        min: None,
        max: None,
        options: Vec::new(),
        apply,
        danger: false,
        secret: false,
        read_only: false,
        cli_flag: None,
    })
}

impl F {
    fn range(mut self, min: i64, max: i64) -> Self {
        self.0.min = Some(min);
        self.0.max = Some(max);
        self
    }

    fn options(mut self, options: &[&str]) -> Self {
        self.0.options = options.iter().map(|o| (*o).to_owned()).collect();
        self
    }

    fn danger(mut self) -> Self {
        self.0.danger = true;
        self
    }

    fn secret(mut self) -> Self {
        self.0.secret = true;
        self
    }

    fn read_only(mut self) -> Self {
        self.0.read_only = true;
        self
    }
}

use Apply::{Live, Restart};
use FieldKind as K;

/// The fields of `station.toml`.
#[must_use]
pub fn station_fields() -> Vec<FieldMeta> {
    [
        field("name", "Station name", K::Text, Live, "The name that the board shows for this station."),
        field(
            "allowed_roots",
            "Allowed folders",
            K::Paths,
            Live,
            "Sessions can run only in these folders and in the folders inside them.",
        )
        .danger(),
        field(
            "max_sessions",
            "Most sessions",
            K::Int,
            Live,
            "The most sessions that run at the same time on this station.",
        )
        .range(1, 256),
        field(
            "allow_setup_cmd",
            "Allow setup commands",
            K::Bool,
            Live,
            "Let a new session run a shell command, from the board, before it starts. The command runs on this machine.",
        )
        .danger(),
        field(
            "allow_shell",
            "Allow terminals",
            K::Bool,
            Live,
            "Let the board open a terminal on this station.",
        )
        .danger(),
        field(
            "allow_bypass_permissions",
            "Allow bypass permissions",
            K::Bool,
            Restart,
            "Offer the mode that skips every permission check (Claude bypassPermissions, Codex danger-full-access) for sessions on this station.",
        )
        .danger(),
        field(
            "max_shells",
            "Most terminals",
            K::Int,
            Live,
            "The most terminals that are open at the same time on this station.",
        )
        .range(0, 64),
        field(
            "offline_approval_policy",
            "Approvals while no deck is connected",
            K::Text,
            Live,
            "Write wait to keep a permission request open, or deny_after(5m) to deny it after a time (units s, m, h).",
        )
        .danger(),
        field(
            "listen",
            "Listen addresses",
            K::Addresses,
            Restart,
            "The addresses where this station accepts a deck. Use a loopback address unless you trust the network.",
        )
        .danger(),
        field(
            "provider_instance",
            "Provider logins",
            K::Instances,
            Restart,
            "The Claude and Codex logins that this station can use, each with its own folder and variables. A stored value is never shown.",
        )
        .danger()
        .secret(),
        field(
            "host",
            "Where sessions run",
            K::Enum,
            Restart,
            "direct: sessions end when the station stops. shim: sessions keep running while the station restarts.",
        )
        .options(&["direct", "shim"])
        .danger(),
        field(
            "shim_buffer_mib",
            "Output buffer (MiB)",
            K::Int,
            Restart,
            "The output that a durable session keeps while no station reads it.",
        )
        .range(1, 256),
        field(
            "repo_groups",
            "Repository groups",
            K::RepoGroups,
            Live,
            "Put repositories in one group for the conflict check. Use it for a repository with several remotes or none.",
        ),
        field(
            "hibernate_after_min",
            "Stop idle sessions after (minutes)",
            K::Int,
            Live,
            "A session that is idle this long stops, and you can resume it in the board. Use 0 to turn this off.",
        )
        .range(0, i64::from(MAX_HIBERNATE_AFTER_MIN)),
        field(
            "trackers",
            "Issue trackers from the file",
            K::Trackers,
            Live,
            "The Jira and Linear entries of the config file. Edit the file to change them.",
        )
        .read_only(),
        field(
            "member_deny",
            "Hidden file patterns",
            K::Texts,
            Live,
            "More file patterns that the members of a shared session cannot see. The built-in patterns stay.",
        ),
        field(
            "model_help.enabled",
            "Model help",
            K::Bool,
            Live,
            "Let this station ask your own provider login for short generated text, such as a title.",
        ),
        field(
            "model_help.claude_model",
            "Claude model for help",
            K::Text,
            Live,
            "The model that model help uses with a Claude login.",
        ),
        field(
            "model_help.codex_model",
            "Codex model for help",
            K::Text,
            Live,
            "The model that model help uses with a Codex login.",
        ),
        field(
            "model_help.provider",
            "Provider for help",
            K::Enum,
            Live,
            "The provider that model help uses. Leave it empty to use the first provider that is signed in.",
        )
        .options(&["claude", "codex"]),
        field(
            "model_help.per_call_max_output_tokens",
            "Most tokens in one answer",
            K::Int,
            Live,
            "The longest answer of one model help call, in tokens.",
        )
        .range(1, 4000),
        field(
            "model_help.daily_token_cap",
            "Daily token cap",
            K::Int,
            Live,
            "The most tokens that model help spends in one UTC day.",
        )
        .range(1, 1_000_000_000),
    ]
    .into_iter()
    .map(|f| f.0)
    .collect()
}

/// The fields of `deck.toml`.
#[must_use]
pub fn deck_fields() -> Vec<FieldMeta> {
    [
        field("name", "Deck name", K::Text, Live, "The name of this deck."),
        field(
            "listen",
            "Board addresses",
            K::Addresses,
            Restart,
            "The addresses where the board is served. Use a loopback address unless you trust the network.",
        )
        .danger(),
        field(
            "link_listen",
            "Station link addresses",
            K::Addresses,
            Restart,
            "The addresses where stations connect to this deck. Leave the list empty if only this deck dials.",
        )
        .danger(),
        field(
            "tailnet_hostname",
            "Tailnet host name",
            K::Text,
            Restart,
            "The host name of this deck on your tailnet. The board accepts a connection that uses it.",
        ),
        field(
            "conflict_mail",
            "Conflict mail",
            K::Bool,
            Live,
            "Tell both sessions once when two sessions change the same file.",
        ),
    ]
    .into_iter()
    .map(|f| f.0)
    .collect()
}
