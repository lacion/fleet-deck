//! The identity of this build (backlog B5): the git commit and its time.

use std::sync::OnceLock;

/// The short git commit of the build; `unknown` when git was not available.
pub const COMMIT: &str = env!("FLEETDECK_BUILD_COMMIT");

/// Unix seconds of the commit; 0 when unknown.
pub const COMMIT_TIME: i64 = parse_time(env!("FLEETDECK_BUILD_TIME"));

/// Decimal digits to a number; 0 for empty text, a bad digit or an overflow.
const fn parse_time(text: &str) -> i64 {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return 0;
    }
    let mut value: i64 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let digit = bytes[i];
        if !digit.is_ascii_digit() {
            return 0;
        }
        let next = match value.checked_mul(10) {
            Some(shifted) => shifted.checked_add((digit - b'0') as i64),
            None => None,
        };
        value = match next {
            Some(v) => v,
            None => return 0,
        };
        i += 1;
    }
    value
}

/// `0.0.0+<commit>`, or `0.0.0` when the commit is unknown.
pub fn version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        if COMMIT == "unknown" {
            env!("CARGO_PKG_VERSION").to_owned()
        } else {
            format!("{}+{COMMIT}", env!("CARGO_PKG_VERSION"))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_starts_with_the_package_version() {
        assert!(version().starts_with(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn the_commit_time_parses_digits_and_gives_zero_otherwise() {
        assert_eq!(parse_time("1760000000"), 1_760_000_000);
        assert_eq!(parse_time(""), 0);
        assert_eq!(parse_time("12x"), 0);
        assert_eq!(parse_time("99999999999999999999"), 0);
    }
}
