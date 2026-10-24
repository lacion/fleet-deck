//! The tests of the shared app layer: the version gate, Claude's name rules
//! and the `ClaudeToolMap` addressability (design `docs/v2/design/mcp-apps.md`
//! §4.2).

use fleetdeck_proto::event::ProviderKind;
use serde_json::json;

use super::*;

fn tool(server: &str, name: &str) -> AppTool {
    AppTool {
        server: server.to_owned(),
        name: name.to_owned(),
        title: None,
        read_only: false,
        visibility: None,
        resource_uri: Some("ui://x/y.html".to_owned()),
    }
}

fn unique(server: &str, tool: &str) -> ClaudeName {
    ClaudeName::Unique {
        server: server.to_owned(),
        tool: tool.to_owned(),
    }
}

#[test]
fn apps_verified_gate() {
    let codex = ProviderKind::Codex;
    let claude = ProviderKind::Claude;
    assert!(!apps_verified(codex, Some("0.162.1")));
    assert!(apps_verified(codex, Some("0.163.0")));
    assert!(apps_verified(codex, Some("0.164.2")));
    assert!(!apps_verified(claude, Some("2.1.307 (Claude Code)")));
    assert!(apps_verified(claude, Some("2.1.308 (Claude Code)")));
    assert!(!apps_verified(codex, None));
    assert!(!apps_verified(codex, Some("not a version")));
    assert!(!apps_verified(ProviderKind::Unknown, Some("9.9.9")));
}

#[test]
fn claude_tool_name_normalizes() {
    assert_eq!(claude_tool_name("my.server", "a b"), "mcp__my_server__a_b");
    assert_eq!(
        claude_tool_name("demo", "show-counter_2"),
        "mcp__demo__show-counter_2"
    );
    assert_eq!(claude_part("a/b:c"), "a_b_c");
}

#[test]
fn claude_parse_matches_the_cli() {
    assert_eq!(
        claude_parse("mcp__a__b__c"),
        Some(("a".to_owned(), Some("b__c".to_owned())))
    );
    assert_eq!(claude_parse("mcp__a"), Some(("a".to_owned(), None)));
    assert_eq!(claude_parse("x__a__b"), None);
    assert_eq!(claude_parse("mcp____b"), None);
    assert_eq!(claude_parse("mcp"), None);
}

#[test]
fn claude_map_resolves_a_unique_pair() {
    let map = ClaudeToolMap::build(&[tool("demo", "show_counter"), tool("demo", "bump")]);
    assert_eq!(
        map.resolve("mcp__demo__show_counter"),
        unique("demo", "show_counter")
    );
    assert_eq!(map.resolve("mcp__demo__bump"), unique("demo", "bump"));
    assert_eq!(map.resolve("mcp__demo__nope"), ClaudeName::Unknown);
    for t in ["show_counter", "bump"] {
        let full = map.address("demo", t).unwrap();
        assert_eq!(map.resolve(&full), unique("demo", t));
    }
    assert_eq!(map.address("demo", "nope"), None);
}

#[test]
fn claude_map_rejects_delimiter_collisions() {
    let map = ClaudeToolMap::build(&[tool("a", "b__c"), tool("a__b", "c")]);
    assert_eq!(map.resolve("mcp__a__b__c"), ClaudeName::Ambiguous);
    assert_eq!(map.address("a", "b__c"), None);
    assert_eq!(map.address("a__b", "c"), None);
}

#[test]
fn claude_map_rejects_normalization_collisions() {
    let servers = ClaudeToolMap::build(&[tool("my.server", "t"), tool("my_server", "t")]);
    assert_eq!(servers.resolve("mcp__my_server__t"), ClaudeName::Ambiguous);
    assert_eq!(servers.address("my.server", "t"), None);
    assert_eq!(servers.address("my_server", "t"), None);
    let tools = ClaudeToolMap::build(&[tool("s", "a.b"), tool("s", "a_b")]);
    assert_eq!(tools.resolve("mcp__s__a_b"), ClaudeName::Ambiguous);
    assert_eq!(tools.address("s", "a.b"), None);
    assert_eq!(tools.address("s", "a_b"), None);
}

#[test]
fn claude_map_rejects_unaddressable_servers() {
    // Claude's own parse of these names picks another server or tool.
    let map = ClaudeToolMap::build(&[tool("a_", "t")]);
    assert_eq!(map.resolve("mcp__a___t"), ClaudeName::Ambiguous);
    assert_eq!(map.address("a_", "t"), None);
    let map = ClaudeToolMap::build(&[tool("x__y", "t")]);
    assert_eq!(map.address("x__y", "t"), None);
    let map = ClaudeToolMap::build(&[tool("s", "")]);
    assert_eq!(map.address("s", ""), None);
}

#[test]
fn claude_map_counts_a_listed_twice_pair_once() {
    let map = ClaudeToolMap::build(&[tool("demo", "bump"), tool("demo", "bump")]);
    assert_eq!(map.resolve("mcp__demo__bump"), unique("demo", "bump"));
}

#[test]
fn ui_meta_reads_visibility_and_the_resource_uri() {
    let meta = json!({"ui": {"resourceUri": "ui://d/c.html", "visibility": ["app"]}});
    assert_eq!(
        ui_meta(Some(&meta)),
        (
            Some(vec!["app".to_owned()]),
            Some("ui://d/c.html".to_owned())
        )
    );
    let legacy = json!({"ui/resourceUri": "ui://d/c.html"});
    assert_eq!(
        ui_meta(Some(&legacy)),
        (None, Some("ui://d/c.html".to_owned()))
    );
    let other = json!({"ui": {"resourceUri": "https://x/y", "visibility": "app"}});
    assert_eq!(ui_meta(Some(&other)), (None, None));
    let mixed = json!({"ui": {"visibility": ["app", 3]}});
    assert_eq!(ui_meta(Some(&mixed)), (None, None));
    assert_eq!(ui_meta(None), (None, None));
}

#[test]
fn app_mime_ignores_case_and_spaces() {
    assert!(is_app_mime("text/html;profile=mcp-app"));
    assert!(is_app_mime(" Text/HTML ; profile=mcp-app "));
    assert!(!is_app_mime("text/html"));
}

#[test]
fn json_len_counts_the_serialized_bytes() {
    let v = json!({"a": [1, 2, "é"]});
    assert_eq!(json_len(&v), v.to_string().len());
}
