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
        hidden: false,
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

    fn hidden(mut self) -> Self {
        self.0.hidden = true;
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
            "allow_fleet_control",
            "Fleet control on this station",
            K::Bool,
            Live,
            "Lets coordinator sessions with a fleet-control grant act on the sessions of this station. Off stops their actions here at once.",
        )
        .danger(),
        field(
            "allow_desktop",
            "Desktop control",
            K::Bool,
            Live,
            "Lets the owner control this machine's desktop through the fleet: screens, the wall, notifications and previews. Off stops new actions at once.",
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
        field(
            "tailscale_bin",
            "Tailscale program",
            K::Text,
            Live,
            "The full path of the tailscale program. Leave it empty to find tailscale on PATH.",
        )
        .danger(),
        field(
            "clone_root",
            "Clone folder",
            K::Path,
            Restart,
            "Fleet Git clones new project homes below this folder. It must be inside an allowed root. Default: the first allowed root + /projects.",
        ),
        field(
            "traits",
            "Station labels",
            K::Texts,
            Restart,
            "Labels that a start can require, for example gpu. Each label is 1 to 32 characters: a-z, 0-9, _ or -.",
        ),
        field(
            "allow_check_cmd",
            "Allow check commands",
            K::Bool,
            Restart,
            "Lets a project run its check command on this station before a land.",
        )
        .danger(),
        field(
            "allow_local_remotes",
            "Allow local remotes",
            K::Bool,
            Restart,
            "Lets a project use a file:// URL or a local path of a bare repository as its remote. For tests and for a repository on this machine.",
        )
        .danger(),
        field(
            "taildrive_mounts",
            "Taildrive mounts",
            K::ShareMounts,
            Restart,
            "Maps a Taildrive share (<machine>/<share>) to the folder where this station mounts it.",
        ),
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
        field(
            "fleet_control",
            "Fleet control",
            K::Bool,
            Live,
            "Lets coordinator sessions with a fleet-control grant drive any session, read the fleet and answer approvals. Off stops every fleet-control action at once.",
        )
        .danger(),
        field(
            "tailscale_cert",
            "Trusted certificate from Tailscale",
            K::Bool,
            Live,
            "The board gets a certificate for its tailnet name that browsers trust. Tailscale must allow HTTPS certificates (admin console → DNS → HTTPS Certificates). The name `<host>.<tailnet>.ts.net` becomes public in Certificate Transparency logs.",
        )
        .danger(),
        field(
            "clone_host",
            "Default Git host",
            K::Text,
            Live,
            "The host for a repository given as owner/name, for example github.com or gitlab.com.",
        ),
        field(
            "clone_org",
            "Default organization",
            K::Text,
            Live,
            "The owner or group for a repository given by name only.",
        ),
        field(
            "clone_transport",
            "Clone with",
            K::Enum,
            Live,
            "https uses your Git credential helper; ssh uses your SSH keys.",
        )
        .options(&["https", "ssh"]),
        field(
            "tailscale_bin",
            "Tailscale program",
            K::Text,
            Live,
            "The full path of the tailscale program. Leave it empty to find tailscale on PATH.",
        )
        .danger(),
        field(
            "tailnet_auto",
            "Use Tailscale automatically",
            K::Bool,
            Restart,
            "On by default. At each start, put the tailnet addresses of this machine in the listen lists.",
        )
        .danger(),
        field(
            "tailnet_auto_listen",
            "Board addresses added automatically",
            K::Addresses,
            Restart,
            "The board addresses that the automatic mode added. Only the deck writes this list.",
        )
        .hidden(),
        field(
            "tailnet_auto_link_listen",
            "Link addresses added automatically",
            K::Addresses,
            Restart,
            "The station link addresses that the automatic mode added. Only the deck writes this list.",
        )
        .hidden(),
    ]
    .into_iter()
    .map(|f| f.0)
    .collect()
}
