//! The identity of this build (backlog B5, DL-192): the git commit, its time
//! and date, and whether it is a release build.

use std::sync::OnceLock;

/// The short git commit of the build; `unknown` when git was not available.
pub const COMMIT: &str = env!("FLEETDECK_BUILD_COMMIT");

/// Unix seconds of the commit; 0 when unknown.
pub const COMMIT_TIME: i64 = parse_time(env!("FLEETDECK_BUILD_TIME"));

/// The UTC date `YYYYMMDD` of the commit; `00000000` when the time is unknown.
pub const DATE: &str = env!("FLEETDECK_BUILD_DATE");

/// True for a release build (`FLEETDECK_RELEASE=1` at build time).
pub const RELEASE: bool = is_release(env!("FLEETDECK_RELEASE"));

const fn is_release(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 1 && bytes[0] == b'1'
}

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

/// The version text. A release is the package version (`2.0.0`); another
/// build is `<package>-dev.<date>+<commit>`, or `<package>-dev` when the commit
/// is unknown.
fn format_version(package: &str, release: bool, commit: &str, date: &str) -> String {
    if release {
        package.to_owned()
    } else if commit == "unknown" {
        format!("{package}-dev")
    } else {
        format!("{package}-dev.{date}+{commit}")
    }
}

/// The version of this build, see [`format_version`].
pub fn version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| format_version(env!("CARGO_PKG_VERSION"), RELEASE, COMMIT, DATE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_starts_with_the_package_version() {
        assert!(version().starts_with(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn a_release_is_the_package_version() {
        assert_eq!(
            format_version("2.0.0", true, "abc1234567", "20261015"),
            "2.0.0"
        );
        assert_eq!(
            format_version("2.0.0", true, "unknown", "00000000"),
            "2.0.0"
        );
    }

    #[test]
    fn a_dev_build_has_the_date_and_the_commit() {
        assert_eq!(
            format_version("2.0.0", false, "abc1234567", "20261015"),
            "2.0.0-dev.20261015+abc1234567"
        );
    }

    #[test]
    fn a_dev_build_without_a_commit_is_dev_only() {
        assert_eq!(
            format_version("2.0.0", false, "unknown", "00000000"),
            "2.0.0-dev"
        );
    }

    #[test]
    fn the_release_flag_is_one_only_for_the_text_1() {
        assert!(is_release("1"));
        assert!(!is_release("0"));
        assert!(!is_release(""));
        assert!(!is_release("11"));
    }

    #[test]
    fn the_build_date_has_eight_digits() {
        assert_eq!(DATE.len(), 8);
        assert!(DATE.bytes().all(|b| b.is_ascii_digit()));
        assert_eq!(DATE == "00000000", COMMIT_TIME == 0);
    }

    #[test]
    fn the_commit_time_parses_digits_and_gives_zero_otherwise() {
        assert_eq!(parse_time("1760000000"), 1_760_000_000);
        assert_eq!(parse_time(""), 0);
        assert_eq!(parse_time("12x"), 0);
        assert_eq!(parse_time("99999999999999999999"), 0);
    }
}
