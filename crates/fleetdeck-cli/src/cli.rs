//! Command-line shape of the `fleetdeck` binary (PLAN §1, §6.5, §8.5, GATE-M1 G12).
//!
//! An invitation is a secret: no subcommand takes it as an argument. `station
//! join` and `deck add-station` read it from a non-echoing prompt, from stdin,
//! or from `--invite-file` (G12).

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::cli_desktop::DesktopCommand;
use crate::cli_omarchy::OmarchyCommand;

/// Fleet Deck v2: one deck drives agent sessions on many stations.
#[derive(Debug, Parser)]
#[command(
    name = "fleetdeck",
    version = fleetdeck_core::build::version(),
    about,
    propagate_version = true
)]
pub struct Cli {
    /// Data directory (databases, identity key, sockets). Default: the
    /// platform data directory of the current user.
    #[arg(long, global = true, env = "FLEETDECK_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
    /// Log format on stderr: `text` or `json`. The filter comes from
    /// `FLEETDECK_LOG` (or `RUST_LOG`), default `info`.
    #[arg(
        long,
        global = true,
        env = "FLEETDECK_LOG_FORMAT",
        default_value = "text"
    )]
    pub log_format: fleetdeck_core::logging::LogFormat,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the deck and a local station in one process (single-machine default).
    Up(ServeArgs),
    /// Run or manage the deck.
    Deck {
        #[command(subcommand)]
        command: Option<DeckCommand>,
    },
    /// Run or manage a station.
    Station {
        #[command(subcommand)]
        command: Option<StationCommand>,
    },
    /// Check the local setup and say what is wrong (read-only).
    Doctor {
        /// Print only the `warn` and `fail` lines and the final line.
        #[arg(long)]
        brief: bool,
    },
    /// Read and change the settings of the running deck or station through
    /// its owner-only local socket.
    Settings {
        #[command(subcommand)]
        command: SettingsCommand,
    },
    /// Show what Tailscale tells about this machine and what the deck
    /// listens on (read-only).
    Tailnet {
        #[command(subcommand)]
        command: TailnetCommand,
    },
    /// Print a one-time board login URL (from the deck's owner-only socket).
    Open {
        /// Print a one-time elevation code for the board instead (single use,
        /// valid for 5 minutes). The board asks for it before a danger change.
        #[arg(long)]
        elevate: bool,
    },
    /// Feed the desktop shell (Omarchy bar) and open the board from it.
    #[command(subcommand)]
    Desktop(DesktopCommand),
    /// Install, remove or check the Omarchy integration (plugin, hooks, theme
    /// template, hotkeys).
    #[command(subcommand)]
    Omarchy(OmarchyCommand),
    /// Run the deck, the station, or both as a service that starts at boot or login.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Print the build identity as one JSON line (the service compares
    /// builds with it); not a user command.
    #[command(hide = true)]
    BuildInfo,
    /// The durable provider shim (ADR-005 §2). The station starts it and
    /// sends the launch frame on its stdin; it is not a user command.
    #[command(hide = true)]
    Shim {
        #[arg(long)]
        id: String,
    },
    /// The provider side of a shim launch (ADR-005 §2): a new session, the
    /// parent-death signal, then `exec` of the provider.
    #[command(hide = true)]
    ShimExec {
        #[arg(long)]
        parent: u32,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        program: Vec<std::ffi::OsString>,
    },
    /// Confines the writes of a provisioning command to the allowed roots
    /// with Landlock, then executes it (B9, DL-200).
    #[command(hide = true)]
    ConfineExec {
        #[arg(long = "write-root", required = true)]
        write_roots: Vec<PathBuf>,
        #[arg(last = true, required = true)]
        program: Vec<std::ffi::OsString>,
    },
    /// The stdio MCP server of a shim-hosted provider (DL-194): it relays
    /// each message to the station socket; it is not a user command.
    #[command(hide = true)]
    McpBridge {
        #[arg(long)]
        socket: PathBuf,
    },
    /// The first-login bootstrap of a provisioned station (P8): the hook of
    /// the package runs it; it is not a user command.
    #[command(hide = true)]
    ProvisionBootstrap {
        /// The absolute path of the provisioning file.
        #[arg(long)]
        file: PathBuf,
        /// Clear a retained terminal error first.
        #[arg(long)]
        retry: bool,
    },
    /// Copies the provisioning file from a supported medium (P8): the
    /// installer runs it; it is not a user command.
    #[command(hide = true)]
    ProvisionImport {
        /// The absolute path of the provisioning file on the medium.
        #[arg(long)]
        source: PathBuf,
    },
}

/// `fleetdeck service` (design service.md §3).
#[derive(Debug, Clone, Subcommand)]
pub enum ServiceCommand {
    /// Install and start the service (default: both = `fleetdeck up`).
    Install {
        #[arg(value_enum, default_value_t = ServiceKind::Both)]
        kind: ServiceKind,
        /// Print the files and the commands; change nothing.
        #[arg(long)]
        dry_run: bool,
        /// List the PATH folders that the service leaves out.
        #[arg(long)]
        verbose: bool,
    },
    /// Stop and remove the service (default: every installed one).
    Uninstall {
        #[arg(value_enum)]
        kind: Option<ServiceKind>,
    },
    /// Start the service (default: every installed one).
    Start {
        #[arg(value_enum)]
        kind: Option<ServiceKind>,
    },
    /// Stop the service until the next boot or login (default: every installed one).
    Stop {
        #[arg(value_enum)]
        kind: Option<ServiceKind>,
    },
    /// Show the installed services.
    Status {
        #[arg(long)]
        json: bool,
    },
}

/// What a service runs: `both` is `fleetdeck up`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ServiceKind {
    Both,
    Deck,
    Station,
}

/// Listen options of a serving process.
#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Board listen address (loopback, LAN, or tailnet only; D13).
    #[arg(long)]
    pub listen: Option<SocketAddr>,
}

#[derive(Debug, Subcommand)]
pub enum DeckCommand {
    /// Run the deck (the default without a subcommand).
    Serve(ServeArgs),
    /// Make a single-use pairing invitation for a station that dials the deck.
    Invite,
    /// Pair with a station that listens (deck dials station). Reads the
    /// invitation from a prompt, stdin, or `--invite-file`.
    AddStation(AddStationArgs),
    /// List the paired stations.
    Stations {
        /// Print the stations as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List the sessions of every paired station (the deck's read model).
    Sessions {
        /// Print the cards as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Route one command to a station through the deck's command router
    /// and wait for its receipt.
    Command {
        /// Station name or id.
        station: String,
        /// The command as JSON, for example
        /// `{"op":"interrupt","session_id":"..."}` (PLAN §6.1).
        command: String,
        /// Print the routed command as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List the commands of the deck's router.
    Commands {
        /// Print the commands as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Unpin a station key and close its links.
    RemoveStation {
        /// Station name or key fingerprint.
        station: String,
    },
    /// Set the owner password of the board (ends every login). Reads it
    /// from a prompt or from stdin.
    Password {
        /// Read the password from the first line of stdin.
        #[arg(long)]
        stdin: bool,
    },
    /// Manage API tokens for bearer clients of the board API.
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    /// Show the audit log, newest first.
    Audit {
        /// The number of entries.
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Print the entries as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Write the provisioning file of a new station (P8): the deck address and fingerprint and the station name.
    ProvisionFile(ProvisionFileArgs),
}

/// The arguments of `fleetdeck deck provision-file` (P8).
#[derive(Debug, Args)]
pub struct ProvisionFileArgs {
    /// The name of the new station.
    pub name: String,
    /// The tailnet address of this deck: `<host>[:<port>]`.
    #[arg(long)]
    pub deck: String,
    /// The absolute path of the new file.
    #[arg(long)]
    pub out: PathBuf,
}

/// `fleetdeck settings` (DL-172, design 1.3). Without `--station` the
/// command talks to the deck; with it, to the station of this machine.
#[derive(Debug, Subcommand)]
pub enum SettingsCommand {
    /// Show the settings: key, value (a secret shows `(set)`) and when a
    /// change applies (`live` or `restart`).
    Get {
        /// Use the station of this machine instead of the deck.
        #[arg(long)]
        station: bool,
        /// Print the settings view as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Change one setting. The value is read as JSON when it parses
    /// (`true`, `5`, `["a"]`, `null` removes the key), else as plain text.
    Set {
        /// The settings key, for example `conflict_mail`.
        key: String,
        /// The new value.
        #[arg(allow_hyphen_values = true)]
        value: String,
        /// Use the station of this machine instead of the deck.
        #[arg(long)]
        station: bool,
    },
    /// Store a secret value of the station. The value comes from stdin
    /// (one line) or from a prompt that does not echo; never from an
    /// argument.
    Secret {
        /// The settings key of the secret, for example
        /// `provider_instance.work.env.TOKEN`.
        key: String,
        /// Use the station of this machine (the deck has no secrets).
        #[arg(long)]
        station: bool,
    },
}

/// `fleetdeck tailnet` (DL-178).
#[derive(Debug, Subcommand)]
pub enum TailnetCommand {
    /// Show the view of the deck and of the local station: is Tailscale
    /// available, the name and addresses of this machine, userspace
    /// networking, and the tailnet listen addresses of the deck.
    Status {
        /// Print the status as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum TokenCommand {
    /// Create a token; its value is printed once.
    Create {
        /// Token name (letters, digits, `-`, `_`).
        name: String,
        /// A scope of the token: read, drive, approve or admin (repeat).
        #[arg(long = "scope", required = true)]
        scopes: Vec<String>,
        /// Lifetime in days (at most 90).
        #[arg(long, default_value_t = 30)]
        days: u32,
    },
    /// List the tokens.
    List {
        /// Print the tokens as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Revoke a token.
    Revoke {
        /// Token name.
        name: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum StationCommand {
    /// Run the station (the default without a subcommand).
    Serve,
    /// Make a single-use invitation for a deck that dials this station.
    Invite {
        /// Address the station listens on for the deck (loopback, LAN, or tailnet).
        #[arg(long)]
        listen: SocketAddr,
    },
    /// Pair this station with a deck. Reads the invitation from a prompt,
    /// stdin, or `--invite-file`; or, with `--tailnet`, asks a deck on your
    /// tailnet to pair (its owner approves on the board).
    Join(JoinArgs),
    /// Unpin the deck key on this station and close its links.
    ForgetDeck,
    /// List the sessions of this station (works without the deck).
    Sessions {
        /// Print the session summaries as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Start a session on this station (works without the deck).
    Run(RunArgs),
    /// Answer an open approval on this station (works without the deck).
    Approve(ApproveArgs),
    /// Interrupt the running turn of a session on this station.
    Interrupt {
        /// Session id.
        session: String,
    },
    /// Refuse new work and wait until no session has open work (before an
    /// upgrade). Fails when sessions are still busy at the timeout.
    Drain {
        /// The most seconds to wait.
        #[arg(long, default_value_t = 600)]
        timeout_secs: u64,
    },
    /// Self-upgrade: prepare (snapshot and trial), activate, roll back.
    Upgrade {
        #[command(subcommand)]
        command: UpgradeCommand,
    },
    /// Print the link key fingerprint of the running station (P8).
    Fingerprint {
        /// Print the full answer of the station as one JSON line.
        #[arg(long)]
        json: bool,
        /// Probe every provider again in the running station.
        #[arg(long, requires = "json")]
        probe_providers: bool,
    },
}

/// `fleetdeck station upgrade` (M3 7-3, DL-128).
#[derive(Debug, Subcommand)]
pub enum UpgradeCommand {
    /// With the station stopped: snapshot `station.db`, run the trial of the
    /// new build on a copy, and install the release. Prints the update id.
    Prepare {
        /// The new `fleetdeck` binary.
        #[arg(long)]
        binary: PathBuf,
        /// The version of the new binary.
        #[arg(long)]
        to_version: String,
    },
    /// Run by the new build: migrate a copy of the snapshot and check the
    /// shim protocols. Never touches the live database.
    Trial {
        /// The snapshot directory of `prepare`.
        #[arg(long)]
        snapshot: PathBuf,
    },
    /// Point the stable launcher at the release of a trialed update.
    Activate {
        /// Update id.
        update: String,
    },
    /// With the station stopped: go back to the previous release. Restores
    /// the snapshot only when no live state advanced and no shim is live.
    Rollback {
        /// Update id.
        update: String,
    },
}

/// `station join`: an invitation, or a request over the tailnet.
#[derive(Debug, Args)]
pub struct JoinArgs {
    #[command(flatten)]
    pub input: InviteInput,
    /// Ask the deck at `<host>[:<port>]`, a device of your tailnet (default
    /// port 7421), to pair without an invitation. The owner approves on the
    /// board.
    #[arg(
        long,
        value_name = "HOST[:PORT]",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with_all = ["invite_file", "stdin"]
    )]
    pub tailnet: Option<Option<String>>,
    /// The deck link key fingerprint (64 hex digits) from the board. Without
    /// it, the command shows the fingerprint that the deck presents and
    /// asks you to confirm it.
    #[arg(long, requires = "tailnet", value_name = "HEX")]
    pub deck_fingerprint: Option<String>,
    /// The scopes this station gives to the deck: read, drive, approve,
    /// admin (default: all four).
    #[arg(long, requires = "tailnet", value_delimiter = ',', value_name = "LIST")]
    pub scopes: Option<Vec<String>>,
    /// The name of this station on the deck (default: the name in
    /// station.toml).
    #[arg(long, requires = "tailnet")]
    pub name: Option<String>,
    /// Remove the pending pairing request of this station (its retry file).
    #[arg(long, requires = "tailnet")]
    pub cancel: bool,
}

/// Where an invitation comes from. Never from argv (G12).
#[derive(Debug, Args)]
pub struct InviteInput {
    /// Read the invitation from this file (mode 0600 required).
    #[arg(long, conflicts_with = "stdin")]
    pub invite_file: Option<PathBuf>,
    /// Read the invitation from standard input.
    #[arg(long)]
    pub stdin: bool,
}

#[derive(Debug, Args)]
pub struct AddStationArgs {
    #[command(flatten)]
    pub input: InviteInput,
    /// The station's name on the board (default: the name in its
    /// station.toml, learned at the first link).
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Provider instance id from `station.toml` (for example `claude`).
    #[arg(long)]
    pub provider: String,
    /// Working directory (must be inside `allowed_roots`).
    #[arg(long)]
    pub cwd: PathBuf,
    /// Permission mode (a native mode of the provider).
    #[arg(long)]
    pub mode: Option<String>,
    /// The first prompt.
    pub prompt: String,
}

#[derive(Debug, Args)]
pub struct ApproveArgs {
    /// Request id of the open approval or question.
    pub request: String,
    /// Native option id to answer with (for example `allow` or `deny`).
    #[arg(long, default_value = "allow")]
    pub option: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("fleetdeck").chain(args.iter().copied()))
    }

    #[test]
    fn the_command_tree_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_plan_subcommand_parses() {
        for args in [
            &["up"][..],
            &["deck"],
            &["deck", "serve"],
            &["deck", "invite"],
            &["deck", "add-station", "--stdin"],
            &["deck", "stations"],
            &["deck", "remove-station", "laptop"],
            &["station"],
            &["station", "serve"],
            &["station", "invite", "--listen", "192.168.1.5:7443"],
            &["station", "join", "--invite-file", "inv.txt"],
            &["station", "join"],
            &["station", "join", "--tailnet", "deck.tail.ts.net"],
            &[
                "station",
                "join",
                "--tailnet",
                "deck.tail.ts.net:7421",
                "--name",
                "laptop",
            ],
            &[
                "station",
                "join",
                "--tailnet",
                "d",
                "--scopes",
                "read,drive",
            ],
            &[
                "station",
                "join",
                "--tailnet",
                "d",
                "--deck-fingerprint",
                "ab",
            ],
            &["station", "join", "--tailnet", "--cancel"],
            &["station", "forget-deck"],
            &["station", "sessions"],
            &["station", "sessions", "--json"],
            &[
                "station",
                "run",
                "--provider",
                "claude",
                "--cwd",
                "/tmp",
                "hello",
            ],
            &["station", "approve", "req-1", "--option", "deny"],
            &["station", "interrupt", "s-1"],
            &["open"],
            &["open", "--elevate"],
            &["doctor"],
            &["settings", "get"],
            &["settings", "get", "--station", "--json"],
            &["settings", "set", "conflict_mail", "false"],
            &["settings", "set", "max_sessions", "-1", "--station"],
            &[
                "settings",
                "secret",
                "provider_instance.work.env.TOKEN",
                "--station",
            ],
        ] {
            assert!(parse(args).is_ok(), "{args:?} should parse");
        }
    }

    #[test]
    fn an_invitation_given_as_an_argument_is_refused() {
        let invitation = "fdinv1:wss://127.0.0.1:7443#abc.def.123";
        assert!(parse(&["station", "join", invitation]).is_err());
        assert!(parse(&["deck", "add-station", invitation]).is_err());
    }

    #[test]
    fn a_secret_value_is_never_an_argument() {
        // `secret` takes the key only: a second positional is refused.
        assert!(parse(&["settings", "secret", "k", "value", "--station"]).is_err());
        assert!(parse(&["settings", "secret"]).is_err());
        assert!(parse(&["settings", "set", "only-a-key"]).is_err());
        let cli = parse(&["settings", "secret", "k"]).unwrap();
        let Command::Settings {
            command: SettingsCommand::Secret { key, station },
        } = cli.command
        else {
            panic!("not a settings secret");
        };
        assert_eq!((key.as_str(), station), ("k", false));
    }

    #[test]
    fn settings_flags_select_the_holder() {
        let cli = parse(&["settings", "set", "a", "-5", "--station"]).unwrap();
        let Command::Settings {
            command:
                SettingsCommand::Set {
                    key,
                    value,
                    station,
                },
        } = cli.command
        else {
            panic!("not a settings set");
        };
        assert_eq!((key.as_str(), value.as_str(), station), ("a", "-5", true));
    }

    #[test]
    fn the_tailnet_flags_need_tailnet_and_exclude_an_invitation() {
        for flag in [
            &["--deck-fingerprint", "ab"][..],
            &["--scopes", "read"],
            &["--name", "x"],
            &["--cancel"],
        ] {
            let mut args = vec!["station", "join"];
            args.extend(flag);
            assert!(parse(&args).is_err(), "{flag:?} needs --tailnet");
        }
        assert!(parse(&["station", "join", "--tailnet", "d", "--stdin"]).is_err());
        assert!(parse(&["station", "join", "--tailnet", "d", "--invite-file", "x"]).is_err());
        // `--tailnet --cancel` has no host.
        let cli = parse(&["station", "join", "--tailnet", "--cancel"]).unwrap();
        let Command::Station {
            command: Some(StationCommand::Join(args)),
        } = cli.command
        else {
            panic!("not a join");
        };
        assert_eq!(args.tailnet, Some(Some(String::new())));
        assert!(args.cancel);
        let cli = parse(&[
            "station",
            "join",
            "--tailnet",
            "d:9",
            "--scopes",
            "read,admin",
        ])
        .unwrap();
        let Command::Station {
            command: Some(StationCommand::Join(args)),
        } = cli.command
        else {
            panic!("not a join");
        };
        assert_eq!(args.tailnet, Some(Some("d:9".into())));
        assert_eq!(args.scopes, Some(vec!["read".into(), "admin".into()]));
    }

    #[test]
    fn stdin_and_invite_file_exclude_each_other() {
        assert!(parse(&["station", "join", "--stdin", "--invite-file", "x"]).is_err());
    }

    #[test]
    fn the_log_format_is_text_or_json() {
        use fleetdeck_core::logging::LogFormat;
        assert_eq!(parse(&["up"]).unwrap().log_format, LogFormat::Text);
        let cli = parse(&["station", "sessions", "--log-format", "json"]).unwrap();
        assert_eq!(cli.log_format, LogFormat::Json);
        assert!(parse(&["up", "--log-format", "xml"]).is_err());
    }
}
