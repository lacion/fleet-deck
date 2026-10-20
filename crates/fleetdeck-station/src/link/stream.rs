//! The event stream of one deck link (ADR-004 §9.4, G4).
//!
//! `events.replay { incarnation, after }` and `session.snapshot` (re)start
//! the stream. The pump answers the request, then sends either the events
//! after the deck's cursor or a snapshot batch (`snapshot_begin`,
//! `snapshot_session` × n, `snapshot_end`), then every committed event as it
//! arrives. Every frame carries the station's incarnation. A snapshot is
//! sent instead of a replay when the deck names another incarnation, when
//! its cursor is older than the retained history or ahead of the head, and
//! for `session.snapshot`.
//!
//! A link without `artifacts_video.v1` gets each `artifact.published` event
//! with kind `video` as `other` (R13 P5b, design `omarchy-p5b.md` §14,
//! O12-15). The projection happens at send time on the copy that the pump
//! holds; the stored event never changes.
//!
//! [`Stream::delivered`] is the highest `station_seq` sent on this link (an
//! event or a snapshot watermark). An `ack` above it is a protocol error.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use fleetdeck_link::control::Control;
use fleetdeck_link::methods::code;
use fleetdeck_proto::artifacts::ArtifactKind;
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::link::{
    ControlFrame, Outcome, SessionSnapshot, StreamMode, StreamStart, WireError,
};
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{EventEnvelope, IncarnationId};
use tokio::task::JoinHandle;

use super::ready::ReadyHandle;
use crate::store::{ReadResult, Store, StoreError};

/// Events read from the log per step.
pub const BATCH: usize = 256;

/// Where a stream starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// The deck's read model stands at `after` of `incarnation`.
    Replay {
        incarnation: Option<IncarnationId>,
        after: u64,
        /// The station accepted the cursor as an acknowledgement. The link
        /// is ready only if the stream then really replays (R13 P8).
        acknowledged: bool,
    },
    /// The deck asked for a snapshot.
    Snapshot,
}

/// The legacy shape of an event for a link without `artifacts_video.v1`: the
/// kind `video` of an `artifact.published` event becomes `other`.
fn project_legacy_event(envelope: &mut EventEnvelope) {
    if let EventBody::Known(Event::ArtifactPublished { artifact_kind, .. }) = &mut envelope.event
        && *artifact_kind == ArtifactKind::Video
    {
        *artifact_kind = ArtifactKind::Other;
    }
}

/// The stream pump of one link. Dropping it stops the pump.
#[derive(Debug)]
pub struct Stream {
    store: Store,
    control: Control,
    delivered: Arc<AtomicU64>,
    /// The watermark of the first snapshot sent on this link (R13 P8).
    first_snapshot: Arc<OnceLock<u64>>,
    /// The readiness of this link, marked by a replay that is chosen.
    ready: Option<ReadyHandle>,
    task: Option<JoinHandle<()>>,
    /// Set on a link with `coordinator.v1`: snapshots carry the binding
    /// projection (ADR-006 §6.1).
    coordinator: Option<crate::coordinator::Bindings>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Stream {
    #[must_use]
    pub fn new(store: Store, control: Control) -> Self {
        Self {
            store,
            control,
            delivered: Arc::new(AtomicU64::new(0)),
            first_snapshot: Arc::new(OnceLock::new()),
            ready: None,
            task: None,
            coordinator: None,
        }
    }

    /// Snapshots of this stream carry the coordinator binding projection
    /// of `bindings` (ADR-006 §6.1, test 25e).
    #[must_use]
    pub fn with_coordinator(mut self, bindings: crate::coordinator::Bindings) -> Self {
        self.coordinator = Some(bindings);
        self
    }

    /// Gives the stream the readiness handle of this link.
    pub fn set_ready(&mut self, ready: ReadyHandle) {
        self.ready = Some(ready);
    }

    /// The highest `station_seq` sent on this link.
    #[must_use]
    pub fn delivered(&self) -> u64 {
        self.delivered.load(Ordering::Acquire)
    }

    /// The watermark of the first snapshot that went out on this link; set
    /// before its first frame is sent. `None` until a snapshot starts.
    #[must_use]
    pub fn first_snapshot(&self) -> Option<u64> {
        self.first_snapshot.get().copied()
    }

    /// Stops the current pump (if any) and starts a new one that first
    /// answers the request `reply` with a [`StreamStart`].
    pub fn restart(&mut self, start: Start, reply: u64) {
        if let Some(old) = self.task.take() {
            old.abort();
        }
        let pump = Pump {
            store: self.store.clone(),
            control: self.control.clone(),
            delivered: Arc::clone(&self.delivered),
            first_snapshot: Arc::clone(&self.first_snapshot),
            ready: self.ready.clone(),
            coordinator: self.coordinator.clone(),
        };
        self.task = Some(tokio::spawn(async move {
            if let Err(e) = pump.run(start, reply).await {
                tracing::debug!(error = %e, "event stream ended");
            }
        }));
    }
}

struct Pump {
    store: Store,
    control: Control,
    delivered: Arc<AtomicU64>,
    first_snapshot: Arc<OnceLock<u64>>,
    ready: Option<ReadyHandle>,
    coordinator: Option<crate::coordinator::Bindings>,
}

#[derive(Debug, thiserror::Error)]
enum PumpError {
    #[error(transparent)]
    Link(#[from] fleetdeck_link::LinkError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl Pump {
    fn incarnation(&self) -> IncarnationId {
        self.store.incarnation().clone()
    }

    async fn run(self, start: Start, reply: u64) -> Result<(), PumpError> {
        // Subscribe before the first read, so no commit is missed.
        let mut head = self.store.subscribe();
        head.borrow_and_update();
        let acknowledged = matches!(
            start,
            Start::Replay {
                acknowledged: true,
                ..
            }
        );
        let first = match self.first_batch(&start).await {
            Ok(first) => first,
            Err(e) => {
                let err = WireError::new("internal", format!("the event log failed: {e}"));
                self.control.respond(reply, Outcome::Err(err)).await?;
                return Err(e.into());
            }
        };
        let mut cursor = match first {
            First::Replay { after, events } => {
                self.answer(reply, StreamMode::Replay, after).await?;
                // The replay is chosen and reported: an accepted cursor
                // makes the link ready. A snapshot never does here.
                if acknowledged && let Some(ready) = &self.ready {
                    ready.mark_ready();
                }
                self.send_events(after, events).await?
            }
            First::Snapshot => {
                let snapshot = self.store.snapshot().await?;
                self.answer(reply, StreamMode::Snapshot, snapshot.watermark)
                    .await?;
                self.send_snapshot(snapshot).await?
            }
        };
        loop {
            head.borrow_and_update();
            match self.store.read_after(cursor, BATCH).await {
                Ok(ReadResult::Events { events, .. }) if events.is_empty() => {
                    if head.changed().await.is_err() {
                        return Ok(());
                    }
                }
                Ok(ReadResult::Events { events, .. }) => {
                    cursor = self.send_events(cursor, events).await?;
                }
                // Retention removed events that this link did not send:
                // only a snapshot keeps the deck consistent.
                Ok(ReadResult::SnapshotRequired { .. }) => {
                    let snapshot = self.store.snapshot().await?;
                    cursor = self.send_snapshot(snapshot).await?;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    async fn first_batch(&self, start: &Start) -> Result<First, StoreError> {
        let Start::Replay {
            incarnation, after, ..
        } = start
        else {
            return Ok(First::Snapshot);
        };
        if incarnation.as_ref() != Some(self.store.incarnation()) {
            return Ok(First::Snapshot);
        }
        match self.store.read_after(*after, BATCH).await {
            Ok(ReadResult::Events { events, .. }) => Ok(First::Replay {
                after: *after,
                events,
            }),
            Ok(ReadResult::SnapshotRequired { .. }) | Err(StoreError::CursorAhead { .. }) => {
                Ok(First::Snapshot)
            }
            Err(e) => Err(e),
        }
    }

    async fn answer(&self, reply: u64, mode: StreamMode, from: u64) -> Result<(), PumpError> {
        let start = StreamStart {
            mode,
            incarnation: self.incarnation(),
            from,
        };
        let value = serde_json::to_value(start).map_err(|e| {
            PumpError::Link(fleetdeck_link::LinkError::Protocol(format!(
                "{}: {e}",
                code::PROTOCOL_ERROR
            )))
        })?;
        self.control.respond(reply, Outcome::Ok(value)).await?;
        Ok(())
    }

    fn deliver(&self, seq: u64) {
        self.delivered.fetch_max(seq, Ordering::AcqRel);
    }

    async fn send_events(
        &self,
        mut cursor: u64,
        events: Vec<fleetdeck_proto::EventEnvelope>,
    ) -> Result<u64, PumpError> {
        let incarnation = self.incarnation();
        let video = self
            .control
            .params()
            .negotiated
            .has(feature::ARTIFACTS_VIDEO);
        for mut envelope in events {
            let seq = envelope.station_seq;
            if !video {
                project_legacy_event(&mut envelope);
            }
            self.control
                .send(ControlFrame::Event {
                    incarnation: incarnation.clone(),
                    envelope,
                })
                .await?;
            self.deliver(seq);
            cursor = seq;
        }
        Ok(cursor)
    }

    async fn send_snapshot(&self, snapshot: crate::store::Snapshot) -> Result<u64, PumpError> {
        let incarnation = snapshot.incarnation;
        let watermark = snapshot.watermark;
        // Before the first frame: the deck cannot acknowledge it earlier.
        let _ = self.first_snapshot.set(watermark);
        // The binding projection right after the summaries were read.
        let coordinator = match &self.coordinator {
            Some(bindings) => Some(bindings.projection_with(&self.store.session_grants().await?)),
            None => None,
        };
        self.control
            .send(ControlFrame::SnapshotBegin {
                incarnation: incarnation.clone(),
                watermark,
                session_count: snapshot.sessions.len() as u64,
            })
            .await?;
        for summary in snapshot.sessions {
            self.control
                .send(ControlFrame::SnapshotSession {
                    incarnation: incarnation.clone(),
                    snapshot: SessionSnapshot { summary },
                })
                .await?;
        }
        self.control
            .send(ControlFrame::SnapshotEnd {
                incarnation,
                watermark,
                coordinator,
            })
            .await?;
        self.deliver(watermark);
        Ok(watermark)
    }
}

enum First {
    Replay {
        after: u64,
        events: Vec<fleetdeck_proto::EventEnvelope>,
    },
    Snapshot,
}
