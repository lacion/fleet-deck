//! MCP Apps in the durable driver (B15, design `docs/v2/design/mcp-apps.md`
//! §4.3 items 1 and 2):
//! - **Candidates.** The candidates of a committed output transaction go to
//!   the app sink of the driver. Until the station attaches a sink, the driver
//!   keeps them in memory ([`MAX_PENDING_APPS`]); [`DurableDriver::attach_app_sink`]
//!   hands the kept ones over in their order, then every later one. This also
//!   covers the output that a restored driver replays before `adopt` completes.
//! - **Requests.** [`DurableDriver::app_exchange`] writes one app request as
//!   an adapter-internal step and waits for its answer. The two
//!   `AppBridge` implementations (`shim_codex.rs`, `shim_claude.rs`) use it.
//!   A wait ends with `AppError::Ended` when the driver commits the provider's
//!   exit, and never at [`DurableDriver::abandon`]: an abandoned driver stops
//!   its pump, so no commit and no change follow.

use std::sync::Arc;

use fleetdeck_proto::SessionId;
use fleetdeck_providers::adapter::ProviderError;
use fleetdeck_providers::apps::{AppCandidate, AppError};
use serde_json::Value;
use tokio::time::Instant;

use super::{DriverError, DurableCore, DurableDriver, Steps};

/// The most candidates that a driver keeps while no sink is attached. A
/// candidate over the limit is dropped (the newest) with a warning.
pub(in crate::manager) const MAX_PENDING_APPS: usize = 32;

/// The answer of an app request: the result, or the error text of the
/// provider.
pub(in crate::manager) type AppAnswer = Result<Value, String>;

/// Where a driver hands its committed candidates. It must not block.
pub type AppSink = Arc<dyn Fn(AppCandidate) + Send + Sync>;

/// The sink of a driver and the candidates that wait for it.
#[derive(Default)]
pub(super) struct PendingApps {
    sink: Option<AppSink>,
    kept: Vec<AppCandidate>,
    /// Set by [`Self::close`]: nothing is kept any more.
    closed: bool,
}

impl PendingApps {
    /// Hands over the kept candidates in their order, then keeps the sink
    /// for every later one.
    fn attach(&mut self, sink: AppSink) {
        for candidate in std::mem::take(&mut self.kept) {
            sink(candidate);
        }
        self.sink = Some(sink);
    }

    /// Hands the candidates to the sink, or keeps them (at most
    /// [`MAX_PENDING_APPS`]) until one is attached.
    fn hand(&mut self, apps: Vec<AppCandidate>) {
        for candidate in apps {
            if let Some(sink) = self.sink.as_ref() {
                sink(candidate);
            } else if self.closed {
                // The session ended: nothing reads the resource.
            } else if self.kept.len() < MAX_PENDING_APPS {
                self.kept.push(candidate);
            } else {
                tracing::warn!(
                    tool = %candidate.tool,
                    "app candidate dropped: too many wait for the sink"
                );
            }
        }
    }

    /// Drops the sink and the kept candidates; later candidates are dropped.
    /// Returns how many were kept.
    fn close(&mut self) -> usize {
        self.closed = true;
        self.sink = None;
        std::mem::take(&mut self.kept).len()
    }
}

impl<C: DurableCore> DurableDriver<C> {
    /// Attaches the app sink: first the kept candidates in their order, then
    /// every later one. The hand-over and the later hand-offs share one lock,
    /// so the order holds.
    pub fn attach_app_sink(&self, sink: AppSink) {
        self.pending_apps().attach(sink);
    }

    /// Ends the app path of this driver without a sink (a restored session
    /// whose exit is already committed): the kept candidates are dropped with
    /// one warning, and later ones are dropped.
    pub fn close_app_sink(&self, session: &SessionId) {
        let dropped = self.pending_apps().close();
        if dropped > 0 {
            tracing::warn!(
                session = %session,
                dropped,
                "app candidates dropped: the session had ended before its restore"
            );
        }
    }

    /// Hands the candidates of a committed output transaction to the sink, or
    /// keeps them until one is attached.
    pub(super) fn hand_apps(&self, apps: Vec<AppCandidate>) {
        if !apps.is_empty() {
            self.pending_apps().hand(apps);
        }
    }

    fn pending_apps(&self) -> std::sync::MutexGuard<'_, PendingApps> {
        self.pending_apps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Writes one app request and waits for its answer, until `deadline` or
    /// without a limit when there is none.
    ///
    /// `send` writes the request into the core and gives its id and the
    /// frame; `take` takes the answer of an id; `forget` drops an id that
    /// nobody waits for any more (its late answer is dropped).
    ///
    /// # Errors
    /// [`AppError::Ended`] when the provider's exit is committed;
    /// [`AppError::Unsupported`] when the core refuses the request before it
    /// writes anything (`NotReady`); [`AppError::Timeout`] after `deadline`;
    /// [`AppError::Answered`] for any other failure of the write.
    pub(in crate::manager) async fn app_exchange(
        &self,
        send: impl FnOnce(&mut C) -> Result<(String, Steps), ProviderError>,
        take: impl Fn(&mut C, &str) -> Option<AppAnswer>,
        forget: impl Fn(&mut C, &str),
        deadline: Option<Instant>,
    ) -> Result<AppAnswer, AppError> {
        let mut changed = self.changed.subscribe();
        let mut sent: Option<String> = None;
        let written = self
            .internal(|core| {
                let (id, steps) = send(core)?;
                sent = Some(id);
                Ok(steps)
            })
            .await;
        let id = match (written, sent) {
            (Ok(()), Some(id)) => id,
            (Err(e), Some(id)) => {
                forget(&mut *self.core.lock().await, &id);
                return Err(AppError::Answered(e.to_string()));
            }
            (Ok(()), None) => {
                return Err(AppError::Answered("the request has no id".to_owned()));
            }
            (Err(DriverError::Provider(ProviderError::SessionEnded)), None) => {
                return Err(AppError::Ended);
            }
            (Err(DriverError::Provider(ProviderError::NotReady(_))), None) => {
                return Err(AppError::Unsupported);
            }
            (Err(e), None) => return Err(AppError::Answered(e.to_string())),
        };
        loop {
            changed.borrow_and_update();
            {
                let mut core = self.core.lock().await;
                if let Some(answer) = take(&mut core, &id) {
                    return Ok(answer);
                }
                if core.ended() {
                    forget(&mut core, &id);
                    return Err(AppError::Ended);
                }
            }
            let step = match deadline {
                Some(at) => tokio::time::timeout_at(at, changed.changed())
                    .await
                    .map_err(|_| AppError::Timeout),
                None => Ok(changed.changed().await),
            };
            match step {
                Ok(Ok(())) => {}
                Err(timeout) => {
                    forget(&mut *self.core.lock().await, &id);
                    return Err(timeout);
                }
                // The channel cannot close while the driver lives.
                Ok(Err(_)) => {
                    forget(&mut *self.core.lock().await, &id);
                    return Err(AppError::Answered("the driver closed".to_owned()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Mutex;

    use serde_json::json;

    use super::*;

    fn candidate(tool: &str) -> AppCandidate {
        AppCandidate {
            node_id: "node-1".into(),
            server: Some("demo".to_owned()),
            server_hint: None,
            tool: tool.to_owned(),
            resource_uri: Some("ui://demo/app.html".to_owned()),
            arguments: json!({}),
            result: None,
        }
    }

    fn names(seen: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        seen.lock().unwrap().clone()
    }

    fn sink(seen: &Arc<Mutex<Vec<String>>>) -> AppSink {
        let seen = Arc::clone(seen);
        Arc::new(move |c| seen.lock().unwrap().push(c.tool))
    }

    /// The type behind `DurableDriver::attach_app_sink`: the driver only
    /// locks it (a driver needs a shim process, which a unit test lacks).
    #[test]
    fn durable_driver_keeps_candidates_until_a_sink_attaches() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut pending = PendingApps::default();
        pending.hand(vec![candidate("one"), candidate("two")]);
        assert!(names(&seen).is_empty());
        pending.attach(sink(&seen));
        // The two kept candidates arrive in order, then a later one.
        assert_eq!(names(&seen), ["one", "two"]);
        pending.hand(vec![candidate("three")]);
        assert_eq!(names(&seen), ["one", "two", "three"]);
        assert_eq!(pending.close(), 0);
    }

    #[test]
    fn a_driver_keeps_at_most_the_pending_limit_and_drops_the_newest() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut pending = PendingApps::default();
        let all: Vec<_> = (0..MAX_PENDING_APPS + 3)
            .map(|n| candidate(&format!("t{n}")))
            .collect();
        pending.hand(all);
        pending.attach(sink(&seen));
        let got = names(&seen);
        assert_eq!(got.len(), MAX_PENDING_APPS);
        assert_eq!(got.first().map(String::as_str), Some("t0"));
        assert_eq!(
            got.last().map(String::as_str),
            Some(format!("t{}", MAX_PENDING_APPS - 1).as_str())
        );
    }

    #[test]
    fn a_closed_sink_drops_the_kept_candidates_and_the_later_ones() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut pending = PendingApps::default();
        pending.hand(vec![candidate("one")]);
        assert_eq!(pending.close(), 1);
        pending.hand(vec![candidate("two")]);
        assert_eq!(pending.close(), 0);
        pending.attach(sink(&seen));
        assert!(names(&seen).is_empty());
    }
}
