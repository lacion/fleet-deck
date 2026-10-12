use std::path::Path;

use super::*;

fn env() -> Env {
    Env {
        home: Some("/home/ana".into()),
        ..Env::default()
    }
}

/// The example of PLAN §6.7, verbatim.
const PLAN_EXAMPLE: &str = r#"
# ~/.config/fleetdeck/station.toml
name = "laptop"
allowed_roots = ["~/projects", "/workspace"]
max_sessions = 12
allow_setup_cmd = false
allow_shell = true            # PTY shells from the board (D10)
max_shells = 8
offline_approval_policy = "wait"
listen = []                   # deck-dials-station mode only; loopback, LAN, or tailnet addresses

[[provider_instance]]
id = "claude"                  # stable id shown in the board and stored on each session
driver = "claude"              # claude | codex
binary = "claude"              # optional path; default: driver name on PATH
config_home = ""               # optional: CLAUDE_CONFIG_DIR / CODEX_HOME for this instance
env = {}                       # optional extra environment for this instance

[[provider_instance]]
id = "codex"
driver = "codex"
"#;

fn err(text: &str) -> String {
    StationConfig::parse(text, &env()).unwrap_err().to_string()
}

#[test]
fn the_plan_example_parses() {
    let c = StationConfig::parse(PLAN_EXAMPLE, &env()).unwrap();
    assert_eq!(c.name, "laptop");
    assert_eq!(
        c.allowed_roots,
        vec![
            PathBuf::from("/home/ana/projects"),
            PathBuf::from("/workspace")
        ]
    );
    assert_eq!(c.max_sessions, 12);
    assert!(!c.allow_setup_cmd && c.allow_shell);
    assert_eq!(c.max_shells, 8);
    assert_eq!(c.offline_approval_policy, OfflineApprovalPolicy::Wait);
    assert!(c.listen.is_empty());
    assert_eq!(c.provider_instances.len(), 2);
    let claude = c.instance(&"claude".into()).unwrap();
    assert_eq!(claude.driver, Driver::Claude);
    assert_eq!(claude.binary, Path::new("claude"));
    assert_eq!(claude.config_home, None);
    let codex = c.instance(&"codex".into()).unwrap();
    assert_eq!(codex.binary, Path::new("codex"));
    assert_eq!(codex.driver.config_home_var(), "CODEX_HOME");
}

#[test]
fn an_empty_file_gives_the_defaults() {
    let c = StationConfig::parse("", &env()).unwrap();
    assert!(!c.name.is_empty());
    assert_eq!(c.allowed_roots, vec![PathBuf::from("/home/ana")]);
    assert_eq!(c.max_sessions, DEFAULT_MAX_SESSIONS);
    assert!(!c.allow_setup_cmd);
    let ids: Vec<&str> = c.provider_instances.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["claude", "codex"]);
}

#[test]
fn two_accounts_on_one_driver_are_two_instances() {
    let c = StationConfig::parse(
        r#"
        [[provider_instance]]
        id = "claude-work"
        driver = "claude"
        config_home = "~/.claude-work"
        env = { ANTHROPIC_API_KEY = "sk-secret-1" }

        [[provider_instance]]
        id = "claude-home"
        driver = "claude"
        binary = "/opt/claude/bin/claude"
        "#,
        &env(),
    )
    .unwrap();
    let work = c.instance(&"claude-work".into()).unwrap();
    assert_eq!(
        work.config_home.as_deref(),
        Some(Path::new("/home/ana/.claude-work"))
    );
    assert_eq!(work.env["ANTHROPIC_API_KEY"].expose(), "sk-secret-1");
    let home = c.instance(&"claude-home".into()).unwrap();
    assert_eq!(home.binary, Path::new("/opt/claude/bin/claude"));
    assert!(c.instance(&"claude".into()).is_none());
    // The key never reaches a log line.
    assert!(!format!("{c:?}").contains("sk-secret-1"));
}

#[test]
fn unknown_keys_and_bad_values_are_errors() {
    assert!(err("max_session = 3").contains("unknown field"));
    assert!(err("[[provider_instance]]\nid = \"x\"\ndriver = \"pi\"").contains("unknown variant"));
    assert!(
        err("[[provider_instance]]\nid = \"x\"\ndriver = \"claude\"\nmodel = \"y\"")
            .contains("unknown field")
    );
    assert!(err("max_sessions = 0").contains("max_sessions"));
    assert!(err("max_shells = 65").contains("max_shells"));
    assert!(err("name = \"my laptop\"").contains("`name`"));
    assert!(err("allowed_roots = []").contains("allowed_roots"));
    assert!(err("allowed_roots = [\"projects\"]").contains("allowed_roots[0]"));
    assert!(err("allowed_roots = [\"/a/../b\"]").contains(".."));
}

#[test]
fn instance_ids_are_checked_and_unique() {
    let twice = "[[provider_instance]]\nid = \"a\"\ndriver = \"claude\"\n\
                 [[provider_instance]]\nid = \"a\"\ndriver = \"codex\"";
    assert!(err(twice).contains("used twice"));
    assert!(err("[[provider_instance]]\nid = \"Claude\"\ndriver = \"claude\"").contains(".id"));
    assert!(err("[[provider_instance]]\nid = \"\"\ndriver = \"claude\"").contains(".id"));
    assert!(
        err("[[provider_instance]]\nid = \"c\"\ndriver = \"claude\"\nbinary = \"bin/claude\"")
            .contains(".binary")
    );
    assert!(
        err("[[provider_instance]]\nid = \"c\"\ndriver = \"claude\"\nenv = { \"A=B\" = \"x\" }")
            .contains(".env")
    );
    // An explicit empty list means no providers on this station.
    let c = StationConfig::parse("provider_instance = []", &env()).unwrap();
    assert!(c.provider_instances.is_empty());
}

#[test]
fn listen_addresses_follow_the_d13_bind_rule() {
    let c = StationConfig::parse(
        "listen = [\"127.0.0.1:7500\", \"192.168.1.5:7500\", \"[fd7a:115c:a1e0::9]:7500\"]",
        &env(),
    )
    .unwrap();
    assert_eq!(c.listen.len(), 3);
    for bad in ["0.0.0.0:7500", "203.0.113.4:7500", "[::]:7500"] {
        let e = err(&format!("listen = [\"{bad}\"]"));
        assert!(e.contains("refusing to listen"), "{bad}: {e}");
    }
    assert!(err("listen = [\"laptop.lan:7500\"]").contains("not an IP address"));
}

#[test]
fn offline_approval_policies() {
    use OfflineApprovalPolicy::*;
    assert_eq!(OfflineApprovalPolicy::parse("wait"), Ok(Wait));
    assert_eq!(
        OfflineApprovalPolicy::parse("deny_after(5m)"),
        Ok(DenyAfter(Duration::from_secs(300)))
    );
    assert_eq!(
        OfflineApprovalPolicy::parse("deny_after( 2h )"),
        Ok(DenyAfter(Duration::from_secs(7200)))
    );
    for bad in [
        "deny",
        "deny_after(0s)",
        "deny_after(5)",
        "deny_after(5d)",
        "deny_after()",
    ] {
        assert!(OfflineApprovalPolicy::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn allowed_roots_contain_paths_by_component() {
    let c = StationConfig::parse("allowed_roots = [\"~/projects\"]", &env()).unwrap();
    assert!(c.is_allowed(Path::new("/home/ana/projects/app")));
    assert!(c.is_allowed(Path::new("/home/ana/projects")));
    assert!(!c.is_allowed(Path::new("/home/ana/projects-old/app")));
    assert!(!c.is_allowed(Path::new("/home/ana")));
}

#[test]
fn deck_defaults_and_rules() {
    let d = DeckConfig::parse("").unwrap();
    assert_eq!(d.name, "deck");
    assert_eq!(d.listen, vec![DEFAULT_BOARD_LISTEN.parse().unwrap()]);
    assert_eq!(d.link_listen, vec![DEFAULT_LINK_LISTEN.parse().unwrap()]);
    assert_eq!(d.tailnet_hostname, None);
    let d = DeckConfig::parse(
        "name = \"home\"\nlisten = [\"100.101.1.2:7420\"]\nlink_listen = []\n\
         tailnet_hostname = \"Deck.tail1234.ts.net\"",
    )
    .unwrap();
    assert!(d.link_listen.is_empty());
    assert_eq!(d.tailnet_hostname.as_deref(), Some("deck.tail1234.ts.net"));
    let e = |t: &str| DeckConfig::parse(t).unwrap_err().to_string();
    assert!(e("listen = []").contains("listen"));
    assert!(e("listen = [\"0.0.0.0:7420\"]").contains("refusing to listen"));
    assert!(e("link_listen = [\"8.8.8.8:7421\"]").contains("refusing to listen"));
    assert!(e("tailnet_hostname = \"bad host\"").contains("tailnet_hostname"));
    assert!(e("login_required = false").contains("unknown field"));
}

#[test]
fn files_load_with_owner_rules() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("station.toml");
    // Missing file: defaults.
    assert!(StationConfig::load(&path, &env()).is_ok());
    fs::write(&path, "name = \"box\"\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(StationConfig::load(&path, &env()).unwrap().name, "box");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(matches!(
        StationConfig::load(&path, &env()),
        Err(ConfigError::Mode { .. })
    ));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&path, "name = [").unwrap();
    let e = StationConfig::load(&path, &env()).unwrap_err().to_string();
    assert!(e.contains("station.toml"), "{e}");
    let deck = dir.path().join("deck.toml");
    fs::write(&deck, "name = \"d\"\n").unwrap();
    fs::set_permissions(&deck, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(DeckConfig::load(&deck).unwrap().name, "d");
}

#[test]
fn durations() {
    assert_eq!(parse_duration("30s"), Ok(Duration::from_secs(30)));
    assert_eq!(parse_duration("1h"), Ok(Duration::from_secs(3600)));
    assert!(parse_duration("").is_err());
    assert!(parse_duration("m").is_err());
    assert!(parse_duration("-1m").is_err());
    assert!(parse_duration(&format!("{}h", u64::MAX)).is_err());
}

#[test]
fn the_host_setting_and_the_shim_buffer() {
    let c = StationConfig::parse("", &env()).unwrap();
    assert_eq!(
        c.host,
        HostChoice::Direct,
        "direct until the owner chooses shim"
    );
    assert_eq!(c.shim_buffer_mib, DEFAULT_SHIM_BUFFER_MIB);
    let c = StationConfig::parse("host = \"shim\"\nshim_buffer_mib = 64", &env()).unwrap();
    assert_eq!(c.host, HostChoice::Shim);
    assert_eq!(c.shim_buffer_mib, 64);
    assert!(err("host = \"docker\"").contains("\"direct\" or \"shim\""));
    assert!(err("shim_buffer_mib = 0").contains("1 to 256"));
    assert!(err("shim_buffer_mib = 257").contains("1 to 256"));
}
