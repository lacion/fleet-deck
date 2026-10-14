//! The retry file of `fleetdeck station join --tailnet` (DL-178, tailnet
//! design 5.4): `<station data>/pair-pending.json`, mode 0600.
//!
//! The station writes it **before** it sends `PairRequest`, with a sync of
//! the file and of its directory, and removes it after it stored the deck
//! pin or after `--cancel`. It holds the complete original request, so a
//! station that crashed or that was renamed still verifies a resumed
//! `PairApproved` against the request it made.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use fleetdeck_proto::link::Scope;
use serde::{Deserialize, Serialize};

use crate::pair_request::CanonicalRequest;
use crate::pairing::{hex, unhex};

/// The file name in the station data directory.
pub const FILE_NAME: &str = "pair-pending.json";

/// Why the retry file can not be used.
#[derive(Debug, thiserror::Error)]
pub enum RetryError {
    #[error("retry file `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the retry file `{0}` does not hold a valid pairing request")]
    Invalid(PathBuf),
}

/// The content of the retry file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryRecord {
    pub version: u32,
    /// The deck as the person typed it (a name or an address).
    pub host: String,
    pub port: u16,
    /// The deck link key fingerprint (64 hex), confirmed by the person.
    pub deck_fingerprint: String,
    pub request_id: String,
    pub name: String,
    pub nonce: String,
    pub offered_scopes: Vec<Scope>,
    /// The request digest (64 hex).
    pub request_digest: String,
}

impl RetryRecord {
    /// A record for a new request.
    #[must_use]
    pub fn new(host: &str, port: u16, deck_fingerprint: &str, request: &CanonicalRequest) -> Self {
        Self {
            version: 1,
            host: host.to_owned(),
            port,
            deck_fingerprint: deck_fingerprint.to_owned(),
            request_id: request.id_hex(),
            name: request.name.clone(),
            nonce: request.nonce_hex(),
            offered_scopes: request.scopes_vec(),
            request_digest: hex(&request.digest()),
        }
    }

    /// The saved request. `None` when a field is invalid or the digest does
    /// not match the fields.
    #[must_use]
    pub fn request(&self) -> Option<CanonicalRequest> {
        let request = CanonicalRequest::from_frame(
            &self.request_id,
            &self.name,
            &self.nonce,
            &self.offered_scopes,
        )
        .ok()?;
        (self.version == 1
            && unhex::<32>(&self.request_digest)? == request.digest()
            && unhex::<32>(&self.deck_fingerprint).is_some())
        .then_some(request)
    }

    #[must_use]
    pub fn offered(&self) -> BTreeSet<Scope> {
        self.offered_scopes.iter().copied().collect()
    }
}

fn io(path: &Path, source: std::io::Error) -> RetryError {
    RetryError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn sync_dir(dir: &Path) -> Result<(), RetryError> {
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| io(dir, e))
}

/// Writes the file (mode 0600): a temporary file, its sync, the rename and
/// the sync of the directory.
///
/// # Errors
/// [`RetryError::Io`].
pub fn write(dir: &Path, record: &RetryRecord) -> Result<(), RetryError> {
    let path = dir.join(FILE_NAME);
    let tmp = dir.join(format!("{FILE_NAME}.tmp"));
    let text = serde_json::to_vec_pretty(record).map_err(|e| io(&path, e.into()))?;
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| io(&tmp, e))?;
    file.write_all(&text).map_err(|e| io(&tmp, e))?;
    file.sync_all().map_err(|e| io(&tmp, e))?;
    drop(file);
    std::fs::rename(&tmp, &path).map_err(|e| io(&path, e))?;
    sync_dir(dir)
}

/// Reads the file; `None` when there is none.
///
/// # Errors
/// [`RetryError`] for a file that can not be read or does not hold a valid
/// request.
pub fn read(dir: &Path) -> Result<Option<RetryRecord>, RetryError> {
    let path = dir.join(FILE_NAME);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(&path, e)),
    };
    let record: RetryRecord =
        serde_json::from_slice(&bytes).map_err(|_| RetryError::Invalid(path.clone()))?;
    if record.request().is_none() {
        return Err(RetryError::Invalid(path));
    }
    Ok(Some(record))
}

/// Removes the file and syncs the directory. Returns whether a file existed.
///
/// # Errors
/// [`RetryError::Io`].
pub fn remove(dir: &Path) -> Result<bool, RetryError> {
    let path = dir.join(FILE_NAME);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            sync_dir(dir)?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io(&path, e)),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn request() -> CanonicalRequest {
        CanonicalRequest::generate("laptop", [Scope::Read, Scope::Drive].into()).unwrap()
    }

    #[test]
    fn the_file_round_trips_with_mode_0600_and_goes_away() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path()).unwrap().is_none());
        let req = request();
        let record = RetryRecord::new("deck.tail.ts.net", 7421, &"ab".repeat(32), &req);
        write(dir.path(), &record).unwrap();
        let mode = std::fs::metadata(dir.path().join(FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o600);
        let back = read(dir.path()).unwrap().unwrap();
        assert_eq!(back, record);
        assert_eq!(back.request().unwrap(), req);
        assert!(remove(dir.path()).unwrap());
        assert!(!remove(dir.path()).unwrap());
        assert!(read(dir.path()).unwrap().is_none());
    }

    #[test]
    fn a_changed_field_makes_the_file_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let mut record = RetryRecord::new("h", 1, &"ab".repeat(32), &request());
        record.name = "other".into();
        write(dir.path(), &record).unwrap();
        assert!(matches!(
            read(dir.path()),
            Err(RetryError::Invalid(_))
        ));
        std::fs::write(dir.path().join(FILE_NAME), b"not json").unwrap();
        assert!(matches!(read(dir.path()), Err(RetryError::Invalid(_))));
    }
}
