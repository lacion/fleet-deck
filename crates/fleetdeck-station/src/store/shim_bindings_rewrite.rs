//! The in-place rewrite of a shim binding row (MCP P4b §4.1): a change of the
//! owner's grant of a running session rewrites the row of its credential in
//! the decision transaction. The credential, the shim and the state stay.

use rusqlite::params;

use super::shim_bindings::BindingRow;
use super::{Decision, Result};
use crate::coordinator::Binding;

impl Decision<'_> {
    /// Rewrites `binding_id`, `grant_id`, `classes`, `scope`,
    /// `control_skill`, `kind`, `generation`, `native_id` and `expires_at`
    /// of the row of `digest` from `binding`, with the encodings of
    /// [`BindingRow::from_binding`]. The state and the shim stay. Returns
    /// `false` when no row has this digest.
    ///
    /// # Errors
    /// A database error.
    pub fn shim_binding_rewrite(
        &self,
        digest: &str,
        binding: &Binding,
        expires_at: Option<i64>,
    ) -> Result<bool> {
        let row = BindingRow::from_binding(
            digest.to_owned(),
            String::new(),
            binding,
            expires_at,
            self.now(),
        );
        let changed = self.connection().execute(
            "UPDATE shim_bindings SET binding_id = ?2, grant_id = ?3, classes = ?4, scope = ?5,
                 control_skill = ?6, kind = ?7, generation = ?8, native_id = ?9, expires_at = ?10
             WHERE digest = ?1",
            params![
                digest,
                row.binding_id,
                row.grant_id,
                row.classes,
                row.scope,
                row.control_skill,
                kind_name(row.kind),
                row.generation,
                row.native_id,
                row.expires_at,
            ],
        )?;
        Ok(changed > 0)
    }
}

fn kind_name(kind: fleetdeck_proto::command::GrantKind) -> &'static str {
    match kind {
        fleetdeck_proto::command::GrantKind::Coordinator => "coordinator",
        fleetdeck_proto::command::GrantKind::Worker => "worker",
    }
}
