//! Why an attach to a shim failed, in the classes that station recovery
//! needs (ADR-005 §3.2 steps 2–5, slice 4, DL-104).

use std::io;

use super::client::ClientError;
use super::wire::Refusal;

/// The failure of [`super::host::ShimHost::attach`].
#[derive(Debug, thiserror::Error)]
pub enum AttachFailure {
    /// The key file is missing or unreadable: this station cannot prove
    /// that it owns the shim.
    #[error("the attach key file is missing")]
    NoKey,
    /// Nothing accepts connections on the socket (a stale or missing
    /// socket file, §3.2 step 3).
    #[error("{0}")]
    Unreachable(ClientError),
    /// The socket accepted, but the shim did not complete the handshake
    /// in time or sent bad frames.
    #[error("{0}")]
    Protocol(ClientError),
    /// The socket belongs to another shim.
    #[error("the socket belongs to another shim")]
    WrongShim,
    /// The shim speaks no protocol version of this build (§5.3).
    #[error("the attach was refused: Incompatible")]
    Incompatible,
    /// The shim refused the attach (§3 step 4).
    #[error("the attach was refused: {0:?}")]
    Refused(Refusal),
    /// The shim sent something other than `attached`.
    #[error("the shim sent no attached frame")]
    NoAttachedFrame,
    /// The shim reports another provider process (§3.1).
    #[error("identity_mismatch")]
    IdentityMismatch,
    /// The replay request could not be sent.
    #[error("{0}")]
    Send(ClientError),
}

impl AttachFailure {
    /// Classifies a failed connect.
    #[must_use]
    pub fn of_connect(e: ClientError) -> Self {
        let unreachable = matches!(
            &e,
            ClientError::Io(io) if matches!(
                io.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            )
        );
        if unreachable {
            Self::Unreachable(e)
        } else {
            Self::Protocol(e)
        }
    }

    /// True when no shim listens on the socket: recovery then reads the
    /// exit file (§3.2 step 3).
    #[must_use]
    pub fn is_unreachable(&self) -> bool {
        matches!(self, Self::Unreachable(_))
    }
}
