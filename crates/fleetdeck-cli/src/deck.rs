//! `fleetdeck deck serve|invite|add-station|stations|sessions|remove-station`
//! (M2 P3-6b; PLAN §8.5) and `deck command|commands` (P3-7a, G3).
//!
//! `serve` runs the deck; the other commands are clients of its local
//! socket `deck.sock`. The invitation of `add-station` comes only from a
//! prompt, stdin or a 0600 file (G12); the one of `invite` goes to stdout
//! once.

use anyhow::bail;
use fleetdeck_core::config::DeckConfig;
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_deck::link::CommandKind;
use fleetdeck_deck::local::{DeckClient, DeckRequest, DeckResponse, InvitationText, StationView};
use fleetdeck_deck::store::DeckCommand;
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::command::SessionCommand;

use crate::cli::{AddStationArgs, ServeArgs};
use crate::station::shutdown_signal;

/// `fleetdeck deck serve`: runs until SIGINT or SIGTERM.
pub async fn serve(dirs: &NodeDirs, args: &ServeArgs) -> anyhow::Result<()> {
    let config = DeckConfig::load(&dirs.deck_config())?;
    if let Some(listen) = args.listen {
        tracing::warn!(%listen, "the board listener comes with PLAN Phase 3 P3-9; ignored");
    }
    fleetdeck_deck::serve::serve(dirs, &config, BackoffPolicy::default(), shutdown_signal())
        .await?;
    Ok(())
}

async fn call(dirs: &NodeDirs, request: &DeckRequest) -> anyhow::Result<DeckResponse> {
    let path = fleetdeck_deck::local::socket_path(&dirs.deck());
    let mut client = DeckClient::connect(&path).await?;
    let response = client.call(request).await?;
    if let DeckResponse::Error { message } = &response {
        bail!("the deck refused the request: {message}");
    }
    Ok(response)
}

/// `fleetdeck deck invite`: prints the invitation.
pub async fn invite(dirs: &NodeDirs) -> anyhow::Result<()> {
    let DeckResponse::Invitation { invitation } = call(dirs, &DeckRequest::Invite).await? else {
        bail!("unexpected answer from the deck");
    };
    eprintln!(
        "single-use invitation, valid for 15 minutes; give it to \
         `fleetdeck station join` on the station:"
    );
    println!("{}", invitation.0);
    Ok(())
}

/// `fleetdeck deck add-station [--name N] [--stdin | --invite-file F]`.
pub async fn add_station(dirs: &NodeDirs, args: &AddStationArgs) -> anyhow::Result<()> {
    let text = crate::invite::read(&args.input)?;
    let request = DeckRequest::AddStation {
        invitation: InvitationText(text.expose().clone()),
        name: args.name.clone(),
    };
    drop(text);
    let DeckResponse::Paired { station } = call(dirs, &request).await? else {
        bail!("unexpected answer from the deck");
    };
    println!(
        "paired with station {} ({}); the deck dials it now",
        station.name, station.key
    );
    Ok(())
}

/// `fleetdeck deck stations [--json]`.
pub async fn stations(dirs: &NodeDirs, json: bool) -> anyhow::Result<()> {
    let DeckResponse::Stations { stations } = call(dirs, &DeckRequest::Stations).await? else {
        bail!("unexpected answer from the deck");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&stations)?);
    } else if stations.is_empty() {
        println!("no stations");
    } else {
        for st in &stations {
            println!("{}", render_station(st));
        }
    }
    Ok(())
}

fn render_station(st: &StationView) -> String {
    format!(
        "{}  {}  {}  {}  {}",
        st.name,
        st.key,
        st.direction,
        st.dial_url.as_deref().unwrap_or("-"),
        if st.reachable {
            "reachable"
        } else {
            "unreachable"
        }
    )
}

/// `fleetdeck deck sessions [--json]`.
pub async fn sessions(dirs: &NodeDirs, json: bool) -> anyhow::Result<()> {
    let DeckResponse::Sessions { sessions } = call(dirs, &DeckRequest::Sessions).await? else {
        bail!("unexpected answer from the deck");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&sessions)?);
    } else if sessions.is_empty() {
        println!("no sessions");
    } else {
        for s in &sessions {
            let status = serde_json::to_value(s.status)?;
            println!(
                "{}  {}  {}  {}",
                s.station_id,
                s.summary.session_id,
                status.as_str().unwrap_or("-"),
                s.summary.cwd.as_deref().unwrap_or("-"),
            );
        }
    }
    Ok(())
}

/// `fleetdeck deck remove-station <name>`.
pub async fn remove_station(dirs: &NodeDirs, station: &str) -> anyhow::Result<()> {
    let request = DeckRequest::RemoveStation {
        station: station.to_owned(),
    };
    let DeckResponse::Removed { station } = call(dirs, &request).await? else {
        bail!("unexpected answer from the deck");
    };
    println!(
        "removed station {} ({}); its links are closed",
        station.name, station.key
    );
    Ok(())
}

/// `fleetdeck deck command <station> <json> [--json]`.
pub async fn command(
    dirs: &NodeDirs,
    station: &str,
    json_text: &str,
    json: bool,
) -> anyhow::Result<()> {
    let command: SessionCommand = serde_json::from_str(json_text)
        .map_err(|e| anyhow::anyhow!("the command is not valid JSON of PLAN §6.1: {e}"))?;
    let request = DeckRequest::Command {
        station: station.to_owned(),
        command: CommandKind::Session { command },
    };
    let DeckResponse::Command { command } = call(dirs, &request).await? else {
        bail!("unexpected answer from the deck");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&command)?);
    } else {
        println!("{}", render_command(&command)?);
    }
    Ok(())
}

/// `fleetdeck deck commands [--json]`.
pub async fn commands(dirs: &NodeDirs, json: bool) -> anyhow::Result<()> {
    let DeckResponse::Commands { commands } = call(dirs, &DeckRequest::Commands).await? else {
        bail!("unexpected answer from the deck");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&commands)?);
    } else if commands.is_empty() {
        println!("no commands");
    } else {
        for c in &commands {
            println!("{}", render_command(c)?);
        }
    }
    Ok(())
}

fn render_command(c: &DeckCommand) -> anyhow::Result<String> {
    let state = serde_json::to_value(c.state)?;
    let detail = match (&c.receipt, &c.reason) {
        (_, Some(reason)) => reason.clone(),
        (Some(r), None) => match &r.reject {
            Some(reject) => format!("rejected: {}", reject.message),
            None => "accepted".to_owned(),
        },
        (None, None) => "-".to_owned(),
    };
    Ok(format!(
        "{}  {}  {}  {}  {}",
        c.command_id,
        c.station_id,
        c.method,
        state.as_str().unwrap_or("-"),
        detail
    ))
}
