//! `deck.provision_info` on the local socket (P8, design `omarchy-p8.md`
//! 2.2): the identity of the running deck for `fleetdeck deck
//! provision-file`.
//!
//! The deck answers only for an address that Tailscale reports as its own
//! (its MagicDNS name or one of its tailnet IPs) and that a bound link
//! listener serves on the given port (the address itself or the wildcard of
//! its family). It is a read: no audit row.

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use fleetdeck_core::tailscale::{Node, Status, TailscaleCli};

use crate::local::{DeckResponse, LocalDeck};

/// The feature identifier of this request in the response.
pub const FEATURE: &str = "deck_provision.v1";

/// The longest wait for the Tailscale status.
const STATUS_WAIT: Duration = Duration::from_secs(3);

const INVALID_REQUEST: &str = "invalid_request";
const NOT_LISTENING: &str = "deck_not_listening";

/// Trim, lowercase, and remove one trailing dot.
fn normalize(text: &str) -> String {
    let lower = text.trim().to_lowercase();
    match lower.strip_suffix('.') {
        Some(rest) => rest.to_owned(),
        None => lower,
    }
}

/// Checks `host` and `port` against the Tailscale identity `this` and the
/// bound link listeners `listening`.
///
/// Returns the canonical `deck` string (`<name>:<port>`, `<ipv4>:<port>` or
/// `[<ipv6>]:<port>`) or the error code (`invalid_request` or
/// `deck_not_listening`).
pub(crate) fn check(
    host: &str,
    port: u16,
    this: &Node,
    listening: &[SocketAddr],
) -> Result<String, &'static str> {
    if port == 0 {
        return Err(INVALID_REQUEST);
    }
    let host = normalize(host);
    if host.is_empty() {
        return Err(INVALID_REQUEST);
    }
    let (candidates, deck) = if let Ok(ip) = host.parse::<IpAddr>() {
        if !this.ips.contains(&ip) {
            return Err(NOT_LISTENING);
        }
        let deck = match ip {
            IpAddr::V4(ip) => format!("{ip}:{port}"),
            IpAddr::V6(ip) => format!("[{ip}]:{port}"),
        };
        (vec![ip], deck)
    } else {
        if !host.contains('.') {
            return Err(INVALID_REQUEST);
        }
        if host != normalize(&this.dns_name) {
            return Err(NOT_LISTENING);
        }
        (this.ips.clone(), format!("{host}:{port}"))
    };
    let serves = |addr: &SocketAddr| {
        addr.port() == port
            && (candidates.contains(&addr.ip())
                || (addr.ip().is_unspecified()
                    && candidates
                        .iter()
                        .any(|c| c.is_ipv4() == addr.ip().is_ipv4())))
    };
    if listening.iter().any(serves) {
        Ok(deck)
    } else {
        Err(NOT_LISTENING)
    }
}

/// The Tailscale status of this machine, with the deck's `tailscale_bin`,
/// inside [`STATUS_WAIT`]. Any failure is `None`.
async fn status(deck: &LocalDeck) -> Option<Status> {
    let deadline = Instant::now() + STATUS_WAIT;
    let configured = deck.live.get().tailscale_bin.clone();
    let call = async {
        // `locate` reads files: it runs off the async threads.
        let cli = tokio::task::spawn_blocking(move || TailscaleCli::locate(configured.as_deref()))
            .await
            .ok()?;
        cli.status_until(deadline).await.ok()
    };
    tokio::time::timeout(STATUS_WAIT, call).await.ok().flatten()
}

fn refused(code: &str) -> DeckResponse {
    DeckResponse::ProvisionError {
        code: code.to_owned(),
    }
}

/// The answer to `deck.provision_info`. A Tailscale error gives
/// `deck_not_listening`.
pub(crate) async fn provision_info(deck: &LocalDeck, host: String, port: u16) -> DeckResponse {
    let Some(status) = status(deck).await else {
        return refused(NOT_LISTENING);
    };
    match check(&host, port, &status.this, deck.node.listen_addrs()) {
        Ok(canonical) => DeckResponse::ProvisionInfo {
            features: vec![FEATURE.to_owned()],
            deck: canonical,
            deck_fingerprint: deck.node.link().node.identity.fingerprint().to_string(),
        },
        Err(code) => refused(code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::DeckRequest;

    fn node(dns_name: &str, ips: &[&str]) -> Node {
        Node {
            host_name: "deck".to_owned(),
            dns_name: dns_name.to_owned(),
            ips: ips.iter().map(|ip| ip.parse().unwrap()).collect(),
            os: None,
            login: None,
            tags: Vec::new(),
            online: true,
        }
    }

    fn both() -> Node {
        node("deck.example.ts.net.", &["100.64.0.7", "fd7a:115c:a1e0::7"])
    }

    fn at(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    #[test]
    fn the_fqdn_is_accepted_with_or_without_the_dot_and_in_uppercase() {
        let listening = [at("100.64.0.7:7421")];
        for host in [
            "deck.example.ts.net",
            "deck.example.ts.net.",
            "DECK.Example.TS.net",
            "  deck.example.ts.net.  ",
        ] {
            assert_eq!(
                check(host, 7421, &both(), &listening),
                Ok("deck.example.ts.net:7421".to_owned()),
                "{host}"
            );
        }
    }

    #[test]
    fn a_dns_name_that_has_no_trailing_dot_of_its_own_is_accepted() {
        let this = node("deck.example.ts.net", &["100.64.0.7"]);
        assert_eq!(
            check(
                "deck.example.ts.net.",
                7421,
                &this,
                &[at("100.64.0.7:7421")]
            ),
            Ok("deck.example.ts.net:7421".to_owned())
        );
    }

    #[test]
    fn a_short_name_empty_host_and_port_zero_are_invalid_requests() {
        let listening = [at("100.64.0.7:7421")];
        assert_eq!(
            check("deck", 7421, &both(), &listening),
            Err("invalid_request")
        );
        assert_eq!(check("", 7421, &both(), &listening), Err("invalid_request"));
        assert_eq!(
            check(" . ", 7421, &both(), &listening),
            Err("invalid_request")
        );
        assert_eq!(
            check("deck.example.ts.net", 0, &both(), &listening),
            Err("invalid_request")
        );
        // The port check comes first.
        assert_eq!(
            check("deck", 0, &both(), &listening),
            Err("invalid_request")
        );
    }

    #[test]
    fn a_name_that_is_not_the_deck_is_not_listening() {
        let listening = [at("100.64.0.7:7421")];
        assert_eq!(
            check("other.example.ts.net", 7421, &both(), &listening),
            Err("deck_not_listening")
        );
    }

    #[test]
    fn a_tailscale_ipv4_and_ipv6_host_are_accepted() {
        let listening = [at("100.64.0.7:7421"), at("[fd7a:115c:a1e0::7]:7421")];
        assert_eq!(
            check("100.64.0.7", 7421, &both(), &listening),
            Ok("100.64.0.7:7421".to_owned())
        );
        assert_eq!(
            check("fd7a:115c:a1e0::7", 7421, &both(), &listening),
            Ok("[fd7a:115c:a1e0::7]:7421".to_owned())
        );
        assert_eq!(
            check("FD7A:115C:A1E0::7", 7421, &both(), &listening),
            Ok("[fd7a:115c:a1e0::7]:7421".to_owned())
        );
    }

    #[test]
    fn an_ip_that_is_not_the_deck_is_not_listening() {
        let listening = [at("0.0.0.0:7421")];
        assert_eq!(
            check("100.64.0.8", 7421, &both(), &listening),
            Err("deck_not_listening")
        );
        assert_eq!(
            check("127.0.0.1", 7421, &both(), &listening),
            Err("deck_not_listening")
        );
    }

    #[test]
    fn a_listener_on_another_port_is_not_listening() {
        let listening = [at("100.64.0.7:7422")];
        assert_eq!(
            check("100.64.0.7", 7421, &both(), &listening),
            Err("deck_not_listening")
        );
        assert_eq!(
            check("deck.example.ts.net", 7421, &both(), &listening),
            Err("deck_not_listening")
        );
        assert_eq!(
            check("100.64.0.7", 7421, &both(), &[]),
            Err("deck_not_listening")
        );
    }

    #[test]
    fn a_listener_on_another_address_is_not_listening() {
        let listening = [at("127.0.0.1:7421")];
        assert_eq!(
            check("100.64.0.7", 7421, &both(), &listening),
            Err("deck_not_listening")
        );
        assert_eq!(
            check("deck.example.ts.net", 7421, &both(), &listening),
            Err("deck_not_listening")
        );
    }

    #[test]
    fn a_name_matches_a_listener_on_any_of_its_ips() {
        let listening = [at("[fd7a:115c:a1e0::7]:7421")];
        assert_eq!(
            check("deck.example.ts.net", 7421, &both(), &listening),
            Ok("deck.example.ts.net:7421".to_owned())
        );
    }

    #[test]
    fn the_ipv4_wildcard_matches_an_ipv4_host_and_not_an_ipv6_one() {
        let listening = [at("0.0.0.0:7421")];
        assert_eq!(
            check("100.64.0.7", 7421, &both(), &listening),
            Ok("100.64.0.7:7421".to_owned())
        );
        assert_eq!(
            check("fd7a:115c:a1e0::7", 7421, &both(), &listening),
            Err("deck_not_listening")
        );
        // A name has both families: the IPv4 wildcard serves it.
        assert_eq!(
            check("deck.example.ts.net", 7421, &both(), &listening),
            Ok("deck.example.ts.net:7421".to_owned())
        );
        // An IPv6-only candidate set is not served by the IPv4 wildcard.
        let v6_only = node("deck.example.ts.net.", &["fd7a:115c:a1e0::7"]);
        assert_eq!(
            check("deck.example.ts.net", 7421, &v6_only, &listening),
            Err("deck_not_listening")
        );
    }

    #[test]
    fn the_ipv6_wildcard_matches_an_ipv6_host() {
        let listening = [at("[::]:7421")];
        assert_eq!(
            check("fd7a:115c:a1e0::7", 7421, &both(), &listening),
            Ok("[fd7a:115c:a1e0::7]:7421".to_owned())
        );
        assert_eq!(
            check("100.64.0.7", 7421, &both(), &listening),
            Err("deck_not_listening")
        );
        let v6_only = node("deck.example.ts.net.", &["fd7a:115c:a1e0::7"]);
        assert_eq!(
            check("deck.example.ts.net", 7421, &v6_only, &listening),
            Ok("deck.example.ts.net:7421".to_owned())
        );
    }

    #[test]
    fn the_wildcard_serves_only_its_port() {
        let listening = [at("0.0.0.0:7422")];
        assert_eq!(
            check("100.64.0.7", 7421, &both(), &listening),
            Err("deck_not_listening")
        );
    }

    #[test]
    fn the_request_json_is_the_wire_text() {
        let request = DeckRequest::ProvisionInfo {
            host: "deck.example.ts.net".to_owned(),
            port: 7421,
        };
        let text = serde_json::to_string(&request).unwrap();
        assert_eq!(
            text,
            r#"{"op":"deck.provision_info","host":"deck.example.ts.net","port":7421}"#
        );
        assert_eq!(serde_json::from_str::<DeckRequest>(&text).unwrap(), request);
    }

    #[test]
    fn the_response_json_is_the_wire_text() {
        let fingerprint = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let info = DeckResponse::ProvisionInfo {
            features: vec![FEATURE.to_owned()],
            deck: "deck.example.ts.net:7421".to_owned(),
            deck_fingerprint: fingerprint.to_owned(),
        };
        let text = serde_json::to_string(&info).unwrap();
        assert_eq!(
            text,
            format!(
                r#"{{"status":"provision_info","features":["deck_provision.v1"],"deck":"deck.example.ts.net:7421","deck_fingerprint":"{fingerprint}"}}"#
            )
        );
        assert_eq!(serde_json::from_str::<DeckResponse>(&text).unwrap(), info);

        let error = refused("deck_not_listening");
        let text = serde_json::to_string(&error).unwrap();
        assert_eq!(
            text,
            r#"{"status":"provision_error","code":"deck_not_listening"}"#
        );
        assert_eq!(serde_json::from_str::<DeckResponse>(&text).unwrap(), error);
    }
}
