//! The credential store of a station: `<station data>/accounts/` (DL-173,
//! design §2.1 and §2.3).
//!
//! - `app-<kind>.json` holds the OAuth app of a tracker.
//! - `acct-<account_id>.json` holds one connected account with its tokens.
//! - The directory has mode 0700 and is made durable at the start. Every
//!   file has mode 0600. A write goes to an O_EXCL 0600 temporary file, is
//!   fsynced, renamed over the final name, and the directory is fsynced.
//!   A read refuses a link, a file that is not a regular file and a file
//!   whose mode is not 0600.
//! - Secret fields are [`Secret`]. `Debug` of the records is written by
//!   hand and hides them.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use fleetdeck_core::Secret;
use fleetdeck_core::paths::ensure_private_dir;
use fleetdeck_proto::accounts::{AccountKind, AccountMethod};
use ring::rand::{SecureRandom, SystemRandom};
use rustix::fs::OFlags;
use serde::{Deserialize, Serialize};

/// Why the store cannot read or write.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("the account store: {0}")]
    Io(#[from] std::io::Error),
    #[error("a store file is a link, is not a regular file or does not have mode 0600")]
    BadFile,
    #[error("a store file does not hold a valid record")]
    BadRecord,
    #[error("the system random source failed")]
    Random,
}

/// The OAuth app of a tracker.
pub struct StoredApp {
    pub client_id: String,
    pub client_secret: Secret<String>,
    pub generation: u32,
}

impl fmt::Debug for StoredApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredApp")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("generation", &self.generation)
            .finish()
    }
}

/// One connected account of the store.
pub struct StoredAccount {
    pub kind: AccountKind,
    pub method: AccountMethod,
    pub site: Option<String>,
    pub cloud_id: Option<String>,
    pub user: String,
    pub access_token: Secret<String>,
    pub refresh_token: Option<Secret<String>>,
    /// Unix seconds.
    pub expires_at: Option<i64>,
    pub email: Option<String>,
}

impl fmt::Debug for StoredAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredAccount")
            .field("kind", &self.kind)
            .field("method", &self.method)
            .field("site", &self.site)
            .field("cloud_id", &self.cloud_id)
            .field("user", &self.user)
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at", &self.expires_at)
            .field("email", &self.email)
            .finish()
    }
}

impl Clone for StoredAccount {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind,
            method: self.method,
            site: self.site.clone(),
            cloud_id: self.cloud_id.clone(),
            user: self.user.clone(),
            access_token: Secret::new(self.access_token.expose().clone()),
            refresh_token: self
                .refresh_token
                .as_ref()
                .map(|t| Secret::new(t.expose().clone())),
            expires_at: self.expires_at,
            email: self.email.clone(),
        }
    }
}

/// The bytes on disk of a [`StoredApp`].
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppDisk {
    client_id: String,
    client_secret: String,
    generation: u32,
}

/// The bytes on disk of a [`StoredAccount`].
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountDisk {
    kind: AccountKind,
    method: AccountMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    site: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cloud_id: Option<String>,
    user: String,
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
}

/// True for an account id of the store: `jira-` or `linear-` and 8 hex
/// digits. Only such an id becomes a file name.
#[must_use]
pub fn valid_account_id(id: &str) -> bool {
    id.strip_prefix("jira-")
        .or_else(|| id.strip_prefix("linear-"))
        .is_some_and(|hex| {
            hex.len() == 8
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

/// The name of a kind in a file name, for the kinds that have an app.
fn app_kind_name(kind: AccountKind) -> Option<&'static str> {
    match kind {
        AccountKind::Jira => Some("jira"),
        AccountKind::Linear => Some("linear"),
        AccountKind::Github | AccountKind::Gitlab => None,
    }
}

/// A new account id for `kind` (Jira or Linear): `<kind>-<8 random hex>`.
///
/// # Errors
/// [`StoreError::Random`], or [`StoreError::BadRecord`] for a forge kind.
pub fn new_account_id(kind: AccountKind) -> Result<String, StoreError> {
    let name = app_kind_name(kind).ok_or(StoreError::BadRecord)?;
    Ok(format!("{name}-{}", random_hex(4)?))
}

fn random_hex(bytes: usize) -> Result<String, StoreError> {
    let mut buf = vec![0u8; bytes];
    SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| StoreError::Random)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// The credential directory of a station.
#[derive(Debug, Clone)]
pub struct AccountStore {
    dir: PathBuf,
}

impl AccountStore {
    /// Creates the directory (mode 0700) and makes it and its parent
    /// durable.
    ///
    /// # Errors
    /// [`StoreError::Io`].
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        ensure_private_dir(dir)?;
        sync_dir(dir)?;
        if let Some(parent) = dir.parent() {
            sync_dir(parent)?;
        }
        Ok(Self {
            dir: dir.to_owned(),
        })
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The app of `kind`, or `None` when the owner has set none.
    ///
    /// # Errors
    /// [`StoreError`].
    pub fn app(&self, kind: AccountKind) -> Result<Option<StoredApp>, StoreError> {
        let Some(name) = app_kind_name(kind) else {
            return Ok(None);
        };
        let Some(bytes) = self.read(&format!("app-{name}.json"))? else {
            return Ok(None);
        };
        let disk: AppDisk = serde_json::from_slice(&bytes).map_err(|_| StoreError::BadRecord)?;
        Ok(Some(StoredApp {
            client_id: disk.client_id,
            client_secret: Secret::new(disk.client_secret),
            generation: disk.generation,
        }))
    }

    /// Stores the app of `kind`.
    ///
    /// # Errors
    /// [`StoreError`].
    pub fn set_app(&self, kind: AccountKind, app: &StoredApp) -> Result<(), StoreError> {
        let name = app_kind_name(kind).ok_or(StoreError::BadRecord)?;
        let disk = AppDisk {
            client_id: app.client_id.clone(),
            client_secret: app.client_secret.expose().clone(),
            generation: app.generation,
        };
        let bytes = serde_json::to_vec(&disk).map_err(|_| StoreError::BadRecord)?;
        self.write(&format!("app-{name}.json"), &bytes)
    }

    /// The account `id`, or `None` when no file has it.
    ///
    /// # Errors
    /// [`StoreError`]; a bad id is [`StoreError::BadRecord`].
    pub fn account(&self, id: &str) -> Result<Option<StoredAccount>, StoreError> {
        if !valid_account_id(id) {
            return Err(StoreError::BadRecord);
        }
        let Some(bytes) = self.read(&format!("acct-{id}.json"))? else {
            return Ok(None);
        };
        let d: AccountDisk = serde_json::from_slice(&bytes).map_err(|_| StoreError::BadRecord)?;
        Ok(Some(StoredAccount {
            kind: d.kind,
            method: d.method,
            site: d.site,
            cloud_id: d.cloud_id,
            user: d.user,
            access_token: Secret::new(d.access_token),
            refresh_token: d.refresh_token.map(Secret::new),
            expires_at: d.expires_at,
            email: d.email,
        }))
    }

    /// Writes the account `id`.
    ///
    /// # Errors
    /// [`StoreError`].
    pub fn put_account(&self, id: &str, a: &StoredAccount) -> Result<(), StoreError> {
        if !valid_account_id(id) {
            return Err(StoreError::BadRecord);
        }
        let disk = AccountDisk {
            kind: a.kind,
            method: a.method,
            site: a.site.clone(),
            cloud_id: a.cloud_id.clone(),
            user: a.user.clone(),
            access_token: a.access_token.expose().clone(),
            refresh_token: a.refresh_token.as_ref().map(|t| t.expose().clone()),
            expires_at: a.expires_at,
            email: a.email.clone(),
        };
        let bytes = serde_json::to_vec(&disk).map_err(|_| StoreError::BadRecord)?;
        self.write(&format!("acct-{id}.json"), &bytes)
    }

    /// Removes the account `id`. Returns whether a file was removed.
    ///
    /// # Errors
    /// [`StoreError`].
    pub fn delete_account(&self, id: &str) -> Result<bool, StoreError> {
        if !valid_account_id(id) {
            return Err(StoreError::BadRecord);
        }
        match fs::remove_file(self.dir.join(format!("acct-{id}.json"))) {
            Ok(()) => {
                sync_dir(&self.dir)?;
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// The ids of the accounts in the directory, sorted. A file with a name
    /// that is not an account id is ignored.
    ///
    /// # Errors
    /// [`StoreError::Io`].
    pub fn account_ids(&self) -> Result<Vec<String>, StoreError> {
        let mut ids = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let name = entry?.file_name();
            let Some(id) = name
                .to_str()
                .and_then(|n| n.strip_prefix("acct-"))
                .and_then(|n| n.strip_suffix(".json"))
            else {
                continue;
            };
            if valid_account_id(id) {
                ids.push(id.to_owned());
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Reads `name`: `None` when it does not exist.
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let path = self.dir.join(name);
        let flags = i32::try_from(OFlags::NOFOLLOW.bits()).unwrap_or(0);
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(flags)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            // O_NOFOLLOW on a link: ELOOP.
            Err(_) => return Err(StoreError::BadFile),
        };
        let meta = file.metadata()?;
        if !meta.is_file() || meta.mode() & 0o777 != 0o600 {
            return Err(StoreError::BadFile);
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    }

    /// Writes `name` atomically.
    fn write(&self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let tmp = self.dir.join(format!(".tmp-{}", random_hex(8)?));
        let done = (|| -> Result<(), StoreError> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&tmp, self.dir.join(name))?;
            Ok(())
        })();
        if done.is_err() {
            // The temporary file may not exist; the error of the write wins.
            let _ = fs::remove_file(&tmp);
        }
        done?;
        sync_dir(&self.dir)?;
        Ok(())
    }
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn account(token: &str) -> StoredAccount {
        StoredAccount {
            kind: AccountKind::Jira,
            method: AccountMethod::Oauth,
            site: Some("https://acme.atlassian.net".into()),
            cloud_id: Some("cloud-1".into()),
            user: "Ana".into(),
            access_token: Secret::new(token.into()),
            refresh_token: Some(Secret::new("refresh-canary-1".into())),
            expires_at: Some(1_900_000_000),
            email: None,
        }
    }

    fn store(tmp: &Path) -> AccountStore {
        AccountStore::open(&tmp.join("data").join("accounts")).unwrap()
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn the_directory_is_private_and_the_files_have_mode_0600() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        assert_eq!(mode(s.dir()), 0o700);
        s.put_account("jira-0a1b2c3d", &account("access-canary-1"))
            .unwrap();
        s.set_app(
            AccountKind::Jira,
            &StoredApp {
                client_id: "cid".into(),
                client_secret: Secret::new("secret-canary-2".into()),
                generation: 3,
            },
        )
        .unwrap();
        assert_eq!(mode(&s.dir().join("acct-jira-0a1b2c3d.json")), 0o600);
        assert_eq!(mode(&s.dir().join("app-jira.json")), 0o600);
        let names: Vec<_> = fs::read_dir(s.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(
            names.iter().all(|n| !n.starts_with(".tmp-")),
            "no temp file stays: {names:?}"
        );
        let got = s.account("jira-0a1b2c3d").unwrap().unwrap();
        assert_eq!(got.access_token.expose(), "access-canary-1");
        assert_eq!(got.cloud_id.as_deref(), Some("cloud-1"));
        let app = s.app(AccountKind::Jira).unwrap().unwrap();
        assert_eq!((app.client_id.as_str(), app.generation), ("cid", 3));
        assert_eq!(s.account_ids().unwrap(), ["jira-0a1b2c3d"]);
    }

    #[test]
    fn a_rewrite_replaces_the_file_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.put_account("linear-00000001", &account("first")).unwrap();
        s.put_account("linear-00000001", &account("second"))
            .unwrap();
        let got = s.account("linear-00000001").unwrap().unwrap();
        assert_eq!(got.access_token.expose(), "second");
        assert!(s.delete_account("linear-00000001").unwrap());
        assert!(!s.delete_account("linear-00000001").unwrap());
        assert!(s.account("linear-00000001").unwrap().is_none());
    }

    #[test]
    fn a_file_with_a_bad_mode_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.put_account("jira-0a1b2c3d", &account("t")).unwrap();
        let path = s.dir().join("acct-jira-0a1b2c3d.json");
        for bad in [0o644, 0o640, 0o666, 0o400] {
            fs::set_permissions(&path, fs::Permissions::from_mode(bad)).unwrap();
            assert!(
                matches!(s.account("jira-0a1b2c3d"), Err(StoreError::BadFile)),
                "mode {bad:o}"
            );
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(s.account("jira-0a1b2c3d").unwrap().is_some());
    }

    #[test]
    fn a_link_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        let target = tmp.path().join("elsewhere.json");
        fs::write(&target, b"{}").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&target, s.dir().join("acct-jira-0a1b2c3d.json")).unwrap();
        assert!(matches!(
            s.account("jira-0a1b2c3d"),
            Err(StoreError::BadFile)
        ));
    }

    #[test]
    fn only_an_account_id_becomes_a_file_name() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        for bad in [
            "../x",
            "jira-xyz",
            "jira-0A1B2C3D",
            "github-0a1b2c3d",
            "jira-0a1b2c3",
        ] {
            assert!(!valid_account_id(bad), "{bad}");
            assert!(s.account(bad).is_err(), "{bad}");
        }
        assert!(valid_account_id(
            &new_account_id(AccountKind::Linear).unwrap()
        ));
        assert!(new_account_id(AccountKind::Github).is_err());
    }

    #[test]
    fn debug_output_hides_every_credential() {
        let text = format!(
            "{:?} {:#?} {:?}",
            account("access-canary-1"),
            account("access-canary-1"),
            StoredApp {
                client_id: "cid".into(),
                client_secret: Secret::new("secret-canary-2".into()),
                generation: 1
            }
        );
        for canary in ["access-canary-1", "refresh-canary-1", "secret-canary-2"] {
            assert!(!text.contains(canary), "{text}");
        }
        assert!(text.contains("cid"));
    }
}
