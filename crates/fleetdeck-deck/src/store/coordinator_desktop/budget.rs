//! The image reservation, the query rate and the cap change of the
//! coordinator desktop tools (R13 P6 §8.1, §8.2, §2.2).

use fleetdeck_proto::coordinator_desktop_text::{code, limits};
use rusqlite::params;

use super::rate;
use crate::store::grants::{
    GrantClass, GrantState, check_desktop_caps, check_input_grant, check_recording_caps, get_grant,
};
use crate::store::{DeckStore, Result, call};

/// A cap change that was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapsChanged {
    /// True when a cap became higher than it was (a stored `None` is 0).
    pub increased: bool,
}

/// The caps that a change sets; `None` keeps the stored value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CapsChange {
    pub desktop_cap: Option<u32>,
    pub capture_cap: Option<u32>,
    pub image_byte_cap: Option<u64>,
    pub recording_cap: Option<u32>,
    pub recording_second_cap: Option<u32>,
    /// R13 P7 §9.1: the input count and the typed-character caps.
    pub input_cap: Option<u32>,
    pub input_character_cap: Option<u32>,
}

/// Why a cap change was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapsError {
    /// No such grant, or the grant is revoked.
    NotFound,
    /// The new caps break a rule of [`check_desktop_caps`], of
    /// [`check_recording_caps`] or of [`check_input_grant`], or the grant has
    /// no recording or input allowance; the fixed text.
    Invalid(&'static str),
    /// A cap would grow and the owner did not elevate (§2.2).
    ElevationRequired,
}

impl DeckStore {
    /// Reserves the bytes of one image fetch (§8.1). The reservation and its
    /// rate row are never refunded. The inner `Err` is the code of the
    /// refusal.
    ///
    /// # Errors
    /// A database error.
    pub async fn desktop_image_reserve(
        &self,
        grant_id: &str,
        now: i64,
    ) -> Result<std::result::Result<(), &'static str>> {
        let grant_id = grant_id.to_owned();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(grant) = get_grant(&tx, &grant_id)? else {
                return Ok(Err(code::NOT_GRANTED));
            };
            if grant.state != GrantState::Active
                || now > grant.expires_at
                || !grant.classes.contains(&GrantClass::Capture)
            {
                return Ok(Err(code::NOT_GRANTED));
            }
            let scope = format!("grant:{grant_id}");
            if rate::count(&tx, &scope, rate::IMAGE, now)? >= limits::IMAGES_PER_WINDOW {
                return Ok(Err(code::RATE_LIMITED));
            }
            let used = grant
                .image_byte_used
                .saturating_add(limits::IMAGE_RESERVE_BYTES);
            if used > grant.image_byte_cap.unwrap_or(0) {
                return Ok(Err(code::IMAGE_BUDGET_REACHED));
            }
            tx.execute(
                "UPDATE tokens SET image_byte_used = image_byte_used + ?2 WHERE grant_id = ?1",
                params![
                    grant_id,
                    i64::try_from(limits::IMAGE_RESERVE_BYTES).unwrap_or(i64::MAX)
                ],
            )?;
            rate::charge(&tx, &scope, rate::IMAGE, now)?;
            tx.commit()?;
            Ok(Ok(()))
        })
        .await
    }

    /// Counts one metadata or status query against the rate (§8.2). The inner
    /// `Err` is the code of the refusal.
    ///
    /// # Errors
    /// A database error.
    pub async fn desktop_query_admit(
        &self,
        grant_id: &str,
        now: i64,
    ) -> Result<std::result::Result<(), &'static str>> {
        let scope = format!("grant:{grant_id}");
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            if rate::count(&tx, &scope, rate::QUERY, now)? >= limits::QUERIES_PER_WINDOW {
                return Ok(Err(code::RATE_LIMITED));
            }
            rate::charge(&tx, &scope, rate::QUERY, now)?;
            tx.commit()?;
            Ok(Ok(()))
        })
        .await
    }

    /// Changes the caps of a grant (§2.2). A `None` field of the change keeps
    /// the stored value. A decrease needs no elevation; an increase needs
    /// `allow_increase`. The counters never change here.
    ///
    /// # Errors
    /// A database error. A refusal is the inner `Err`.
    pub async fn desktop_caps_update(
        &self,
        grant_id: &str,
        change: CapsChange,
        allow_increase: bool,
    ) -> Result<std::result::Result<CapsChanged, CapsError>> {
        let grant_id = grant_id.to_owned();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(grant) = get_grant(&tx, &grant_id)? else {
                return Ok(Err(CapsError::NotFound));
            };
            if grant.state == GrantState::Revoked {
                return Ok(Err(CapsError::NotFound));
            }
            // A grant whose recording limit is 0 has no allowance: the board
            // hides its recording limits, and only a new grant brings them back.
            if (change.recording_cap.is_some() || change.recording_second_cap.is_some())
                && grant.recording_cap == 0
            {
                return Ok(Err(CapsError::Invalid(
                    "recording limits need a grant that allows recordings",
                )));
            }
            // The same rule for the input limits (R13 P7 §3.1).
            if (change.input_cap.is_some() || change.input_character_cap.is_some())
                && !grant.allow_input
            {
                return Ok(Err(CapsError::Invalid(
                    "input limits need a grant that allows input",
                )));
            }
            let desktop_new = change.desktop_cap.or(grant.desktop_cap);
            let capture_new = change.capture_cap.or(grant.capture_cap);
            let image_new = change.image_byte_cap.or(grant.image_byte_cap);
            if let Err(text) =
                check_desktop_caps(&grant.classes, desktop_new, capture_new, image_new)
            {
                return Ok(Err(CapsError::Invalid(text)));
            }
            let recording_new = change.recording_cap.unwrap_or(grant.recording_cap);
            let second_new = change
                .recording_second_cap
                .unwrap_or(grant.recording_second_cap);
            if let Err(text) = check_recording_caps(&grant.classes, recording_new, second_new) {
                return Ok(Err(CapsError::Invalid(text)));
            }
            let input_new = change.input_cap.unwrap_or(grant.input_cap);
            let input_character_new = change
                .input_character_cap
                .unwrap_or(grant.input_character_cap);
            if let Err(text) = check_input_grant(
                &grant.classes,
                (grant.allow_input, grant.allow_click),
                grant.input_scope,
                (input_new, input_character_new),
            ) {
                return Ok(Err(CapsError::Invalid(text)));
            }
            let increased = desktop_new.unwrap_or(0) > grant.desktop_cap.unwrap_or(0)
                || capture_new.unwrap_or(0) > grant.capture_cap.unwrap_or(0)
                || image_new.unwrap_or(0) > grant.image_byte_cap.unwrap_or(0)
                || recording_new > grant.recording_cap
                || second_new > grant.recording_second_cap
                || input_new > grant.input_cap
                || input_character_new > grant.input_character_cap;
            if increased && !allow_increase {
                return Ok(Err(CapsError::ElevationRequired));
            }
            tx.execute(
                "UPDATE tokens SET desktop_cap = ?2, capture_cap = ?3, image_byte_cap = ?4,
                 recording_cap = ?5, recording_second_cap = ?6, input_cap = ?7,
                 input_character_cap = ?8
                 WHERE grant_id = ?1",
                params![
                    grant_id,
                    desktop_new,
                    capture_new,
                    // SQLite integers are signed 64-bit; the cap is at most 1 GiB.
                    image_new.and_then(|c| i64::try_from(c).ok()),
                    recording_new,
                    second_new,
                    input_new,
                    input_character_new
                ],
            )?;
            tx.commit()?;
            Ok(Ok(CapsChanged { increased }))
        })
        .await
    }
}
