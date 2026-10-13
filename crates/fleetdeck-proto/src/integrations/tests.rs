//! Tests of the integration types and their field checks.

use super::*;

fn forge() -> ForgeBinding {
    ForgeBinding {
        kind: ForgeKind::Gitlab,
        host: "gitlab.com".into(),
        repo: "group/sub/project".into(),
    }
}

#[test]
fn bindings_check_their_fields() {
    assert!(forge().check().is_ok());
    let mut bad = forge();
    bad.repo = "../etc/passwd".into();
    assert!(bad.check().is_err());
    bad.repo = "solo".into();
    assert!(bad.check().is_err());
    bad.repo = "a/-flag".into();
    assert!(bad.check().is_err());
    let mut bad = forge();
    bad.host = "evil.com/x".into();
    assert!(bad.check().is_err());
    bad.host = "host:8443".into();
    assert!(bad.check().is_ok());
}

#[test]
fn issue_keys_follow_the_binding() {
    let t = Binding::Tracker(TrackerBinding {
        tracker: "acme".into(),
        kind: TrackerKind::Jira,
        key: "PROJ".into(),
    });
    let f = Binding::Forge(forge());
    assert!(valid_issue_key(&t, "PROJ-123"));
    assert!(!valid_issue_key(&t, "PROJ-"));
    assert!(!valid_issue_key(&t, "#12"));
    assert!(valid_issue_key(&f, "#12"));
    assert!(valid_issue_key(&f, "12"));
    assert!(!valid_issue_key(&f, "PROJ-1"));
    assert_eq!(issue_number("#12"), Some(12));
}

#[test]
fn branch_names_refuse_flags_and_ref_syntax() {
    assert!(valid_branch("PROJ-1-fix-login"));
    assert!(valid_branch("feature/x"));
    for bad in ["-x", "a b", "a..b", "a:b", "a~1", "x/", "", "a@{1}"] {
        assert!(!valid_branch(bad), "{bad}");
    }
}

#[test]
fn a_write_checks_its_id_and_text() {
    let comment = |id: &str, body: &str| PrComment {
        command_id: id.into(),
        forge: forge(),
        number: 3,
        body: body.into(),
    };
    assert!(comment("c-1", "hello").check().is_ok());
    assert!(comment("", "hello").check().is_err());
    assert!(comment("a b", "hello").check().is_err());
    assert!(comment("c-1", "  ").check().is_err());
    assert!(
        comment("c-1", &"x".repeat(MAX_BODY_CHARS + 1))
            .check()
            .is_err()
    );
    let approve = PrReview {
        command_id: "r-1".into(),
        forge: forge(),
        number: 3,
        event: ReviewEvent::Approve,
        body: String::new(),
    };
    assert!(approve.check().is_ok());
    let request = PrReview {
        event: ReviewEvent::RequestChanges,
        ..approve
    };
    assert!(request.check().is_err(), "a change request needs text");
}

#[test]
fn the_search_limit_is_bounded() {
    let search = |limit| IssueSearch {
        binding: Binding::Forge(forge()),
        query: None,
        mine: false,
        status: None,
        limit,
    };
    assert!(search(None).check().is_ok());
    assert_eq!(search(None).limit(), DEFAULT_SEARCH_LIMIT);
    assert!(search(Some(0)).check().is_err());
    assert!(search(Some(101)).check().is_err());
}

#[test]
fn an_issue_ref_checks_its_fields() {
    let ok = IssueRef {
        system: "jira".into(),
        key: "PROJ-1".into(),
        url: "https://acme.atlassian.net/browse/PROJ-1".into(),
        title: "Fix login".into(),
    };
    assert!(ok.check().is_ok());
    for bad in [
        IssueRef {
            system: "svn".into(),
            ..ok.clone()
        },
        IssueRef {
            url: "javascript:alert(1)".into(),
            ..ok.clone()
        },
        IssueRef {
            title: "a\nb".into(),
            ..ok.clone()
        },
        IssueRef {
            key: String::new(),
            ..ok.clone()
        },
    ] {
        assert!(bad.check().is_err(), "{bad:?}");
    }
}

#[test]
fn an_unknown_field_fails_closed() {
    let r = serde_json::from_value::<PrRef>(serde_json::json!({
        "forge": {"kind": "github", "host": "github.com", "repo": "a/b"},
        "number": 1, "extra": true
    }));
    assert!(r.is_err());
}
