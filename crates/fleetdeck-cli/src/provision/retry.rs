//! The retry table of the bootstrap (design `omarchy-p8.md` 3.3, with D18
//! and the change D29 of P8-1).

use std::time::Duration;

/// What the bootstrap does with the code of a failed join.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    RetryAfter(Duration),
    Stop,
    /// Read the local pairing again; skip the join only if it matches.
    AlreadyPaired,
}

/// The wait after a short failure.
pub const SHORT: Duration = Duration::from_secs(30);
/// The wait after a denial, a cancellation or a revocation (D29 = C).
pub const LONG: Duration = Duration::from_secs(600);

/// The action for `code`. Every code that is not listed is `Stop`.
#[must_use]
pub fn classify(code: &str) -> Action {
    match code {
        "window_closed"
        | "not_accepted"
        | "expired"
        | "busy"
        | "timeout"
        | "failed"
        | "no_address"
        | "tailscale_unavailable"
        | "not_in_tailnet" => Action::RetryAfter(SHORT),
        "denied" | "cancelled" | "revoked" => Action::RetryAfter(LONG),
        "already_paired" => Action::AlreadyPaired,
        _ => Action::Stop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_covers_every_listed_code() {
        for code in [
            "window_closed",
            "not_accepted",
            "expired",
            "busy",
            "timeout",
            "failed",
            "no_address",
            "tailscale_unavailable",
            "not_in_tailnet",
        ] {
            assert_eq!(classify(code), Action::RetryAfter(SHORT), "{code}");
        }
        assert_eq!(SHORT, Duration::from_secs(30));
        for code in ["denied", "cancelled", "revoked"] {
            assert_eq!(classify(code), Action::RetryAfter(LONG), "{code}");
        }
        assert_eq!(LONG, Duration::from_secs(600));
        assert_eq!(classify("already_paired"), Action::AlreadyPaired);
        for code in [
            "fingerprint_mismatch",
            "request_id_conflict",
            "pending",
            "unsupported",
            "invalid_request",
            "retry_file",
            "unknown_request",
            "client_gone",
            "probe_failed",
            "self_connect",
            "something_new",
            "",
            "Denied",
        ] {
            assert_eq!(classify(code), Action::Stop, "{code}");
        }
    }
}
