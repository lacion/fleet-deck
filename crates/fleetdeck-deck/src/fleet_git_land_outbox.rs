//! The land outbox sender (design §11.2 "Outbox"; FG-8). It sends each
//! `pending` row as `session.land` with its fixed `command_id` and the
//! payload stored at admission. Each send builds a new envelope: the current
//! station incarnation and a fresh `retry_until` (decision Q4). The station
//! deduplicates by `command_id`, so a resend returns the stored receipt.
//!
//! - An accepted receipt sets `accepted`.
//! - A stored rejected receipt sets `rejected` and ends the land `failed`
//!   with the station's code (decision Q5).
//! - No receipt, `incarnation_mismatch`, `expired`, a link error or a
//!   timeout: the row stays `pending` and the backoff goes on.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::command::{
    CommandEnvelope, RETRY_WINDOW_MS, Receipt, ReceiptState, RejectCode,
};
use fleetdeck_proto::fleet_git::LandAbandonAnswer;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{CommandId, StationId};
use tokio::sync::watch;

use crate::fleet_git_land::{LandKick, status};
use crate::fleet_git_land_abandon::abandon_call;
use crate::link::DeckLink;
use crate::store::land_outbox::{DueSend, SendGate};
use crate::store::{DeckStore, LinkFeature};

/// The deadline of `session.land` (decision Q4).
pub const SEND_DEADLINE: Duration = Duration::from_secs(30);
/// The longest sleep when nothing is due.
const IDLE: Duration = Duration::from_secs(30);
/// The shortest sleep, so that a store error never spins the loop.
const FLOOR: Duration = Duration::from_millis(100);

/// Runs the sender until `shutdown` is set.
pub async fn run(
    store: DeckStore,
    links: Arc<DeckLink>,
    kick: Arc<LandKick>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        send_due(&store, &links).await;
        let wait = match store.land_outbox_next_due().await {
            Ok(Some(at)) => {
                let ms = u64::try_from(at.saturating_sub(store.now_ms())).unwrap_or(0);
                Duration::from_millis(ms).min(IDLE)
            }
            _ => IDLE,
        };
        tokio::select! {
            () = kick.outbox.notified() => {}
            () = tokio::time::sleep(wait.max(FLOOR)) => {}
            changed = shutdown.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

async fn send_due(store: &DeckStore, links: &Arc<DeckLink>) {
    let due = match store.land_outbox_due().await {
        Ok(due) => due,
        Err(e) => {
            tracing::warn!(error = %e, "land outbox");
            return;
        }
    };
    for command_id in due {
        match store.land_send_gate(command_id.clone()).await {
            Ok(SendGate::Send(send)) => deliver(store, links, &send).await,
            Ok(SendGate::Verify { send, code, reason }) => {
                verify(store, links, &send, code, reason).await;
            }
            Ok(SendGate::Wait | SendGate::Ended) => {}
            Err(e) => tracing::warn!(error = %e, command = %command_id, "land send gate"),
        }
    }
}

/// One send of `session.land`.
async fn deliver(store: &DeckStore, links: &Arc<DeckLink>, send: &DueSend) {
    let station = StationId::from(send.station_id.clone());
    if store.link_feature(&station, feature::FLEET_GIT_LAND) != LinkFeature::Negotiated {
        return;
    }
    let Some(control) = links.control_for(&station) else {
        return;
    };
    let Ok(Some(rec)) = store.station(station.clone()).await else {
        return;
    };
    let Some(incarnation) = rec.incarnation else {
        return;
    };
    let envelope = CommandEnvelope {
        command_id: CommandId::from(send.command_id.clone()),
        target_incarnation: Some(incarnation),
        retry_until: store.now_ms().saturating_add(RETRY_WINDOW_MS),
        origin: send.origin,
        authority: send.authority.clone(),
        command: send.command.clone(),
    };
    let Ok(params) = serde_json::to_value(&envelope) else {
        return;
    };
    let reply = tokio::time::timeout(
        SEND_DEADLINE,
        control.request("session.land", Some(&envelope.command_id), params),
    )
    .await;
    let value = match reply {
        Ok(Ok(Outcome::Ok(v))) => v,
        Ok(Ok(Outcome::Err(e))) => {
            tracing::debug!(land = %send.land_id, code = %e.code, "session.land: link error");
            return;
        }
        Ok(Err(e)) => {
            tracing::debug!(land = %send.land_id, error = %e, "session.land: link ended");
            return;
        }
        Err(_) => {
            tracing::debug!(land = %send.land_id, "session.land timed out");
            return;
        }
    };
    let text = value.to_string();
    let Ok(receipt) = serde_json::from_value::<Receipt>(value) else {
        tracing::warn!(land = %send.land_id, "session.land: no valid receipt");
        return;
    };
    if receipt.command_id.as_str() != send.command_id {
        return;
    }
    let result = match receipt.state {
        ReceiptState::Accepted => {
            store
                .land_outbox_accepted(send.command_id.clone(), text)
                .await
        }
        ReceiptState::Rejected => {
            let Some(reject) = receipt.reject else {
                return;
            };
            // These two store no receipt: the send is repeated (decision Q4).
            if matches!(
                reject.code,
                RejectCode::IncarnationMismatch | RejectCode::Expired
            ) {
                return;
            }
            store
                .land_outbox_rejected(send.command_id.clone(), reject_code(&reject), text)
                .await
        }
    };
    if let Err(e) = result {
        tracing::warn!(error = %e, land = %send.land_id, "land receipt");
    }
}

/// The code and the text of a rejected receipt (decision Q5): the
/// `<code>: <text>` prefix of the message, else the reject code name.
fn reject_code(reject: &fleetdeck_proto::Reject) -> (String, String) {
    if let Some((code, rest)) = reject.message.split_once(": ")
        && !code.is_empty()
        && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    {
        return (code.to_owned(), rest.to_owned());
    }
    let name = serde_json::to_value(reject.code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "rejected".to_owned());
    (name, reject.message.clone())
}

/// A resend that lost its authority (design §11.2): the earlier send may have
/// started a run. A known status goes on as a run; an unknown land is fenced
/// first and then ended.
async fn verify(
    store: &DeckStore,
    links: &Arc<DeckLink>,
    send: &DueSend,
    code: &'static str,
    reason: String,
) {
    let station = StationId::from(send.station_id.clone());
    if store.link_feature(&station, feature::FLEET_GIT_LAND) != LinkFeature::Negotiated {
        return;
    }
    let Some(answer) = status(links, &station, &send.land_id).await else {
        return;
    };
    match answer {
        fleetdeck_proto::fleet_git::LandStatusAnswer::Unknown => {
            let Some(LandAbandonAnswer::Fenced { .. }) =
                abandon_call(links, &station, &send.land_id).await
            else {
                return;
            };
            if let Err(e) = store
                .land_verify_end(send.land_id.clone(), code, reason)
                .await
            {
                tracing::warn!(error = %e, land = %send.land_id, "land authority end");
            }
        }
        answer => {
            if let Err(e) = store.land_apply_status(send.land_id.clone(), answer).await {
                tracing::warn!(error = %e, land = %send.land_id, "land status");
            }
        }
    }
}
