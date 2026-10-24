//! The input part of the coordinator desktop store (R13 P7 §3.5, §7.6, §9):
//! the stored charges of an input operation, and the `attempted` step of an
//! input operation, which writes the exposure snapshot and signs the browser
//! sessions of the target machine out in the same transaction.

use rusqlite::params;
use serde::Serialize;

use crate::store::board_fence::{SignedOut, sign_out_in_tx};
use crate::store::{DeckStore, Result, call};

/// The JSON of the `charged` column of an input operation (R13 P7 §9.1): the
/// two P6 counters and the two input counters. The other tools keep the JSON
/// that they always had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(super) struct StoredInputCharges {
    pub desktop: u32,
    pub capture: u32,
    pub input: u32,
    pub input_characters: u32,
}

impl DeckStore {
    /// `prepared` becomes `attempted` for an input operation, before the
    /// send (R13 P7 §7.6): in one transaction the row gets its state,
    /// `attempted_at` and `exposure_addrs` (the JSON array of `addrs`, the
    /// machine addresses of the target), and every owner login session and
    /// member session whose last address is in `addrs`, or that has a socket
    /// at one of `addrs` in the registry, is deleted. Returns
    /// the deleted sessions; `None` when the row is not a `prepared` input
    /// operation (nothing changes then). The caller then clears the exposure
    /// cache, writes the audit rows and revokes the sockets
    /// ([`crate::board_fence`]).
    ///
    /// # Errors
    /// A database error (nothing is committed).
    pub async fn desktop_op_attempted_input(
        &self,
        operation_id: &str,
        addrs: &[String],
        now: i64,
    ) -> Result<Option<Vec<SignedOut>>> {
        let operation_id = operation_id.to_owned();
        let addrs = addrs.to_vec();
        let snapshot = serde_json::to_string(&addrs)?;
        // The sessions of the sockets (pending or live) at the snapshot
        // addresses, whatever their last address: the transaction deletes
        // them too. A socket that registers after this read is found by its
        // address when the sockets are revoked.
        let sessions = self.registry().sessions_at(&addrs);
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let n = tx.execute(
                "UPDATE coordinator_desktop_ops
                 SET state = 'attempted', updated_at = ?2, attempted_at = ?2, exposure_addrs = ?3
                 WHERE operation_id = ?1 AND state = 'prepared'
                   AND tool IN ('key_press', 'type_text', 'pointer_move', 'pointer_click')",
                params![operation_id, now, snapshot],
            )?;
            if n != 1 {
                return Ok(None);
            }
            let signed_out = sign_out_in_tx(&tx, &addrs, &sessions, now)?;
            tx.commit()?;
            Ok(Some(signed_out))
        })
        .await
    }
}
