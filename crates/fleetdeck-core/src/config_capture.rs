//! The `capture_monitors` key of `station.toml` (R13 P5a): the monitors that
//! a screenshot may capture. An empty list means every monitor.

use super::{ConfigError, invalid};

/// The most `capture_monitors` entries.
const MAX_ENTRIES: usize = 16;

/// Validates the raw `capture_monitors` list.
pub(crate) fn capture_monitors(raw: Option<Vec<String>>) -> Result<Vec<String>, ConfigError> {
    let monitors = raw.unwrap_or_default();
    if monitors.len() > MAX_ENTRIES {
        return Err(invalid("capture_monitors", "give at most 16 monitors"));
    }
    for (i, name) in monitors.iter().enumerate() {
        if !valid_monitor(name) {
            return Err(invalid(
                format!("capture_monitors[{i}]"),
                "use 1-64 letters, digits, `.`, `_`, `-`",
            ));
        }
        if monitors[..i].contains(name) {
            return Err(invalid(
                format!("capture_monitors[{i}]"),
                "Each monitor at most once.",
            ));
        }
    }
    Ok(monitors)
}

/// A monitor name: 1 to 64 of ASCII letters, digits, `.`, `_` and `-`.
fn valid_monitor(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Option<Vec<String>> {
        Some(items.iter().map(|s| (*s).to_owned()).collect())
    }

    fn msg(raw: Option<Vec<String>>) -> String {
        capture_monitors(raw).unwrap_err().to_string()
    }

    #[test]
    fn absent_and_empty_mean_every_monitor() {
        assert!(capture_monitors(None).unwrap().is_empty());
        assert!(capture_monitors(list(&[])).unwrap().is_empty());
    }

    #[test]
    fn valid_names_pass_in_order() {
        let ok = capture_monitors(list(&["DP-1", "HDMI-A-1", "eDP.1", "m_2"])).unwrap();
        assert_eq!(ok, ["DP-1", "HDMI-A-1", "eDP.1", "m_2"]);
        let long = "a".repeat(64);
        assert!(capture_monitors(list(&[long.as_str()])).is_ok());
    }

    #[test]
    fn more_than_16_entries_are_refused() {
        let names: Vec<String> = (0..17).map(|i| format!("DP-{i}")).collect();
        let text = msg(Some(names));
        assert!(text.contains("capture_monitors"), "{text}");
        assert!(text.contains("give at most 16 monitors"), "{text}");
        let sixteen: Vec<String> = (0..16).map(|i| format!("DP-{i}")).collect();
        assert!(capture_monitors(Some(sixteen)).is_ok());
    }

    #[test]
    fn a_bad_name_is_refused_with_its_index() {
        let too_long = "a".repeat(65);
        for bad in ["", "DP 1", "DP/1", "é", "a\0b", too_long.as_str()] {
            let text = msg(list(&["DP-1", bad]));
            assert!(text.contains("capture_monitors[1]"), "{bad:?}: {text}");
            assert!(text.contains("use 1-64 letters, digits"), "{bad:?}: {text}");
        }
    }

    #[test]
    fn a_repeated_name_is_refused_with_its_index() {
        let text = msg(list(&["DP-1", "HDMI-A-1", "DP-1"]));
        assert!(text.contains("capture_monitors[2]"), "{text}");
        assert!(text.contains("Each monitor at most once."), "{text}");
    }
}
