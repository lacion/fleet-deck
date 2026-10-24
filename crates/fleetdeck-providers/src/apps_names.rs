//! Claude's MCP tool names (design `docs/v2/design/mcp-apps.md` §4.2, facts 20):
//! the normalization, Claude's own parse of a full name, and the
//! [`ClaudeToolMap`] that decides which `(server, tool)` pairs a full name
//! can address without doubt. The map never picks "the first match".

use std::collections::{HashMap, HashSet};

use super::AppTool;

/// Claude's normalization of one name part: every character outside
/// `[A-Za-z0-9_-]` becomes `_`.
#[must_use]
pub fn claude_part(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Claude's tool name of an MCP tool: `mcp__{claude_part(server)}__{claude_part(tool)}`.
#[must_use]
pub fn claude_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{}__{}", claude_part(server), claude_part(tool))
}

/// Claude's own parse of a full name (fact 20): `split("__")`; `None` unless the
/// first part is `mcp` and the second part is not empty; the tool is the rest
/// joined with `__` (`None` when there is no rest).
#[must_use]
pub fn claude_parse(full: &str) -> Option<(String, Option<String>)> {
    let mut parts = full.split("__");
    if parts.next()? != "mcp" {
        return None;
    }
    let server = parts.next().filter(|s| !s.is_empty())?;
    let rest = parts.collect::<Vec<_>>().join("__");
    Some((server.to_owned(), (!rest.is_empty()).then_some(rest)))
}

/// The canonical map between Claude full names and the (raw server, raw tool)
/// pairs of one `tools()` answer. Built fresh from each answer; never "the first match".
#[derive(Debug, Clone, Default)]
pub struct ClaudeToolMap {
    /// Full name → the distinct pairs that give it.
    by_name: HashMap<String, Vec<(String, String)>>,
    /// Normalized server name → the distinct raw servers that give it.
    by_part: HashMap<String, Vec<String>>,
}

/// The answer of a lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeName {
    /// Exactly one pair, and the pair is addressable (below).
    Unique { server: String, tool: String },
    /// Two or more pairs give this name, or the pair is not addressable.
    Ambiguous,
    /// No pair gives this name.
    Unknown,
}

impl ClaudeToolMap {
    /// The map of one `tools()` answer. The same pair listed twice counts once.
    #[must_use]
    pub fn build(tools: &[AppTool]) -> Self {
        let mut map = Self::default();
        let mut seen: HashSet<(&str, &str)> = HashSet::new();
        for tool in tools {
            if !seen.insert((tool.server.as_str(), tool.name.as_str())) {
                continue;
            }
            map.by_name
                .entry(claude_tool_name(&tool.server, &tool.name))
                .or_default()
                .push((tool.server.clone(), tool.name.clone()));
            let servers = map.by_part.entry(claude_part(&tool.server)).or_default();
            if !servers.contains(&tool.server) {
                servers.push(tool.server.clone());
            }
        }
        map
    }

    /// The pair of a full name (capture of a Claude candidate).
    #[must_use]
    pub fn resolve(&self, full: &str) -> ClaudeName {
        match self.by_name.get(full).map(Vec::as_slice) {
            None | Some([]) => ClaudeName::Unknown,
            Some([(server, tool)]) if self.addressable(full, server, tool) => ClaudeName::Unique {
                server: server.clone(),
                tool: tool.clone(),
            },
            Some(_) => ClaudeName::Ambiguous,
        }
    }

    /// The full name of a pair when `resolve` of that name gives exactly this
    /// pair back; else `None` (an app call of that pair is `tool_not_callable`).
    #[must_use]
    pub fn address(&self, server: &str, tool: &str) -> Option<String> {
        let name = claude_tool_name(server, tool);
        match self.resolve(&name) {
            ClaudeName::Unique { server: s, tool: t } if s == server && t == tool => Some(name),
            _ => None,
        }
    }

    /// The three rules of §4.2 for the pair `(server, tool)` with the name `full`:
    /// no other pair has the name, no other server has the normalized server
    /// name, and Claude's own parse of the name gives this pair back.
    fn addressable(&self, full: &str, server: &str, tool: &str) -> bool {
        let name_is_unique = self.by_name.get(full).is_some_and(|p| p.len() == 1);
        let part = claude_part(server);
        let part_is_unique = self.by_part.get(&part).is_some_and(|s| s.len() == 1);
        name_is_unique
            && part_is_unique
            && claude_parse(full) == Some((part, Some(claude_part(tool))))
    }
}
