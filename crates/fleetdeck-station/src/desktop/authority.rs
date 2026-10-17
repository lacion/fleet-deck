//! The station side of the dispatch authority of `desktop.act` (R13 §5E item
//! 1, §5F items 1 and 2, §5G item 1; feature `desktop_authority.v1`).
//!
//! STEP 1 STUB. [`admit`] refuses every action with the text of the design.
//! The P3b station dispatch task replaces the body: the receipt clock check,
//! the single use of `action_id` for the connection epoch (at most 4 096
//! ids), the epoch check against `Control::params().local_desktop_epoch`,
//! the connection check (`Control::is_current`) before the id is consumed and
//! before the first side effect, the hash check
//! (`fleetdeck_link::desktop_authority::action_hash`), the 10 s monotonic
//! limit and the quiet-hours re-check. The signature stays.

use fleetdeck_link::control::Control;
use fleetdeck_proto::desktop::DesktopAction;
use fleetdeck_proto::desktop_authority::{AUTHORITY_REFUSED, DesktopAuthority};

use super::{Fail, code};

/// An action that the deck authorized. The dispatch task fills it with what
/// the start check needs (the receipt instant, the deadline, the epoch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admitted;

/// Checks the authority of `action` that arrived on `control`.
///
/// # Errors
/// `forbidden` with [`AUTHORITY_REFUSED`] (the stub always refuses).
pub fn admit(
    _control: &Control,
    _authority: Option<DesktopAuthority>,
    _action: &DesktopAction,
) -> Result<Admitted, Fail> {
    Err(Fail::new(code::FORBIDDEN, AUTHORITY_REFUSED))
}
