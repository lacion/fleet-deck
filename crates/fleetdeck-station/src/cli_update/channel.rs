//! The target of an update and the fetches that find it (design §3.5).
//!
//! Every fetch is `curl` with the config on stdin, built with
//! [`crate::integrations::http::config`]. The hosts are constants: no host
//! and no path comes from the wire or from a response, except a package name
//! that [`super::verify::platform_packages`] restricts to the `@openai/`
//! scope.

use std::path::Path;

use fleetdeck_proto::cli_update::{is_release, reason};
use serde_json::Value;

use super::verify::{
    self, Verified, VerifiedPackage, check_provenance, check_version_doc, platform_packages,
};
use crate::integrations::exec::{self, ExecError, Programs, Spec, curl_env};
use crate::integrations::http::{self, Request};

pub const CLAUDE_RELEASES: &str = "https://downloads.claude.ai/claude-code-releases";
pub const NPM_REGISTRY: &str = "https://registry.npmjs.org";
/// The npm name of Codex.
pub const CODEX_PACKAGE: &str = "@openai/codex";

const MAX_POINTER: usize = 64;
const MAX_MANIFEST: usize = 256 * 1024;
const MAX_MANIFEST_SIG: usize = 16 * 1024;
const MAX_DIST_TAGS: usize = 64 * 1024;
const MAX_VERSION_DOC: usize = 1024 * 1024;
const MAX_HEAD: usize = 4 * 1024;

/// The curl config of one GET (`head = false`) or one HEAD that prints only
/// the `Last-Modified` header (`head = true`). `max_bytes` is curl's
/// `max-filesize`.
#[must_use]
pub fn fetch_config(url: &str, max_bytes: usize, head: bool) -> String {
    let req = Request {
        url: url.to_owned(),
        method: if head { "HEAD" } else { "GET" },
        ..Request::default()
    };
    let mut out = http::config(&req);
    out.push_str(&format!("max-filesize = {max_bytes}\n"));
    if head {
        out.push_str("head\noutput = \"/dev/null\"\nwrite-out = \"%header{last-modified}\"\n");
    }
    out
}

async fn run_curl(
    programs: &Programs,
    url: &str,
    max: usize,
    head: bool,
) -> Result<Vec<u8>, &'static str> {
    let spec = Spec {
        program: programs.curl.clone(),
        args: http::ARGS.map(str::to_owned).to_vec(),
        cwd: None,
        env: curl_env(),
        stdin: Some(fetch_config(url, max, head).into_bytes()),
        max_stdout: max,
        timeout: exec::CALL_TIMEOUT,
    };
    match exec::run(spec).await {
        Ok(out) if out.code == 0 => Ok(out.stdout),
        Ok(_) | Err(ExecError::Spawn(_) | ExecError::Timeout | ExecError::TooLarge) => {
            Err(reason::CHANNEL_UNREACHABLE)
        }
    }
}

/// `target` of a pointer file: at most 64 bytes, trimmed, a release.
///
/// # Errors
/// `not_a_version`.
pub fn parse_pointer(bytes: &[u8]) -> Result<String, &'static str> {
    if bytes.len() > MAX_POINTER {
        return Err(reason::NOT_A_VERSION);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| reason::NOT_A_VERSION)?;
    let text = text.trim();
    if is_release(text) {
        Ok(text.to_owned())
    } else {
        Err(reason::NOT_A_VERSION)
    }
}

/// `dist-tags.latest` of an npm `dist-tags` answer.
///
/// # Errors
/// `not_a_version`.
pub fn parse_dist_tags(bytes: &[u8]) -> Result<String, &'static str> {
    let doc: Value = serde_json::from_slice(bytes).map_err(|_| reason::NOT_A_VERSION)?;
    doc.get("latest")
        .and_then(Value::as_str)
        .filter(|v| is_release(v))
        .map(str::to_owned)
        .ok_or(reason::NOT_A_VERSION)
}

/// Days from 1970-01-01 to a civil date (proleptic Gregorian).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn unix_ms(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> Option<i64> {
    let valid = (1..=12).contains(&mo)
        && (1..=31).contains(&d)
        && (0..=23).contains(&h)
        && (0..=59).contains(&mi)
        && (0..=60).contains(&s)
        && (1970..=9999).contains(&y);
    valid.then(|| (days_from_civil(y, mo, d) * 86_400 + h * 3_600 + mi * 60 + s) * 1000)
}

/// An RFC 3339 UTC time (`2026-10-22T19:32:59Z`, also with a fraction) as
/// Unix ms.
#[must_use]
pub fn parse_rfc3339(text: &str) -> Option<i64> {
    let text = text.trim().strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let time = time.split('.').next()?;
    let mut d = date.split('-');
    let (y, mo, da) = (d.next()?, d.next()?, d.next()?);
    let mut t = time.split(':');
    let (h, mi, s) = (t.next()?, t.next()?, t.next()?);
    if d.next().is_some() || t.next().is_some() {
        return None;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    unix_ms(num(y)?, num(mo)?, num(da)?, num(h)?, num(mi)?, num(s)?)
}

/// An RFC 1123 GMT date (`Fri, 23 Oct 2026 04:52:34 GMT`) as Unix ms.
#[must_use]
pub fn parse_http_date(text: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let rest = text.trim().split_once(", ")?.1;
    let mut words = rest.split_whitespace();
    let day = words.next()?.parse::<i64>().ok()?;
    let name = words.next()?;
    let month = MONTHS.iter().position(|m| *m == name)?;
    let year = words.next()?.parse::<i64>().ok()?;
    let time = words.next()?;
    if words.next()? != "GMT" || words.next().is_some() {
        return None;
    }
    let mut t = time.split(':');
    let h = t.next()?.parse::<i64>().ok()?;
    let mi = t.next()?.parse::<i64>().ok()?;
    let s = t.next()?.parse::<i64>().ok()?;
    if t.next().is_some() {
        return None;
    }
    unix_ms(year, i64::try_from(month).ok()? + 1, day, h, mi, s)
}

/// The Claude platform key: `<os>-<arch>`, and `-musl` on Linux with musl.
#[must_use]
pub fn platform_name(os: &str, arch: &str, musl: bool) -> Option<&'static str> {
    Some(match (os, arch, musl) {
        ("linux", "x64", false) => "linux-x64",
        ("linux", "x64", true) => "linux-x64-musl",
        ("linux", "arm64", false) => "linux-arm64",
        ("linux", "arm64", true) => "linux-arm64-musl",
        ("darwin", "x64", _) => "darwin-x64",
        ("darwin", "arm64", _) => "darwin-arm64",
        _ => return None,
    })
}

/// `(os, arch)` of this build, as the vendors name them.
#[must_use]
pub fn os_arch() -> Option<(&'static str, &'static str)> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => return None,
    };
    Some((os, arch))
}

/// The Claude platform key of this station.
#[must_use]
pub fn current_platform() -> Option<&'static str> {
    let (os, arch) = os_arch()?;
    let musl = os == "linux"
        && (Path::new("/lib/ld-musl-x86_64.so.1").exists()
            || Path::new("/lib/ld-musl-aarch64.so.1").exists());
    platform_name(os, arch, musl)
}

/// The URL of the native binary of `target` for `platform`, the form of the
/// vendor's `bootstrap.sh`. `None` for a `target` that is not a release.
#[must_use]
pub fn native_binary_url(target: &str, platform: &str) -> Option<String> {
    is_release(target).then(|| format!("{CLAUDE_RELEASES}/{target}/{platform}/claude"))
}

/// Reads the signed manifest: `version` must equal `target`; the checksum
/// of `platform` and `buildDate` as Unix ms.
///
/// # Errors
/// `version_mismatch`, `signature_invalid` (no valid checksum of the
/// platform).
pub fn parse_manifest(
    bytes: &[u8],
    target: &str,
    platform: &str,
) -> Result<(Option<i64>, String), &'static str> {
    let doc: Value = serde_json::from_slice(bytes).map_err(|_| reason::SIGNATURE_INVALID)?;
    if doc.get("version").and_then(Value::as_str) != Some(target) {
        return Err(reason::VERSION_MISMATCH);
    }
    let published = doc
        .get("buildDate")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339);
    let sum = doc
        .pointer(&format!("/platforms/{platform}/checksum"))
        .and_then(Value::as_str)
        .filter(|s| s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .ok_or(reason::SIGNATURE_INVALID)?;
    Ok((published, sum.to_owned()))
}

/// Steps 1 to 5 for native Claude: the target, the signed manifest and the
/// checksum of this platform.
///
/// # Errors
/// A code of [`reason`].
pub async fn resolve_claude(
    programs: &Programs,
    gpgv: &Path,
    tmp_root: &Path,
    pin: Option<&str>,
    channel: &str,
) -> Result<Verified, &'static str> {
    let target = match pin {
        Some(pin) if is_release(pin) => pin.to_owned(),
        Some(_) => return Err(reason::NOT_A_VERSION),
        None => {
            if !matches!(channel, "stable" | "latest") {
                return Err(reason::NOT_A_VERSION);
            }
            let url = format!("{CLAUDE_RELEASES}/{channel}");
            parse_pointer(&run_curl(programs, &url, MAX_POINTER + 1, false).await?)?
        }
    };
    let base = format!("{CLAUDE_RELEASES}/{target}");
    let manifest = run_curl(
        programs,
        &format!("{base}/manifest.json"),
        MAX_MANIFEST,
        false,
    )
    .await?;
    let sig = run_curl(
        programs,
        &format!("{base}/manifest.json.sig"),
        MAX_MANIFEST_SIG,
        false,
    )
    .await?;
    verify::verify_claude_manifest(gpgv, tmp_root, &manifest, &sig).await?;
    let platform = current_platform().ok_or(reason::SIGNATURE_INVALID)?;
    let (published_at, expected_sha256) = parse_manifest(&manifest, &target, platform)?;
    Ok(Verified::Native {
        target,
        published_at,
        platform,
        expected_sha256,
    })
}

async fn fetch_doc(programs: &Programs, name: &str, version: &str) -> Result<Value, &'static str> {
    let url = format!("{NPM_REGISTRY}/{name}/{version}");
    let bytes = run_curl(programs, &url, MAX_VERSION_DOC, false).await?;
    serde_json::from_slice(&bytes).map_err(|_| reason::SIGNATURE_INVALID)
}

/// Steps 6 to 12 for Codex: the target, every version document with its
/// signature, the provenance and the verified set.
///
/// # Errors
/// A code of [`reason`].
pub async fn resolve_codex(
    programs: &Programs,
    pin: Option<&str>,
) -> Result<Verified, &'static str> {
    let target = match pin {
        Some(pin) if is_release(pin) => pin.to_owned(),
        Some(_) => return Err(reason::NOT_A_VERSION),
        None => {
            let url = format!("{NPM_REGISTRY}/-/package/@openai%2fcodex/dist-tags");
            parse_dist_tags(&run_curl(programs, &url, MAX_DIST_TAGS, false).await?)?
        }
    };
    let main = fetch_doc(programs, CODEX_PACKAGE, &target).await?;
    let mut set: Vec<VerifiedPackage> = vec![check_version_doc(&main, CODEX_PACKAGE, &target)?];
    let (os, arch) = os_arch().ok_or(reason::SIGNATURE_INVALID)?;
    for (name, version) in platform_packages(&main, &target, os, arch)? {
        let doc = fetch_doc(programs, &name, &version).await?;
        let package = check_version_doc(&doc, &name, &version)?;
        if !set.contains(&package) {
            set.push(package);
        }
    }
    check_provenance(&main)?;
    let tarball = format!("{NPM_REGISTRY}/{CODEX_PACKAGE}/-/codex-{target}.tgz");
    let published_at = run_curl(programs, &tarball, MAX_HEAD, true)
        .await
        .ok()
        .and_then(|b| parse_http_date(&String::from_utf8_lossy(&b)));
    Ok(Verified::Npm {
        target,
        published_at,
        set,
    })
}

#[cfg(test)]
#[path = "channel_tests.rs"]
mod tests;
