//! The second step of a launch with `secrets` (DL-194): the shim proves its
//! executable, then reads the session token from the same stdin.

use fleetdeck_core::Secret;
use tokio::io::AsyncRead;

use super::{LaunchReply, failed, reply};
use crate::host::shim::exe_hash::{own_exe, sha256_file};
use crate::host::shim::launch::{LAUNCH_PROTOCOL, MAX_SECRETS_FRAME, SecretsFrame};
use crate::host::shim::wire::read_frame;

/// Writes `hello` with the hash of the shim's own executable and reads the
/// [`SecretsFrame`]. `Err(code)` is the exit code: after a failed hash the
/// shim has written `failed`; when the station closed stdin or sent a bad
/// frame (it refused this shim) it writes nothing and exits with 1.
pub(super) async fn exchange<R: AsyncRead + Unpin>(stdin: &mut R) -> Result<Secret<String>, i32> {
    let exe_sha256 = match own_exe().and_then(|exe| sha256_file(&exe)) {
        Ok(hash) => hash,
        Err(e) => {
            tracing::debug!(error = %e, "the shim cannot hash its executable");
            return Err(failed(false, "the shim cannot hash its executable"));
        }
    };
    reply(&LaunchReply::Hello {
        exe_sha256,
        protocol: LAUNCH_PROTOCOL,
    });
    match read_frame::<_, SecretsFrame>(stdin).await {
        Ok(Some((frame, payload)))
            if payload.is_empty()
                && serde_json::to_vec(&frame).is_ok_and(|h| h.len() <= MAX_SECRETS_FRAME) =>
        {
            Ok(Secret::new(frame.token))
        }
        Ok(Some(_)) => {
            tracing::debug!("the secrets frame is too large or has a payload");
            Err(1)
        }
        Ok(None) => {
            tracing::debug!("the station closed stdin before the secrets frame");
            Err(1)
        }
        Err(_) => {
            tracing::debug!("the secrets frame is not valid");
            Err(1)
        }
    }
}
