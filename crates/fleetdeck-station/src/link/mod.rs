//! The station side of a deck link (M2 P3-5; ADR-004 §9.3–§9.5).
//!
//! [`serve`] runs one live link of a paired deck until it ends:
//!
//! - `events.replay` and `session.snapshot` (re)start the event stream
//!   ([`stream`], G4);
//! - an `ack` is checked against the station's incarnation and the highest
//!   `station_seq` delivered on this link (else `protocol_error` closes the
//!   link) and stored for the deck's key;
//! - every other request is answered by [`intake`] in its own task, so a
//!   slow decision never delays acks or the stream (G3, G5).

mod intake;
pub mod peers;
pub mod ready;
pub mod runtime;
pub mod stream;

use std::sync::Arc;

use fleetdeck_core::identity::PeerKey;
use fleetdeck_link::control::{Control, Inbound, InboundRequest};
use fleetdeck_link::methods::code;
use fleetdeck_link::node::NodeConfig;
use fleetdeck_link::revoke::REVOKED;
use fleetdeck_link::{Closing, LiveLink};
use fleetdeck_proto::link::{ControlFrame, Outcome, ReplayParams, WireError};

pub use intake::NOT_IMPLEMENTED;
pub use peers::StationPeers;
pub use runtime::{NodeError, StationNode, TailnetJoin, TailnetResult};
use stream::{Start, Stream};

use crate::manager::SessionManager;
use crate::store::StoreError;
use crate::store::peers::key_hex;

/// What every link of this station shares.
#[derive(Debug)]
pub struct StationLink {
    pub manager: Arc<SessionManager>,
    pub node: NodeConfig,
    pub peers: Arc<StationPeers>,
    /// The station name of `station.toml`.
    pub name: String,
    /// The human shells (PLAN §6.8); they outlive every link.
    pub shells: Arc<crate::shell::Shells>,
    /// The deck side of the coordinator endpoint (M3 6-5): it follows the
    /// current deck link.
    pub coordinator: crate::coordinator::LinkForward,
    /// The directory of `files.write_temp` (DL-147); `None` answers
    /// `unsupported`.
    pub paste_dir: Option<std::path::PathBuf>,
    /// The Fleet Git runtime: traits, config digest, deck control and the
    /// Git options of a clone (FG-5, FG-6, FG-11).
    pub fleet_git: Arc<crate::fleet_git_runtime::FleetGitRuntime>,
    /// The last fleet summary of the deck and the live link count (R13 P4).
    pub summary: Arc<crate::fleet_summary::SummaryCell>,
    /// The connection-local readiness of the live links (R13 P8).
    pub ready: Arc<ready::LinkReady>,
}

/// Holds the deck control of a link in the Fleet Git runtime for as long as
/// the link serves, and clears it with its token (a newer link stays).
struct DeckSlot<'a> {
    runtime: &'a crate::fleet_git_runtime::FleetGitRuntime,
    token: Option<u64>,
}

impl<'a> DeckSlot<'a> {
    /// `homes`: the link negotiated `fleet_git.homes.v1`.
    fn new(
        runtime: &'a crate::fleet_git_runtime::FleetGitRuntime,
        control: &Control,
        homes: bool,
    ) -> Self {
        Self {
            runtime,
            token: homes.then(|| runtime.set_deck(control.clone())),
        }
    }
}

/// Holds the deck control of a link in the summary cell for as long as the
/// link serves, when the link negotiated `desktop_shell.v1` (R13 P4 task E).
struct ShellSlot<'a> {
    cell: &'a crate::fleet_summary::SummaryCell,
    token: Option<u64>,
}

impl<'a> ShellSlot<'a> {
    fn new(cell: &'a crate::fleet_summary::SummaryCell, control: &Control) -> Self {
        let shell = control
            .params()
            .negotiated
            .has(fleetdeck_proto::version::feature::DESKTOP_SHELL);
        Self {
            cell,
            token: shell.then(|| cell.set_deck(control.clone())),
        }
    }
}

impl ShellSlot<'_> {
    /// True when the link negotiated `desktop_shell.v1`.
    fn active(&self) -> bool {
        self.token.is_some()
    }
}

impl Drop for ShellSlot<'_> {
    fn drop(&mut self) {
        if let Some(token) = self.token {
            self.cell.clear_deck(token);
        }
    }
}

impl Drop for DeckSlot<'_> {
    fn drop(&mut self) {
        if let Some(token) = self.token {
            self.runtime.clear_deck(token);
        }
    }
}

/// Serves `link` until it ends; returns why it ended.
pub async fn serve(ctx: Arc<StationLink>, link: LiveLink) -> Closing {
    let LiveLink {
        peer,
        control,
        mut inbound,
        data,
        peer_incarnation,
        ..
    } = link;
    let coordinator = ctx.coordinator.connect(control.clone(), peer_incarnation);
    let data = data.map(|d| {
        let handle = d.handle.clone();
        tokio::spawn(crate::shell::data::serve(
            Arc::clone(&ctx.shells),
            Arc::clone(&ctx.manager),
            d.handle,
            d.events,
        ));
        handle
    });
    let mut stream = Stream::new(ctx.manager.store().clone(), control.clone());
    // A deck without `fleet_git.v1` cannot show or stop Fleet Git work: the
    // loops pause while this link is up (design §3.3).
    let _pause = intake::PauseGuard::new(
        ctx.manager.store().fleet_git_paused(),
        control
            .params()
            .negotiated
            .has(fleetdeck_proto::version::feature::FLEET_GIT),
    );
    // The artifact authorization of a later phase asks the deck over this
    // control (FG-12); it needs `fleet_git.homes.v1`.
    let _summary_up = ctx.summary.link_up();
    // `display.directive` of the local wall op goes over this control when
    // the link negotiated `desktop_shell.v1` (R13 P4 task E).
    let shell = ShellSlot::new(&ctx.summary, &control);
    // The stored palette of `theme-sync` goes to the deck once at each link
    // up (R13 P4 task F1).
    if shell.active() {
        crate::local_palette::resend(&ctx.summary, &control);
    }
    // `station fingerprint` reports `link_up` from this (R13 P8): ready after
    // the deck acknowledged the first snapshot or the stream really replays
    // after an accepted `events.replay` on this connection.
    let ready = ctx.ready.open(peer.fingerprint());
    stream.set_ready(ready.handle());
    let _deck = DeckSlot::new(
        &ctx.fleet_git,
        &control,
        control
            .params()
            .negotiated
            .has(fleetdeck_proto::version::feature::FLEET_GIT_HOMES),
    );
    if control
        .params()
        .negotiated
        .has(fleetdeck_proto::version::feature::COORDINATOR)
    {
        stream = stream.with_coordinator(ctx.manager.coordinator_bindings().clone());
    }
    while let Some(message) = inbound.recv().await {
        match message {
            Inbound::Request(req) => {
                request(
                    &ctx,
                    &control,
                    &peer,
                    &mut stream,
                    data.as_ref(),
                    coordinator,
                    req,
                )
                .await;
            }
            Inbound::Frame(frame) => match *frame {
                ControlFrame::Ack {
                    incarnation,
                    station_seq,
                } => {
                    if ack(&ctx, &control, &peer, &stream, &incarnation, station_seq).await {
                        ctx.coordinator.acknowledged(coordinator, station_seq);
                        ready.acked(station_seq, stream.first_snapshot());
                    }
                }
                // The control channel lets only `ack` reach a station.
                other => control.close(
                    code::PROTOCOL_ERROR,
                    &format!("a station does not take `{}` frames", other.type_name()),
                ),
            },
        }
    }
    drop(stream);
    ctx.coordinator.disconnect(coordinator);
    control.closed().await
}

async fn request(
    ctx: &Arc<StationLink>,
    control: &Control,
    peer: &PeerKey,
    stream: &mut Stream,
    data: Option<&fleetdeck_link::data::Data>,
    coordinator: u64,
    req: InboundRequest,
) {
    match req.method.as_str() {
        "shell.attach" => {
            let reply = intake::shell_attach(ctx, data, req.params);
            if let Err(e) = control.respond(req.id, reply).await {
                tracing::debug!(error = %e, "response");
            }
        }
        // BG-1: the token of a maintenance stream needs the data channel.
        "maintenance.attach" => {
            let reply = intake::maintenance_attach(data, req.params);
            if let Err(e) = control.respond(req.id, reply).await {
                tracing::debug!(error = %e, "response");
            }
        }
        // FG-9: the export token needs the data channel of this link.
        "session.move_export" => {
            let reply = intake::move_export(ctx, data, req.params).await;
            if let Err(e) = control.respond(req.id, reply).await {
                tracing::debug!(error = %e, "response");
            }
        }
        // FG-9: the upload token needs the data channel of this link.
        "session.move_prepare" => {
            let reply = intake::move_prepare(ctx, data, req.params).await;
            if let Err(e) = control.respond(req.id, reply).await {
                tracing::debug!(error = %e, "response");
            }
        }
        "events.replay" => match serde_json::from_value::<ReplayParams>(req.params) {
            Ok(p) => {
                // The deck commits its cursor with the events, so the cursor
                // acknowledges the events through `after`. A replay that
                // sends nothing still lifts the lifecycle fence.
                let store = ctx.manager.store();
                let acknowledged = p.incarnation.as_ref() == Some(store.incarnation())
                    && p.after <= store.head()
                    && store.ack(key_hex(peer), p.after).await.is_ok();
                if acknowledged {
                    ctx.coordinator.acknowledged(coordinator, p.after);
                }
                // The link turns ready in the stream, only if it replays.
                stream.restart(
                    Start::Replay {
                        incarnation: p.incarnation,
                        after: p.after,
                        acknowledged,
                    },
                    req.id,
                );
            }
            Err(e) => {
                let err = WireError::new(code::PROTOCOL_ERROR, format!("params: {e}"));
                if let Err(e) = control.respond(req.id, Outcome::Err(err)).await {
                    tracing::debug!(error = %e, "response");
                }
            }
        },
        "session.snapshot" => stream.restart(Start::Snapshot, req.id),
        _ => {
            let (ctx, control, peer) = (Arc::clone(ctx), control.clone(), *peer);
            tokio::spawn(async move { intake::handle(&ctx, &control, &peer, req).await });
        }
    }
}

async fn ack(
    ctx: &StationLink,
    control: &Control,
    peer: &PeerKey,
    stream: &Stream,
    incarnation: &fleetdeck_proto::IncarnationId,
    station_seq: u64,
) -> bool {
    let store = ctx.manager.store();
    if incarnation != store.incarnation() {
        control.close(
            code::PROTOCOL_ERROR,
            &format!(
                "ack for incarnation {incarnation}, the station is {}",
                store.incarnation()
            ),
        );
        return false;
    }
    let delivered = stream.delivered();
    if station_seq > delivered {
        control.close(
            code::PROTOCOL_ERROR,
            &format!("ack of {station_seq}, but only {delivered} was delivered on this link"),
        );
        return false;
    }
    match store.ack(key_hex(peer), station_seq).await {
        Ok(()) => true,
        // Revoked while the link was open.
        Err(StoreError::UnknownDeck(_)) => {
            control.close(REVOKED, "the deck is not paired");
            false
        }
        Err(e) => {
            tracing::warn!(error = %e, "deck ack");
            false
        }
    }
}
