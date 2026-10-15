//! `fleetdeck open`, `deck password`, `deck token create|list|revoke` and
//! `deck audit` (M2 P3-8; PLAN §9, A13). All are clients of `deck.sock`.

use anyhow::bail;
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_deck::local::{DeckRequest, DeckResponse, InvitationText};
use fleetdeck_deck::local_auth::{AuthRequest, AuthResponse};
use fleetdeck_proto::link::Scope;

use crate::cli::TokenCommand;
use crate::deck::call;

async fn auth(dirs: &NodeDirs, auth: AuthRequest) -> anyhow::Result<AuthResponse> {
    match call(dirs, &DeckRequest::Auth { auth }).await? {
        DeckResponse::Auth { auth } => Ok(auth),
        _ => bail!("unexpected answer from the deck"),
    }
}

/// What `fleetdeck open` says about the certificate of the printed address.
fn certificate_note(trusted: bool, self_signed: bool, sha256: &str) -> String {
    if trusted {
        "The certificate of this address is trusted by browsers.".to_owned()
    } else if self_signed {
        format!(
            "the board uses the deck's self-signed certificate; your browser warns once.\n\
             accept it only if its SHA-256 fingerprint is\n  {sha256}"
        )
    } else {
        format!("board certificate SHA-256: {sha256}")
    }
}

/// `fleetdeck open`: prints a one-time login URL of the board.
pub async fn open(dirs: &NodeDirs) -> anyhow::Result<()> {
    let AuthResponse::LoginLink {
        url,
        certificate_sha256,
        self_signed,
        trusted,
        ..
    } = auth(dirs, AuthRequest::LoginLink).await?
    else {
        bail!("unexpected answer from the deck");
    };
    eprintln!(
        "{}",
        certificate_note(trusted, self_signed, &certificate_sha256)
    );
    eprintln!("single-use login link, valid for 5 minutes; open it in your browser:");
    println!("{}", url.0);
    Ok(())
}

/// `fleetdeck open --elevate`: prints a one-time elevation code (DL-172).
pub async fn elevate(dirs: &NodeDirs) -> anyhow::Result<()> {
    let DeckResponse::ElevateCode { code, .. } = call(dirs, &DeckRequest::ElevateCode).await?
    else {
        bail!("unexpected answer from the deck");
    };
    println!(
        "Elevation code: {} (one use, valid for 5 minutes). Type it in the board.",
        code.0
    );
    Ok(())
}

/// `fleetdeck deck password [--stdin]`.
pub async fn password(dirs: &NodeDirs, stdin: bool) -> anyhow::Result<()> {
    let password = if stdin {
        crate::invite::secret_line(std::io::stdin().lock(), "password")?
    } else {
        let first = crate::invite::prompt("New owner password: ", "password", "use --stdin")?;
        let again = crate::invite::prompt("Repeat the password: ", "password", "use --stdin")?;
        if first.expose() != again.expose() {
            bail!("the two passwords differ");
        }
        first
    };
    let request = AuthRequest::SetPassword {
        password: InvitationText(password.expose().clone()),
    };
    drop(password);
    auth(dirs, request).await?;
    println!("the owner password is set; every earlier login has ended");
    Ok(())
}

fn scope(text: &str) -> anyhow::Result<Scope> {
    serde_json::from_value(serde_json::Value::String(text.to_owned()))
        .map_err(|_| anyhow::anyhow!("`{text}` is not a scope (read, drive, approve, admin)"))
}

/// `fleetdeck deck token create|list|revoke`.
pub async fn token(dirs: &NodeDirs, command: &TokenCommand) -> anyhow::Result<()> {
    match command {
        TokenCommand::Create { name, scopes, days } => {
            let scopes = scopes
                .iter()
                .map(|s| scope(s))
                .collect::<anyhow::Result<_>>()?;
            let request = AuthRequest::CreateToken {
                name: name.clone(),
                scopes,
                ttl_days: *days,
            };
            let AuthResponse::Token { token, value } = auth(dirs, request).await? else {
                bail!("unexpected answer from the deck");
            };
            eprintln!(
                "API token `{}` (shown once; send it as `Authorization: Bearer <token>`):",
                token.name
            );
            println!("{}", value.0);
        }
        TokenCommand::List { json } => {
            let AuthResponse::Tokens { tokens } = auth(dirs, AuthRequest::Tokens).await? else {
                bail!("unexpected answer from the deck");
            };
            if *json {
                println!("{}", serde_json::to_string_pretty(&tokens)?);
            } else if tokens.is_empty() {
                println!("no tokens");
            } else {
                for t in &tokens {
                    let scopes: Vec<String> = t
                        .scopes
                        .iter()
                        .map(|s| format!("{s:?}").to_lowercase())
                        .collect();
                    println!(
                        "{}  {}  expires_at={}",
                        t.name,
                        scopes.join(","),
                        t.expires_at
                    );
                }
            }
        }
        TokenCommand::Revoke { name } => {
            let request = AuthRequest::RevokeToken { name: name.clone() };
            auth(dirs, request).await?;
            println!("revoked token {name}");
        }
    }
    Ok(())
}

/// `fleetdeck deck audit [--limit N] [--json]`.
pub async fn audit(dirs: &NodeDirs, limit: u32, json: bool) -> anyhow::Result<()> {
    let AuthResponse::Audit { entries } = auth(dirs, AuthRequest::Audit { limit }).await? else {
        bail!("unexpected answer from the deck");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else if entries.is_empty() {
        println!("no audit entries");
    } else {
        for e in &entries {
            let result = serde_json::to_value(e.result)?;
            println!(
                "{}  {}  {}  {}  {}  {}",
                e.at,
                e.actor,
                e.action,
                e.target.as_deref().unwrap_or("-"),
                result.as_str().unwrap_or("-"),
                e.detail.as_deref().unwrap_or("-"),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::certificate_note;

    #[test]
    fn a_trusted_address_says_so_and_shows_no_fingerprint_warning() {
        let note = certificate_note(true, false, "aa:bb");
        assert_eq!(
            note,
            "The certificate of this address is trusted by browsers."
        );
    }

    #[test]
    fn the_other_addresses_keep_the_fingerprint_notes() {
        let own = certificate_note(false, true, "aa:bb");
        assert!(own.contains("self-signed") && own.contains("SHA-256 fingerprint is\n  aa:bb"));
        assert_eq!(
            certificate_note(false, false, "aa:bb"),
            "board certificate SHA-256: aa:bb"
        );
    }
}
