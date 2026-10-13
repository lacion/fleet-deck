use std::path::Path;

use fleetdeck_proto::integrations::TrackerKind;

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
    assert!(
        !c.allow_shell,
        "the human shell is off unless the owner turns it on"
    );
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
    assert!(d.conflict_mail);
    let d = DeckConfig::parse(
        "name = \"home\"\nlisten = [\"100.101.1.2:7420\"]\nlink_listen = []\n\
         tailnet_hostname = \"Deck.tail1234.ts.net\"",
    )
    .unwrap();
    assert!(d.link_listen.is_empty());
    assert_eq!(d.tailnet_hostname.as_deref(), Some("deck.tail1234.ts.net"));
    assert!(d.conflict_mail);
    assert!(
        !DeckConfig::parse("conflict_mail = false")
            .unwrap()
            .conflict_mail
    );
    assert!(
        DeckConfig::parse("conflict_mail = true")
            .unwrap()
            .conflict_mail
    );
    assert!(DeckConfig::parse("conflict_mail = 1").is_err());
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
    // Multibyte or odd text is an error, never a panic (DL-136).
    for bad in ["é", "5é", "€", "5 m", "+5m", "5d", "s", "1.5h"] {
        assert!(parse_duration(bad).is_err(), "{bad}");
    }
    assert!(err("offline_approval_policy = \"deny_after(é)\"").contains("not a duration"));
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

#[test]
fn explicit_repo_groups_are_paths_with_names() {
    let c = StationConfig::parse(
        "[repo_groups]\n\"~/projects/app\" = \"fleet\"\n\"/w/lib\" = \" core \"",
        &env(),
    )
    .unwrap();
    assert_eq!(
        c.repo_groups
            .get(Path::new("/home/ana/projects/app"))
            .map(String::as_str),
        Some("fleet")
    );
    assert_eq!(
        c.repo_groups.get(Path::new("/w/lib")).map(String::as_str),
        Some("core")
    );
    assert!(err("[repo_groups]\n\"/w/a\" = \"  \"").contains("group name"));
    assert!(
        StationConfig::parse("", &env())
            .unwrap()
            .repo_groups
            .is_empty()
    );
}

#[test]
fn member_deny_adds_patterns_and_refuses_bad_ones() {
    let c = StationConfig::parse("member_deny = [\"*.tfstate\", \"private/\"]", &env()).unwrap();
    assert_eq!(c.member_deny, ["*.tfstate", "private/"]);
    assert!(
        StationConfig::parse("", &env())
            .unwrap()
            .member_deny
            .is_empty()
    );
    assert!(err("member_deny = [\"/etc\"]").contains("member_deny[0]"));
    assert!(err("member_deny = [\"a/../b\"]").contains("relative"));
    assert!(err("member_deny = [\"\"]").contains("1 to 200"));
}

/// DL-136: a TOML error names the file position but never echoes the
/// source line or a quoted value, because `env` can hold provider secrets.
#[test]
fn parse_errors_never_echo_config_values() {
    let canary = "SYNTHETIC_SECRET_CANARY";
    let unclosed = format!(
        "[[provider_instance]]\nid = \"test\"\ndriver = \"claude\"\nenv = {{ API_TOKEN = \"{canary}\",\n"
    );
    let wrong_type = format!("allow_shell = \"{canary}\"\n");
    let deck = format!("name = \"{canary}\"\nlisten = [\"{canary}\"\n");
    let number = "[[provider_instance]]\nid = 'test'\ndriver = 'claude'\nenv = { API_TOKEN = 1234567890123456789 }\n".to_owned();
    let float = "[[provider_instance]]\nid = 'test'\ndriver = 'claude'\nenv = { API_TOKEN = 1234567890.12345 }\n".to_owned();
    let escaped = format!("allow_shell = 'prefix\"{canary}'\n");
    let variant = format!("[[provider_instance]]\nid = 'test'\ndriver = '{canary}'\n");
    let key = format!("{canary} = 1\n");
    for text in [&number, &float] {
        let e = err(text);
        assert!(!e.contains("1234567890"), "{e}");
        let debug = format!("{:?}", StationConfig::parse(text, &env()).unwrap_err());
        assert!(!debug.contains("1234567890"), "{debug}");
        assert!(e.contains("invalid type"), "{e}");
    }
    for text in [&escaped, &variant, &key] {
        let e = err(text);
        assert!(!e.contains(canary), "{e}");
        let debug = format!("{:?}", StationConfig::parse(text, &env()).unwrap_err());
        assert!(!debug.contains(canary), "{debug}");
    }
    assert!(err(&variant).contains("unknown variant"));
    for text in [&unclosed, &wrong_type] {
        let e = err(text);
        assert!(!e.contains(canary), "{e}");
        assert!(e.contains("line "), "{e}");
    }
    let e = DeckConfig::parse(&deck).unwrap_err().to_string();
    assert!(!e.contains(canary), "{e}");
    assert!(
        err(&unclosed).contains("line 4, column"),
        "{}",
        err(&unclosed)
    );
}

#[test]
fn hibernate_after_min_is_off_by_default_and_bounded() {
    let c = StationConfig::parse("", &env()).unwrap();
    assert_eq!(c.hibernate_after_min, 0);
    let c = StationConfig::parse("hibernate_after_min = 45", &env()).unwrap();
    assert_eq!(c.hibernate_after_min, 45);
    let c = StationConfig::parse("hibernate_after_min = 10080", &env()).unwrap();
    assert_eq!(c.hibernate_after_min, MAX_HIBERNATE_AFTER_MIN);
    assert!(err("hibernate_after_min = 10081").contains("hibernate_after_min"));
}

const TRACKERS: &str = r#"
[[trackers]]
id = "acme"
kind = "jira"
site = "https://acme.atlassian.net/"
email = "ana@acme.test"
token_env = "JIRA_API_TOKEN"

[[trackers]]
id = "lin"
kind = "linear"
token_file = "~/.config/fleetdeck/linear.token"
"#;

#[test]
fn trackers_parse_with_a_site_an_email_and_a_token_source() {
    let c = StationConfig::parse(TRACKERS, &env()).unwrap();
    assert_eq!(c.trackers.len(), 2);
    let jira = &c.trackers[0];
    assert_eq!(jira.kind, TrackerKind::Jira);
    assert_eq!(jira.site.as_deref(), Some("https://acme.atlassian.net"));
    assert_eq!(jira.email.as_deref(), Some("ana@acme.test"));
    assert_eq!(jira.token, TokenSource::Env("JIRA_API_TOKEN".into()));
    let linear = &c.trackers[1];
    assert_eq!(linear.site, None);
    assert_eq!(
        linear.token,
        TokenSource::File(PathBuf::from("/home/ana/.config/fleetdeck/linear.token"))
    );
    assert!(
        StationConfig::parse("", &env())
            .unwrap()
            .trackers
            .is_empty()
    );
}

#[test]
fn a_bad_tracker_entry_is_refused() {
    let jira = |extra: &str| {
        format!(
            "[[trackers]]\nid = \"a\"\nkind = \"jira\"\nsite = \"https://x.test\"\nemail = \"a@x.test\"\n{extra}"
        )
    };
    assert!(StationConfig::parse(&jira("token_env = \"T\""), &env()).is_ok());
    for bad in [
        jira(""),
        jira("token_env = \"T\"\ntoken_file = \"/t\""),
        jira("token_env = \"has space\""),
        jira("token_file = \"relative\""),
        "[[trackers]]\nid = \"a\"\nkind = \"jira\"\nemail = \"a@x\"\ntoken_env = \"T\"".to_owned(),
        "[[trackers]]\nid = \"a\"\nkind = \"jira\"\nsite = \"http://x.test\"\nemail = \"a@x\"\ntoken_env = \"T\"".to_owned(),
        "[[trackers]]\nid = \"a\"\nkind = \"jira\"\nsite = \"https://x.test/path\"\nemail = \"a@x\"\ntoken_env = \"T\"".to_owned(),
        "[[trackers]]\nid = \"a\"\nkind = \"linear\"\nsite = \"https://x.test\"\ntoken_env = \"T\"".to_owned(),
        "[[trackers]]\nid = \"A!\"\nkind = \"linear\"\ntoken_env = \"T\"".to_owned(),
        "[[trackers]]\nid = \"a\"\nkind = \"svn\"\ntoken_env = \"T\"".to_owned(),
        format!("{}{}", jira("token_env = \"T\"\n"), jira("token_env = \"T\"")),
    ] {
        assert!(StationConfig::parse(&bad, &env()).is_err(), "{bad}");
    }
}

#[test]
fn a_token_file_must_be_private_and_clean() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t");
    std::fs::write(&path, "  secret-token\n").unwrap();
    let tracker = TrackerConfig {
        id: "a".into(),
        kind: TrackerKind::Linear,
        site: None,
        email: None,
        token: TokenSource::File(path.clone()),
    };
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(tracker.read_token(), Err(TokenError::Mode { .. })));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(tracker.read_token().unwrap().expose(), "secret-token");
    std::fs::write(&path, "bad\ntoken").unwrap();
    assert!(matches!(tracker.read_token(), Err(TokenError::Bad)));
    std::fs::write(&path, "  \n").unwrap();
    assert!(matches!(tracker.read_token(), Err(TokenError::Bad)));
    let missing = TrackerConfig {
        token: TokenSource::Env("FLEETDECK_TEST_NO_SUCH_VARIABLE".into()),
        ..tracker
    };
    assert!(matches!(
        missing.read_token(),
        Err(TokenError::EnvMissing(_))
    ));
}

#[test]
fn model_help_defaults_are_on_with_the_owner_budget() {
    let c = StationConfig::parse("", &env()).unwrap();
    assert_eq!(c.model_help, ModelHelpConfig::default());
    assert!(c.model_help.enabled);
    assert_eq!(c.model_help.claude_model, "claude-haiku-5-5");
    assert_eq!(c.model_help.codex_model, "gpt-5.6-luna");
    assert_eq!(c.model_help.provider, None);
    assert_eq!(c.model_help.per_call_max_output_tokens, 400);
    assert_eq!(c.model_help.daily_token_cap, 200_000);
}

#[test]
fn a_model_help_table_sets_each_field() {
    let c = StationConfig::parse(
        r#"
[model_help]
enabled = false
claude_model = "claude-haiku-5-5"
codex_model = "luna-2"
provider = "codex"
per_call_max_output_tokens = 120
daily_token_cap = 5000
"#,
        &env(),
    )
    .unwrap();
    assert!(!c.model_help.enabled);
    assert_eq!(c.model_help.codex_model, "luna-2");
    assert_eq!(c.model_help.provider, Some(Driver::Codex));
    assert_eq!(c.model_help.per_call_max_output_tokens, 120);
    assert_eq!(c.model_help.daily_token_cap, 5000);
}

#[test]
fn a_bad_model_help_value_is_refused() {
    for (text, field) in [
        ("[model_help]\nclaude_model = \"--help\"", "claude_model"),
        ("[model_help]\ncodex_model = \"a b\"", "codex_model"),
        ("[model_help]\nclaude_model = \"\"", "claude_model"),
        (
            "[model_help]\nper_call_max_output_tokens = 0",
            "per_call_max_output_tokens",
        ),
        ("[model_help]\ndaily_token_cap = 0", "daily_token_cap"),
    ] {
        let message = err(text);
        assert!(message.contains(field), "{text}: {message}");
    }
    assert!(err("[model_help]\nprovider = \"pi\"").contains("unknown variant"));
    assert!(err("[model_help]\nbogus = 1").contains("unknown field"));
}
