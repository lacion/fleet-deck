//! Crash injection at the effect boundaries of PLAN §6.4 (M2 P2-8).
//!
//! The crash tests run `fleetdeck station serve` with `FLEETDECK_FAULT`
//! set to `<point>@<label>[#<n>]`. At the `n`-th hit (default 1) of the
//! point with that label, the station aborts the process (no destructors,
//! no graceful stop), which is what a crash or `kill -9` does. Labels:
//!
//! | point | where | label |
//! |---|---|---|
//! | `before_commit` | the decision transaction, before its commit | command method (`session.prompt`) |
//! | `after_commit` | right after that commit, before the receipt goes out | command method |
//! | `after_claim` | the worker committed the claim, before the I/O | effect kind (`prompt`) |
//! | `after_send` | the I/O returned, before the result is stored | effect kind |
//! | `before_confirm` | provider evidence confirms an effect, inside its uncommitted transaction | effect kind |
//! | `after_step` | a revert step committed, before the next external step (DL-117) | the new step (`rolled_back`) |
//! | `after_fence` | the binding fence entry was published, before the row change (DL-194) | the operation (`revoke`, `rotate`) |
//! | `after_binding_end` | the revoke ended the binding row, before the decision commits (DL-194) | command method (`session.coordinator_revoke`) |
//!
//! The hook does something only in a build with the `fault-injection`
//! feature (the CLI's tests enable it); in a normal build [`hit`] is empty.

/// The environment variable with the fault spec.
pub const FAULT_ENV: &str = "FLEETDECK_FAULT";

/// One effect boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    BeforeCommit,
    AfterCommit,
    AfterClaim,
    AfterSend,
    BeforeConfirm,
    AfterStep,
    AfterFence,
    AfterBindingEnd,
}

impl FaultPoint {
    pub const ALL: [Self; 8] = [
        Self::BeforeCommit,
        Self::AfterCommit,
        Self::AfterClaim,
        Self::AfterSend,
        Self::BeforeConfirm,
        Self::AfterStep,
        Self::AfterFence,
        Self::AfterBindingEnd,
    ];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::BeforeCommit => "before_commit",
            Self::AfterCommit => "after_commit",
            Self::AfterClaim => "after_claim",
            Self::AfterSend => "after_send",
            Self::BeforeConfirm => "before_confirm",
            Self::AfterStep => "after_step",
            Self::AfterFence => "after_fence",
            Self::AfterBindingEnd => "after_binding_end",
        }
    }
}

/// A parsed `FLEETDECK_FAULT` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultSpec {
    pub point: FaultPoint,
    pub label: String,
    /// The hit that crashes (1 = the first).
    pub nth: u32,
}

impl FaultSpec {
    /// Parses `<point>@<label>[#<n>]`.
    ///
    /// # Errors
    /// A text that is not a spec.
    pub fn parse(text: &str) -> Result<Self, String> {
        let bad = || format!("`{text}` is not a fault spec like after_claim@prompt#1");
        let (point, rest) = text.split_once('@').ok_or_else(bad)?;
        let point = FaultPoint::ALL
            .into_iter()
            .find(|p| p.name() == point)
            .ok_or_else(bad)?;
        let (label, nth) = match rest.split_once('#') {
            Some((label, n)) => (label, n.parse::<u32>().map_err(|_| bad())?),
            None => (rest, 1),
        };
        if label.is_empty() || nth == 0 {
            return Err(bad());
        }
        Ok(Self {
            point,
            label: label.to_owned(),
            nth,
        })
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!("{}@{}#{}", self.point.name(), self.label, self.nth)
    }
}

/// Crashes the process at the armed fault (feature `fault-injection`).
#[cfg(feature = "fault-injection")]
pub fn hit(point: FaultPoint, label: &str) {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU32, Ordering};

    static ARMED: OnceLock<Option<(FaultSpec, AtomicU32)>> = OnceLock::new();
    let armed = ARMED.get_or_init(|| {
        let text = std::env::var(FAULT_ENV).ok()?;
        match FaultSpec::parse(&text) {
            Ok(spec) => Some((spec, AtomicU32::new(0))),
            Err(e) => {
                tracing::error!(error = %e, "fault injection is off");
                None
            }
        }
    });
    let Some((spec, hits)) = armed else {
        return;
    };
    if spec.point != point || spec.label != label {
        return;
    }
    if hits.fetch_add(1, Ordering::SeqCst) + 1 == spec.nth {
        eprintln!("fleetdeck: fault injection: crash at {}", spec.render());
        std::process::abort();
    }
}

/// Crashes the process at the armed fault (feature `fault-injection`).
#[cfg(not(feature = "fault-injection"))]
#[inline(always)]
pub fn hit(_point: FaultPoint, _label: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_parse_and_render() {
        let spec = FaultSpec::parse("after_claim@prompt").unwrap();
        assert_eq!(spec.point, FaultPoint::AfterClaim);
        assert_eq!(spec.label, "prompt");
        assert_eq!(spec.nth, 1);
        assert_eq!(spec.render(), "after_claim@prompt#1");
        for point in FaultPoint::ALL {
            let text = format!("{}@session.prompt#3", point.name());
            let spec = FaultSpec::parse(&text).unwrap();
            assert_eq!((spec.point, spec.nth), (point, 3));
            assert_eq!(spec.render(), text);
        }
        for bad in [
            "",
            "after_claim",
            "later@prompt",
            "after_claim@",
            "after_send@x#0",
            "after_send@x#y",
        ] {
            assert!(FaultSpec::parse(bad).is_err(), "{bad}");
        }
    }
}
