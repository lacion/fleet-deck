//! The reasoning effort of a session (B18, docs/v2/design/effort.md).

/// The longest effort text that the station passes on.
pub const MAX_EFFORT_CHARS: usize = 16;

/// True for an effort id that the station passes on: 1 to 16 of `a-z`,
/// `0-9`, `-`, `_` (`low`, `xhigh`, `max`, `ultra`). A syntax check only:
/// it keeps spaces, markup and long text out of the store and the board.
/// It does not check that the model offers the level (`bogus` passes):
/// the board shows what the provider reports. A chosen level is checked
/// against the advertised levels in Phase 2 (§9.2).
#[must_use]
pub fn valid_effort(text: &str) -> bool {
    (1..=MAX_EFFORT_CHARS).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// The provider's value as the station passes it on: a valid id, else
/// `None` (not shown).
#[must_use]
pub fn effort_of(text: Option<&str>) -> Option<String> {
    text.filter(|t| valid_effort(t)).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_effort_accepts_provider_ids_and_refuses_others() {
        for ok in ["low", "xhigh", "max", "ultra", "x_1-2", "bogus"] {
            assert!(valid_effort(ok), "{ok}");
        }
        for bad in ["", "High", "very high", "bogus value", "abcdefghijklmnopq"] {
            assert!(!valid_effort(bad), "{bad}");
        }
        assert_eq!("abcdefghijklmnopq".len(), MAX_EFFORT_CHARS + 1);
        assert_eq!(effort_of(Some("high")).as_deref(), Some("high"));
        assert_eq!(effort_of(Some("High")), None);
        assert_eq!(effort_of(None), None);
    }
}
