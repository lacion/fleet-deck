//! The signature checks of an update target (design §3.5, steps 3, 7 to 10).
//!
//! Claude: the manifest is checked with `gpgv` against the pinned release
//! key. Codex: each npm version document is checked against the pinned npm
//! registry key with `ring`. Nothing here fetches; [`super::channel`] does.

use std::ffi::OsString;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_proto::cli_update::reason;
use serde_json::Value;

use crate::integrations::exec::{self, Spec};

/// The fingerprint of the Claude Code release signing key (vendor docs).
pub const CLAUDE_KEY_FPR: &str = "31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE";
/// The same key, dearmored (`gpg --dearmor` of `claude-code.asc`).
pub const CLAUDE_KEYRING: &[u8] = include_bytes!("keys/claude-code.gpg");
/// The npm registry signing keys (`/-/npm/v1/keys`), keyid to SPKI DER
/// (base64).
pub const NPM_KEYS: &[(&str, &str)] = &[(
    "SHA256:DhQ8wR5APBvFHLF/+Tc+AYvPOdTpcIDqOhxsBHRwC7U",
    "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEY6Ya7W++7aUPzvMTrezH6Ycx3c+HOKYCcNGybJZSCJq/fd7Qa8uuAKtdIkUQtQiEKERhAmE5lMMJhP8OkDOa2g==",
)];

/// The DER prefix of an ECDSA P-256 `SubjectPublicKeyInfo`; 65 bytes of
/// point follow.
const SPKI_PREFIX: [u8; 26] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a,
    0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
];

/// The time limit of the `gpgv` run.
const GPGV_TIMEOUT: Duration = Duration::from_secs(20);

/// The result of §3.5 (revision 2). No path, no URL from a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verified {
    Native {
        target: String,
        published_at: Option<i64>,
        platform: &'static str,
        expected_sha256: String,
    },
    Npm {
        target: String,
        published_at: Option<i64>,
        set: Vec<VerifiedPackage>,
    },
}

/// One package of the verified set: its signed integrity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPackage {
    pub name: String,
    pub version: String,
    pub integrity: String,
}

/// The verdict of the `--status-fd` lines of `gpgv` and its exit code. It
/// passes only with exit 0 and a `VALIDSIG` whose last field is
/// [`CLAUDE_KEY_FPR`].
///
/// # Errors
/// `signature_unknown_key` for a `VALIDSIG` of another key,
/// `signature_invalid` for anything else.
pub fn check_validsig(status: &str, exit_code: i32) -> Result<(), &'static str> {
    let mut other_key = false;
    for line in status.lines() {
        let Some(rest) = line.strip_prefix("[GNUPG:] VALIDSIG ") else {
            continue;
        };
        if rest.split_whitespace().last() == Some(CLAUDE_KEY_FPR) {
            return if exit_code == 0 {
                Ok(())
            } else {
                Err(reason::SIGNATURE_INVALID)
            };
        }
        other_key = true;
    }
    Err(if other_key && exit_code == 0 {
        reason::SIGNATURE_UNKNOWN_KEY
    } else {
        reason::SIGNATURE_INVALID
    })
}

/// A new private directory (0700) under `root`.
fn private_dir(root: &Path) -> std::io::Result<PathBuf> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = root.join(format!("v{}-{nanos}", std::process::id()));
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    Ok(dir)
}

fn write_inputs(dir: &Path, manifest: &[u8], sig: &[u8]) -> std::io::Result<()> {
    std::fs::write(dir.join("claude-code.gpg"), CLAUDE_KEYRING)?;
    std::fs::write(dir.join("manifest.json"), manifest)?;
    std::fs::write(dir.join("manifest.json.sig"), sig)?;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(dir.join("gnupg"))
}

/// Checks `manifest` and its detached `sig` with `gpgv` and the pinned key,
/// in a private temp directory under `tmp_root` that is removed after.
///
/// # Errors
/// `signature_unknown_key` or `signature_invalid`.
pub async fn verify_claude_manifest(
    gpgv: &Path,
    tmp_root: &Path,
    manifest: &[u8],
    sig: &[u8],
) -> Result<(), &'static str> {
    let dir = private_dir(tmp_root).map_err(|_| reason::SIGNATURE_INVALID)?;
    let verdict = run_gpgv(gpgv, &dir, manifest, sig).await;
    let _ = std::fs::remove_dir_all(&dir);
    verdict
}

async fn run_gpgv(
    gpgv: &Path,
    dir: &Path,
    manifest: &[u8],
    sig: &[u8],
) -> Result<(), &'static str> {
    write_inputs(dir, manifest, sig).map_err(|_| reason::SIGNATURE_INVALID)?;
    let text = |name: &str| dir.join(name).to_string_lossy().into_owned();
    let mut env: Vec<(OsString, OsString)> = vec![("LANG".into(), "C".into())];
    if let Some(path) = std::env::var_os("PATH") {
        env.push(("PATH".into(), path));
    }
    let spec = Spec {
        program: gpgv.to_path_buf(),
        args: vec![
            "--homedir".into(),
            text("gnupg"),
            "--keyring".into(),
            text("claude-code.gpg"),
            "--status-fd".into(),
            "1".into(),
            text("manifest.json.sig"),
            text("manifest.json"),
        ],
        cwd: None,
        env,
        stdin: None,
        max_stdout: 64 * 1024,
        timeout: GPGV_TIMEOUT,
    };
    let out = exec::run(spec)
        .await
        .map_err(|_| reason::SIGNATURE_INVALID)?;
    check_validsig(&String::from_utf8_lossy(&out.stdout), out.code)
}

/// Checks one ECDSA P-256 signature of the npm registry over
/// `<name>@<version>:<integrity>`.
///
/// # Errors
/// `signature_invalid`.
pub fn verify_npm_signature(
    name: &str,
    version: &str,
    integrity: &str,
    sig_b64: &str,
    spki_b64: &str,
) -> Result<(), &'static str> {
    let spki = STANDARD
        .decode(spki_b64)
        .map_err(|_| reason::SIGNATURE_INVALID)?;
    if spki.len() != 91 || spki[..SPKI_PREFIX.len()] != SPKI_PREFIX {
        return Err(reason::SIGNATURE_INVALID);
    }
    let sig = STANDARD
        .decode(sig_b64)
        .map_err(|_| reason::SIGNATURE_INVALID)?;
    let point = &spki[spki.len() - 65..];
    let message = format!("{name}@{version}:{integrity}");
    ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_ASN1, point)
        .verify(message.as_bytes(), &sig)
        .map_err(|_| reason::SIGNATURE_INVALID)
}

/// Checks a version document: `name` and `version` match, the integrity is
/// SHA-512, and one signature of a pinned key verifies.
///
/// # Errors
/// `signature_invalid` (a mismatch, no signature, a bad signature) or
/// `signature_unknown_key` (every signature names a key that is not pinned).
pub fn check_version_doc(
    doc: &Value,
    name: &str,
    version: &str,
) -> Result<VerifiedPackage, &'static str> {
    if doc.get("name").and_then(Value::as_str) != Some(name)
        || doc.get("version").and_then(Value::as_str) != Some(version)
    {
        return Err(reason::SIGNATURE_INVALID);
    }
    let dist = doc.get("dist").ok_or(reason::SIGNATURE_INVALID)?;
    let integrity = dist
        .get("integrity")
        .and_then(Value::as_str)
        .filter(|i| i.starts_with("sha512-"))
        .ok_or(reason::SIGNATURE_INVALID)?;
    let signatures = dist
        .get("signatures")
        .and_then(Value::as_array)
        .filter(|s| !s.is_empty())
        .ok_or(reason::SIGNATURE_INVALID)?;
    let mut known_key = false;
    for entry in signatures {
        let keyid = entry.get("keyid").and_then(Value::as_str).unwrap_or("");
        let sig = entry.get("sig").and_then(Value::as_str).unwrap_or("");
        let Some((_, spki)) = NPM_KEYS.iter().find(|(id, _)| *id == keyid) else {
            continue;
        };
        known_key = true;
        if verify_npm_signature(name, version, integrity, sig, spki).is_ok() {
            return Ok(VerifiedPackage {
                name: name.to_owned(),
                version: version.to_owned(),
                integrity: integrity.to_owned(),
            });
        }
    }
    Err(if known_key {
        reason::SIGNATURE_INVALID
    } else {
        reason::SIGNATURE_UNKNOWN_KEY
    })
}

/// Step 10: the main version document names a SLSA provenance attestation
/// (the presence only, not the Sigstore bundle).
///
/// # Errors
/// `provenance_missing`.
pub fn check_provenance(doc: &Value) -> Result<(), &'static str> {
    let kind = doc
        .pointer("/dist/attestations/provenance/predicateType")
        .and_then(Value::as_str);
    if kind == Some("https://slsa.dev/provenance/v1") {
        Ok(())
    } else {
        Err(reason::PROVENANCE_MISSING)
    }
}

/// Whether `text` is a safe package suffix: `[a-z0-9-]{1,32}`.
fn is_suffix(text: &str) -> bool {
    (1..=32).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Whether `name` is a package name that the station puts in a URL path:
/// the `@openai/` scope and a short safe name.
fn is_openai_name(name: &str) -> bool {
    name.strip_prefix("@openai/").is_some_and(|rest| {
        (1..=64).contains(&rest.len())
            && rest.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.' | b'_')
            })
    })
}

/// Step 9: the platform packages that the main version document lists, as
/// `(name, version)`, without duplicates. `os` and `arch` are `linux` and
/// `x64`; both the glibc and the `-musl` keys match.
///
/// # Errors
/// `unverified_package` for an entry that names a package or a version that
/// the station does not accept.
pub fn platform_packages(
    main: &Value,
    target: &str,
    os: &str,
    arch: &str,
) -> Result<Vec<(String, String)>, &'static str> {
    let Some(deps) = main.get("optionalDependencies").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    let plain = format!("-{os}-{arch}");
    let musl = format!("-{os}-{arch}-musl");
    let mut out: Vec<(String, String)> = Vec::new();
    for (key, spec) in deps {
        if !(key.ends_with(&plain) || key.ends_with(&musl)) {
            continue;
        }
        let spec = spec.as_str().ok_or(reason::UNVERIFIED_PACKAGE)?;
        let (name, version) = match spec.strip_prefix("npm:") {
            Some(alias) => alias
                .rsplit_once('@')
                .map(|(n, v)| (n.to_owned(), v.to_owned()))
                .ok_or(reason::UNVERIFIED_PACKAGE)?,
            None => (key.clone(), spec.to_owned()),
        };
        let suffix_ok = version == target
            || version
                .strip_prefix(target)
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(is_suffix);
        if !is_openai_name(&name) || !suffix_ok {
            return Err(reason::UNVERIFIED_PACKAGE);
        }
        if !out.contains(&(name.clone(), version.clone())) {
            out.push((name, version));
        }
    }
    Ok(out)
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;
