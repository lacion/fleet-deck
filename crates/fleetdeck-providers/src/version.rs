//! The provider version check (R8.2, DL-162): the probed CLI version against
//! the oldest version that the adapter's replay fixtures and live smoke cover
//! and against the adapter's deny list. An older CLI only warns; a version on
//! the deny list is refused for starts.

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::VersionStatus;

/// The oldest CLI version of `kind` that the replay fixtures cover.
#[must_use]
pub fn tested_min_version(kind: ProviderKind) -> Option<&'static str> {
    match kind {
        ProviderKind::Claude => Some(crate::claude::TESTED_MIN_VERSION),
        ProviderKind::Codex => Some(crate::codex::TESTED_MIN_VERSION),
        ProviderKind::Unknown => None,
    }
}

/// The versions of `kind` that are known to break the protocol.
#[must_use]
pub fn known_broken(kind: ProviderKind) -> &'static [&'static str] {
    match kind {
        ProviderKind::Claude => crate::claude::KNOWN_BROKEN,
        ProviderKind::Codex => crate::codex::KNOWN_BROKEN,
        ProviderKind::Unknown => &[],
    }
}

/// The verdict for a probed version of `kind`.
#[must_use]
pub fn version_status(kind: ProviderKind, version: Option<&str>) -> VersionStatus {
    classify(version, tested_min_version(kind), known_broken(kind))
}

/// The verdict for `version` against a tested minimum and a deny list. No
/// version, no minimum or a version that does not parse is `Unknown`.
#[must_use]
pub fn classify(version: Option<&str>, min: Option<&str>, broken: &[&str]) -> VersionStatus {
    let (Some(version), Some(min)) = (version, min) else {
        return VersionStatus::Unknown;
    };
    let (Some(have), Some(floor)) = (parse(version), parse(min)) else {
        return VersionStatus::Unknown;
    };
    if broken.iter().any(|b| parse(b).is_some_and(|b| b == have)) {
        VersionStatus::Broken
    } else if have < floor {
        VersionStatus::OlderThanTested
    } else {
        VersionStatus::Ok
    }
}

/// The numeric parts of a version (`2.1.289`); a suffix after the first
/// non-numeric part of the last number (`0.161.0-alpha.1`) is ignored.
fn parse(version: &str) -> Option<Vec<u64>> {
    let core = version.trim().split(['-', '+', ' ']).next()?;
    let parts: Option<Vec<u64>> = core.split('.').map(|p| p.parse().ok()).collect();
    parts.filter(|p| !p.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Option<&str> = Some("2.1.289");

    #[test]
    fn the_tested_minimum_and_newer_versions_are_ok() {
        assert_eq!(classify(Some("2.1.289"), MIN, &[]), VersionStatus::Ok);
        assert_eq!(classify(Some("2.1.296"), MIN, &[]), VersionStatus::Ok);
        assert_eq!(classify(Some("2.2.0"), MIN, &[]), VersionStatus::Ok);
        assert_eq!(classify(Some("3.0.0"), MIN, &[]), VersionStatus::Ok);
    }

    #[test]
    fn an_older_version_is_older_than_tested() {
        assert_eq!(
            classify(Some("2.1.288"), MIN, &[]),
            VersionStatus::OlderThanTested
        );
        assert_eq!(
            classify(Some("1.99.999"), MIN, &[]),
            VersionStatus::OlderThanTested
        );
        // Numbers compare as numbers, not as text.
        assert_eq!(classify(Some("2.1.1000"), MIN, &[]), VersionStatus::Ok);
        assert_eq!(
            classify(Some("2.1.30"), MIN, &[]),
            VersionStatus::OlderThanTested
        );
    }

    #[test]
    fn a_denied_version_is_broken_even_when_it_is_newer_than_the_minimum() {
        assert_eq!(
            classify(Some("2.1.300"), MIN, &["2.1.300"]),
            VersionStatus::Broken
        );
        assert_eq!(
            classify(Some("2.1.301"), MIN, &["2.1.300"]),
            VersionStatus::Ok
        );
    }

    #[test]
    fn a_missing_or_unparsable_version_is_unknown() {
        assert_eq!(classify(None, MIN, &[]), VersionStatus::Unknown);
        assert_eq!(classify(Some("nightly"), MIN, &[]), VersionStatus::Unknown);
        assert_eq!(classify(Some("2.1.289"), None, &[]), VersionStatus::Unknown);
        assert_eq!(classify(Some(""), MIN, &[]), VersionStatus::Unknown);
    }

    #[test]
    fn a_pre_release_suffix_is_ignored() {
        assert_eq!(
            classify(Some("0.161.0-alpha.1"), Some("0.160.0"), &[]),
            VersionStatus::Ok
        );
    }

    #[test]
    fn the_shipped_adapters_name_a_parsable_minimum_and_a_valid_deny_list() {
        for kind in [ProviderKind::Claude, ProviderKind::Codex] {
            let min = tested_min_version(kind).unwrap();
            assert!(parse(min).is_some(), "{kind:?}: {min}");
            assert_eq!(version_status(kind, Some(min)), VersionStatus::Ok);
            for b in known_broken(kind) {
                assert!(parse(b).is_some(), "{kind:?}: {b}");
            }
        }
        assert_eq!(
            version_status(ProviderKind::Unknown, Some("1.0.0")),
            VersionStatus::Unknown
        );
    }
}
