//! `fleetdeck deck provision-file <name> --deck <host>[:<port>] --out <path>`
//! (P8, design `omarchy-p8.md` 2.2 to 2.4): writes the provisioning file of
//! a new station.
//!
//! The running deck gives its identity through the local request
//! `deck.provision_info` after it checks that it serves the address. The
//! file is written with [`fleetdeck_core::private_file`]: never over an
//! existing path. Every refusal prints `<code>: <text>` (exit 1); success
//! prints "Provisioning file written." on stderr and nothing on stdout.

use std::time::Duration;

use fleetdeck_core::paths::NodeDirs;
use fleetdeck_core::private_file::{self, PrivateWriteError};
use fleetdeck_core::provision::{self, ProvisionFile};
use fleetdeck_deck::local::{DeckClient, DeckRequest, DeckResponse};
use fleetdeck_deck::local_provision::FEATURE;

use crate::cli::ProvisionFileArgs;
use crate::tailnet_join::{Refused, check_name, parse_target};

/// The whole local call: connect, send, answer.
const CALL_WAIT: Duration = Duration::from_secs(5);

/// The prefix of the temporary leaf next to the output file.
const TEMP_PREFIX: &str = ".provision.";

const INVALID_ARGUMENTS: &str = "The provisioning arguments are not valid.";
const INVALID_RESPONSE: &str = "The local process returned an invalid provisioning response.";
const WRITE_FAILED: &str = "The provisioning file could not be written.";

/// How the local call failed before it gave a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallFail {
    /// The connection to `deck.sock` failed.
    Connect,
    /// The deadline of [`CALL_WAIT`].
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

/// The refusal for an error of the output path check or of the write.
fn write_refused(e: &PrivateWriteError) -> Refused {
    match e {
        PrivateWriteError::Exists => refused("output_exists", "The output path already exists."),
        PrivateWriteError::Unsafe => refused(
            "unsafe_output",
            "The output path is not private or contains a link.",
        ),
        PrivateWriteError::Io(_) => refused("write_failed", WRITE_FAILED),
    }
}

/// `fleetdeck deck provision-file`.
pub async fn run(dirs: &NodeDirs, args: &ProvisionFileArgs) -> anyhow::Result<()> {
    let invalid = || refused("invalid_request", INVALID_ARGUMENTS);
    check_name(&args.name).map_err(|_| invalid())?;
    let (host, port) = parse_target(&args.deck).map_err(|_| invalid())?;
    if !args.out.is_absolute() {
        return Err(invalid().into());
    }
    private_file::check_new_private(&args.out).map_err(|e| write_refused(&e))?;
    let (deck, deck_fingerprint) = interpret(ask(dirs, host, port).await)?;
    let file = ProvisionFile {
        version: 1,
        deck,
        deck_fingerprint,
        name: args.name.clone(),
    };
    let text = file.to_toml();
    if provision::parse(text.as_bytes()).as_ref() != Ok(&file) {
        return Err(refused("write_failed", WRITE_FAILED).into());
    }
    private_file::write_new_private(&args.out, text.as_bytes(), TEMP_PREFIX)
        .map_err(|e| write_refused(&e))?;
    eprintln!("Provisioning file written.");
    Ok(())
}

/// The local call to the deck, inside [`CALL_WAIT`].
async fn ask(dirs: &NodeDirs, host: String, port: u16) -> Result<DeckResponse, CallFail> {
    let path = fleetdeck_deck::local::socket_path(&dirs.deck());
    let request = DeckRequest::ProvisionInfo { host, port };
    let call = async {
        let mut client = DeckClient::connect(&path)
            .await
            .map_err(|_| CallFail::Connect)?;
        client.call(&request).await.map_err(|_| CallFail::Io)
    };
    match tokio::time::timeout(CALL_WAIT, call).await {
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

/// The `(deck, deck_fingerprint)` of a good response, or the refusal.
fn interpret(r: Result<DeckResponse, CallFail>) -> Result<(String, String), Refused> {
    let unsupported = || {
        refused(
            "unsupported",
            "The running process does not support provisioning v1.",
        )
    };
    let invalid_response = || refused("invalid_response", INVALID_RESPONSE);
    match r {
        Err(CallFail::Connect) => Err(refused("unavailable", "The local deck is not running.")),
        Err(CallFail::Timeout) => Err(refused(
            "timeout",
            "The local process did not answer in time.",
        )),
        Err(CallFail::Io) => Err(invalid_response()),
        // An older deck answers an unknown op with an error.
        Ok(DeckResponse::Error { .. }) => Err(unsupported()),
        Ok(DeckResponse::ProvisionInfo {
            features,
            deck,
            deck_fingerprint,
        }) => {
            if !features.iter().any(|f| f == FEATURE) {
                return Err(unsupported());
            }
            if !is_fingerprint(&deck_fingerprint) || parse_target(&deck).is_err() {
                return Err(invalid_response());
            }
            Ok((deck, deck_fingerprint))
        }
        Ok(DeckResponse::ProvisionError { code }) => match code.as_str() {
            "invalid_request" => Err(refused("invalid_request", INVALID_ARGUMENTS)),
            "deck_not_listening" => Err(refused(
                "deck_not_listening",
                "The deck does not listen on that tailnet address and port.",
            )),
            _ => Err(invalid_response()),
        },
        Ok(_) => Err(invalid_response()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn info(features: &[&str], deck: &str, fingerprint: &str) -> DeckResponse {
        DeckResponse::ProvisionInfo {
            features: features.iter().map(|f| (*f).to_owned()).collect(),
            deck: deck.to_owned(),
            deck_fingerprint: fingerprint.to_owned(),
        }
    }

    fn error(code: &str) -> DeckResponse {
        DeckResponse::ProvisionError {
            code: code.to_owned(),
        }
    }

    /// The refusal as `<code>: <detail>`.
    fn refusal(r: Result<DeckResponse, CallFail>) -> String {
        interpret(r).unwrap_err().to_string()
    }

    #[test]
    fn a_good_response_gives_the_deck_and_the_fingerprint() {
        let got = interpret(Ok(info(&[FEATURE], "deck.example.ts.net:7421", FP)));
        assert_eq!(
            got.unwrap(),
            ("deck.example.ts.net:7421".to_owned(), FP.to_owned())
        );
        let got = interpret(Ok(info(&["other", FEATURE], "[fd7a::1]:7421", FP)));
        assert_eq!(got.unwrap(), ("[fd7a::1]:7421".to_owned(), FP.to_owned()));
    }

    #[test]
    fn a_connect_error_is_unavailable() {
        assert_eq!(
            refusal(Err(CallFail::Connect)),
            "unavailable: The local deck is not running."
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
    fn an_older_deck_is_unsupported() {
        let older = DeckResponse::Error {
            message: "unknown op".to_owned(),
        };
        assert_eq!(
            refusal(Ok(older)),
            "unsupported: The running process does not support provisioning v1."
        );
    }

    #[test]
    fn a_response_without_the_feature_is_unsupported() {
        for features in [&[][..], &["deck_provision.v2"][..]] {
            assert_eq!(
                refusal(Ok(info(features, "deck.example.ts.net:7421", FP))),
                "unsupported: The running process does not support provisioning v1."
            );
        }
    }

    #[test]
    fn the_two_deck_errors_have_their_texts() {
        assert_eq!(
            refusal(Ok(error("invalid_request"))),
            "invalid_request: The provisioning arguments are not valid."
        );
        assert_eq!(
            refusal(Ok(error("deck_not_listening"))),
            "deck_not_listening: The deck does not listen on that tailnet address and port."
        );
    }

    #[test]
    fn everything_else_is_an_invalid_response() {
        let invalid =
            "invalid_response: The local process returned an invalid provisioning response.";
        // An I/O or parse error after the connect.
        assert_eq!(refusal(Err(CallFail::Io)), invalid);
        // Another status.
        assert_eq!(
            refusal(Ok(DeckResponse::Stations { stations: vec![] })),
            invalid
        );
        // Another error code.
        assert_eq!(refusal(Ok(error("other"))), invalid);
        assert_eq!(refusal(Ok(error(""))), invalid);
        // A fingerprint that is not 64 lowercase hex.
        for bad in [
            FP.to_uppercase(),
            FP[..63].to_owned(),
            format!("{FP}0"),
            String::new(),
        ] {
            assert_eq!(
                refusal(Ok(info(&[FEATURE], "deck.example.ts.net:7421", &bad))),
                invalid
            );
        }
        // A deck that does not parse with `parse_target`.
        for bad in [
            "",
            "deck:notaport",
            "deck.example.ts.net:0",
            "[::1",
            "a b:7421",
        ] {
            assert_eq!(refusal(Ok(info(&[FEATURE], bad, FP))), invalid, "{bad}");
        }
    }
}
