//! The restart request (DL-172, design 1.6): a process asks to be started
//! again with its startup context. The CLI owns the exec; the deck and the
//! station call this trait without a dependency on the CLI.

/// Asks the process to restart.
pub trait Restart: Send + Sync + std::fmt::Debug {
    /// Starts the graceful shutdown of the process, as a SIGTERM does. After
    /// the drain, the process executes itself again. A second call changes
    /// nothing.
    fn request(&self);
}
