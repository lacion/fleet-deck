//! The settings writer of one holder (design 1.3, T1, U1, U2): one save at
//! a time, atomic and private, with the secret store written first.
//!
//! The methods are synchronous and hold a `std::sync::Mutex`: a caller in
//! async code uses `spawn_blocking`. Step 6 of 1.3 (publish) is the
//! caller's job after `Ok`.

use std::collections::BTreeSet;
use std::fs::File;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use fleetdeck_proto::settings::{Durability, SecretSet, SettingsChange, SettingsSaved};
use toml_edit::DocumentMut;

use super::apply::{apply_changes, referenced_secrets, set_env_reference};
use super::diff::{danger_keys, live_keys, restart_keys};
use super::error::{SaveError, key_of};
use super::fs::{Fs, RealFs};
use super::holder::Holder;
use super::store::{ensure_store, list, random_hex, secret_name, write_secret};
use super::view::{read_config, revision_of};
use crate::config::{ConfigError, valid_env_key};
use crate::paths::Env;

/// The note at the top of a saved file.
const HEADER: &str = "# Edit with the board or `fleetdeck settings`; edit by hand only while Fleet Deck is stopped.\n";
/// The longest secret value in bytes.
const MAX_SECRET_VALUE: usize = 8192;

/// What the holder last read or wrote (S1).
struct Seen {
    revision: String,
    mtime: Option<SystemTime>,
}

struct Inner {
    seen: Seen,
    store_ready: bool,
    /// Directories of the store that were created but not yet synced.
    unsynced: Vec<PathBuf>,
}

/// The only writer of one config file while its process runs.
pub struct SettingsWriter<H: Holder> {
    path: PathBuf,
    store: Option<PathBuf>,
    env: Env,
    fs: Arc<dyn Fs>,
    inner: Mutex<Inner>,
    _holder: PhantomData<fn() -> H>,
}

/// The files that one operation writes or replaces.
struct Plan {
    new_text: String,
    /// The secret files that this operation created.
    fresh: Vec<String>,
}

impl<H: Holder> SettingsWriter<H> {
    /// Creates the writer, reads the file (S1) and creates and syncs the
    /// store directory (U2). A failure of the store is logged; the writer
    /// retries it when a save needs the store.
    #[must_use]
    pub fn new(path: &Path, store: Option<&Path>, env: &Env) -> Self {
        Self::with_fs(path, store, env, Arc::new(RealFs))
    }

    /// Like [`Self::new`], with another file system (tests).
    #[must_use]
    pub fn with_fs(path: &Path, store: Option<&Path>, env: &Env, fs: Arc<dyn Fs>) -> Self {
        let seen = match read_config(path) {
            Ok((bytes, mtime)) => Seen {
                revision: revision_of(&bytes),
                mtime,
            },
            Err(e) => {
                tracing::warn!("settings: cannot read the config file ({})", e.kind());
                Seen {
                    revision: String::new(),
                    mtime: None,
                }
            }
        };
        let mut unsynced = Vec::new();
        let store_ready = match store {
            None => true,
            Some(dir) => match ensure_store(fs.as_ref(), dir, &mut unsynced) {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!("settings: the secret store is not ready ({})", e.kind());
                    false
                }
            },
        };
        Self {
            path: path.to_path_buf(),
            store: store.map(Path::to_path_buf),
            env: env.clone(),
            fs,
            inner: Mutex::new(Inner {
                seen,
                store_ready,
                unsynced,
            }),
            _holder: PhantomData,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn parse(&self, text: &str) -> Result<H::Parsed, ConfigError> {
        H::parse(text, &self.env, self.store.as_deref())
    }

    /// Reads the installed file and checks it against what the holder last
    /// saw (S1) and against the revision of the board (1.3, step 1).
    fn begin(&self, inner: &Inner, revision: &str) -> Result<String, SaveError> {
        let (bytes, mtime) = read_config(&self.path).map_err(|e| {
            tracing::warn!("settings: cannot read the config file ({})", e.kind());
            SaveError::SettingsWriteFailed
        })?;
        let current = revision_of(&bytes);
        let mtime_differs = matches!((inner.seen.mtime, mtime), (Some(a), Some(b)) if a != b);
        if current != revision {
            return Err(SaveError::SettingsChanged);
        }
        if current != inner.seen.revision || mtime_differs {
            return Err(SaveError::ChangedOnDisk);
        }
        String::from_utf8(bytes).map_err(|_| SaveError::invalid("file"))
    }

    /// Reads the file again after a hand edit and takes it as the new base
    /// (the answer to `settings_changed_on_disk`). The caller publishes the
    /// returned config.
    ///
    /// # Errors
    /// The file cannot be read or does not parse.
    pub fn resync(&self) -> Result<H::Parsed, ConfigError> {
        let mut inner = self.lock();
        let (bytes, mtime) = read_config(&self.path).map_err(|source| ConfigError::Io {
            path: self.path.clone(),
            source,
        })?;
        let text = String::from_utf8(bytes.clone()).map_err(|_| ConfigError::Toml {
            path: self.path.clone(),
            message: "the file is not UTF-8 text".to_owned(),
        })?;
        let parsed = self.parse(&text)?;
        inner.seen = Seen {
            revision: revision_of(&bytes),
            mtime,
        };
        Ok(parsed)
    }

    fn store_dir(&self, inner: &mut Inner) -> Result<&Path, SaveError> {
        let dir = self
            .store
            .as_deref()
            .ok_or(SaveError::SecretStoreUnavailable)?;
        if !inner.store_ready {
            ensure_store(self.fs.as_ref(), dir, &mut inner.unsynced).map_err(|e| {
                tracing::warn!("settings: the secret store is not ready ({})", e.kind());
                SaveError::SecretStoreUnavailable
            })?;
            inner.store_ready = true;
        }
        Ok(dir)
    }

    /// Saves a change: 1.3, steps 1 to 5. `elevated` tells that the caller
    /// checked a recent sign-in; a danger change without it gives
    /// `elevation_required` before any write. `prepare` builds the live
    /// changes from the new config (1.4, step 1) and publishes nothing.
    ///
    /// # Errors
    /// [`SaveError`]; the file is the old one unless the result is `Ok`.
    pub fn save<P>(
        &self,
        change: &SettingsChange,
        elevated: bool,
        prepare: impl FnOnce(&H::Parsed) -> Result<P, (String, String)>,
    ) -> Result<(SettingsSaved, H::Parsed, P), SaveError> {
        let mut inner = self.lock();
        let old_text = self.begin(&inner, &change.revision)?;
        let old = self
            .parse(&old_text)
            .map_err(|e| SaveError::invalid(&key_of(&e, "file")))?;
        let mut doc: DocumentMut = old_text.parse().map_err(|_| SaveError::invalid("file"))?;
        let first = change.changes.keys().next().map_or("file", String::as_str);
        apply_changes(&mut doc, &H::fields(), &change.changes, &H::values(&old))?;
        let new_text = with_header(&doc.to_string());
        let new = self
            .parse(&new_text)
            .map_err(|e| SaveError::invalid(&key_of(&e, first)))?;
        let plan = Plan {
            new_text,
            fresh: Vec::new(),
        };
        let restart = None;
        self.finish(&mut inner, plan, &old, new, elevated, restart, prepare)
    }

    /// Stores a secret and points the config to it (T1 order). The key is
    /// `provider_instance.<id>.env.<VAR>`. A plain inline value of that
    /// variable becomes a reference. The change always needs `elevated`.
    ///
    /// # Errors
    /// [`SaveError`]; the old file and the old secrets stay unless `Ok`.
    pub fn set_secret<P>(
        &self,
        set: &SecretSet,
        revision: &str,
        elevated: bool,
        prepare: impl FnOnce(&H::Parsed) -> Result<P, (String, String)>,
    ) -> Result<(SettingsSaved, H::Parsed, P), SaveError> {
        let mut inner = self.lock();
        let old_text = self.begin(&inner, revision)?;
        let (id, var) = split_secret_key(&set.key).ok_or_else(|| SaveError::invalid(&set.key))?;
        let value_ok = (1..=MAX_SECRET_VALUE).contains(&set.value.len())
            && !set.value.chars().any(char::is_control);
        if !value_ok || !valid_env_key(var) {
            return Err(SaveError::invalid(&set.key));
        }
        if !elevated {
            return Err(SaveError::ElevationRequired {
                keys: vec!["provider_instance".to_owned()],
            });
        }
        let dir = self.store_dir(&mut inner)?.to_path_buf();
        let old = self
            .parse(&old_text)
            .map_err(|e| SaveError::invalid(&key_of(&e, "file")))?;
        let mut doc: DocumentMut = old_text.parse().map_err(|_| SaveError::invalid("file"))?;
        let name = secret_name(id, var).map_err(|_| SaveError::SettingsWriteFailed)?;
        set_env_reference(&mut doc, id, var, &name, &H::instance_ids(&old))?;
        // T1 step 1: the secret is durable before any TOML points to it.
        let plan = Plan {
            new_text: with_header(&doc.to_string()),
            fresh: vec![name.clone()],
        };
        if let Err(e) = write_secret(self.fs.as_ref(), &dir, &name, &set.value) {
            tracing::warn!("settings: cannot write a secret ({})", e.kind());
            self.drop_fresh(&dir, &plan.fresh);
            return Err(SaveError::SettingsWriteFailed);
        }
        let new = match self.parse(&plan.new_text) {
            Ok(n) => n,
            Err(e) => {
                self.drop_fresh(&dir, &plan.fresh);
                return Err(SaveError::invalid(&key_of(&e, &set.key)));
            }
        };
        self.finish(
            &mut inner,
            plan,
            &old,
            new,
            true,
            Some(vec!["provider_instance".to_owned()]),
            prepare,
        )
    }

    /// Removes secret files that this operation made and that no file
    /// references. Safe at any time before the TOML rename.
    fn drop_fresh(&self, dir: &Path, names: &[String]) {
        for name in names {
            match self.fs.remove(&dir.join(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!("settings: cannot remove an unused secret ({})", e.kind()),
            }
        }
    }

    /// Steps 3 to 5 of 1.3 and step 3 of T1.
    #[allow(clippy::too_many_arguments)]
    fn finish<P>(
        &self,
        inner: &mut Inner,
        plan: Plan,
        old: &H::Parsed,
        new: H::Parsed,
        elevated: bool,
        restart_override: Option<Vec<String>>,
        prepare: impl FnOnce(&H::Parsed) -> Result<P, (String, String)>,
    ) -> Result<(SettingsSaved, H::Parsed, P), SaveError> {
        let dir = self.store.clone();
        let fail = |this: &Self, e: SaveError| {
            if let Some(dir) = &dir {
                this.drop_fresh(dir, &plan.fresh);
            }
            e
        };
        let danger = danger_keys::<H>(old, &new);
        if !elevated && !danger.is_empty() {
            return Err(fail(self, SaveError::ElevationRequired { keys: danger }));
        }
        let new_doc: DocumentMut = plan
            .new_text
            .parse()
            .map_err(|_| fail(self, SaveError::invalid("file")))?;
        let new_refs = referenced_secrets(&new_doc);
        // A TOML that points into the store is renamed only when the store
        // directory is durable (U2).
        if !new_refs.is_empty() && self.store_dir(inner).is_err() {
            return Err(fail(self, SaveError::SecretStoreUnavailable));
        }
        let prepared = prepare(&new)
            .map_err(|(key, message)| fail(self, SaveError::CannotApply { key, message }))?;
        let durable = self.install(&plan.new_text).map_err(|e| fail(self, e))?;
        // The rename is the commit point: from here on the new file is the
        // truth, and the answer is a success.
        let bytes = plan.new_text.as_bytes();
        inner.seen = Seen {
            revision: revision_of(bytes),
            mtime: read_config(&self.path).ok().and_then(|(_, m)| m),
        };
        if durable {
            // T1 step 3: the new file is confirmed on disk, so every secret
            // that it does not reference (the old name, the leftovers of an
            // uncertain save) can go.
            self.sweep(&new_refs);
        }
        let restart = restart_override.unwrap_or_else(|| restart_keys::<H>(old, &new));
        let saved = SettingsSaved {
            values: H::values(&new),
            applied: live_keys::<H>(old, &new),
            restart_needed: restart,
            unapplied: Vec::new(),
            revision: revision_of(bytes),
            durability: if durable {
                Durability::Confirmed
            } else {
                Durability::Uncertain
            },
        };
        Ok((saved, new, prepared))
    }

    /// 1.3, step 5: temp file, write, fsync, parse again, rename, fsync of
    /// the directory. `Ok(false)`: renamed, but the directory sync failed.
    fn install(&self, text: &str) -> Result<bool, SaveError> {
        let dir = self.path.parent().ok_or(SaveError::SettingsWriteFailed)?;
        let tmp = dir.join(format!(
            "{}.tmp-{}",
            self.path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            random_hex(8).map_err(|_| SaveError::SettingsWriteFailed)?
        ));
        let fail = |e: std::io::Error| {
            tracing::warn!("settings: cannot write the config file ({})", e.kind());
            match self.fs.remove(&tmp) {
                Ok(()) => {}
                Err(r) if r.kind() == std::io::ErrorKind::NotFound => {}
                Err(r) => tracing::warn!("settings: cannot remove a temp file ({})", r.kind()),
            }
            SaveError::SettingsWriteFailed
        };
        if !dir.exists() {
            ensure_store(self.fs.as_ref(), dir, &mut Vec::new()).map_err(&fail)?;
        }
        let mut file = self.fs.create_excl_0600(&tmp).map_err(&fail)?;
        self.fs
            .write(&mut file, &tmp, text.as_bytes())
            .and_then(|()| self.fs.fsync(&file, &tmp))
            .map_err(&fail)?;
        drop(file);
        let again = std::fs::read(&tmp).map_err(&fail)?;
        let reparsed = String::from_utf8(again)
            .ok()
            .filter(|t| t == text)
            .and_then(|t| self.parse(&t).ok());
        if reparsed.is_none() {
            return Err(fail(std::io::Error::other("the temp file differs")));
        }
        self.fs.rename(&tmp, &self.path).map_err(&fail)?;
        match self.fs.fsync_dir(dir) {
            Ok(()) => Ok(true),
            Err(e) => {
                tracing::warn!(
                    "settings: the disk did not confirm the write ({})",
                    e.kind()
                );
                Ok(false)
            }
        }
    }

    /// Deletes the files of the store that `keep` does not name (and the
    /// temp files). Call it only when the installed file is confirmed on
    /// disk. A failure is logged. Returns the deleted names.
    fn sweep(&self, keep: &BTreeSet<String>) -> Vec<String> {
        let Some(dir) = &self.store else {
            return Vec::new();
        };
        let Ok(names) = list(dir) else {
            return Vec::new();
        };
        let mut gone = Vec::new();
        for name in names {
            let path = dir.join(&name);
            let plain = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file());
            if keep.contains(&name) || !plain {
                continue;
            }
            match self.fs.remove(&path) {
                Ok(()) => gone.push(name),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!("settings: cannot remove an unused secret ({})", e.kind()),
            }
        }
        if !gone.is_empty()
            && let Err(e) = self.fs.fsync_dir(dir)
        {
            tracing::warn!("settings: cannot sync the secret store ({})", e.kind());
        }
        gone
    }

    /// U1: deletes the secret files that the installed file does not
    /// reference. It first syncs the file and its directory, and deletes
    /// nothing when that fails. Call it before the holder accepts saves.
    /// Returns the names that it deleted.
    pub fn collect_orphans(&self) -> Vec<String> {
        let mut inner = self.lock();
        let Some(dir) = self.store.clone() else {
            return Vec::new();
        };
        if self.store_dir(&mut inner).is_err() {
            return Vec::new();
        }
        let refs = match read_config(&self.path)
            .ok()
            .and_then(|(b, _)| String::from_utf8(b).ok())
            .and_then(|t| t.parse::<DocumentMut>().ok())
        {
            Some(doc) => referenced_secrets(&doc),
            None => {
                tracing::warn!("settings: the config file cannot be read, so no secret is removed");
                return Vec::new();
            }
        };
        let synced = File::open(&self.path)
            .and_then(|f| self.fs.fsync(&f, &self.path))
            .and_then(|()| {
                self.fs
                    .fsync_dir(self.path.parent().unwrap_or_else(|| Path::new(".")))
            });
        if let Err(e) = synced {
            tracing::warn!(
                "settings: the config file is not confirmed on disk ({}), so no secret is removed",
                e.kind()
            );
            return Vec::new();
        }
        let _ = dir;
        self.sweep(&refs)
    }
}

/// The text of a saved file, with the note at the top when it is missing.
fn with_header(text: &str) -> String {
    if text.contains("Edit with the board or") {
        text.to_owned()
    } else {
        format!("{HEADER}{text}")
    }
}

/// `provider_instance.<id>.env.<VAR>` gives the id and the variable.
fn split_secret_key(key: &str) -> Option<(&str, &str)> {
    let rest = key.strip_prefix("provider_instance.")?;
    let (id, var) = rest.split_once(".env.")?;
    (!id.is_empty() && !id.contains('.') && !var.is_empty() && !var.contains('.'))
        .then_some((id, var))
}
