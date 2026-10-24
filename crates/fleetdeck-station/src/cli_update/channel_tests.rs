#![allow(clippy::unwrap_used)]

use super::*;

const MANIFEST: &[u8] = include_bytes!("testdata/manifest-2.1.309.json");
const CODEX_DOC: &str = include_str!("testdata/codex-0.163.0.json");
const BOOTSTRAP: &str = include_str!("testdata/bootstrap.sh");

#[test]
fn a_pointer_is_trimmed_and_a_pre_release_is_refused() {
    assert_eq!(parse_pointer(b"2.1.310\n"), Ok("2.1.310".to_owned()));
    assert_eq!(parse_pointer(b"  0.164.0  "), Ok("0.164.0".to_owned()));
    for bad in [
        &b"2.1.310-beta"[..],
        b"",
        b"latest",
        b"2.1",
        b"--help",
        &[0xff, 0xfe],
    ] {
        assert_eq!(parse_pointer(bad), Err(reason::NOT_A_VERSION));
    }
    let long = vec![b'1'; 65];
    assert_eq!(parse_pointer(&long), Err(reason::NOT_A_VERSION));
}

#[test]
fn dist_tags_give_the_latest_release_only() {
    let ok = br#"{"latest":"0.164.0","alpha":"0.165.0-alpha.1"}"#;
    assert_eq!(parse_dist_tags(ok), Ok("0.164.0".to_owned()));
    for bad in [
        &br#"{"alpha":"0.165.0-alpha.1"}"#[..],
        br#"{"latest":"0.165.0-alpha.1"}"#,
        br#"{"latest":7}"#,
        b"not json",
    ] {
        assert_eq!(parse_dist_tags(bad), Err(reason::NOT_A_VERSION));
    }
}

#[test]
fn rfc_3339_times_parse_to_unix_ms() {
    assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(
        parse_rfc3339("2026-10-22T19:32:59Z"),
        Some(1_792_697_579_000)
    );
    assert_eq!(
        parse_rfc3339("2026-10-22T19:32:59.123Z"),
        Some(1_792_697_579_000)
    );
    for bad in [
        "2026-10-22 19:32:59Z",
        "2026-10-22T19:32:59+02:00",
        "2026-13-01T00:00:00Z",
        "x",
    ] {
        assert_eq!(parse_rfc3339(bad), None, "{bad}");
    }
}

#[test]
fn rfc_1123_dates_parse_to_unix_ms() {
    assert_eq!(
        parse_http_date("Fri, 23 Oct 2026 04:52:34 GMT"),
        Some(1_792_731_154_000)
    );
    assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
    for bad in [
        "Fri, 23 Oct 2026 04:52:34 UTC",
        "Fri, 23 Foo 2026 04:52:34 GMT",
        "2026-10-23",
        "",
    ] {
        assert_eq!(parse_http_date(bad), None, "{bad}");
    }
}

#[test]
fn the_fetch_config_is_https_only_never_follows_and_holds_no_user_value() {
    let get = fetch_config(
        "https://registry.npmjs.org/@openai/codex/0.163.0",
        1024,
        false,
    );
    assert!(get.contains("proto = \"=https\""));
    assert!(get.contains("no-location"));
    assert!(get.contains("max-time = 20"));
    assert!(get.contains("max-filesize = 1024"));
    assert!(get.contains("url = \"https://registry.npmjs.org/@openai/codex/0.163.0\""));
    assert!(!get.contains("header"));
    assert!(!get.contains("data-binary"));
    let head = fetch_config("https://registry.npmjs.org/x.tgz", 4096, true);
    assert!(head.contains("head\n"));
    assert!(head.contains("%header{last-modified}"));
    assert!(head.contains("proto = \"=https\""));
}

#[test]
fn the_platform_key_follows_the_os_arch_and_musl() {
    assert_eq!(platform_name("linux", "x64", false), Some("linux-x64"));
    assert_eq!(platform_name("linux", "x64", true), Some("linux-x64-musl"));
    assert_eq!(
        platform_name("linux", "arm64", true),
        Some("linux-arm64-musl")
    );
    assert_eq!(
        platform_name("darwin", "arm64", false),
        Some("darwin-arm64")
    );
    assert_eq!(platform_name("darwin", "x64", true), Some("darwin-x64"));
    assert_eq!(platform_name("windows", "x64", false), None);
}

#[test]
fn the_manifest_gives_one_checksum_and_the_build_date() {
    let (published, sum) = parse_manifest(MANIFEST, "2.1.309", "linux-x64").unwrap();
    assert!(sum.starts_with("4d704a8a"));
    assert_eq!(sum.len(), 64);
    assert_eq!(published, parse_rfc3339("2026-10-22T19:32:59Z"));
    // Another platform has another checksum.
    let (_, other) = parse_manifest(MANIFEST, "2.1.309", "linux-x64-musl").unwrap();
    assert_ne!(sum, other);
    // A version that is not the target, a missing platform.
    assert_eq!(
        parse_manifest(MANIFEST, "2.1.310", "linux-x64"),
        Err(reason::VERSION_MISMATCH)
    );
    assert_eq!(
        parse_manifest(MANIFEST, "2.1.309", "plan9-x64"),
        Err(reason::SIGNATURE_INVALID)
    );
    assert_eq!(
        parse_manifest(b"{", "2.1.309", "linux-x64"),
        Err(reason::SIGNATURE_INVALID)
    );
}

#[test]
fn a_checksum_that_is_not_lower_case_hex_is_refused() {
    let bad = format!(
        r#"{{"version":"1.2.3","platforms":{{"linux-x64":{{"checksum":"{}"}}}}}}"#,
        "A".repeat(64)
    );
    assert_eq!(
        parse_manifest(bad.as_bytes(), "1.2.3", "linux-x64"),
        Err(reason::SIGNATURE_INVALID)
    );
}

#[test]
fn the_native_download_url_equals_the_form_of_the_vendor_script() {
    // The script downloads "$DOWNLOAD_BASE_URL/$version/$platform/claude".
    assert!(
        BOOTSTRAP
            .contains("DOWNLOAD_BASE_URL=\"https://downloads.claude.ai/claude-code-releases\"")
    );
    assert!(BOOTSTRAP.contains("\"$DOWNLOAD_BASE_URL/$version/$platform/claude\""));
    assert_eq!(
        native_binary_url("2.1.309", "linux-x64").as_deref(),
        Some("https://downloads.claude.ai/claude-code-releases/2.1.309/linux-x64/claude")
    );
    assert_eq!(native_binary_url("--help", "linux-x64"), None);
    assert_eq!(native_binary_url("2.1.309-beta", "linux-x64"), None);
}

#[test]
fn the_recorded_codex_document_names_the_pinned_registry_and_package() {
    let doc: Value = serde_json::from_str(CODEX_DOC).unwrap();
    assert_eq!(doc["name"], CODEX_PACKAGE);
    assert!(
        doc["dist"]["tarball"]
            .as_str()
            .unwrap()
            .starts_with(NPM_REGISTRY)
    );
}
