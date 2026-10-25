//! The mode tables and the mode rules of a coordinator grant (ADR-008 §5.2,
//! MCP P0 §2.2). `coordinator_ops` re-exports the public items.

/// Claude native modes, most restrictive first (the provider adapter's
/// `NATIVE_MODES`).
pub(crate) const CLAUDE_MODES: &[&str] = &[
    "default",
    "plan",
    "acceptEdits",
    "dontAsk",
    "auto",
    "bypassPermissions",
];
/// Codex approval policies and sandboxes, most restrictive first.
pub(crate) const CODEX_POLICIES: &[&str] = &["untrusted", "on-request", "never"];
pub(crate) const CODEX_SANDBOXES: &[&str] = &["read-only", "workspace-write", "danger-full-access"];
/// The mode of a Codex start without a mode (the adapter's default).
pub(crate) const CODEX_DEFAULT: &str = "untrusted/workspace-write";

/// True for a mode that skips the permission checks (ADR-008 §5.2): Claude
/// `bypassPermissions`, or Codex approval `never` with the sandbox
/// `danger-full-access`. No coordinator operation starts, resumes or sets a
/// session into one.
#[must_use]
pub fn is_bypass(family: &str, mode: Option<&str>) -> bool {
    match family {
        "claude" => mode == Some("bypassPermissions"),
        "codex" => mode
            .and_then(|m| m.split_once('/'))
            .is_some_and(|(policy, sandbox)| policy == "never" && sandbox == "danger-full-access"),
        _ => false,
    }
}

/// Is `mode` (or the provider default) within `ceiling` for `family`?
/// Unknown families and unknown modes fail closed. No order across
/// families is inferred (ADR-006 §4).
#[must_use]
pub fn mode_within(family: &str, mode: Option<&str>, ceiling: &str) -> bool {
    let at = |list: &[&str], m: &str| list.iter().position(|x| *x == m);
    match family {
        "claude" => {
            let m = mode.unwrap_or("default");
            matches!((at(CLAUDE_MODES, m), at(CLAUDE_MODES, ceiling)), (Some(a), Some(b)) if a <= b)
        }
        "codex" => {
            let parse = |s: &str| {
                let (p, b) = s.split_once('/')?;
                Some((at(CODEX_POLICIES, p)?, at(CODEX_SANDBOXES, b)?))
            };
            match (parse(mode.unwrap_or(CODEX_DEFAULT)), parse(ceiling)) {
                (Some((p, s)), Some((cp, cs))) => p <= cp && s <= cs,
                _ => false,
            }
        }
        _ => false,
    }
}

/// Whether `ceiling` is a valid mode ceiling for `family` (MCP P0 §2.2 rule 5):
/// a known mode of the family that is not a bypass mode.
#[must_use]
pub fn valid_ceiling(family: &str, ceiling: &str) -> bool {
    mode_within(family, Some(ceiling), ceiling) && !is_bypass(family, Some(ceiling))
}
