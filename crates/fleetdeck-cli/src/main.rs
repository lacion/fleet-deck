//! `fleetdeck`: the one binary of Fleet Deck v2 (PLAN §1, D2).

mod auth;
mod cli;
mod cli_desktop;
mod cli_omarchy;
mod confine;
mod deck;
mod deck_provision;
mod desktop_crash;
mod desktop_feed;
mod desktop_open;
mod desktop_wall;
mod doctor;
mod doctor_omarchy;
mod invite;
mod notify;
mod omarchy;
mod pairing;
mod provision;
mod provision_import;
mod restart;
mod service;
mod settings;
mod station;
mod station_fingerprint;
mod tailnet;
mod tailnet_join;
mod up;
mod upgrade;

use std::process::ExitCode;

use clap::Parser as _;

use crate::cli::{Cli, Command, DeckCommand, StationCommand};

fn main() -> ExitCode {
    // The restart executes the process again with this context (DL-172).
    let startup = restart::StartupContext::capture();
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
    // The shim and its provider helper run with a cleared environment:
    // they need no data directory and no logging set-up (ADR-005 §2).
    match &cli.command {
        Command::Shim { id } => return exit_code(fleetdeck_providers::host::shim::server::run(id)),
        Command::ShimExec {
            parent,
            fsize,
            stdin_fd3,
            program,
        } => {
            let Some((program, args)) = program.split_first() else {
                return ExitCode::from(2);
            };
            let options = fleetdeck_providers::host::shim::server::ExecOptions {
                fsize: *fsize,
                stdin_fd3: *stdin_fd3,
            };
            return exit_code(fleetdeck_providers::host::shim::server::exec_provider_with(
                *parent, options, program, args,
            ));
        }
        Command::ConfineExec {
            write_roots,
            program,
        } => return exit_code(confine::run(write_roots, program)),
        // Stdout is the MCP channel: no logging set-up, no data directory.
        Command::McpBridge { socket } => {
            std::process::exit(fleetdeck_station::coordinator::bridge::run(socket));
        }
        // It needs no data directory and no logging either.
        Command::BuildInfo => {
            let text = |value: &str| serde_json::Value::from(value).to_string();
            println!(
                r#"{{"version":{},"commit":{},"commit_time":{},"release":{}}}"#,
                text(fleetdeck_core::build::version()),
                text(fleetdeck_core::build::COMMIT),
                fleetdeck_core::build::COMMIT_TIME,
                fleetdeck_core::build::RELEASE
            );
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let handle = restart::RestartHandle::default();
    match run(cli, &handle) {
        // The runtime is dropped and the drain is done: execute again.
        Ok(code) if handle.requested() && code == ExitCode::SUCCESS => {
            restart::restart_failed(&startup.exec())
        }
        Ok(code) => code,
        // A refusal of `station join --tailnet` prints `<code>: <detail>`.
        Err(err) => {
            match err.downcast_ref::<tailnet_join::Refused>() {
                Some(refused) => eprintln!("{refused}"),
                None => eprintln!("fleetdeck: {err:#}"),
            }
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli, restart: &restart::RestartHandle) -> anyhow::Result<ExitCode> {
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
        return runtime
            .block_on(local_main(local, &dirs, &env, restart))
            .map(|()| ExitCode::SUCCESS);
    }
    let runtime = || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
    };
    let done = |result: anyhow::Result<()>| result.map(|()| ExitCode::SUCCESS);
    match &cli.command {
        Command::Deck { command } => {
            done(runtime()?.block_on(deck_main(command.as_ref(), &dirs, restart)))
        }
        Command::Up(args) => done(runtime()?.block_on(up::up(&dirs, &env, args, restart))),
        Command::Open { elevate: false } => done(runtime()?.block_on(auth::open(&dirs))),
        Command::Open { elevate: true } => done(runtime()?.block_on(auth::elevate(&dirs))),
        Command::Desktop(command) => done(runtime()?.block_on(desktop_main(command, &dirs))),
        Command::Doctor { brief } => Ok(runtime()?.block_on(doctor::run(&dirs, &env, *brief))),
        Command::Omarchy(command) => runtime()?.block_on(omarchy::run(&dirs, command)),
        Command::Settings { command } => done(runtime()?.block_on(settings::run(&dirs, command))),
        Command::Tailnet { command } => {
            done(runtime()?.block_on(tailnet::run(&dirs, &env, command)))
        }
        Command::Service { command } => done(runtime()?.block_on(async {
            let ctx = service::Context::detect(&dirs, &env, cli.data_dir.is_some())?;
            let command = command.clone();
            tokio::task::spawn_blocking(move || {
                service::run(&command, &ctx, &service::System, &mut std::io::stdout())
            })
            .await?
        })),
        Command::ProvisionBootstrap {
            file,
            retry,
            retry_authorization,
        } => {
            let ctx = service::Context::detect(&dirs, &env, cli.data_dir.is_some())?;
            let effects = provision::real::RealEffects::new(dirs.clone(), env.clone(), ctx);
            let exit = runtime()?.block_on(provision::run::run(
                &effects,
                file,
                *retry,
                retry_authorization.as_deref(),
            ));
            Ok(ExitCode::from(exit.code()))
        }
        Command::ProvisionSupervise { file } => {
            let ctx = service::Context::detect(&dirs, &env, cli.data_dir.is_some())?;
            let effects = provision::real::RealEffects::new(dirs.clone(), env.clone(), ctx);
            let exit = runtime()?.block_on(provision::supervise::supervise(&effects, file));
            Ok(ExitCode::from(exit.code()))
        }
        Command::ProvisionImport { source } => {
            provision_import::run(source, &env).map(|()| ExitCode::SUCCESS)
        }
        Command::Station { .. }
        | Command::Shim { .. }
        | Command::ShimExec { .. }
        | Command::ConfineExec { .. }
        | Command::McpBridge { .. }
        | Command::BuildInfo => {
            anyhow::bail!("internal error: the command was not dispatched")
        }
    }
}

fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// The desktop commands (R13 P4 task B2).
async fn desktop_main(
    command: &cli_desktop::DesktopCommand,
    dirs: &fleetdeck_core::paths::NodeDirs,
) -> anyhow::Result<()> {
    match command {
        cli_desktop::DesktopCommand::Feed { notify } => desktop_feed::run(dirs, *notify).await,
        cli_desktop::DesktopCommand::Open { target, request } => {
            desktop_open::run(dirs, target.as_deref(), request.as_deref()).await
        }
        cli_desktop::DesktopCommand::Crash { last: _ } => desktop_crash::run(dirs).await,
        cli_desktop::DesktopCommand::ScreensaverRun => {
            println!("The screensaver now runs in the station service.");
            Ok(())
        }
        cli_desktop::DesktopCommand::Wall { command } => match command {
            cli_desktop::WallCommand::Next => {
                desktop_wall::directive(dirs, desktop_wall::NEXT_NEEDS_YOU).await
            }
            cli_desktop::WallCommand::Mode {
                command: cli_desktop::WallModeCommand::Next,
            } => desktop_wall::directive(dirs, desktop_wall::NEXT_MODE).await,
            cli_desktop::WallCommand::Show => desktop_wall::show().await,
        },
    }
}

/// The deck commands (P3-6b).
async fn deck_main(
    command: Option<&DeckCommand>,
    dirs: &fleetdeck_core::paths::NodeDirs,
    restart: &restart::RestartHandle,
) -> anyhow::Result<()> {
    match command {
        None => deck::serve(dirs, &cli::ServeArgs { listen: None }, restart).await,
        Some(DeckCommand::Serve(args)) => deck::serve(dirs, args, restart).await,
        Some(DeckCommand::Invite) => deck::invite(dirs).await,
        Some(DeckCommand::AddStation(args)) => deck::add_station(dirs, args).await,
        Some(DeckCommand::Stations { json }) => deck::stations(dirs, *json).await,
        Some(DeckCommand::Sessions { json }) => deck::sessions(dirs, *json).await,
        Some(DeckCommand::RemoveStation { station }) => deck::remove_station(dirs, station).await,
        Some(DeckCommand::Command {
            station,
            command,
            json,
        }) => deck::command(dirs, station, command, *json).await,
        Some(DeckCommand::Commands { json }) => deck::commands(dirs, *json).await,
        Some(DeckCommand::Password { stdin }) => auth::password(dirs, *stdin).await,
        Some(DeckCommand::Token { command }) => auth::token(dirs, command).await,
        Some(DeckCommand::Audit { limit, json }) => auth::audit(dirs, *limit, *json).await,
        Some(DeckCommand::ProvisionFile(args)) => deck_provision::run(dirs, args).await,
    }
}

/// The station commands (P2-7, P3-5c).
enum Local<'a> {
    Serve,
    Sessions { json: bool },
    Run(&'a cli::RunArgs),
    Approve(&'a cli::ApproveArgs),
    Interrupt(&'a str),
    Drain(u64),
    Upgrade(&'a cli::UpgradeCommand),
    Invite(std::net::SocketAddr),
    Join(&'a cli::JoinArgs),
    ForgetDeck,
    Fingerprint { json: bool, probe_providers: bool },
}

fn station_local(command: Option<&StationCommand>) -> Option<Local<'_>> {
    Some(match command {
        None | Some(StationCommand::Serve) => Local::Serve,
        Some(StationCommand::Sessions { json }) => Local::Sessions { json: *json },
        Some(StationCommand::Run(args)) => Local::Run(args),
        Some(StationCommand::Approve(args)) => Local::Approve(args),
        Some(StationCommand::Interrupt { session }) => Local::Interrupt(session),
        Some(StationCommand::Drain { timeout_secs }) => Local::Drain(*timeout_secs),
        Some(StationCommand::Upgrade { command }) => Local::Upgrade(command),
        Some(StationCommand::Invite { listen }) => Local::Invite(*listen),
        Some(StationCommand::Join(input)) => Local::Join(input),
        Some(StationCommand::ForgetDeck) => Local::ForgetDeck,
        Some(StationCommand::Fingerprint {
            json,
            probe_providers,
        }) => Local::Fingerprint {
            json: *json,
            probe_providers: *probe_providers,
        },
    })
}

async fn local_main(
    local: Local<'_>,
    dirs: &fleetdeck_core::paths::NodeDirs,
    env: &fleetdeck_core::paths::Env,
    restart: &restart::RestartHandle,
) -> anyhow::Result<()> {
    match local {
        Local::Serve => station::serve(dirs, env, restart).await,
        Local::Sessions { json } => station::sessions(dirs, json).await,
        Local::Run(args) => station::run(dirs, args).await,
        Local::Approve(args) => station::approve(dirs, args).await,
        Local::Interrupt(session) => station::interrupt(dirs, session).await,
        Local::Drain(secs) => upgrade::drain(dirs, secs).await,
        Local::Upgrade(command) => upgrade::run(dirs, command).await,
        Local::Invite(listen) => pairing::invite(dirs, listen).await,
        Local::Join(args) => pairing::join(dirs, env, args).await,
        Local::ForgetDeck => pairing::forget_deck(dirs).await,
        Local::Fingerprint {
            json,
            probe_providers,
        } => station_fingerprint::run(dirs, json, probe_providers).await,
    }
}
