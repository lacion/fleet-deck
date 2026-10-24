//! The wall control side of the board WebSocket (B17 §3.1.4): a display
//! socket that declared `wall_control.v1` gets a `sync` frame first and then
//! every frame of the relay of its display. A display socket without the
//! feature is only counted (the hub answers `older` for it). Owner and member
//! sockets never attach and never get a frame.

use fleetdeck_proto::board::BoardServerMessage;
use fleetdeck_proto::wall_control::WallControlFrame;
use tokio::sync::broadcast::{Receiver, error::RecvError};

use super::{Session, WsError};
use crate::coordinator_desktop::effective_directive;
use crate::link::DeckLink;
use crate::store::Principal;
use crate::wall_control::SocketGuard;

/// What a display socket holds for the relay: the guard that counts it and,
/// when it declared the feature, the receiver of its display.
pub(super) struct WallFeed {
    display_id: String,
    rx: Option<Receiver<WallControlFrame>>,
    _guard: SocketGuard,
}

/// The next frame of the feed. Never ready for a socket without a feed or
/// without the feature.
pub(super) async fn next_frame(feed: &mut Option<WallFeed>) -> Result<WallControlFrame, RecvError> {
    match feed.as_mut().and_then(|f| f.rx.as_mut()) {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// The generation that the display shows now. `None`: the display does not
/// exist or is revoked. A free function: the socket inside a session is not
/// `Sync`, so no `&Session` may live across an await.
async fn generation(link: &DeckLink, display_id: &str) -> Result<Option<u64>, WsError> {
    Ok(effective_directive(link, display_id)
        .await?
        .map(|view| view.generation))
}

impl Session {
    /// Attaches a display socket to the relay. `declared`: the socket
    /// declared `wall_control.v1`; it then gets the `sync` frame at once.
    /// Any other principal gets no feed. `None` for a display that is gone.
    pub(super) async fn wall_attach(
        &mut self,
        declared: bool,
    ) -> Result<Option<WallFeed>, WsError> {
        let Principal::Display(login) = &self.principal else {
            return Ok(None);
        };
        let display_id = login.display_id.clone();
        let hub = std::sync::Arc::clone(&self.board.wall_controls);
        if !declared {
            let guard = hub.attach_older(&display_id);
            return Ok(Some(WallFeed {
                display_id,
                rx: None,
                _guard: guard,
            }));
        }
        let link = std::sync::Arc::clone(&self.board.link);
        let Some(generation) = generation(&link, &display_id).await? else {
            return Ok(None);
        };
        let (rx, sync, guard) = hub.attach_ready(&display_id, generation);
        self.send(&BoardServerMessage::WallControl { frame: sync })
            .await?;
        Ok(Some(WallFeed {
            display_id,
            rx: Some(rx),
            _guard: guard,
        }))
    }

    /// Sends one frame of the relay. After a lag the socket gets the `sync`
    /// frame of the effective generation instead (the selection and the
    /// frame position are exact again; a lost relative scroll is lost).
    /// Returns false when the relay ended (the display was revoked).
    pub(super) async fn wall_forward(
        &mut self,
        feed: Option<&WallFeed>,
        frame: Result<WallControlFrame, RecvError>,
    ) -> Result<bool, WsError> {
        let frame = match frame {
            Ok(frame) => frame,
            Err(RecvError::Closed) => return Ok(false),
            Err(RecvError::Lagged(_)) => {
                let Some(feed) = feed else {
                    return Ok(false);
                };
                let link = std::sync::Arc::clone(&self.board.link);
                let Some(generation) = generation(&link, &feed.display_id).await? else {
                    return Ok(false);
                };
                self.board.wall_controls.sync(&feed.display_id, generation)
            }
        };
        self.send(&BoardServerMessage::WallControl { frame })
            .await?;
        Ok(true)
    }
}
