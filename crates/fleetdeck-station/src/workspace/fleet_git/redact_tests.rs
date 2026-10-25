use super::redact;

#[test]
fn the_last_non_empty_line_is_taken() {
    assert_eq!(
        redact("remote: first\nfatal: the end\n\n  \n"),
        "fatal: the end"
    );
    assert_eq!(redact(""), "");
    assert_eq!(redact("\n \n"), "");
}

#[test]
fn url_user_information_is_removed() {
    let out = redact("fatal: unable to access 'https://user:secret@example.com/acme/app.git/'");
    assert!(!out.contains("secret"), "{out}");
    assert!(!out.contains("user"), "{out}");
    assert!(out.contains("https://example.com/acme/app.git/"), "{out}");
}

#[test]
fn token_words_are_masked() {
    for token in [
        "ghp_abcdef123456",
        "gho_abcdef123456",
        "github_pat_11ABC_xyz",
        "glpat-abc123",
        "xoxb-1234-5678",
    ] {
        let out = redact(&format!("error: bad token {token} for the host"));
        assert!(!out.contains(token), "{out}");
        assert_eq!(out, "error: bad token [redacted] for the host");
    }
}

#[test]
fn a_quoted_token_is_masked() {
    let out = redact("error: 'ghp_abcdef123456' was refused");
    assert_eq!(out, "error: [redacted] was refused");
}

#[test]
fn the_value_after_bearer_basic_and_authorization_is_masked() {
    assert_eq!(
        redact("sent Bearer abc.def.ghi to the host"),
        "sent Bearer [redacted] to the host"
    );
    assert_eq!(
        redact("sent Basic dXNlcjpwYXNz now"),
        "sent Basic [redacted] now"
    );
    let out = redact("Authorization: s3cr3t");
    assert!(!out.contains("s3cr3t"), "{out}");
    let out = redact("Authorization: Bearer s3cr3t");
    assert!(!out.contains("s3cr3t"), "{out}");
}

#[test]
fn the_white_space_stays_and_control_characters_go() {
    assert_eq!(redact("a  b\t c"), "a  b c");
    assert_eq!(redact("a\u{1b}[31mred\u{7}"), "a[31mred");
}

#[test]
fn the_line_is_cut_to_300_characters() {
    let out = redact(&"é".repeat(500));
    assert_eq!(out.chars().count(), 300);
}
