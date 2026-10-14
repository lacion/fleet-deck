//! `fleetdeck station invite|join|forget-deck` (M2 P3-5c; PLAN §8.5).
//!
//! The running station does the pairing, so these commands are clients of
//! its local socket like `station run`. The invitation of `join` comes only
//! from a prompt, stdin or a 0600 file (G12); the one of `invite` goes to
//! stdout once, for the owner to paste into the board.

use std::net::SocketAddr;

use anyhow::bail;
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_station::local::{InvitationText, LocalRequest, LocalResponse};

use crate::cli::JoinArgs;
use crate::station::call;

/// `fleetdeck station invite --listen ADDR`: prints the invitation.
pub async fn invite(dirs: &NodeDirs, listen: SocketAddr) -> anyhow::Result<()> {
    let LocalResponse::Invitation { invitation } =
        call(dirs, &LocalRequest::Invite { listen }).await?
    else {
        bail!("unexpected answer from the station");
    };
    eprintln!(
        "single-use invitation, valid for 15 minutes; paste it into the board \
         (Add station) or give it to `fleetdeck deck add-station`:"
    );
    println!("{}", invitation.0);
    Ok(())
}

/// `fleetdeck station join [--stdin | --invite-file F]`, or `--tailnet`.
pub async fn join(
    dirs: &NodeDirs,
    env: &fleetdeck_core::paths::Env,
    args: &JoinArgs,
) -> anyhow::Result<()> {
    if args.tailnet.is_some() {
        return crate::tailnet_join::run(dirs, env, args).await;
    }
    let text = crate::invite::read(&args.input)?;
    let request = LocalRequest::Join {
        invitation: InvitationText(text.expose().clone()),
    };
    drop(text);
    let LocalResponse::Paired { deck } = call(dirs, &request).await? else {
        bail!("unexpected answer from the station");
    };
    println!("paired with deck {deck}; the station dials it now");
    Ok(())
}

/// `fleetdeck station forget-deck`.
pub async fn forget_deck(dirs: &NodeDirs) -> anyhow::Result<()> {
    let LocalResponse::Forgotten { count } = call(dirs, &LocalRequest::ForgetDeck).await? else {
        bail!("unexpected answer from the station");
    };
    match count {
        0 => println!("no deck was paired"),
        n => println!("forgot {n} deck key(s); their links are closed"),
    }
    Ok(())
}
