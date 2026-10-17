//! The worker side of the durable station coordinator outbox (M3 6-5,
//! ADR-006 §6.5, DL-218). The rows and their state rules live in
//! [`crate::store::coordinator_outbox`].
//!
//! - Intake: the tool call gets its `command_id` and complete envelope and
//!   commits one `pending` row before any send. The optional tool parameter
//!   `idempotency_key` (1–64 chars) returns the first call's row for the
//!   same arguments and `idempotency_key_reused` for other arguments.
//! - Transmission: the fence comes first; a binding that is no longer the
//!   active binding of the row's generation ends the row (never sent →
//!   `failed`, sent → `ambiguous`) and nothing leaves. Then `sent_at`
//!   commits, then the stored envelope goes out unchanged.
//! - [`LinkForward::flush`] ends rows past `retry_until` and sends the
//!   other `pending` rows again over the current link; the deck
//!   deduplicates. The station node runs it every [`FLUSH_EVERY`].
//! - The tool result is the deck's receipt for a `done` row and
//!   `{ command_id, state }` for a `pending` or `ambiguous` row, never a
//!   success or a failure; a `failed` row is a tool error.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::{CommandId, Receipt};
use ring::digest::{SHA256, digest};
use serde_json::{Value, json};

use super::bindings::{BindingState, Bindings, Caller};
use super::forward::{LinkForward, without_key};
use crate::store::Store;
use crate::store::coordinator_outbox::{Intake, NewCall, OutboxRow, OutboxState};

/// How often the station node sends the pending rows again.
pub const FLUSH_EVERY: Duration = Duration::from_secs(2);

/// The longest `idempotency_key`.
const KEY_MAX: usize = 64;

/// The store and the bindings of the outbox.
#[derive(Clone, Debug)]
pub struct CallOutbox {
    pub(super) store: Store,
    bindings: Bindings,
    /// Rows with a transmission in flight; a flush does not send them again
    /// at the same time.
    in_flight: Arc<Mutex<HashSet<String>>>,
}

fn key(args: &Value) -> Result<Option<String>, String> {
    match args.get("idempotency_key") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(k)) if !k.is_empty() && k.chars().count() <= KEY_MAX => {
            Ok(Some(k.clone()))
        }
        Some(_) => Err("invalid_params: `idempotency_key` has 1 to 64 characters".to_owned()),
    }
}

fn args_digest(tool: &str, args: &Value) -> String {
    let text = format!("{tool}\n{args}");
    digest(&SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Why the binding of `row` may no longer transmit it (the fence).
fn fence(bindings: &Bindings, row: &OutboxRow) -> Option<&'static str> {
    let Some(b) = bindings.get(&row.binding_id) else {
        return Some("revoked");
    };
    match b.state {
        BindingState::Active if b.generation == row.generation => None,
        BindingState::Active | BindingState::Pending => Some("binding_stale"),
        BindingState::Expired => Some("expired"),
        BindingState::Revoked => Some("revoked"),
    }
}

/// The tool result of a row.
fn answer(row: &OutboxRow) -> Result<Value, String> {
    let id = row.command_id.as_str();
    match row.state {
        OutboxState::Done => row
            .receipt
            .as_deref()
            .map_or(Ok(Value::Null), serde_json::from_str)
            .map_err(|e| format!("internal: {e}")),
        OutboxState::Pending | OutboxState::Ambiguous => Ok(json!({
            "command_id": id,
            "state": row.state.as_str(),
            "reason": row.reason,
        })),
        OutboxState::Failed => Err(format!(
            "{}: command {id} was not accepted",
            row.reason.as_deref().unwrap_or("failed")
        )),
    }
}

impl CallOutbox {
    #[must_use]
    pub fn new(store: Store, bindings: Bindings) -> Self {
        Self {
            store,
            bindings,
            in_flight: Arc::default(),
        }
    }

    fn claim(&self, id: &CommandId) -> bool {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.as_str().to_owned())
    }

    fn release(&self, id: &CommandId) {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id.as_str());
    }
}

fn store_err(e: impl std::fmt::Display) -> String {
    format!("internal: {e}")
}

impl LinkForward {
    /// This forwarder with the durable outbox of `store`; its envelopes use
    /// the store clock.
    #[must_use]
    pub fn with_outbox(store: Store, bindings: Bindings) -> Self {
        let clock = store.clone();
        let mut this = Self::with_clock(Arc::new(move || clock.now_ms()));
        this.outbox = Some(CallOutbox::new(store, bindings));
        this
    }

    fn incarnation(&self) -> Option<fleetdeck_proto::IncarnationId> {
        self.link().map(|l| l.incarnation).or_else(|| {
            self.last_incarnation
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        })
    }

    /// Intake and first transmission of one call.
    pub(super) async fn durable_call(
        &self,
        outbox: &CallOutbox,
        caller: Caller,
        tool: &str,
        args: Value,
    ) -> Result<Value, String> {
        let key = key(&args)?;
        let args = without_key(&args);
        let Some(incarnation) = self.incarnation() else {
            return Err("deck_unreachable: no deck was connected yet".to_owned());
        };
        let (_, Some(command_id), mut envelope) =
            Self::envelope(&caller, tool, &args, &incarnation, (self.clock)())?
        else {
            return Err("internal: a call without a command id".to_owned());
        };
        if let Some(call) = envelope.get_mut("call") {
            call["idempotency_key"] = json!(key);
        }
        let retry_until = envelope
            .get("retry_until")
            .and_then(Value::as_i64)
            .ok_or_else(|| "internal: no retry_until".to_owned())?;
        let new = NewCall {
            command_id,
            binding_id: caller.binding_id.clone(),
            generation: caller.generation,
            idempotency_key: key,
            args_digest: args_digest(tool, &args),
            envelope: envelope.to_string(),
            retry_until,
        };
        match outbox
            .store
            .coordinator_intake(new)
            .await
            .map_err(store_err)?
        {
            Intake::KeyReused => Err(
                "idempotency_key_reused: this key belongs to a call with other arguments"
                    .to_owned(),
            ),
            Intake::Existing(row) => answer(&row),
            Intake::New(row) => answer(&self.transmit(outbox, row).await?),
        }
    }

    /// The fence, then one transmission of a `pending` row.
    async fn transmit(&self, outbox: &CallOutbox, row: OutboxRow) -> Result<OutboxRow, String> {
        let id = row.command_id.clone();
        if let Some(reason) = fence(&outbox.bindings, &row) {
            return outbox
                .store
                .coordinator_fence(id, reason)
                .await
                .map_err(store_err);
        }
        let Some(link) = self.link() else {
            return Ok(row);
        };
        // The lifecycle fence (§6.5, 25e): the row stays pending and a
        // later flush sends it after the acknowledgment.
        if self.fenced(&link).await? {
            return Ok(row);
        }
        if !outbox.claim(&id) {
            return Ok(row);
        }
        let out = self.send(outbox, &link.transport, row).await;
        outbox.release(&id);
        out
    }

    async fn send(
        &self,
        outbox: &CallOutbox,
        transport: &Arc<dyn super::forward::Transport>,
        row: OutboxRow,
    ) -> Result<OutboxRow, String> {
        let id = row.command_id.clone();
        // `sent_at` commits before the network write.
        let Some(sent) = outbox
            .store
            .coordinator_mark_sent(id.clone())
            .await
            .map_err(store_err)?
        else {
            return outbox
                .store
                .coordinator_row(id)
                .await
                .map_err(store_err)?
                .ok_or_else(|| "internal: the outbox row is gone".to_owned());
        };
        let params: Value = serde_json::from_str(&sent.envelope).map_err(store_err)?;
        match transport
            .request("coordinator.call", Some(&id), params)
            .await
        {
            Ok(Outcome::Ok(v)) => match serde_json::from_value::<Receipt>(v) {
                Ok(mut receipt) => {
                    receipt.command_id = id;
                    outbox
                        .store
                        .coordinator_settle(receipt)
                        .await
                        .map_err(store_err)
                }
                Err(_) => Ok(sent),
            },
            // No definitive receipt: the row stays `pending` for a resend.
            Ok(Outcome::Err(_)) | Err(_) => Ok(sent),
        }
    }

    /// Ends the rows past `retry_until`, then sends every other `pending`
    /// row again over the current link (§6.5).
    ///
    /// # Errors
    /// A store error.
    pub async fn flush(&self) -> Result<(), String> {
        let Some(outbox) = &self.outbox else {
            return Ok(());
        };
        outbox.store.coordinator_expire().await.map_err(store_err)?;
        for row in outbox
            .store
            .coordinator_pending()
            .await
            .map_err(store_err)?
        {
            self.transmit(outbox, row).await?;
        }
        Ok(())
    }

    /// Runs [`Self::flush`] every [`FLUSH_EVERY`] until the task is aborted.
    pub async fn run(self) {
        loop {
            if let Err(e) = self.flush().await {
                tracing::warn!(error = %e, "coordinator outbox");
            }
            tokio::time::sleep(FLUSH_EVERY).await;
        }
    }
}
