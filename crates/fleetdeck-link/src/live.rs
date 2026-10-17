//! A running link: the control channel and its data channel with one
//! lifetime (ADR-004 §2.4).
//!
//! - When the control channel ends (close, superseded, error), the data
//!   channel ends with the same reason, and its pending writes fail.
//! - When the data channel ends first, the control channel closes: with the
//!   violation's reason when this side found a mux violation, otherwise with
//!   `data_channel_lost` after up to [`DATA_LOSS_GRACE`] for the peer's
//!   `close` frame (the two connections can end in either order).
//! - Dropping the local data handles does not end the link.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::PeerKey;
use tokio::sync::{mpsc, watch};

use crate::control::{ChannelParams, Control, ControlWatch, Inbound, spawn};
use crate::data::{self, Data, DataEvent, DataParams};
use crate::hello::Side;
use crate::mux::reason as mux_reason;
use crate::node::{NodeConfig, OpenLink};
use crate::table::{Closing, LinkId};

/// How long the control side waits for the peer's `close` frame before it
/// names a lost data channel as the reason.
pub const DATA_LOSS_GRACE: Duration = Duration::from_millis(500);
/// The control close reason when the data channel ended first.
pub const DATA_CHANNEL_LOST: &str = "data_channel_lost";

/// The data channel of a live link.
#[derive(Debug)]
pub struct DataLink {
    pub handle: Data,
    pub events: mpsc::UnboundedReceiver<DataEvent>,
}

/// A link whose channels run (the result of [`start`]).
pub struct LiveLink {
    pub peer: PeerKey,
    pub id: LinkId,
    pub side: Side,
    /// The database incarnation that the peer's `hello` named (ADR-004
    /// §9.3): a station targets the deck's with `coordinator.*`.
    pub peer_incarnation: fleetdeck_proto::IncarnationId,
    /// The remote address of the control connection (see `OpenLink`).
    pub remote: Option<SocketAddr>,
    pub control: Control,
    pub inbound: mpsc::Receiver<Inbound>,
    /// Present when both sides negotiated `data_streams.v1`.
    pub data: Option<DataLink>,
}

impl fmt::Debug for LiveLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveLink")
            .field("peer", &self.peer.fingerprint().short())
            .field("id", &self.id)
            .field("side", &self.side)
            .field("control", &self.control)
            .field("data", &self.data.is_some())
            .finish_non_exhaustive()
    }
}

/// Starts the channels of an established link. The roles are the ones bound
/// at pairing: this node's role and the role of the pinned peer key (setup
/// admits only keys pinned with `node.peer_role()`).
#[must_use]
pub fn start(node: &NodeConfig, link: OpenLink) -> LiveLink {
    let params = ChannelParams {
        local: node.local.role,
        peer_role: node.peer_role(),
        peer: link.peer.fingerprint(),
        negotiated: link.negotiated,
        local_desktop_epoch: link.desktop_epochs.local,
        peer_desktop_epoch: link.desktop_epochs.peer,
        grants: Arc::clone(&node.grants),
        config: node.control,
    };
    let (control, inbound) = spawn(link.control, params, link.closing, Some(link.guard));
    let data = link.data.map(|ws| {
        let params = DataParams {
            side: link.side,
            attempt_id: link.id.attempt_id.clone(),
            tokens: node.tokens.clone(),
            write_limit: node.control.write_limit,
        };
        let (stop, stop_rx) = watch::channel(None);
        let (handle, events) = data::spawn(ws, params, stop_rx);
        tokio::spawn(tie(control.watcher(), handle.clone(), stop));
        DataLink { handle, events }
    });
    LiveLink {
        peer: link.peer,
        id: link.id,
        side: link.side,
        peer_incarnation: link.peer_incarnation,
        remote: link.remote,
        control,
        inbound,
        data,
    }
}

/// Holds the two channels to one lifetime. It keeps neither alive: it
/// holds no `Control` and the data task ignores dropped handles.
async fn tie(control: ControlWatch, data: Data, stop: watch::Sender<Option<Closing>>) {
    tokio::select! {
        c = control.closed() => {
            stop.send_replace(Some(c));
        }
        d = data.closed() => {
            if d.reason == mux_reason::FLOW_CONTROL_VIOLATION || d.reason == mux_reason::PROTOCOL_ERROR {
                control.close(&d.reason, &d.detail);
            } else if control.ended().is_none()
                && tokio::time::timeout(DATA_LOSS_GRACE, control.closed()).await.is_err()
            {
                control.close(DATA_CHANNEL_LOST, &format!("{}: {}", d.reason, d.detail));
            }
        }
    }
}
