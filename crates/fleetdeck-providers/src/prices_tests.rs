use super::*;

fn row(provider: ProviderKind, model: &str) -> Option<(f64, f64, f64, f64)> {
    estimate_price_of(provider, model).map(|p| (p.input, p.cache_read, p.cache_write, p.output))
}

#[test]
fn estimate_price_of_has_the_fitted_claude_rows() {
    let c = ProviderKind::Claude;
    assert_eq!(row(c, "claude-opus-5-5"), Some((4.0, 0.2, 5.0, 20.0)));
    assert_eq!(row(c, "claude-sonnet-5-5"), Some((2.0, 0.1, 2.5, 10.0)));
    assert_eq!(row(c, "claude-haiku-5-5"), Some((0.1, 0.01, 0.125, 0.5)));
    assert_eq!(row(c, "claude-opus-5-5[1m]"), row(c, "claude-opus-5-5"));
    assert_eq!(
        row(c, "claude-sonnet-5-5-2026-01-02"),
        row(c, "claude-sonnet-5-5")
    );
    assert_eq!(row(c, "claude-haiku-4-5-20251001"), None);
    assert_eq!(row(c, "claude-fable-5-1"), None);
    // Codex keeps the rows of `price_of`.
    assert_eq!(
        estimate_price_of(ProviderKind::Codex, "gpt-5"),
        price_of(ProviderKind::Codex, "gpt-5")
    );
}

#[test]
fn estimate_cost_matches_the_recorded_cost_states() {
    let c = ProviderKind::Claude;
    let opus = estimate_cost(
        c,
        "claude-opus-5-5",
        18_601,
        1_039_922_797,
        119_025_395,
        2_140_297,
    )
    .unwrap();
    assert!((opus - 845.9919).abs() < 0.01, "{opus}");
    let sonnet = estimate_cost(
        c,
        "claude-sonnet-5-5",
        21_998,
        1_734_626_170,
        68_605_469,
        12_197_000,
    )
    .unwrap();
    assert!((sonnet - 466.9903).abs() < 0.01, "{sonnet}");
    let haiku = estimate_cost(c, "claude-haiku-5-5", 18, 114_047, 127_790, 2_069).unwrap();
    assert!((haiku - 0.01815).abs() < 0.00001, "{haiku}");
    assert_eq!(estimate_cost(c, "claude-fable-5-1", 1, 1, 1, 1), None);
}

#[test]
fn price_of_still_has_no_claude_row() {
    assert!(price_of(ProviderKind::Claude, "claude-opus-5-5").is_none());
    assert!(price_of(ProviderKind::Claude, "claude-sonnet-5-5").is_none());
    assert!(price_of(ProviderKind::Claude, "claude-haiku-5-5").is_none());
}
