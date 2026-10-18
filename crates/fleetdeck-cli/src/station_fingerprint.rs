//! `fleetdeck station fingerprint [--json] [--probe-providers]` (P8, design
//! `omarchy-p8.md` 1, 2.1, 2.4 and 5.3): prints the link key fingerprint of
//! the running station.
//!
//! The default output is the fingerprint in 16 groups of four lowercase hex
//! digits, one space between the groups, and a newline. `--json` prints the
//! response of the station as one compact line. Every refusal prints one
//! stderr line `<code>: <text>` and exits 1, with nothing on stdout.

use std::time::Duration;

use fleetdeck_core::paths::NodeDirs;
use fleetdeck_station::local::{LocalClient, LocalRequest, LocalResponse, socket_path};
use fleetdeck_station::local_fingerprint::FEATURE;

use crate::tailnet_join::Refused;

/// The whole local call (connect, request, answer) without a provider probe.
const CALL_WAIT: Duration = Duration::from_secs(5);
/// The whole local call with `--probe-providers` (design 5.3).
const PROBE_CALL_WAIT: Duration = Duration::from_secs(35);

const INVALID_RESPONSE: &str = "The local process returned an invalid provisioning response.";

/// How the local call failed before it gave a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallFail {
    /// The connection to `station.sock` failed.
    Connect,
    /// The deadline of the call.
    Timeout,
    /// An I/O or parse error after the connection.
    Io,
}

fn refused(code: &str, detail: &str) -> Refused {
    Refused {
        code: code.to_owned(),
        detail: detail.to_owned(),
    }
}

/// The fingerprint `hex` in 16 groups of four characters, separated by one
/// ASCII space (design 1).
#[must_use]
pub fn grouped(hex: &str) -> String {
    let chars: Vec<char> = hex.chars().collect();
    chars
        .chunks(4)
        .map(|group| group.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `fleetdeck station fingerprint`.
pub async fn run(dirs: &NodeDirs, json: bool, probe_providers: bool) -> anyhow::Result<()> {
    let response = interpret(ask(dirs, probe_providers).await)?;
    if json {
        println!("{}", serde_json::to_string(&response)?);
    } else if let LocalResponse::Fingerprint { fingerprint, .. } = &response {
        println!("{}", grouped(fingerprint));
    }
    Ok(())
}

/// The local call to the station, inside its deadline.
async fn ask(dirs: &NodeDirs, probe_providers: bool) -> Result<LocalResponse, CallFail> {
    let path = socket_path(&dirs.station());
    let request = LocalRequest::StationFingerprint { probe_providers };
    let call = async {
        let mut client = LocalClient::connect(&path)
            .await
            .map_err(|_| CallFail::Connect)?;
        client.call(&request).await.map_err(|_| CallFail::Io)
    };
    let wait = if probe_providers {
        PROBE_CALL_WAIT
    } else {
        CALL_WAIT
    };
    match tokio::time::timeout(wait, call).await {
        Ok(result) => result,
        Err(_) => Err(CallFail::Timeout),
    }
}

/// 64 lowercase hex digits.
fn is_fingerprint(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A good `Fingerprint` response, or the refusal.
fn interpret(r: Result<LocalResponse, CallFail>) -> Result<LocalResponse, Refused> {
    let unsupported = || {
        refused(
            "unsupported",
            "The running process does not support provisioning v1.",
        )
    };
    let invalid_response = || refused("invalid_response", INVALID_RESPONSE);
    match r {
        Err(CallFail::Connect) => Err(refused("unavailable", "The local station is not running.")),
        Err(CallFail::Timeout) => Err(refused(
            "timeout",
            "The local process did not answer in time.",
        )),
        Err(CallFail::Io) => Err(invalid_response()),
        // An older station answers an unknown op with an error.
        Ok(LocalResponse::Error { .. }) => Err(unsupported()),
        Ok(LocalResponse::ProvisionError { code }) => match code.as_str() {
            "unavailable" => Err(refused("unavailable", "The local station is not running.")),
            _ => Err(invalid_response()),
        },
        Ok(LocalResponse::Fingerprint {
            features,
            fingerprint,
            paired_deck,
            link_up,
            build,
            binary_sha256,
            providers,
        }) => {
            if !features.iter().any(|f| f == FEATURE) {
                return Err(unsupported());
            }
            if !is_fingerprint(&fingerprint)
                || paired_deck.as_deref().is_some_and(|d| !is_fingerprint(d))
                || binary_sha256.is_empty()
            {
                return Err(invalid_response());
            }
            Ok(LocalResponse::Fingerprint {
                features,
                fingerprint,
                paired_deck,
                link_up,
                build,
                binary_sha256,
                providers,
            })
        }
        Ok(_) => Err(invalid_response()),
    }
}

#[cfg(test)]
mod tests {
    use fleetdeck_station::local_fingerprint::BuildIdentity;

    use super::*;

    const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const INVALID: &str =
        "invalid_response: The local process returned an invalid provisioning response.";
    const UNSUPPORTED: &str = "unsupported: The running process does not support provisioning v1.";

    fn info(features: &[&str], fingerprint: &str, paired: Option<&str>) -> LocalResponse {
        LocalResponse::Fingerprint {
            features: features.iter().map(|f| (*f).to_owned()).collect(),
            fingerprint: fingerprint.to_owned(),
            paired_deck: paired.map(str::to_owned),
            link_up: false,
            build: BuildIdentity {
                version: "2.0.0".to_owned(),
                commit: "abc1234".to_owned(),
                release: false,
            },
            binary_sha256: FP.to_owned(),
            providers: None,
        }
    }

    fn error(code: &str) -> LocalResponse {
        LocalResponse::ProvisionError {
            code: code.to_owned(),
        }
    }

    /// The refusal as `<code>: <detail>`.
    fn refusal(r: Result<LocalResponse, CallFail>) -> String {
        interpret(r).unwrap_err().to_string()
    }

    #[test]
    fn grouped_has_16_groups_of_4_and_one_space() {
        assert_eq!(
            grouped(FP),
            "0123 4567 89ab cdef 0123 4567 89ab cdef 0123 4567 89ab cdef 0123 4567 89ab cdef"
        );
        assert_eq!(grouped(FP).split(' ').count(), 16);
        assert_eq!(grouped(""), "");
    }

    #[test]
    fn a_good_response_passes_unchanged() {
        let good = info(&[FEATURE], FP, None);
        assert_eq!(interpret(Ok(good.clone())).unwrap(), good);
        let paired = info(&["other", FEATURE], FP, Some(FP));
        assert_eq!(interpret(Ok(paired.clone())).unwrap(), paired);
    }

    #[test]
    fn a_connect_error_is_unavailable() {
        assert_eq!(
            refusal(Err(CallFail::Connect)),
            "unavailable: The local station is not running."
        );
        assert_eq!(
            refusal(Ok(error("unavailable"))),
            "unavailable: The local station is not running."
        );
    }

    #[test]
    fn the_deadline_is_a_timeout() {
        assert_eq!(
            refusal(Err(CallFail::Timeout)),
            "timeout: The local process did not answer in time."
        );
    }

    #[test]
    fn an_older_station_and_a_missing_feature_are_unsupported() {
        let older = LocalResponse::Error {
            message: "bad request".to_owned(),
        };
        assert_eq!(refusal(Ok(older)), UNSUPPORTED);
        for features in [&[][..], &["station_provision.v2"][..]] {
            assert_eq!(refusal(Ok(info(features, FP, None))), UNSUPPORTED);
        }
    }

    #[test]
    fn everything_else_is_an_invalid_response() {
        // An I/O or parse error after the connect.
        assert_eq!(refusal(Err(CallFail::Io)), INVALID);
        // Another status.
        assert_eq!(
            refusal(Ok(LocalResponse::Sessions { sessions: vec![] })),
            INVALID
        );
        // Another code, also the code of an unreadable binary.
        for code in ["binary_unreadable", "other", ""] {
            assert_eq!(refusal(Ok(error(code))), INVALID, "{code}");
        }
        // A fingerprint or a paired deck that is not 64 lowercase hex.
        for bad in [
            FP.to_uppercase(),
            FP[..63].to_owned(),
            format!("{FP}0"),
            String::new(),
        ] {
            assert_eq!(refusal(Ok(info(&[FEATURE], &bad, None))), INVALID);
            assert_eq!(refusal(Ok(info(&[FEATURE], FP, Some(&bad)))), INVALID);
        }
        // An empty binary hash.
        let mut empty = info(&[FEATURE], FP, None);
        if let LocalResponse::Fingerprint { binary_sha256, .. } = &mut empty {
            binary_sha256.clear();
        }
        assert_eq!(refusal(Ok(empty)), INVALID);
    }

    #[test]
    fn the_deadlines_are_five_and_thirty_five_seconds() {
        assert_eq!(CALL_WAIT, Duration::from_secs(5));
        assert_eq!(PROBE_CALL_WAIT, Duration::from_secs(35));
    }
}
