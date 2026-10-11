//! `fleetdeck`: the one binary of Fleet Deck v2 (PLAN §1, D2).

mod cli;
mod invite;
mod pairing;
mod station;

use std::process::ExitCode;

use clap::Parser as _;

use crate::cli::{Cli, Command, DeckCommand, StationCommand};

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        // clap would print the unexpected argument: never echo an
        // invitation that was given as one (G12).
        Err(e) if std::env::args_os().any(|a| a.to_string_lossy().contains(invite::PREFIX)) => {
            drop(e);
            eprintln!(
                "fleetdeck: an invitation is never a command-line argument (the shell \
                 history and the process list would keep it); use --invite-file, --stdin \
                 or the prompt"
            );
            return ExitCode::from(2);
        }
        Err(e) => e.exit(),
    };
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("fleetdeck: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    fleetdeck_core::logging::init(cli.log_format)?;
    let env = fleetdeck_core::paths::Env::from_process();
    let dirs = fleetdeck_core::paths::NodeDirs::resolve(cli.data_dir.as_deref(), &env)?;
    tracing::debug!(config = %dirs.config.display(), data = %dirs.data.display(), "directories");
    if let Command::Station { command } = &cli.command
        && let Some(local) = station_local(command.as_ref())
    {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        return runtime.block_on(local_main(local, &dirs, &env));
    }
    // G12: read and check the invitation first, so its source rules hold
    // before the deck's pairing command exists (P3-6).
    if let Command::Deck {
        command: Some(DeckCommand::AddStation(input)),
    } = &cli.command
    {
        let _invitation = invite::read(input)?;
    }
    let (name, phase) = match &cli.command {
        Command::Up(_) => ("up", 4),
        Command::Open => ("open", 3),
        Command::Deck { command } => match command {
            None | Some(DeckCommand::Serve(_)) => ("deck serve", 3),
            Some(DeckCommand::Invite) => ("deck invite", 3),
            Some(DeckCommand::AddStation(_)) => ("deck add-station", 3),
            Some(DeckCommand::Stations) => ("deck stations", 3),
            Some(DeckCommand::RemoveStation { .. }) => ("deck remove-station", 3),
        },
        Command::Station { .. } => {
            anyhow::bail!("internal error: the station command was not dispatched")
        }
    };
    anyhow::bail!("`fleetdeck {name}` is not implemented yet (PLAN Phase {phase})")
}

/// The station commands (P2-7, P3-5c).
enum Local<'a> {
    Serve,
    Sessions { json: bool },
    Run(&'a cli::RunArgs),
    Approve(&'a cli::ApproveArgs),
    Interrupt(&'a str),
    Invite(std::net::SocketAddr),
    Join(&'a cli::InviteInput),
    ForgetDeck,
}

fn station_local(command: Option<&StationCommand>) -> Option<Local<'_>> {
    Some(match command {
        None | Some(StationCommand::Serve) => Local::Serve,
        Some(StationCommand::Sessions { json }) => Local::Sessions { json: *json },
        Some(StationCommand::Run(args)) => Local::Run(args),
        Some(StationCommand::Approve(args)) => Local::Approve(args),
        Some(StationCommand::Interrupt { session }) => Local::Interrupt(session),
        Some(StationCommand::Invite { listen }) => Local::Invite(*listen),
        Some(StationCommand::Join(input)) => Local::Join(input),
        Some(StationCommand::ForgetDeck) => Local::ForgetDeck,
    })
}

async fn local_main(
    local: Local<'_>,
    dirs: &fleetdeck_core::paths::NodeDirs,
    env: &fleetdeck_core::paths::Env,
) -> anyhow::Result<()> {
    match local {
        Local::Serve => station::serve(dirs, env).await,
        Local::Sessions { json } => station::sessions(dirs, json).await,
        Local::Run(args) => station::run(dirs, args).await,
        Local::Approve(args) => station::approve(dirs, args).await,
        Local::Interrupt(session) => station::interrupt(dirs, session).await,
        Local::Invite(listen) => pairing::invite(dirs, listen).await,
        Local::Join(input) => pairing::join(dirs, input).await,
        Local::ForgetDeck => pairing::forget_deck(dirs).await,
    }
}
