use super::*;

const INJECTIONS: &[&str] = &[
    "a;reboot", "$(id)", "`id`", "a\nb", "a\r\nb", "-rf", "--exec=x", "a b", "a|b", "a&&b", "",
];

fn long() -> String {
    "x".repeat(100_000)
}

#[test]
fn a_monitor_must_equal_a_live_name() {
    let live = vec!["DP-1".to_owned(), "eDP-1".to_owned()];
    assert!(monitor("DP-1", &live).is_ok());
    for bad in ["dp-1", "DP-1 ", "DP-2", "", "DP-1;x", "$(id)", "-DP-1"] {
        assert_eq!(monitor(bad, &live).unwrap_err().code, "invalid_params");
    }
    assert!(monitor(&long(), &live).is_err());
}

#[test]
fn a_workspace_is_1_to_99() {
    assert_eq!(workspace(1).unwrap(), 1);
    assert_eq!(workspace(99).unwrap(), 99);
    assert!(workspace(0).is_err());
    assert!(workspace(100).is_err());
    assert!(workspace(255).is_err());
}

#[test]
fn an_address_is_0x_and_lower_hex() {
    for ok in ["0x1", "0xabcdef0123456789", "0x5a3e8a40"] {
        assert_eq!(address(ok).unwrap(), ok);
    }
    for bad in [
        "0x",
        "5a3e8a40",
        "0X1",
        "0xABC",
        "0x12345678901234567",
        "0xg",
        "0x1 ",
        "0x1\n",
        "0x1;x",
    ] {
        assert!(address(bad).is_err(), "{bad}");
    }
    for bad in INJECTIONS {
        assert!(address(bad).is_err(), "{bad}");
    }
    assert!(address(&long()).is_err());
}

#[test]
fn a_display_id_is_64_safe_characters() {
    for ok in ["wall", "a-b_C9", &"a".repeat(64)] {
        assert_eq!(display_id(ok).unwrap(), ok);
    }
    // A leading `-` is inside the grammar: the id is never an argv word of
    // its own, only a part of `wall-<id>` and a class name.
    assert_eq!(display_id("-rf").unwrap(), "-rf");
    for bad in INJECTIONS.iter().filter(|s| **s != "-rf") {
        assert!(display_id(bad).is_err(), "{bad}");
    }
    assert!(display_id(&"a".repeat(65)).is_err());
    assert!(display_id("a/b").is_err());
    assert!(display_id("a.b").is_err());
    assert!(display_id("é").is_err());
}

#[test]
fn notification_markup_is_removed_and_entities_are_decoded_once() {
    assert_eq!(plain_text("<b>Hi</b> there", 80), "Hi there");
    assert_eq!(
        plain_text("a &amp; b &lt;ok&gt; &quot;q&quot; &apos;", 80),
        "a & b ok \"q\" '"
    );
    assert_eq!(plain_text("&#65;&#x42;", 80), "AB");
    // One pass: `&amp;lt;` is `&lt;` and no more.
    assert_eq!(plain_text("&amp;lt;", 80), "&lt;");
    // A decoded tag never survives: its angle brackets are removed.
    assert_eq!(
        plain_text("&lt;script&gt;x&lt;/script&gt;", 80),
        "scriptx/script"
    );
    // An unclosed `<` is removed.
    assert_eq!(plain_text("a < b", 80), "a  b");
    assert_eq!(plain_text("1 <2 and 3> 4", 80), "1  4");
    // Unknown and broken entities stay as text.
    assert_eq!(
        plain_text("&nbsp; &#; &#xZZ; & &", 80),
        "&nbsp; &#; &#xZZ; & &"
    );
    // Control characters are removed.
    assert_eq!(plain_text("a\u{0}b\nc\u{7}d\u{1b}[31m", 80), "abcd[31m");
    assert_eq!(plain_text("&#10;x&#0;", 80), "x");
}

#[test]
fn notification_text_is_cut_by_characters() {
    assert_eq!(title(&"é".repeat(200)).unwrap().chars().count(), 80);
    assert_eq!(body(&"é".repeat(1000)).chars().count(), 300);
    assert_eq!(body(&long()).chars().count(), 300);
    assert_eq!(title("<b></b>").unwrap_err().code, "invalid_params");
    assert_eq!(body("<b></b>"), "");
    // Injection strings are plain text; the option terminator protects them.
    assert_eq!(title("-rf").unwrap(), "-rf");
    assert_eq!(title("$(id); x").unwrap(), "$(id); x");
}

#[test]
fn a_url_is_strict_http_or_https() {
    for ok in [
        "http://example.com",
        "https://example.com/",
        "https://a-b.example.com:8443/p?q=1&r=2#frag",
        "HTTPS://Example.com/x",
        "http://127.0.0.1:3000/#/display-pair",
        "http://localhost",
    ] {
        assert_eq!(url(ok).unwrap(), ok);
    }
    for bad in [
        "",
        "example.com",
        "ftp://example.com",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "http://",
        "http:///x",
        "http://exa mple.com",
        "http://example.com/a b",
        "http://example.com/\n",
        "http://example.com/\t",
        "http://user@example.com/",
        "http://user:pw@example.com/",
        "http://example.com:/",
        "http://example.com:0/",
        "http://example.com:99999/",
        "http://example.com:80a/",
        "http://exa_mple.com/",
        "http://example.com/é",
        "http://example.com/\u{7f}",
        "--app=http://example.com",
        "-http://example.com",
        "https:/example.com",
    ] {
        assert_eq!(url(bad).unwrap_err().code, "invalid_params", "{bad}");
    }
    let long_path = format!("https://example.com/{}", "a".repeat(2048));
    assert!(url(&long_path).is_err());
    let exactly = format!("https://example.com/{}", "a".repeat(2048 - 20));
    assert_eq!(exactly.len(), 2048);
    assert!(url(&exactly).is_ok());
    assert!(
        url("http://example.com/;$(id)").is_ok(),
        "a path is one argv element"
    );
}

#[test]
fn a_url_takes_a_bracketed_ipv6_host() {
    for ok in [
        "http://[::1]/",
        "https://[::1]:7420",
        "https://[::1]:7420/#/display-pair",
        "https://[2001:db8::1]/x?q=1",
        "http://[::ffff:192.0.2.1]:8080/",
        "HTTP://[FE80::1]",
    ] {
        assert_eq!(url(ok).unwrap(), ok);
    }
    for bad in [
        // Not an address.
        "http://[::g]/",
        "http://[]/",
        "http://[1.2.3.4]/",
        "http://[example.com]/",
        // A zone id is refused.
        "http://[fe80::1%eth0]/",
        // The bracket is not closed, or text follows it.
        "http://[::1/",
        "http://[::1]x/",
        "http://[::1]:/",
        "http://[::1]:0/",
        "http://[::1]:99999/",
        "http://[::1]:80a/",
        "http://[::1]::80/",
        // A bare IPv6 address has no brackets.
        "http://::1/",
        "http://2001:db8::1/",
        // No user info.
        "http://user@[::1]/",
    ] {
        assert_eq!(url(bad).unwrap_err().code, "invalid_params", "{bad}");
    }
}

#[test]
fn a_failure_never_shows_the_value() {
    let e = url("http://example.com/SECRET value").unwrap_err();
    assert!(!e.message.contains("SECRET"), "{}", e.message);
    let e = monitor("SECRET", &[]).unwrap_err();
    assert!(!e.message.contains("SECRET"), "{}", e.message);
}
