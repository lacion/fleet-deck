//! The launch of a shim (ADR-005 §2, §3 "Launch").
//!
//! The station starts `<exe> shim --id <shim_id>` with a cleared
//! environment, writes one frame to the shim's stdin (header: the
//! [`LaunchSpec`]; payload: the 32 attach-key bytes) and closes the pipe,
//! so the key never appears in argv, an environment, a file other than the
//! 0600 key file, or a log. The shim answers one JSON line on its stdout
//! ([`LaunchReply`]) and never writes there again.
//!
//! With [`LaunchSpec::secrets`] (DL-194) the exchange has a second step:
//! the station writes the spec frame and keeps stdin open; the shim
//! answers one `hello` line with the SHA-256 of its own executable; the
//! station writes the [`SecretsFrame`] frame (header only) and closes
//! stdin; the shim answers `ready` (or `failed`) as above. The station
//! sends the token only when the hash is the one of the file it launched.
//! Without `secrets` the one-frame exchange is unchanged.
//!
//! The shim starts the provider through `<exe> shim-exec --parent <pid> --
//! <program> <args>`: the helper makes the provider a session and group
//! leader, asks the kernel for `SIGKILL` when the shim dies (Linux), checks
//! that its parent is still the shim, then executes the provider. This
//! keeps `unsafe` out of the workspace (no `pre_exec`).

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::wire::ProviderIdentity;

/// The launch protocol version of the two-step exchange (DL-194). It is
/// separate from the attach [`super::wire::PROTOCOL`].
pub const LAUNCH_PROTOCOL: u32 = 2;
/// The largest header of the second launch frame (DL-194); its payload is
/// empty.
pub const MAX_SECRETS_FRAME: usize = 1024;

/// The default time before an unacknowledged exit is published (7 days).
pub const EXIT_PUBLISH_AFTER_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// The default retry interval of a failed exit publication (one hour).
pub const EXIT_RETRY_MS: u64 = 60 * 60 * 1000;

/// What the shim needs to start. It holds no secret: the key travels as
/// the frame payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchSpec {
    pub shim_id: String,
    /// The station database incarnation that owns the shim.
    pub incarnation: String,
    /// `<station dir>/shims`: journal, lock, key and exit file.
    pub shims_dir: PathBuf,
    pub socket_path: PathBuf,
    pub buffer_bytes: usize,
    pub exit_publish_after_ms: u64,
    pub exit_retry_ms: u64,
    /// The provider program, its arguments and directory (bytes, so a
    /// non-UTF-8 path survives).
    pub program: Vec<u8>,
    pub args: Vec<Vec<u8>>,
    pub cwd: Vec<u8>,
    /// The complete provider environment, already scrubbed by the station.
    pub env: Vec<(Vec<u8>, Vec<u8>)>,
    /// true: the shim proves its executable before it gets the session
    /// secrets (DL-194).
    #[serde(default)]
    pub secrets: bool,
}

impl LaunchSpec {
    #[must_use]
    pub fn program(&self) -> OsString {
        OsString::from_vec(self.program.clone())
    }

    #[must_use]
    pub fn args(&self) -> Vec<OsString> {
        self.args.iter().cloned().map(OsString::from_vec).collect()
    }

    #[must_use]
    pub fn cwd(&self) -> PathBuf {
        PathBuf::from(OsString::from_vec(self.cwd.clone()))
    }

    #[must_use]
    pub fn env(&self) -> Vec<(OsString, OsString)> {
        self.env
            .iter()
            .map(|(k, v)| (OsString::from_vec(k.clone()), OsString::from_vec(v.clone())))
            .collect()
    }

    /// Fills the provider fields from a host [`super::super::ProcessSpec`].
    #[must_use]
    pub fn provider_from(mut self, spec: &super::super::ProcessSpec) -> Self {
        self.program = spec.program.as_os_str().as_bytes().to_vec();
        self.args = spec
            .args
            .iter()
            .map(|a| a.as_os_str().as_bytes().to_vec())
            .collect();
        self.cwd = spec.cwd.as_os_str().as_bytes().to_vec();
        self.env = spec
            .env
            .iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
            .collect();
        self
    }
}

/// The one line that the shim writes on its stdout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case", deny_unknown_fields)]
pub enum LaunchReply {
    /// With `secrets` (DL-194): the SHA-256 of the shim's executable, sent
    /// before anything is spawned.
    Hello { exe_sha256: String, protocol: u32 },
    /// The socket is bound and the provider runs.
    Ready { provider_identity: ProviderIdentity },
    /// The journal holds `fenced`: no socket, no provider.
    Fenced,
    /// Another shim holds the launch lock.
    Locked,
    /// The launch failed before or at the provider start.
    Failed {
        /// True when `spawn_attempted` was written: a provider may exist.
        spawn_attempted: bool,
        error: String,
    },
}

/// The second launch frame (DL-194): the session token for the provider's
/// environment. It is never logged, journaled or stored.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsFrame {
    pub token: String,
}

impl std::fmt::Debug for SecretsFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretsFrame { token: \"<redacted>\" }")
    }
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
