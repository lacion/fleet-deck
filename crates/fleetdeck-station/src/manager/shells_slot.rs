//! The slot that holds the shells of the station for the manager and its
//! target.

use std::sync::Arc;

/// The shells of the station, shared by the manager and its target. The
/// link node sets them after the manager started (DL-150).
#[derive(Clone, Default)]
pub struct ShellsSlot(Arc<std::sync::OnceLock<Arc<crate::shell::Shells>>>);

impl ShellsSlot {
    /// Sets the shells; a second call changes nothing.
    pub fn set(&self, shells: Arc<crate::shell::Shells>) {
        let _ = self.0.set(shells);
    }

    /// The shells, once set.
    #[must_use]
    pub fn get(&self) -> Option<Arc<crate::shell::Shells>> {
        self.0.get().cloned()
    }
}
