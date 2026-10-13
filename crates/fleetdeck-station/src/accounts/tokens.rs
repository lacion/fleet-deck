//! `accounts.token_set` (DL-173, design §2.4): a Jira site, e-mail and API
//! token, a Linear personal key, or a GitHub or GitLab token.
//!
//! A tracker token is checked with one identity call and stored as an
//! account with the method `token`. A forge token goes to the CLI on stdin.
//! The answer holds metadata only; a failed check says only that the
//! tracker did not accept the token.

use fleetdeck_core::Secret;
use fleetdeck_proto::accounts::{AccountKind, AccountMethod, AccountView, TokenSet};
use fleetdeck_proto::integrations::{TrackerKind, code, valid_host};

use super::store::StoredAccount;
use super::{Accounts, invalid};
use crate::integrations::exec::Fail;
use crate::integrations::tracker::Tracker;

/// A non-empty text of at most `max` characters without a control
/// character.
fn text_ok(value: &str, max: usize) -> bool {
    !value.is_empty() && value.chars().count() <= max && !value.chars().any(char::is_control)
}

/// `https://<host>[:port]` without a path: the site of a Jira token. A
/// final `/` is dropped.
#[must_use]
pub fn normalize_site(site: &str) -> Option<String> {
    let host = site.strip_prefix("https://")?.trim_end_matches('/');
    valid_host(host).then(|| format!("https://{}", host.to_ascii_lowercase()))
}

fn email_ok(email: &str) -> bool {
    email.len() <= 254
        && email.contains('@')
        && !email
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '"' || c == '\\')
}

/// The failure of the identity call of a token check: the tracker's own
/// refusal (its text is dropped) or a fixed failure such as a timeout.
fn check_failed(fail: Fail) -> Fail {
    if fail.code == code::UPSTREAM {
        Fail::unavailable("the tracker did not accept this token")
    } else {
        fail
    }
}

impl Accounts {
    /// `accounts.token_set`.
    ///
    /// # Errors
    /// `invalid_params`; `unavailable` when the tracker or the CLI refuses
    /// the token; the errors of a CLI login.
    pub async fn token_set(&self, p: TokenSet) -> Result<AccountView, Fail> {
        if !text_ok(&p.token, 4096) {
            return Err(invalid("the token is not valid"));
        }
        if p.email.as_deref().is_some_and(|e| !email_ok(e)) {
            return Err(invalid("the e-mail is not valid"));
        }
        let TokenSet {
            kind,
            site,
            email,
            token,
        } = p;
        match kind {
            AccountKind::Github | AccountKind::Gitlab => {
                if email.is_some() {
                    return Err(invalid("a forge token takes no e-mail"));
                }
                let host = site.unwrap_or_else(|| {
                    if kind == AccountKind::Github {
                        "github.com".to_owned()
                    } else {
                        "gitlab.com".to_owned()
                    }
                });
                self.forge_token_login(kind, &host, &token).await
            }
            AccountKind::Jira => {
                let site = site
                    .as_deref()
                    .and_then(normalize_site)
                    .ok_or_else(|| invalid("the Jira site must be https://<host>"))?;
                let email =
                    email.ok_or_else(|| invalid("a Jira token needs the account e-mail"))?;
                let tracker = Tracker::stateless(
                    &self.programs,
                    TrackerKind::Jira,
                    AccountMethod::Token,
                    Some(site.clone()),
                    Some(email.clone()),
                    Secret::new(token.clone()),
                );
                let user = tracker.whoami().await.map_err(check_failed)?;
                self.store_token(AccountKind::Jira, Some(site), Some(email), user, token)
                    .await
            }
            AccountKind::Linear => {
                if site.is_some() || email.is_some() {
                    return Err(invalid("a Linear key takes neither a site nor an e-mail"));
                }
                let tracker = Tracker::stateless(
                    &self.programs,
                    TrackerKind::Linear,
                    AccountMethod::Token,
                    None,
                    None,
                    Secret::new(token.clone()),
                );
                let (user, workspace) = tracker.linear_identity().await.map_err(check_failed)?;
                let site = workspace.map(|k| format!("https://linear.app/{k}"));
                self.store_token(AccountKind::Linear, site, None, user, token)
                    .await
            }
        }
    }

    async fn store_token(
        &self,
        kind: AccountKind,
        site: Option<String>,
        email: Option<String>,
        user: String,
        token: String,
    ) -> Result<AccountView, Fail> {
        let account = StoredAccount {
            kind,
            method: AccountMethod::Token,
            site,
            cloud_id: None,
            user,
            access_token: Secret::new(token),
            refresh_token: None,
            expires_at: None,
            email,
            generation: 0,
        };
        let id = self.save_account(&account).await?;
        Ok(self.view(&id, &account))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_site_is_https_and_has_no_path() {
        assert_eq!(
            normalize_site("https://Acme.Atlassian.net/").as_deref(),
            Some("https://acme.atlassian.net")
        );
        for bad in [
            "http://acme.atlassian.net",
            "https://acme.atlassian.net/x",
            "acme.atlassian.net",
            "https://",
            "https://a b",
        ] {
            assert_eq!(normalize_site(bad), None, "{bad}");
        }
    }
}
