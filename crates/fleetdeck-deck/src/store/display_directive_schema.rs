//! The invalidation of the stored directive of a display (R13 §5C item 2,
//! §5D item 4, §6D item 12). The table `display_directives` is made by the V25
//! migration ([`super::display_v25`]); the directive store of the P2b
//! directives task builds on it.
//!
//! The helper is real, not a stub: the transactions of a ceiling change, a
//! revoke, a binding change and a pairing claim call it in the same
//! transaction as their own write.

use rusqlite::{Connection, params};

use super::Result;

/// Deletes the directive of `display_id`, if it has one, and moves the
/// monotonic `directive_generation` of the display up by one: the display
/// returns to `mission`. True when a directive was deleted.
///
/// The caller holds the transaction.
///
/// # Errors
/// A database error.
pub fn invalidate_directive(conn: &Connection, display_id: &str) -> Result<bool> {
    let n = conn.execute(
        "DELETE FROM display_directives WHERE display_id = ?1",
        params![display_id],
    )?;
    if n > 0 {
        conn.execute(
            "UPDATE displays SET directive_generation = directive_generation + 1
             WHERE display_id = ?1",
            params![display_id],
        )?;
    }
    Ok(n > 0)
}
