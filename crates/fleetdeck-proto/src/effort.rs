//! The reasoning effort of a session (B18, docs/v2/design/effort.md).

use crate::event::ModelChoice;

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

/// The model id without a trailing context suffix:
/// `claude-opus-5-5[1m]` → `claude-opus-5-5`; other ids unchanged.
#[must_use]
pub fn base_model(id: &str) -> &str {
    match id.strip_suffix(']').and_then(|rest| rest.rsplit_once('[')) {
        Some((base, _)) if !base.is_empty() => base,
        _ => id,
    }
}

/// The advertised choice of `model` (the summary model or the provider's
/// current model). In list order, the first choice where:
/// 1. `value == model`; else
/// 2. `resolved == Some(model)`; else
/// 3. `base_model(value)` or `base_model(resolved)` equals `base_model(model)`.
///
/// `None` when no choice matches.
#[must_use]
pub fn choice_for<'a>(models: &'a [ModelChoice], model: &str) -> Option<&'a ModelChoice> {
    models
        .iter()
        .find(|c| c.value == model)
        .or_else(|| models.iter().find(|c| c.resolved.as_deref() == Some(model)))
        .or_else(|| {
            let base = base_model(model);
            models.iter().find(|c| {
                base_model(&c.value) == base
                    || c.resolved.as_deref().is_some_and(|r| base_model(r) == base)
            })
        })
}

/// True when `a` and `b` name the same model: equal, or one choice has
/// both among its `value` and `resolved` (`sonnet` and `claude-sonnet-5-5`).
#[must_use]
pub fn same_model(models: &[ModelChoice], a: &str, b: &str) -> bool {
    a == b
        || models.iter().any(|c| {
            let names = |x: &str| c.value == x || c.resolved.as_deref() == Some(x);
            names(a) && names(b)
        })
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

    fn choice(value: &str, resolved: Option<&str>) -> ModelChoice {
        ModelChoice {
            value: value.to_owned(),
            label: value.to_uppercase(),
            description: None,
            resolved: resolved.map(str::to_owned),
            efforts: None,
            default_effort: None,
        }
    }

    fn claude_models() -> Vec<ModelChoice> {
        vec![
            choice("default", Some("claude-opus-5-5")),
            choice("opus", Some("claude-opus-5-5")),
            choice("sonnet", Some("claude-sonnet-5-5")),
            choice("haiku", Some("claude-haiku-5-5")),
        ]
    }

    #[test]
    fn base_model_removes_only_a_trailing_context_suffix() {
        assert_eq!(base_model("claude-opus-5-5[1m]"), "claude-opus-5-5");
        assert_eq!(base_model("claude-opus-5-5"), "claude-opus-5-5");
        assert_eq!(base_model("opus"), "opus");
        assert_eq!(base_model("[1m]"), "[1m]");
        assert_eq!(base_model("a[1m]b"), "a[1m]b");
    }

    #[test]
    fn choice_for_matches_value_resolved_suffix_and_list_order() {
        let models = claude_models();
        let value_of = |m: &str| choice_for(&models, m).map(|c| c.value.as_str());
        // Exact `value`.
        assert_eq!(value_of("sonnet"), Some("sonnet"));
        assert_eq!(value_of("opus"), Some("opus"));
        // `resolved`.
        assert_eq!(value_of("claude-haiku-5-5"), Some("haiku"));
        // The context suffix: the choice with `resolved: claude-opus-5-5`.
        let suffixed = choice_for(&models, "claude-opus-5-5[1m]").unwrap();
        assert_eq!(suffixed.resolved.as_deref(), Some("claude-opus-5-5"));
        // First match in list order: `default` and `opus` resolve to one id.
        assert_eq!(value_of("claude-opus-5-5"), Some("default"));
        assert_eq!(value_of("claude-opus-5-5[1m]"), Some("default"));
        // No match.
        assert_eq!(value_of("gpt-6.1-sol"), None);
        assert!(choice_for(&[], "opus").is_none());
    }

    #[test]
    fn same_model_joins_a_value_and_its_resolved_id() {
        let models = claude_models();
        assert!(same_model(&models, "sonnet", "claude-sonnet-5-5"));
        assert!(same_model(&models, "claude-sonnet-5-5", "sonnet"));
        assert!(same_model(&models, "sonnet", "sonnet"));
        assert!(same_model(&models, "x", "x"));
        assert!(!same_model(&models, "sonnet", "claude-haiku-5-5"));
        assert!(!same_model(&models, "sonnet", "opus"));
        assert!(!same_model(&[], "sonnet", "claude-sonnet-5-5"));
    }
}
