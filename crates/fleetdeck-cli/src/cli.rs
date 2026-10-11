//! Command-line shape of the `fleetdeck` binary (PLAN §1, §6.5, §8.5, GATE-M1 G12).
//!
//! An invitation is a secret: no subcommand takes it as an argument. `station
//! join` and `deck add-station` read it from a non-echoing prompt, from stdin,
//! or from `--invite-file` (G12).

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Fleet Deck v2: one deck drives agent sessions on many stations.
#[derive(Debug, Parser)]
#[command(name = "fleetdeck", version, about, propagate_version = true)]
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
    /// Print a one-time board login URL (read from an owner-only local file).
    Open,
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
    AddStation(InviteInput),
    /// List the paired stations.
    Stations,
    /// Unpin a station key and close its links.
    RemoveStation {
        /// Station name or key fingerprint.
        station: String,
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
    /// stdin, or `--invite-file`.
    Join(InviteInput),
    /// Unpin the deck key on this station and close its links.
    ForgetDeck,
    /// List the sessions of this station (works without the deck).
    Sessions,
    /// Start a session on this station (works without the deck).
    Run(RunArgs),
    /// Answer an open approval on this station (works without the deck).
    Approve(ApproveArgs),
    /// Interrupt the running turn of a session on this station.
    Interrupt {
        /// Session id.
        session: String,
    },
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
            &["station", "forget-deck"],
            &["station", "sessions"],
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
