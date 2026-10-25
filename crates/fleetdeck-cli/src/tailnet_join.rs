//! `fleetdeck station join --tailnet <host[:port]>` (DL-178, tailnet design
//! 5.2): a station asks a deck on the tailnet to pair, and the owner
//! approves on the board.
//!
//! The running station does the work; this command is a client of its local
//! socket, like `station join` with an invitation. The station owns the
//! retry file `pair-pending.json`.
//!
//! - With `--deck-fingerprint` (64 hex) the station pairs pinned to that
//!   link key fingerprint.
//! - Without it, the command asks the station to **observe** the key of the
//!   deck (a TLS connect, no request, no write), prints all 64 hex digits
//!   and asks "Does this match the link key fingerprint on the board?
//!   [y/N]". It needs a terminal for that.
//! - `--cancel` removes the retry file only.
//!
//! A refusal prints `<code>: <detail>` and the exit code is 1. A success
//! prints `paired with <deck name>`.

use std::io::{BufRead, Write};
use std::net::IpAddr;
use std::os::fd::AsFd;
use std::time::Duration;

use anyhow::{Context, bail};
use fleetdeck_core::config::StationConfig;
use fleetdeck_core::paths::{Env, NodeDirs};
use fleetdeck_proto::link::Scope;
use fleetdeck_station::link::runtime::DECK_SCOPES;
use fleetdeck_station::local::{LocalClient, LocalRequest, LocalResponse, socket_path};

use crate::cli::JoinArgs;

/// The link port of a deck when `--tailnet` gives none.
pub const DEFAULT_PORT: u16 = 7421;

/// The longest wait for the answer of the station: it waits at most 11
/// minutes for the owner.
const WAIT: Duration = Duration::from_secs(11 * 60 + 30);

/// A refusal with a stable code. `main` prints it as `<code>: <detail>`
/// and exits with 1.
#[derive(Debug)]
pub struct Refused {
    pub code: String,
    pub detail: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for Refused {}

fn refuse(code: &str, detail: impl Into<String>) -> anyhow::Error {
    Refused {
        code: code.to_owned(),
        detail: detail.into(),
    }
    .into()
}

/// `host[:port]`, an IPv6 address in brackets when it has a port.
pub fn parse_target(text: &str) -> anyhow::Result<(String, u16)> {
    let text = text.trim();
    let bad = || {
        refuse(
            "invalid_request",
            format!("`{text}` is not <host>[:<port>]"),
        )
    };
    if text.is_empty() {
        return Err(bad());
    }
    if let Ok(ip) = text.parse::<IpAddr>() {
        return Ok((ip.to_string(), DEFAULT_PORT));
    }
    let (host, port) = if let Some(rest) = text.strip_prefix('[') {
        let (host, tail) = rest.split_once(']').ok_or_else(bad)?;
        match tail {
            "" => (host, None),
            t => (host, Some(t.strip_prefix(':').ok_or_else(bad)?)),
        }
    } else {
        match text.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (host, Some(port)),
            Some(_) => return Err(bad()),
            None => (text, None),
        }
    };
    if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad());
    }
    let port = match port {
        None => DEFAULT_PORT,
        Some(p) => p.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(bad)?,
    };
    Ok((host.to_owned(), port))
}

/// 64 lowercase hex digits as the 32 bytes of the fingerprint.
pub fn parse_fingerprint(text: &str) -> anyhow::Result<[u8; 32]> {
    let text = text.trim();
    let bad = || {
        refuse(
            "invalid_request",
            "--deck-fingerprint is the full link key fingerprint: 64 lowercase hex digits",
        )
    };
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(bad());
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(text.get(2 * i..2 * i + 2).ok_or_else(bad)?, 16)
            .map_err(|_| bad())?;
    }
    Ok(out)
}

/// The scopes of `--scopes`: any of read, drive, approve, admin.
pub fn parse_scopes(list: &[String]) -> anyhow::Result<Vec<Scope>> {
    let mut out = Vec::new();
    for item in list.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let scope = match item {
            "read" => Scope::Read,
            "drive" => Scope::Drive,
            "approve" => Scope::Approve,
            "admin" => Scope::Admin,
            other => {
                return Err(refuse(
                    "invalid_request",
                    format!("`{other}` is not a scope: use read, drive, approve or admin"),
                ));
            }
        };
        if !out.contains(&scope) {
            out.push(scope);
        }
    }
    if out.is_empty() {
        return Err(refuse("invalid_request", "--scopes names no scope"));
    }
    Ok(out)
}

/// The name of `--name`: 1 to 64 characters, no control or bidi character.
pub fn check_name(name: &str) -> anyhow::Result<()> {
    fleetdeck_link::pair_request::validate_name(name)
        .map_err(|e| refuse("invalid_request", format!("--name: {e}")))
}

async fn call(dirs: &NodeDirs, request: &LocalRequest) -> anyhow::Result<LocalResponse> {
    let path = socket_path(&dirs.station());
    let mut client = LocalClient::connect(&path).await?;
    match tokio::time::timeout(WAIT, client.call(request)).await {
        Ok(r) => Ok(r?),
        Err(_) => Err(refuse(
            "timeout",
            "the station gave no answer in 11 minutes",
        )),
    }
}

/// The yes of the question; anything else is a no.
fn confirmed(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn has_terminal() -> bool {
    let stdin = std::io::stdin();
    rustix::termios::isatty(stdin.as_fd())
}

fn ask_to_confirm(fingerprint: &str) -> anyhow::Result<bool> {
    let mut err = std::io::stderr().lock();
    writeln!(
        err,
        "The deck shows this link key fingerprint:\n  {fingerprint}"
    )?;
    write!(
        err,
        "Does this match the link key fingerprint on the board? [y/N] "
    )?;
    err.flush()?;
    drop(err);
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(confirmed(&line))
}

/// What the station answered, as the exit of this command.
fn finish(response: LocalResponse) -> anyhow::Result<LocalResponse> {
    match response {
        LocalResponse::TailnetRefused { code, detail } => Err(refuse(&code, detail)),
        LocalResponse::Error { message } => Err(refuse("error", message)),
        other => Ok(other),
    }
}

/// `fleetdeck station join --tailnet ...`.
pub async fn run(dirs: &NodeDirs, env: &Env, args: &JoinArgs) -> anyhow::Result<()> {
    if args.cancel {
        return cancel(dirs).await;
    }
    let Some(Some(target)) = &args.tailnet else {
        return Err(refuse(
            "invalid_request",
            "give the deck: --tailnet <host>[:<port>]",
        ));
    };
    let (host, port) = parse_target(target)?;
    let scopes = match &args.scopes {
        Some(list) => parse_scopes(list)?,
        None => DECK_SCOPES.to_vec(),
    };
    let name = match &args.name {
        Some(name) => name.clone(),
        None => {
            StationConfig::load_with_store(
                &dirs.station_config(),
                env,
                &dirs.station().join("secrets"),
            )
            .context("the station name (station.toml)")?
            .name
        }
    };
    check_name(&name)?;
    let mut fingerprint = args
        .deck_fingerprint
        .as_deref()
        .map(parse_fingerprint)
        .transpose()?;
    if fingerprint.is_none() && !has_terminal() {
        // The person must confirm the fingerprint; there is nobody to ask.
        return Err(refuse(
            "fingerprint_required",
            "give --deck-fingerprint (no terminal to confirm the fingerprint)",
        ));
    }
    loop {
        let request = LocalRequest::JoinTailnet {
            host: host.clone(),
            port,
            deck_fingerprint: fingerprint,
            scopes: scopes.clone(),
            name: name.clone(),
        };
        // A refusal is quick; the hint is for the wait for the owner.
        let hint = fingerprint.is_some().then(|| {
            tokio::spawn(async {
                tokio::time::sleep(Duration::from_millis(1500)).await;
                eprintln!(
                    "Waiting for the owner of the deck to approve this request on the board (at most 10 minutes)..."
                );
            })
        });
        let answer = call(dirs, &request).await;
        if let Some(hint) = hint {
            hint.abort();
        }
        match finish(answer?)? {
            LocalResponse::TailnetPaired { device, deck } => {
                println!("paired with {device}");
                tracing::debug!(deck = %deck, "paired");
                return Ok(());
            }
            LocalResponse::TailnetObserved { fingerprint: seen } => {
                // The probe wrote nothing. The person confirms what the
                // board shows.
                if !has_terminal() {
                    return Err(refuse(
                        "fingerprint_required",
                        "give --deck-fingerprint (no terminal to confirm the fingerprint)",
                    ));
                }
                if !ask_to_confirm(&seen)? {
                    return Err(refuse(
                        "cancelled",
                        "the fingerprint was not confirmed; nothing was sent",
                    ));
                }
                fingerprint = Some(parse_fingerprint(&seen)?);
            }
            other => bail!("unexpected answer from the station: {other:?}"),
        }
    }
}

/// `--cancel`: removes the retry file only.
async fn cancel(dirs: &NodeDirs) -> anyhow::Result<()> {
    match finish(call(dirs, &LocalRequest::JoinTailnetCancel).await?)? {
        LocalResponse::TailnetCancelled { removed: true } => {
            println!("the pending pairing request was cancelled");
        }
        LocalResponse::TailnetCancelled { removed: false } => {
            println!("no pairing request was pending");
        }
        other => bail!("unexpected answer from the station: {other:?}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn target(text: &str) -> (String, u16) {
        parse_target(text).unwrap()
    }

    #[test]
    fn a_target_is_a_host_with_an_optional_port() {
        assert_eq!(
            target("deck.tail.ts.net"),
            ("deck.tail.ts.net".into(), 7421)
        );
        assert_eq!(
            target("deck.tail.ts.net:9000"),
            ("deck.tail.ts.net".into(), 9000)
        );
        assert_eq!(target("deck"), ("deck".into(), 7421));
        assert_eq!(target("100.64.0.5"), ("100.64.0.5".into(), 7421));
        assert_eq!(target("100.64.0.5:7421"), ("100.64.0.5".into(), 7421));
        assert_eq!(
            target("fd7a:115c:a1e0::5"),
            ("fd7a:115c:a1e0::5".into(), 7421)
        );
        assert_eq!(
            target("[fd7a:115c:a1e0::5]"),
            ("fd7a:115c:a1e0::5".into(), 7421)
        );
        assert_eq!(
            target("[fd7a:115c:a1e0::5]:9"),
            ("fd7a:115c:a1e0::5".into(), 9)
        );
    }

    #[test]
    fn a_bad_target_is_refused() {
        for bad in [
            "",
            " ",
            ":7421",
            "host:0",
            "host:99999",
            "host:x",
            "[::1",
            "[::1]x",
            "a:b:c",
            "ho st",
        ] {
            assert!(parse_target(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_fingerprint_is_64_lowercase_hex_digits() {
        let lower = "ab".repeat(32);
        assert_eq!(parse_fingerprint(&lower).unwrap(), [0xab; 32]);
        for bad in [
            "",
            "ab",
            &"zz".repeat(32),
            &"ab".repeat(31),
            &"ab".repeat(33),
            &lower.to_uppercase(),
        ] {
            assert!(parse_fingerprint(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn scopes_are_read_drive_approve_admin_and_never_coordinate() {
        let list = |s: &[&str]| s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            parse_scopes(&list(&["read", "drive", "read"])).unwrap(),
            vec![Scope::Read, Scope::Drive]
        );
        assert!(parse_scopes(&list(&["coordinate"])).is_err());
        assert!(parse_scopes(&list(&["root"])).is_err());
        assert!(parse_scopes(&list(&[])).is_err());
        assert!(parse_scopes(&list(&[" "])).is_err());
    }

    #[test]
    fn the_name_has_one_to_64_characters_without_control_or_bidi_characters() {
        assert!(check_name("laptop").is_ok());
        assert!(check_name("").is_err());
        assert!(check_name(&"x".repeat(65)).is_err());
        assert!(check_name("a\u{202E}b").is_err());
        assert!(check_name("a\nb").is_err());
    }

    #[test]
    fn only_y_or_yes_confirms() {
        assert!(confirmed("y\n"));
        assert!(confirmed(" YES "));
        for no in ["", "n", "no", "yep", "\n"] {
            assert!(!confirmed(no), "{no:?}");
        }
    }

    #[test]
    fn a_refusal_prints_its_code_and_detail() {
        let e = refuse("denied", "the owner denied the request");
        assert_eq!(e.to_string(), "denied: the owner denied the request");
        assert!(e.downcast_ref::<Refused>().is_some());
    }
}
