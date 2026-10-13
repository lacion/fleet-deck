//! `fleetdeck up` (M2 P4-4; PLAN §1, Phase 4): the single-machine mode. One
//! process runs the deck (board with the embedded web client, link node,
//! `deck.sock`) and a local station (`station.sock`) from the same config
//! and data directories as `deck serve` and `station serve`.
//!
//! At the first start the station pairs with the deck over loopback: the
//! deck makes a single-use invitation in memory and the station joins it
//! (the invitation never leaves the process). Later starts find the pairing
//! and only reconnect. A station that is paired with another deck is an
//! error: `up` never replaces a pairing (DL-061).

use std::sync::{Arc, OnceLock};

use anyhow::{Context as _, bail};
use fleetdeck_core::config::{DeckConfig, StationConfig};
use fleetdeck_core::paths::{Env, NodeDirs};
use fleetdeck_deck::serve::{RunningDeck, StartOptions};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::StationId;
use fleetdeck_station::serve::RunningStation;

use crate::cli::ServeArgs;
use crate::restart::{RestartHandle, stop_or_restart};

/// What `up` did with the pairing of the local station.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pairing {
    /// The station was already paired with this deck.
    Kept,
    /// The station joined this deck now.
    Joined,
}

/// Pairs the local station with the deck if it has no deck yet.
///
/// # Errors
/// The station is paired with another deck, the deck has no link listener,
/// or the pairing fails.
pub async fn pair_local(deck: &RunningDeck, station: &RunningStation) -> anyhow::Result<Pairing> {
    let deck_key = deck.identity.fingerprint();
    match station.node.paired_deck().await? {
        Some(fp) if fp == deck_key => return Ok(Pairing::Kept),
        Some(fp) => bail!(
            "the local station is paired with another deck ({}); run `fleetdeck station \
             forget-deck` to pair it with this deck",
            fp.short()
        ),
        None => {}
    }
    let invitation = deck.node.invite().map_err(|e| match e {
        fleetdeck_deck::link::NodeError::NoListener => anyhow::anyhow!(
            "`fleetdeck up` pairs the local station over the deck's link listener; \
             set `link_listen` in deck.toml (the default is 127.0.0.1:7421)"
        ),
        other => anyhow::Error::new(other),
    })?;
    station
        .node
        .join(&invitation)
        .await
        .context("pairing the local station with the deck")?;
    Ok(Pairing::Joined)
}

/// Tells the deck which station is the local one, so that a restart of the
/// deck restarts that station (DL-172).
async fn register_local_station(
    deck: &RunningDeck,
    station: &RunningStation,
    local: &OnceLock<StationId>,
) -> anyhow::Result<()> {
    let record = deck
        .store
        .station_by_key(station.identity.public_key())
        .await?
        .context("the local station is not in the deck's registry")?;
    let _ = local.set(record.station_id);
    Ok(())
}

/// `fleetdeck up [--listen ADDR]`: runs until SIGINT or SIGTERM, or until a
/// restart is requested.
pub async fn up(
    dirs: &NodeDirs,
    env: &Env,
    args: &ServeArgs,
    restart: &RestartHandle,
) -> anyhow::Result<()> {
    let mut deck_config = DeckConfig::load(&dirs.deck_config())?;
    // The deck asks the local station to restart (one process), and the
    // station holds the same restart handle.
    let mut options = StartOptions::default();
    if let Some(listen) = args.listen {
        fleetdeck_core::netscope::check_bind(listen)
            .map_err(|e| anyhow::anyhow!("--listen: {e}"))?;
        deck_config.listen = vec![listen];
        options.overrides = vec![("listen".to_owned(), "--listen".to_owned())];
    }
    let local_station = Arc::clone(&options.local_station);
    let station_config = StationConfig::load_with_store(
        &dirs.station_config(),
        env,
        &dirs.station().join("secrets"),
    )?;
    let deck = RunningDeck::start(dirs, &deck_config, BackoffPolicy::default(), options).await?;
    let station =
        match RunningStation::start(dirs, &station_config, Some(Arc::new(restart.clone()))).await {
            Ok(s) => s,
            Err(e) => {
                deck.stop().await;
                return Err(e.into());
            }
        };
    let paired = match pair_local(&deck, &station).await {
        Ok(p) => p,
        Err(e) => {
            station.stop().await;
            deck.stop().await;
            return Err(e);
        }
    };
    if let Err(e) = register_local_station(&deck, &station, &local_station).await {
        station.stop().await;
        deck.stop().await;
        return Err(e);
    }
    let url = deck
        .endpoint
        .as_ref()
        .map_or("(no board listener)", |e| e.url.as_str());
    eprintln!(
        "fleetdeck up: deck `{}` and station `{}` are running ({}); board at {url}",
        deck_config.name,
        station_config.name,
        match paired {
            Pairing::Kept => "paired",
            Pairing::Joined => "paired now",
        },
    );
    eprintln!("run `fleetdeck open` in another terminal for a one-time login link");
    stop_or_restart(restart).await;
    station.stop().await;
    deck.stop().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use fleetdeck_testkit::TempNode;

    use super::*;

    async fn deck(node: &TempNode) -> RunningDeck {
        let config =
            DeckConfig::parse("listen = [\"127.0.0.1:0\"]\nlink_listen = [\"127.0.0.1:0\"]\n")
                .unwrap();
        RunningDeck::start(
            node.dirs(),
            &config,
            BackoffPolicy::default(),
            StartOptions::default(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn the_local_station_joins_once_and_never_leaves_another_deck() {
        let home = TempNode::new("home").unwrap();
        let other = TempNode::new("other").unwrap();
        home.write_station_toml("name = \"home\"\nallowed_roots = [\"~/projects\"]\n")
            .unwrap();
        let station = RunningStation::start(home.dirs(), &home.station_config().unwrap(), None)
            .await
            .unwrap();
        let first = deck(&home).await;
        assert_eq!(pair_local(&first, &station).await.unwrap(), Pairing::Joined);
        assert_eq!(pair_local(&first, &station).await.unwrap(), Pairing::Kept);
        let second = deck(&other).await;
        let err = pair_local(&second, &station).await.unwrap_err().to_string();
        assert!(err.contains("paired with another deck"), "{err}");
        assert!(err.contains("fleetdeck station forget-deck"), "{err}");
        station.stop().await;
        first.stop().await;
        second.stop().await;
    }
}
