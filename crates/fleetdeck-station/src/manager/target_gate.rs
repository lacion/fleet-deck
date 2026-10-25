//! The test hooks of the session target (tests and fault-injection builds
//! only): a point where a test holds a task.

/// One point where a test holds an effect before its authority gate (tests
/// and fault-injection builds only).
#[derive(Default)]
pub struct GateHook {
    armed: std::sync::atomic::AtomicBool,
    /// The held task notifies this when it waits.
    pub reached: tokio::sync::Notify,
    /// The test notifies this to let the held task go on.
    pub release: tokio::sync::Notify,
}

impl GateHook {
    /// The next task that reaches the hook waits there.
    pub fn arm(&self) {
        self.armed.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(in crate::manager) async fn hold(&self) {
        if self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.reached.notify_one();
            self.release.notified().await;
        }
    }
}

impl super::super::SessionManager {
    /// The point where a test holds an effect right before its authority
    /// gate (tests and fault-injection builds only).
    #[must_use]
    pub fn gate_hook(&self) -> &GateHook {
        &self.target.gate_hook
    }

    /// The point where a test holds a start or a resume right before the
    /// station takes its launch identity (tests and fault-injection builds
    /// only).
    #[must_use]
    pub fn launch_hook(&self) -> &GateHook {
        &self.target.launch_hook
    }
}
