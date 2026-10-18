//! The owner closes a preview that an agent opened (R13 P6 follow-up, §6.4:
//! "the owner can close previews through existing board controls"). The
//! handle must name a preview record of the deck of the link. Any agent of
//! that deck may be the opener. The identity check of `close_window` follows.

use fleetdeck_proto::coordinator_desktop_text::{code as p6_code, text as p6_text};

use super::Fail;
use super::actions::{CLOSE_WAIT, Cx};

/// `desktop_window_not_found` with its fixed text (§11.1).
fn not_found() -> Fail {
    Fail::new(
        p6_code::DESKTOP_WINDOW_NOT_FOUND,
        p6_text(p6_code::DESKTOP_WINDOW_NOT_FOUND).unwrap_or_default(),
    )
}

impl Cx<'_> {
    /// `preview_close` of the owner: no coordinator context, so the
    /// recheck of `close_window` for a coordinator does not run.
    ///
    /// # Errors
    /// `desktop_window_not_found` for an unknown handle, a handle of another
    /// deck or a call without a deck; the failures of `close_window`.
    pub(super) async fn owner_preview_close(&self, window_id: &str) -> Result<(), Fail> {
        let deck_fp = self.deck_fp.ok_or_else(not_found)?;
        let (_, record) = self
            .events
            .preview(window_id)
            .filter(|(owner, _)| owner.deck_fp == deck_fp)
            .ok_or_else(not_found)?;
        self.close_window(&record).await?;
        self.events.wait_gone(&record.address, CLOSE_WAIT).await;
        Ok(())
    }
}
