//! `fleetdeck`: the one binary of Fleet Deck v2 (PLAN §1, D2).

mod cli;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command, DeckCommand, StationCommand};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("fleetdeck: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
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
        Command::Station { command } => match command {
            None | Some(StationCommand::Serve) => ("station serve", 2),
            Some(StationCommand::Invite { .. }) => ("station invite", 3),
            Some(StationCommand::Join(_)) => ("station join", 3),
            Some(StationCommand::ForgetDeck) => ("station forget-deck", 3),
            Some(StationCommand::Sessions) => ("station sessions", 2),
            Some(StationCommand::Run(_)) => ("station run", 2),
            Some(StationCommand::Approve(_)) => ("station approve", 2),
            Some(StationCommand::Interrupt { .. }) => ("station interrupt", 2),
        },
    };
    anyhow::bail!("`fleetdeck {name}` is not implemented yet (PLAN Phase {phase})")
}
