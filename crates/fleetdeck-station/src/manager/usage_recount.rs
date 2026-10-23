//! The owner's recount of an old usage count (B20, DL-448, design
//! `usage-cost.md` §4). The session manager decides it; the decision reads
//! the provider record (a later task).

use fleetdeck_proto::RejectCode;

use super::decide::reject;
use crate::store::DecideError;

/// The refusal of a recount that reaches `decide::decide` (a direct call has
/// no session lock and no provider record reader).
pub(super) fn refuse_direct() -> DecideError {
    reject(
        RejectCode::InvalidState,
        "usage_recount_direct: The session manager decides a usage recount.",
    )
}
