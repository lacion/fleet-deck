#![allow(clippy::unwrap_used)]

use super::*;

const MAIN: &str = include_str!("testdata/codex-0.163.0.json");
const PLATFORM: &str = include_str!("testdata/codex-0.163.0-linux-x64.json");
const MANIFEST: &[u8] = include_bytes!("testdata/manifest-2.1.309.json");
const MANIFEST_SIG: &[u8] = include_bytes!("testdata/manifest-2.1.309.json.sig");

const NAME: &str = "@openai/codex";

fn main_doc() -> Value {
    serde_json::from_str(MAIN).unwrap()
}

fn platform_doc() -> Value {
    serde_json::from_str(PLATFORM).unwrap()
}

#[test]
fn the_recorded_codex_documents_verify_and_give_the_verified_set() {
    let main = main_doc();
    let first = check_version_doc(&main, NAME, "0.163.0").unwrap();
    assert_eq!(first.name, NAME);
    assert_eq!(first.version, "0.163.0");
    assert!(first.integrity.starts_with("sha512-HNd7vR9o"));
    check_provenance(&main).unwrap();
    let platforms = platform_packages(&main, "0.163.0", "linux", "x64").unwrap();
    assert_eq!(
        platforms,
        vec![(NAME.to_owned(), "0.163.0-linux-x64".to_owned())]
    );
    let second = check_version_doc(&platform_doc(), NAME, "0.163.0-linux-x64").unwrap();
    assert!(second.integrity.starts_with("sha512-uwRaOsrY"));
}

#[test]
fn the_platform_packages_cover_glibc_and_musl_of_one_platform_only() {
    let doc = serde_json::json!({ "optionalDependencies": {
        "@openai/codex-linux-x64": "npm:@openai/codex@0.163.0-linux-x64",
        "@openai/codex-linux-x64-musl": "npm:@openai/codex@0.163.0-linux-x64-musl",
        "@openai/codex-linux-arm64": "npm:@openai/codex@0.163.0-linux-arm64",
        "@openai/codex-darwin-x64": "npm:@openai/codex@0.163.0-darwin-x64",
        "@openai/codex-win32-x64": "npm:@openai/codex@0.163.0-win32-x64"
    }});
    let mut got = platform_packages(&doc, "0.163.0", "linux", "x64").unwrap();
    got.sort();
    assert_eq!(
        got,
        vec![
            (NAME.to_owned(), "0.163.0-linux-x64".to_owned()),
            (NAME.to_owned(), "0.163.0-linux-x64-musl".to_owned()),
        ]
    );
}

#[test]
fn a_plain_spec_names_the_key_as_the_package() {
    let doc = serde_json::json!({ "optionalDependencies": {
        "@openai/codex-linux-x64": "0.163.0"
    }});
    assert_eq!(
        platform_packages(&doc, "0.163.0", "linux", "x64").unwrap(),
        vec![("@openai/codex-linux-x64".to_owned(), "0.163.0".to_owned())]
    );
}

#[test]
fn a_platform_spec_that_the_station_does_not_accept_is_refused() {
    for spec in [
        "npm:@openai/codex@0.164.0-linux-x64",
        "npm:@evil/codex@0.163.0-linux-x64",
        "npm:@openai/codex@0.163.0-LINUX",
        "npm:@openai/../codex@0.163.0-linux-x64",
        "npm:@openai/codex@0.163.0-",
        "https://evil.example/x.tgz",
    ] {
        let doc = serde_json::json!({ "optionalDependencies": {
            "@openai/codex-linux-x64": spec
        }});
        assert_eq!(
            platform_packages(&doc, "0.163.0", "linux", "x64"),
            Err(reason::UNVERIFIED_PACKAGE),
            "{spec}"
        );
    }
}

#[test]
fn one_changed_byte_in_the_integrity_fails_signature_invalid() {
    let mut doc = main_doc();
    let integrity = doc["dist"]["integrity"].as_str().unwrap().to_owned();
    let mut changed = integrity.clone();
    changed.replace_range(8..9, if &integrity[8..9] == "N" { "O" } else { "N" });
    doc["dist"]["integrity"] = changed.into();
    assert_eq!(
        check_version_doc(&doc, NAME, "0.163.0"),
        Err(reason::SIGNATURE_INVALID)
    );
}

#[test]
fn a_document_for_another_name_or_version_fails() {
    assert_eq!(
        check_version_doc(&main_doc(), NAME, "0.164.0"),
        Err(reason::SIGNATURE_INVALID)
    );
    assert_eq!(
        check_version_doc(&main_doc(), "@openai/other", "0.163.0"),
        Err(reason::SIGNATURE_INVALID)
    );
    let mut doc = main_doc();
    doc["dist"]["integrity"] = "sha1-abc".into();
    assert_eq!(
        check_version_doc(&doc, NAME, "0.163.0"),
        Err(reason::SIGNATURE_INVALID)
    );
    let mut doc = main_doc();
    doc["dist"]["signatures"] = serde_json::json!([]);
    assert_eq!(
        check_version_doc(&doc, NAME, "0.163.0"),
        Err(reason::SIGNATURE_INVALID)
    );
}

#[test]
fn an_unknown_keyid_fails_signature_unknown_key() {
    let mut doc = main_doc();
    for sig in doc["dist"]["signatures"].as_array_mut().unwrap() {
        sig["keyid"] = "SHA256:jl3bwswu80PjjokCgh0o2w5c2U4LhQAE57gj9cz1kzA".into();
    }
    assert_eq!(
        check_version_doc(&doc, NAME, "0.163.0"),
        Err(reason::SIGNATURE_UNKNOWN_KEY)
    );
}

#[test]
fn a_known_key_with_a_bad_signature_is_signature_invalid() {
    let mut doc = main_doc();
    for sig in doc["dist"]["signatures"].as_array_mut().unwrap() {
        sig["sig"] = "AAAA".into();
    }
    assert_eq!(
        check_version_doc(&doc, NAME, "0.163.0"),
        Err(reason::SIGNATURE_INVALID)
    );
}

#[test]
fn a_bad_spki_prefix_or_length_fails() {
    let (_, spki) = NPM_KEYS[0];
    let integrity = main_doc()["dist"]["integrity"].as_str().unwrap().to_owned();
    let sig = main_doc()["dist"]["signatures"][0]["sig"]
        .as_str()
        .unwrap()
        .to_owned();
    // The pinned key verifies.
    verify_npm_signature(NAME, "0.163.0", &integrity, &sig, spki).unwrap();
    // A changed prefix byte.
    let mut bytes = STANDARD.decode(spki).unwrap();
    bytes[10] ^= 1;
    assert_eq!(
        verify_npm_signature(NAME, "0.163.0", &integrity, &sig, &STANDARD.encode(&bytes)),
        Err(reason::SIGNATURE_INVALID)
    );
    // A short key.
    let short = STANDARD.encode(&STANDARD.decode(spki).unwrap()[..90]);
    assert_eq!(
        verify_npm_signature(NAME, "0.163.0", &integrity, &sig, &short),
        Err(reason::SIGNATURE_INVALID)
    );
    // Text that is not base64.
    assert_eq!(
        verify_npm_signature(NAME, "0.163.0", &integrity, &sig, "!!"),
        Err(reason::SIGNATURE_INVALID)
    );
}

#[test]
fn a_missing_provenance_fails() {
    let mut doc = main_doc();
    doc["dist"]["attestations"]["provenance"]["predicateType"] = "https://example.com/x".into();
    assert_eq!(check_provenance(&doc), Err(reason::PROVENANCE_MISSING));
    let mut doc = main_doc();
    doc["dist"].as_object_mut().unwrap().remove("attestations");
    assert_eq!(check_provenance(&doc), Err(reason::PROVENANCE_MISSING));
}

#[test]
fn the_validsig_lines_decide_the_gpgv_verdict() {
    let good = "[GNUPG:] NEWSIG\n[GNUPG:] GOODSIG BAA929FF1A7ECACE Anthropic Claude Code Release Signing <security@anthropic.com>\n\
[GNUPG:] VALIDSIG 31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE 2026-10-22 1792698427 0 4 0 1 10 00 31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE\n";
    assert_eq!(check_validsig(good, 0), Ok(()));
    // The exit code counts.
    assert_eq!(check_validsig(good, 1), Err(reason::SIGNATURE_INVALID));
    // Another key.
    let other = good.replace(
        "31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE",
        "0000000000000000000000000000000000000000",
    );
    assert_eq!(
        check_validsig(&other, 0),
        Err(reason::SIGNATURE_UNKNOWN_KEY)
    );
    // A GOODSIG without VALIDSIG, an empty answer, a similar line.
    assert_eq!(
        check_validsig("[GNUPG:] GOODSIG BAA929FF1A7ECACE x\n", 0),
        Err(reason::SIGNATURE_INVALID)
    );
    assert_eq!(check_validsig("", 0), Err(reason::SIGNATURE_INVALID));
    assert_eq!(
        check_validsig(
            "x [GNUPG:] VALIDSIG 31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE\n",
            0
        ),
        Err(reason::SIGNATURE_INVALID)
    );
}

#[test]
fn the_keyring_holds_the_pinned_key() {
    // The OpenPGP packet stream starts with a public key packet (tag 6).
    assert_eq!(CLAUDE_KEYRING[0] & 0xc0, 0x80);
    assert_eq!(CLAUDE_KEY_FPR.len(), 40);
}

fn gpgv_for_tests() -> Option<PathBuf> {
    crate::cli_update::detect::find_gpgv(std::env::var_os("PATH").as_deref())
}

#[tokio::test]
async fn the_recorded_claude_manifest_passes_gpgv_and_a_changed_one_fails() {
    let Some(gpgv) = gpgv_for_tests() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("cli-update/tmp");
    verify_claude_manifest(&gpgv, &root, MANIFEST, MANIFEST_SIG)
        .await
        .unwrap();
    let mut changed = MANIFEST.to_vec();
    let at = changed.iter().position(|b| *b == b'2').unwrap();
    changed[at] = b'3';
    assert_eq!(
        verify_claude_manifest(&gpgv, &root, &changed, MANIFEST_SIG).await,
        Err(reason::SIGNATURE_INVALID)
    );
    // The temp directories are gone.
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
}
