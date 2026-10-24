//! The mode and the model of the next turn (split from `core.rs` for the
//! module size rule).

use super::super::{COLLABORATION_MODES, MAX_MODEL_CHARS, Mode};
use super::{CodexCore, Output};
use crate::adapter::{ProviderError, Result};

impl CodexCore {
    /// Sets the collaboration mode of the next turns (`plan` or `default`).
    ///
    /// # Errors
    /// [`ProviderError::InvalidMode`] for another name.
    pub fn set_collaboration(&mut self, mode: &str) -> Result<Vec<Output>> {
        self.live()?;
        if !COLLABORATION_MODES.contains(&mode) {
            return Err(ProviderError::InvalidMode(mode.to_owned()));
        }
        self.collaboration = Some(mode.to_owned());
        Ok(Vec::new())
    }

    /// Changes the mode for the next turn. Codex takes the approval policy and
    /// sandbox per `turn/start`; `mode.changed` follows only when the
    /// app-server accepted a turn with the new mode (trap 1).
    ///
    /// # Errors
    /// [`ProviderError::InvalidMode`] for a mode that the session does not offer.
    pub fn set_mode(&mut self, mode: &str) -> Result<Vec<Output>> {
        self.live()?;
        let offered = self.config.capabilities.modes.iter().any(|m| m == mode);
        let parsed = Mode::parse(mode).filter(|_| offered);
        self.pending_mode =
            Some(parsed.ok_or_else(|| ProviderError::InvalidMode(mode.to_owned()))?);
        Ok(Vec::new())
    }

    /// Changes the model for the next turn (`model` of `turn/start`);
    /// `model.changed` follows when the app-server accepted that turn.
    ///
    /// # Errors
    /// [`ProviderError::InvalidModel`] for an empty, long or spaced name.
    pub fn set_model(&mut self, model: &str) -> Result<Vec<Output>> {
        self.live()?;
        if model.is_empty()
            || model.chars().count() > MAX_MODEL_CHARS
            || model.chars().any(char::is_whitespace)
        {
            return Err(ProviderError::InvalidModel(model.to_owned()));
        }
        self.pending_model = Some(model.to_owned());
        Ok(Vec::new())
    }
}
