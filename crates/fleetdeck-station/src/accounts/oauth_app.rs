//! The app of a Linear sign-in (DL-185, design `linear-builtin-app.md`).
//!
//! - The built-in app is a public client: a client id in the station and no
//!   client secret. It works only with two localhost redirect URIs.
//! - The effective app of a new Linear sign-in is the owner's stored app
//!   when one is set, else the built-in app. Jira has no built-in app.
//! - An account remembers the app that made it (`app_source`,
//!   `app_client_id`). Its refresh uses that app, never another one. An
//!   account without that record is legacy: the app that issued it is
//!   unknown, and the station binds it only when Linear accepts a refresh
//!   with the stored owner app.

use fleetdeck_core::Secret;
use fleetdeck_proto::accounts::AccountKind;
use serde_json::Value;

use super::http;
use super::oauth::{LINEAR_BUILTIN_CLIENT_ID, LINEAR_TOKEN};
use super::store::{AppSourceDisk, StoredAccount, StoredApp};
use super::{Accounts, codes, store_fail};
use crate::integrations::exec::Fail;

/// The fixed text of `builtin_needs_localhost`.
pub(super) const BUILTIN_NEEDS_LOCALHOST_TEXT: &str = "The built-in Linear app works only from the board on the deck machine at https://127.0.0.1:7420 or https://localhost:7420. Open the board there, or set up your own Linear app.";

/// Who owns the app of a sign-in or an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppSource {
    /// The built-in Fleet Deck app of Linear.
    Builtin,
    /// The app that the owner registered.
    Owner,
}

impl From<AppSource> for AppSourceDisk {
    fn from(source: AppSource) -> Self {
        match source {
            AppSource::Builtin => Self::Builtin,
            AppSource::Owner => Self::Owner,
        }
    }
}

/// The app of one sign-in or one refresh. `Debug` hides the secret.
pub struct EffectiveApp {
    pub source: AppSource,
    pub client_id: String,
    /// `None` for the built-in app.
    pub client_secret: Option<Secret<String>>,
    /// The generation of the owner app; 0 for the built-in app.
    pub generation: u32,
}

impl std::fmt::Debug for EffectiveApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectiveApp")
            .field("source", &self.source)
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("generation", &self.generation)
            .finish()
    }
}

impl EffectiveApp {
    fn builtin(client_id: String) -> Self {
        Self {
            source: AppSource::Builtin,
            client_id,
            client_secret: None,
            generation: 0,
        }
    }

    fn owner(app: StoredApp) -> Self {
        Self {
            source: AppSource::Owner,
            client_id: app.client_id,
            client_secret: Some(app.client_secret),
            generation: app.generation,
        }
    }

    /// The same source, client id and generation.
    pub(super) fn same_as(&self, other: &Self) -> bool {
        self.source == other.source
            && self.client_id == other.client_id
            && self.generation == other.generation
    }
}

/// `app_changed` with its fixed text.
pub(super) fn app_changed() -> Fail {
    Fail::new(codes::APP_CHANGED, super::APP_CHANGED_TEXT)
}

impl Accounts {
    /// The app of a new sign-in of `kind`: the stored owner app, else for
    /// Linear the built-in app when it is on (`builtin_probe`). Jira without
    /// an owner app has none.
    pub(super) fn effective_app(&self, kind: AccountKind) -> Result<Option<EffectiveApp>, Fail> {
        let owner = self.store()?.app(kind).map_err(store_fail)?;
        Ok(match (owner, kind) {
            (Some(app), _) => Some(EffectiveApp::owner(app)),
            // The release gate (design §4): without it, Linear needs an
            // owner app for a new sign-in.
            (None, AccountKind::Linear) if (self.builtin_probe)() => {
                Some(EffectiveApp::builtin(LINEAR_BUILTIN_CLIENT_ID.to_owned()))
            }
            (None, _) => None,
        })
    }

    /// The refresh request of a Linear account with its own app. Returns
    /// the answer and, for a legacy account that Linear accepted, the client
    /// id of the owner app that the caller binds in the write of the new
    /// tokens.
    ///
    /// # Errors
    /// `app_changed` (no request) when the app of the account is not the
    /// stored one, and for a legacy account when no owner app is stored or
    /// Linear answers the refresh with a non-2xx status; the provider
    /// failure otherwise (also for a legacy account whose request got no
    /// answer).
    pub(super) async fn refresh_linear(
        &self,
        account: &StoredAccount,
        old: &Secret<String>,
    ) -> Result<(Value, Option<String>), Fail> {
        let owner = self.store()?.app(AccountKind::Linear).map_err(store_fail)?;
        let (app, legacy) = match (account.app_source, account.app_client_id.as_ref()) {
            (Some(AppSourceDisk::Builtin), Some(client_id)) => {
                (EffectiveApp::builtin(client_id.clone()), false)
            }
            (Some(AppSourceDisk::Owner), Some(client_id)) => match owner {
                Some(app) if app.client_id == *client_id => (EffectiveApp::owner(app), false),
                _ => return Err(app_changed()),
            },
            (None, None) => match owner {
                Some(app) => (EffectiveApp::owner(app), true),
                None => return Err(app_changed()),
            },
            // A record that has one field only is not one that the station
            // wrote.
            _ => return Err(app_changed()),
        };
        let mut fields = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", old.expose().as_str()),
            ("client_id", app.client_id.as_str()),
        ];
        if let Some(secret) = &app.client_secret {
            fields.push(("client_secret", secret.expose().as_str()));
        }
        match http::post_form(&self.programs, LINEAR_TOKEN, &fields).await {
            Ok(answer) => Ok((answer, legacy.then(|| app.client_id.clone()))),
            // A legacy account: Linear refused the one app that the station
            // may try, and no other is tried. A failure without an answer
            // (timeout, curl) says nothing about the app.
            Err(e) if legacy && e.status != 0 => Err(app_changed()),
            Err(e) => Err(e.fail),
        }
    }
}

#[cfg(test)]
mod tests {
    //! The built-in app, the account identity, the legacy accounts and the
    //! fixed errors (design §6), with a fake `curl` that logs its stdin.

    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use fleetdeck_core::logging::{LogFormat, env_filter, subscriber};
    use fleetdeck_proto::accounts::{
        AccountMethod, AccountStatus, AppSet, OAuthFinishParams, OAuthStartParams,
    };

    use super::super::oauth::LINEAR_BUILTIN_REDIRECTS;
    use super::super::test_support::{curl, log, programs, service};
    use super::super::{APP_CHANGED_TEXT, now_secs};
    use super::*;

    const SECRET: &str = "client-secret-canary-1";
    const CODE: &str = "auth-code-canary-2";
    const ACCESS: &str = "access-canary-3";
    const REFRESH: &str = "refresh-canary-4";
    const ACCESS2: &str = "access-canary-5";
    const REFRESH2: &str = "refresh-canary-6";
    const BODY_CANARY: &str = "linear-body-canary-7";
    const STATE: &str = "state_0123456789abcdefghijklmnopqrstu";
    const OWN_REDIRECT: &str = "https://board.example/oauth/callback";
    const ACCT: &str = "linear-00000001";
    const SITE: &str = "https://linear.app/acme";

    fn token_answer() -> String {
        format!(
            r#"{{"access_token":"{ACCESS}","refresh_token":"{REFRESH}","expires_in":3600,"scope":"x"}}"#
        )
    }

    fn refresh_answer() -> String {
        format!(r#"{{"access_token":"{ACCESS2}","refresh_token":"{REFRESH2}","expires_in":3600}}"#)
    }

    const VIEWER: &str =
        r#"{"data":{"viewer":{"name":"Ana L","organization":{"name":"Acme","urlKey":"acme"}}}}"#;

    /// A service whose fake curl answers the Linear token endpoint, the
    /// viewer query and a refresh.
    fn linear(tmp: &Path) -> Accounts {
        let token = token_answer();
        let refresh = refresh_answer();
        with_cases(
            tmp,
            &[
                ("grant_type=refresh_token", &refresh, 200),
                ("api.linear.app/oauth/token", &token, 200),
                ("viewer", VIEWER, 200),
            ],
        )
    }

    fn with_cases(tmp: &Path, cases: &[(&str, &str, u16)]) -> Accounts {
        let mut p = programs(tmp);
        p.curl = curl(&tmp.join("curl"), cases);
        let mut a = service(tmp, p);
        // These tests are about the built-in app: the gate is open.
        a.builtin_probe = || true;
        a
    }

    fn set_owner(a: &Accounts, client_id: &str) {
        a.app_set(AppSet {
            kind: AccountKind::Linear,
            client_id: client_id.into(),
            client_secret: SECRET.into(),
        })
        .unwrap();
    }

    fn start(a: &Accounts, kind: AccountKind, redirect: &str) -> Result<String, Fail> {
        a.oauth_start(OAuthStartParams {
            kind,
            state: STATE.into(),
            redirect_uri: redirect.into(),
        })
        .map(|s| s.authorize_url)
    }

    fn finish() -> OAuthFinishParams {
        OAuthFinishParams {
            state: STATE.into(),
            code: CODE.into(),
        }
    }

    /// The form bodies that the fake curl got for a token request.
    fn bodies(tmp: &Path) -> Vec<String> {
        log(&tmp.join("curl"), "stdin.log")
            .lines()
            .filter(|l| l.starts_with("data-binary = \"grant_type"))
            .map(str::to_owned)
            .collect()
    }

    fn account(source: Option<AppSourceDisk>, client_id: Option<&str>) -> StoredAccount {
        StoredAccount {
            kind: AccountKind::Linear,
            method: AccountMethod::Oauth,
            site: Some(SITE.into()),
            cloud_id: None,
            user: "Ana".into(),
            access_token: Secret::new("old-access".into()),
            refresh_token: Some(Secret::new("old-refresh".into())),
            expires_at: Some(now_secs() - 10),
            email: None,
            generation: 0,
            app_source: source,
            app_client_id: client_id.map(str::to_owned),
        }
    }

    fn status_of(a: &Accounts, id: &str) -> (AccountStatus, String) {
        let stored = a.store().unwrap().account(id).unwrap().unwrap();
        let v = a.view(id, &stored);
        (v.status, v.message)
    }

    #[test]
    fn the_builtin_app_starts_a_linear_sign_in_without_an_owner_app() {
        for redirect in LINEAR_BUILTIN_REDIRECTS {
            let tmp = tempfile::tempdir().unwrap();
            let a = with_cases(tmp.path(), &[]);
            let url = start(&a, AccountKind::Linear, redirect).unwrap();
            assert!(
                url.starts_with(&format!(
                    "https://linear.app/oauth/authorize?client_id={LINEAR_BUILTIN_CLIENT_ID}&redirect_uri="
                )),
                "{url}"
            );
            assert!(url.contains("&code_challenge_method=S256") && !url.contains(SECRET));
        }
    }

    #[test]
    fn a_redirect_that_the_builtin_app_does_not_register_is_refused_before_any_request() {
        let tmp = tempfile::tempdir().unwrap();
        let a = with_cases(tmp.path(), &[]);
        for bad in [
            "https://board.example/oauth/callback",
            "https://127.0.0.1:7421/oauth/callback",
            "https://localhost:7421/oauth/callback",
            "http://127.0.0.1:7420/oauth/callback",
            "http://localhost:7420/oauth/callback",
            "https://[::1]:7420/oauth/callback",
            "https://127.0.0.1/oauth/callback",
            "https://127.0.0.1:7420.evil.example/oauth/callback",
            "https://localhost.evil.example:7420/oauth/callback",
            "https://LOCALHOST:7420/oauth/callback",
            "https://127.0.0.1:7420/oauth/callback2",
            "https://127.0.0.1:7420/oauth/callback?x=1",
            "https://127.0.0.1:7420/oauth/callback#x",
        ] {
            let err = start(&a, AccountKind::Linear, bad).unwrap_err();
            assert_eq!(err.code, "builtin_needs_localhost", "{bad}");
            assert_eq!(err.message, BUILTIN_NEEDS_LOCALHOST_TEXT, "{bad}");
        }
        assert!(log(&tmp.path().join("curl"), "argv.log").is_empty());
        assert!(a.transactions.lock().unwrap().is_empty());
    }

    #[test]
    fn the_owner_app_wins_when_set_and_jira_without_an_app_is_still_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let a = with_cases(tmp.path(), &[]);
        set_owner(&a, "client-1");
        let url = start(&a, AccountKind::Linear, OWN_REDIRECT).unwrap();
        assert!(url.contains("client_id=client-1&"), "{url}");
        assert!(!url.contains(LINEAR_BUILTIN_CLIENT_ID));
        let err = start(&a, AccountKind::Jira, OWN_REDIRECT).unwrap_err();
        assert_eq!(err.code, "unavailable");
        let err = start(&a, AccountKind::Jira, LINEAR_BUILTIN_REDIRECTS[0]).unwrap_err();
        assert_eq!(err.code, "unavailable");
    }

    #[tokio::test]
    async fn a_builtin_sign_in_sends_no_secret_and_an_owner_sign_in_sends_it() {
        let tmp = tempfile::tempdir().unwrap();
        let a = linear(tmp.path());
        start(&a, AccountKind::Linear, LINEAR_BUILTIN_REDIRECTS[1]).unwrap();
        let view = a.oauth_finish(finish()).await.unwrap();
        let body = bodies(tmp.path()).remove(0);
        assert!(!body.contains("client_secret"), "{body}");
        assert!(
            body.contains(&format!("client_id={LINEAR_BUILTIN_CLIENT_ID}"))
                && body.contains("code_verifier=")
                && body.contains(&format!("code={CODE}")),
            "{body}"
        );
        let stored = a
            .store()
            .unwrap()
            .account(&view.account_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.app_source, Some(AppSourceDisk::Builtin));
        assert_eq!(
            stored.app_client_id.as_deref(),
            Some(LINEAR_BUILTIN_CLIENT_ID)
        );

        let tmp = tempfile::tempdir().unwrap();
        let a = linear(tmp.path());
        set_owner(&a, "client-1");
        start(&a, AccountKind::Linear, OWN_REDIRECT).unwrap();
        let view = a.oauth_finish(finish()).await.unwrap();
        let body = bodies(tmp.path()).remove(0);
        assert!(body.contains(&format!("client_secret={SECRET}")), "{body}");
        assert!(body.contains("client_id=client-1"), "{body}");
        let stored = a
            .store()
            .unwrap()
            .account(&view.account_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.app_source, Some(AppSourceDisk::Owner));
        assert_eq!(stored.app_client_id.as_deref(), Some("client-1"));
        let argv = log(&tmp.path().join("curl"), "argv.log");
        assert!(!argv.contains(SECRET) && !argv.contains(CODE), "{argv}");
    }

    #[tokio::test]
    async fn an_owner_app_set_while_a_builtin_sign_in_is_open_ends_it() {
        let tmp = tempfile::tempdir().unwrap();
        let a = linear(tmp.path());
        start(&a, AccountKind::Linear, LINEAR_BUILTIN_REDIRECTS[0]).unwrap();
        set_owner(&a, "client-1");
        let err = a.oauth_finish(finish()).await.unwrap_err();
        assert_eq!(err.code, "invalid_params");
        assert!(a.store().unwrap().account_ids().unwrap().is_empty());
        assert!(bodies(tmp.path()).is_empty());
    }

    #[test]
    fn two_apps_are_the_same_only_with_the_same_source_client_id_and_generation() {
        let owner = |id: &str, generation| EffectiveApp {
            source: AppSource::Owner,
            client_id: id.into(),
            client_secret: None,
            generation,
        };
        let builtin = EffectiveApp::builtin("x".into());
        assert!(owner("x", 1).same_as(&owner("x", 1)));
        assert!(!owner("x", 1).same_as(&owner("x", 2)));
        assert!(!owner("x", 1).same_as(&owner("y", 1)));
        assert!(!owner("x", 0).same_as(&builtin));
        assert!(builtin.same_as(&EffectiveApp::builtin("x".into())));
    }

    #[tokio::test]
    async fn the_same_workspace_by_two_apps_is_two_records_and_the_same_app_is_one() {
        let tmp = tempfile::tempdir().unwrap();
        let a = linear(tmp.path());
        start(&a, AccountKind::Linear, LINEAR_BUILTIN_REDIRECTS[1]).unwrap();
        let first = a.oauth_finish(finish()).await.unwrap();
        start(&a, AccountKind::Linear, LINEAR_BUILTIN_REDIRECTS[0]).unwrap();
        let again = a.oauth_finish(finish()).await.unwrap();
        assert_eq!(first.account_id, again.account_id, "the same app");
        assert_eq!(a.store().unwrap().account_ids().unwrap().len(), 1);

        set_owner(&a, "client-1");
        start(&a, AccountKind::Linear, OWN_REDIRECT).unwrap();
        let owner = a.oauth_finish(finish()).await.unwrap();
        assert_ne!(first.account_id, owner.account_id);
        assert_eq!(a.store().unwrap().account_ids().unwrap().len(), 2);
        start(&a, AccountKind::Linear, OWN_REDIRECT).unwrap();
        let owner_again = a.oauth_finish(finish()).await.unwrap();
        assert_eq!(owner.account_id, owner_again.account_id);
        assert_eq!(a.store().unwrap().account_ids().unwrap().len(), 2);

        // Another owner client id is another app: a third record.
        set_owner(&a, "client-2");
        start(&a, AccountKind::Linear, OWN_REDIRECT).unwrap();
        let third = a.oauth_finish(finish()).await.unwrap();
        assert!(third.account_id != owner.account_id && third.account_id != first.account_id);
        assert_eq!(a.store().unwrap().account_ids().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_legacy_record_is_never_replaced_by_a_sign_in() {
        let tmp = tempfile::tempdir().unwrap();
        let a = linear(tmp.path());
        a.store()
            .unwrap()
            .put_account(ACCT, &account(None, None))
            .unwrap();
        start(&a, AccountKind::Linear, LINEAR_BUILTIN_REDIRECTS[1]).unwrap();
        let new = a.oauth_finish(finish()).await.unwrap();
        assert_ne!(new.account_id, ACCT);
        let old = a.store().unwrap().account(ACCT).unwrap().unwrap();
        assert_eq!(old.access_token.expose(), "old-access");
        assert!(old.app_source.is_none() && old.app_client_id.is_none());
        assert_eq!(a.store().unwrap().account_ids().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_builtin_account_refreshes_with_its_own_client_id_after_an_owner_app_is_set() {
        let tmp = tempfile::tempdir().unwrap();
        let a = linear(tmp.path());
        a.store()
            .unwrap()
            .put_account(
                ACCT,
                &account(Some(AppSourceDisk::Builtin), Some("builtin-made-it")),
            )
            .unwrap();
        set_owner(&a, "client-1");
        let token = a.fresh(ACCT).await.unwrap();
        assert_eq!(token.expose(), ACCESS2);
        let body = bodies(tmp.path()).remove(0);
        assert!(body.contains("grant_type=refresh_token"), "{body}");
        assert!(body.contains("client_id=builtin-made-it"), "{body}");
        assert!(
            !body.contains("client_secret") && !body.contains(SECRET),
            "{body}"
        );
        let stored = a.store().unwrap().account(ACCT).unwrap().unwrap();
        assert_eq!(stored.app_source, Some(AppSourceDisk::Builtin));
        assert_eq!(stored.app_client_id.as_deref(), Some("builtin-made-it"));
        assert_eq!(stored.refresh_token.unwrap().expose(), REFRESH2);
    }

    #[tokio::test]
    async fn an_owner_account_refreshes_with_the_secret_while_its_client_id_is_stored() {
        let tmp = tempfile::tempdir().unwrap();
        let a = linear(tmp.path());
        set_owner(&a, "client-1");
        a.store()
            .unwrap()
            .put_account(ACCT, &account(Some(AppSourceDisk::Owner), Some("client-1")))
            .unwrap();
        a.fresh(ACCT).await.unwrap();
        let body = bodies(tmp.path()).remove(0);
        assert!(body.contains("client_id=client-1"), "{body}");
        assert!(body.contains(&format!("client_secret={SECRET}")), "{body}");
    }

    #[tokio::test]
    async fn an_owner_account_after_the_client_id_changed_is_app_changed_with_no_request() {
        let tmp = tempfile::tempdir().unwrap();
        let a = linear(tmp.path());
        set_owner(&a, "client-2");
        a.store()
            .unwrap()
            .put_account(ACCT, &account(Some(AppSourceDisk::Owner), Some("client-1")))
            .unwrap();
        let err = a.fresh(ACCT).await.unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            ("app_changed", APP_CHANGED_TEXT)
        );
        assert!(log(&tmp.path().join("curl"), "argv.log").is_empty());
        assert_eq!(
            status_of(&a, ACCT),
            (AccountStatus::NeedsReconnect, APP_CHANGED_TEXT.to_owned())
        );
        let list = a
            .call("accounts.list", serde_json::json!({}), &[])
            .await
            .unwrap();
        assert_eq!(list["accounts"][0]["status"], "needs_reconnect");
        assert_eq!(list["accounts"][0]["message"], APP_CHANGED_TEXT);
        // No owner app at all: the same.
        let tmp = tempfile::tempdir().unwrap();
        let a = linear(tmp.path());
        a.store()
            .unwrap()
            .put_account(ACCT, &account(Some(AppSourceDisk::Owner), Some("client-1")))
            .unwrap();
        assert_eq!(a.fresh(ACCT).await.unwrap_err().code, "app_changed");
        assert!(log(&tmp.path().join("curl"), "argv.log").is_empty());
    }

    #[tokio::test]
    async fn a_reconnect_with_the_same_app_clears_the_mark() {
        let tmp = tempfile::tempdir().unwrap();
        let a = linear(tmp.path());
        set_owner(&a, "client-2");
        let mut old = account(Some(AppSourceDisk::Owner), Some("client-2"));
        old.refresh_token = None;
        a.store().unwrap().put_account(ACCT, &old).unwrap();
        a.set_needs_reconnect(ACCT);
        assert_eq!(status_of(&a, ACCT).0, AccountStatus::NeedsReconnect);
        start(&a, AccountKind::Linear, OWN_REDIRECT).unwrap();
        let view = a.oauth_finish(finish()).await.unwrap();
        assert_eq!(view.account_id, ACCT);
        assert_eq!(view.status, AccountStatus::Ok);
    }

    #[tokio::test]
    async fn a_legacy_account_is_bound_to_the_owner_app_when_linear_accepts_the_refresh() {
        let tmp = tempfile::tempdir().unwrap();
        let a = linear(tmp.path());
        set_owner(&a, "client-1");
        a.store()
            .unwrap()
            .put_account(ACCT, &account(None, None))
            .unwrap();
        assert_eq!(a.fresh(ACCT).await.unwrap().expose(), ACCESS2);
        let body = bodies(tmp.path()).remove(0);
        assert!(
            body.contains("client_id=client-1") && body.contains(SECRET),
            "{body}"
        );
        let stored = a.store().unwrap().account(ACCT).unwrap().unwrap();
        assert_eq!(stored.app_source, Some(AppSourceDisk::Owner));
        assert_eq!(stored.app_client_id.as_deref(), Some("client-1"));
        assert_eq!(stored.access_token.expose(), ACCESS2);
        assert_eq!(stored.refresh_token.unwrap().expose(), REFRESH2);
        // The tokens and the binding came in one write.
        assert_eq!(stored.generation, 2);
    }

    #[tokio::test]
    async fn a_legacy_account_that_linear_refuses_stays_legacy_with_app_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let a = with_cases(
            tmp.path(),
            &[(
                "grant_type=refresh_token",
                r#"{"error":"invalid_grant"}"#,
                400,
            )],
        );
        set_owner(&a, "client-1");
        a.store()
            .unwrap()
            .put_account(ACCT, &account(None, None))
            .unwrap();
        let err = a.fresh(ACCT).await.unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            ("app_changed", APP_CHANGED_TEXT)
        );
        let stored = a.store().unwrap().account(ACCT).unwrap().unwrap();
        assert!(stored.app_source.is_none() && stored.app_client_id.is_none());
        assert_eq!(stored.access_token.expose(), "old-access");
        assert_eq!(bodies(tmp.path()).len(), 1, "no second app is tried");
        assert_eq!(status_of(&a, ACCT).0, AccountStatus::NeedsReconnect);
    }

    #[tokio::test]
    async fn a_legacy_account_is_not_bound_by_an_answer_without_tokens() {
        let tmp = tempfile::tempdir().unwrap();
        let a = with_cases(tmp.path(), &[("grant_type=refresh_token", "{}", 200)]);
        set_owner(&a, "client-1");
        a.store()
            .unwrap()
            .put_account(ACCT, &account(None, None))
            .unwrap();
        assert_eq!(a.fresh(ACCT).await.unwrap_err().code, "app_changed");
        let stored = a.store().unwrap().account(ACCT).unwrap().unwrap();
        assert!(stored.app_source.is_none() && stored.app_client_id.is_none());
    }

    #[tokio::test]
    async fn a_legacy_account_without_an_owner_app_is_app_changed_with_no_request() {
        let tmp = tempfile::tempdir().unwrap();
        let a = linear(tmp.path());
        a.store()
            .unwrap()
            .put_account(ACCT, &account(None, None))
            .unwrap();
        assert_eq!(a.fresh(ACCT).await.unwrap_err().code, "app_changed");
        assert!(log(&tmp.path().join("curl"), "argv.log").is_empty());
        let stored = a.store().unwrap().account(ACCT).unwrap().unwrap();
        assert!(stored.app_source.is_none());
    }

    #[tokio::test]
    async fn an_old_file_reads_as_legacy_and_nothing_is_rewritten_at_start() {
        let tmp = tempfile::tempdir().unwrap();
        let a = linear(tmp.path());
        let file = a.store().unwrap().dir().join(format!("acct-{ACCT}.json"));
        let old = r#"{"kind":"linear","method":"oauth","site":"https://linear.app/acme","user":"Ana","access_token":"old-access","refresh_token":"old-refresh","expires_at":4000000000,"generation":3}"#;
        let mut f = std::fs::File::create(&file).unwrap();
        f.write_all(old.as_bytes()).unwrap();
        drop(f);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let stored = a.store().unwrap().account(ACCT).unwrap().unwrap();
        assert!(stored.app_source.is_none() && stored.app_client_id.is_none());
        let list = a
            .call("accounts.list", serde_json::json!({}), &[])
            .await
            .unwrap();
        assert_eq!(list["accounts"][0]["status"], "ok");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), old);
    }

    #[test]
    fn the_account_record_keeps_its_app_and_writes_no_field_for_none() {
        let tmp = tempfile::tempdir().unwrap();
        let a = with_cases(tmp.path(), &[]);
        let store = a.store().unwrap();
        store
            .put_account(
                ACCT,
                &account(Some(AppSourceDisk::Builtin), Some(LINEAR_BUILTIN_CLIENT_ID)),
            )
            .unwrap();
        let text = std::fs::read_to_string(store.dir().join(format!("acct-{ACCT}.json"))).unwrap();
        assert!(text.contains(r#""app_source":"builtin""#), "{text}");
        assert!(text.contains(&format!(r#""app_client_id":"{LINEAR_BUILTIN_CLIENT_ID}""#)));
        store
            .put_account("linear-00000002", &account(None, None))
            .unwrap();
        let text = std::fs::read_to_string(store.dir().join("acct-linear-00000002.json")).unwrap();
        assert!(
            !text.contains("app_source") && !text.contains("app_client_id"),
            "{text}"
        );
    }

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl Write for Buf {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_linear_answer_body_reaches_no_error_no_log_and_no_response() {
        let buf = Buf::default();
        let sub = subscriber(
            LogFormat::Text,
            env_filter("info").unwrap(),
            {
                let buf = buf.clone();
                move || buf.clone()
            },
            false,
        );
        let _guard = tracing::subscriber::set_default(sub);
        let answer = format!(
            r#"{{"error":"invalid_grant","error_description":"{BODY_CANARY} {ACCESS} {CODE} {REFRESH}"}}"#
        );
        let tmp = tempfile::tempdir().unwrap();
        let a = with_cases(
            tmp.path(),
            &[
                ("grant_type=refresh_token", &answer, 400),
                ("api.linear.app/oauth/token", &answer, 400),
            ],
        );
        // The code exchange.
        start(&a, AccountKind::Linear, LINEAR_BUILTIN_REDIRECTS[1]).unwrap();
        let err = a.oauth_finish(finish()).await.unwrap_err();
        assert_eq!(err.code, "upstream");
        assert_eq!(err.message, "the server answered HTTP 400");
        let mut texts = vec![format!("{err:?}")];
        // The refresh of a built-in account and of an owner account.
        set_owner(&a, "client-1");
        for (id, source, client) in [
            ("linear-00000003", AppSourceDisk::Builtin, "builtin-made-it"),
            ("linear-00000004", AppSourceDisk::Owner, "client-1"),
        ] {
            a.store()
                .unwrap()
                .put_account(id, &account(Some(source), Some(client)))
                .unwrap();
            let err = a.fresh(id).await.unwrap_err();
            assert_eq!(err.code, "unavailable");
            texts.push(format!("{err:?}"));
            texts.push(status_of(&a, id).1);
        }
        // A legacy account that Linear refuses.
        a.store()
            .unwrap()
            .put_account("linear-00000005", &account(None, None))
            .unwrap();
        let err = a.fresh("linear-00000005").await.unwrap_err();
        texts.push(format!("{err:?}"));
        let list = a
            .call("accounts.list", serde_json::json!({}), &[])
            .await
            .unwrap();
        texts.push(list.to_string());
        let verifier = bodies(tmp.path())[0]
            .split("code_verifier=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .trim_end_matches('"')
            .to_owned();
        assert!(verifier.len() >= 40, "{verifier}");
        let logged = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(!logged.is_empty(), "the failed refresh is logged");
        texts.push(logged);
        texts.push(log(&tmp.path().join("curl"), "argv.log"));
        for text in &texts {
            for canary in [
                BODY_CANARY,
                ACCESS,
                CODE,
                REFRESH,
                SECRET,
                verifier.as_str(),
            ] {
                assert!(!text.contains(canary), "{canary} is in {text}");
            }
        }
    }

    #[test]
    fn the_debug_text_of_an_app_and_a_transaction_hides_the_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let a = with_cases(tmp.path(), &[]);
        set_owner(&a, "client-1");
        start(&a, AccountKind::Linear, OWN_REDIRECT).unwrap();
        let open = a.transactions.lock().unwrap();
        let text = format!("{:?} {:#?}", open[0], open[0]);
        assert!(!text.contains(SECRET), "{text}");
        assert!(text.contains("client-1"));
    }
}
