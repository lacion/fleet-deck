//! The data channel of one station link on the deck side (ADR-004 §2.7).
//!
//! The deck opens streams with tokens that the station issued (for example
//! `shell.attach`, PLAN §6.8). One dispatcher task reads the channel's
//! events and routes them to the owner of each stream. Events of a stream
//! that has no owner yet (the station can write before [`DataHub::open`]
//! returns) wait until the owner registers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use fleetdeck_link::data::{Data, DataEvent};
use fleetdeck_link::live::DataLink;
use fleetdeck_link::ws::LinkError;
use tokio::sync::mpsc;

#[derive(Default)]
struct Routes {
    owners: HashMap<u32, mpsc::UnboundedSender<DataEvent>>,
    waiting: HashMap<u32, Vec<DataEvent>>,
}

/// The data channel of one link.
pub struct DataHub {
    handle: Data,
    routes: Mutex<Routes>,
}

impl std::fmt::Debug for DataHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataHub")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

fn stream_of(event: &DataEvent) -> u32 {
    match event {
        DataEvent::Opened { stream, .. }
        | DataEvent::Data { stream, .. }
        | DataEvent::Closed { stream }
        | DataEvent::Reset { stream, .. } => *stream,
    }
}

/// One stream that the deck opened: its handle and its events.
#[derive(Debug)]
pub struct DeckStream {
    pub id: u32,
    pub handle: Data,
    pub events: mpsc::UnboundedReceiver<DataEvent>,
    hub: Arc<DataHub>,
}

impl Drop for DeckStream {
    fn drop(&mut self) {
        let mut routes = self
            .hub
            .routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        routes.owners.remove(&self.id);
        routes.waiting.remove(&self.id);
    }
}

impl DataHub {
    /// Starts the dispatcher of `data`.
    #[must_use]
    pub fn start(data: DataLink) -> Arc<Self> {
        let hub = Arc::new(Self {
            handle: data.handle,
            routes: Mutex::default(),
        });
        let weak = Arc::downgrade(&hub);
        let mut events = data.events;
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let Some(hub) = weak.upgrade() else { return };
                let stream = stream_of(&event);
                let mut routes = hub.routes.lock().unwrap_or_else(PoisonError::into_inner);
                match routes.owners.get(&stream) {
                    Some(owner) => {
                        let _ = owner.send(event);
                    }
                    None => routes.waiting.entry(stream).or_default().push(event),
                }
            }
        });
        hub
    }

    /// Opens a stream with a token that the station issued.
    ///
    /// # Errors
    /// The link error of the open (a bad token resets the stream later).
    pub async fn open(self: &Arc<Self>, token: &str) -> Result<DeckStream, LinkError> {
        let id = self.handle.open(token).await?;
        let (tx, rx) = mpsc::unbounded_channel();
        {
            let mut routes = self.routes.lock().unwrap_or_else(PoisonError::into_inner);
            for event in routes.waiting.remove(&id).unwrap_or_default() {
                let _ = tx.send(event);
            }
            routes.owners.insert(id, tx);
        }
        Ok(DeckStream {
            id,
            handle: self.handle.clone(),
            events: rx,
            hub: Arc::clone(self),
        })
    }
}
