//! The authentication of a credential and the authority check of a routed
//! command, moved out of `bindings.rs`; no behaviour change.

use std::time::Instant;

use tokio::sync::OwnedSemaphorePermit;

use super::{BINDING_RATE, Binding, BindingHandle, BindingState, digest, well_formed};

/// The result of an authentication.
pub(crate) enum Auth {
    /// Missing, malformed, unknown, expired or revoked: one answer (§5.3).
    Unauthorized,
    /// The binding is over its rate or concurrency limit.
    Limited,
    /// The binding is held until the replay of its shim finished (DL-194
    /// §9.7).
    Starting,
    Ok {
        binding: Box<Binding>,
        permit: tokio::sync::OwnedSemaphorePermit,
    },
}

/// The kind of a long-lived stream (MCP P4b §8): it has its own limits, not
/// the request permits of the binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamKind {
    /// A `subscriptions/listen` stream of the HTTP ingress.
    Listen,
    /// A `WATCH` connection of the bridge socket.
    Watch,
}

/// The permits of one stream: the binding's and the station's. They drop
/// together with the stream.
#[derive(Debug)]
#[allow(dead_code, reason = "the permits are held for their drop")]
pub(crate) struct StreamPermits(OwnedSemaphorePermit, OwnedSemaphorePermit);

/// The result of the authentication of a stream.
pub(crate) enum StreamAuth {
    /// Missing, malformed, unknown, expired or revoked: one answer (§5.3).
    Unauthorized,
    /// The binding is over its rate or its stream limits, or the station is.
    Limited,
    /// The binding is held until the replay of its shim finished.
    Starting,
    Ok {
        binding: Box<Binding>,
        handle: BindingHandle,
        permits: StreamPermits,
    },
}

impl super::Bindings {
    /// Authenticates a stream with the checks and the order of
    /// [`Self::authenticate`], and takes the stream permits of its kind
    /// instead of a request permit. The change receiver is registered under
    /// the same lock as the authentication (MCP P4b §8.1).
    pub(crate) fn authenticate_stream(&self, header: Option<&str>, kind: StreamKind) -> StreamAuth {
        let Some(token) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
            tracing::info!(case = "missing", "coordinator credential refused");
            return StreamAuth::Unauthorized;
        };
        if !well_formed(token) {
            tracing::info!(case = "malformed", "coordinator credential refused");
            return StreamAuth::Unauthorized;
        }
        let mut map = self.lock();
        let key = digest(token);
        let Some(e) = map.get_mut(&key) else {
            tracing::info!(case = "unknown", "coordinator credential refused");
            return StreamAuth::Unauthorized;
        };
        if matches!(
            e.binding.state,
            BindingState::Revoked | BindingState::Expired
        ) {
            tracing::info!(case = "ended", binding = %e.binding.binding_id, "coordinator credential refused");
            return StreamAuth::Unauthorized;
        }
        if self.is_held(&key) {
            return StreamAuth::Starting;
        }
        let now = Instant::now();
        if now.duration_since(e.window.0).as_secs() >= 1 {
            e.window = (now, 0);
        }
        if e.window.1 >= BINDING_RATE {
            return StreamAuth::Limited;
        }
        let (binding_limit, station_limit) = match kind {
            StreamKind::Listen => (&e.listens, &self.station_streams.listens),
            StreamKind::Watch => (&e.watches, &self.station_streams.watches),
        };
        let Ok(binding_permit) = binding_limit.clone().try_acquire_owned() else {
            return StreamAuth::Limited;
        };
        let Ok(station_permit) = station_limit.clone().try_acquire_owned() else {
            return StreamAuth::Limited;
        };
        e.window.1 += 1;
        StreamAuth::Ok {
            binding: Box::new(e.binding.clone()),
            handle: BindingHandle {
                digest: key,
                incarnation: e.incarnation,
                changes: e.changes.subscribe(),
            },
            permits: StreamPermits(binding_permit, station_permit),
        }
    }

    pub(crate) fn authenticate(&self, header: Option<&str>) -> Auth {
        let Some(token) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
            tracing::info!(case = "missing", "coordinator credential refused");
            return Auth::Unauthorized;
        };
        if !well_formed(token) {
            tracing::info!(case = "malformed", "coordinator credential refused");
            return Auth::Unauthorized;
        }
        let mut map = self.lock();
        let key = digest(token);
        let Some(e) = map.get_mut(&key) else {
            tracing::info!(case = "unknown", "coordinator credential refused");
            return Auth::Unauthorized;
        };
        // An ended binding is refused before the replay hold: a held
        // credential whose binding ended gets the uniform answer, never
        // "starting" (MCP P1 §3.4).
        if matches!(
            e.binding.state,
            BindingState::Revoked | BindingState::Expired
        ) {
            tracing::info!(case = "ended", binding = %e.binding.binding_id, "coordinator credential refused");
            return Auth::Unauthorized;
        }
        if self.is_held(&key) {
            return Auth::Starting;
        }
        let now = Instant::now();
        if now.duration_since(e.window.0).as_secs() >= 1 {
            e.window = (now, 0);
        }
        if e.window.1 >= BINDING_RATE {
            return Auth::Limited;
        }
        let Ok(permit) = e.inflight.clone().try_acquire_owned() else {
            return Auth::Limited;
        };
        e.window.1 += 1;
        Auth::Ok {
            binding: Box::new(e.binding.clone()),
            permit,
        }
    }
}

/// The whole round trip of `coordinator.authority_check` (ADR-008 §6.2).
pub const AUTHORITY_CHECK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// The result of one `coordinator.authority_check` (ADR-008 §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authority {
    /// The deck answered `valid`.
    Valid,
    /// The deck answered `invalid`, or refused with `forbidden`: the
    /// authority ended (`authority_lost`).
    Lost,
    /// No answer: no deck link, a link without `coordinator.v2`, a
    /// timeout, another error or an answer that does not parse
    /// (`authority_unknown`).
    Unknown,
}

impl super::Bindings {
    /// Asks the deck whether the authority of a routed command still holds
    /// (ADR-008 §6.2): one request, 5 s in all. The station never caches an
    /// answer.
    pub async fn authority_check(
        &self,
        check: &fleetdeck_proto::mutation::AuthorityCheck,
    ) -> Authority {
        self.authority_check_within(check, AUTHORITY_CHECK_DEADLINE)
            .await
    }

    /// [`Self::authority_check`] with another deadline (station tests).
    pub async fn authority_check_within(
        &self,
        check: &fleetdeck_proto::mutation::AuthorityCheck,
        deadline: std::time::Duration,
    ) -> Authority {
        use fleetdeck_proto::link::Outcome;
        use fleetdeck_proto::mutation::AuthorityAnswer;
        let Some(deck) = self.deck() else {
            return Authority::Unknown;
        };
        let Ok(params) = serde_json::to_value(check) else {
            return Authority::Unknown;
        };
        let asked = tokio::time::timeout(
            deadline,
            deck.request("coordinator.authority_check", None, params),
        )
        .await;
        match asked {
            Ok(Ok(Outcome::Ok(value))) => match serde_json::from_value::<AuthorityAnswer>(value) {
                Ok(AuthorityAnswer { valid: true, .. }) => Authority::Valid,
                Ok(_) => Authority::Lost,
                Err(_) => Authority::Unknown,
            },
            Ok(Ok(Outcome::Err(e))) if e.code == fleetdeck_link::methods::code::FORBIDDEN => {
                Authority::Lost
            }
            Ok(Ok(Outcome::Err(_)) | Err(_)) | Err(_) => Authority::Unknown,
        }
    }
}
