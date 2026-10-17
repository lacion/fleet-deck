//! The station side of a shim: launch, connect, attach, frames (ADR-005
//! §3). The station layer (`ShimHost`, the `shims` table, recovery) builds
//! on this.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use fleetdeck_core::Secret;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::process::Child;

use super::launch::{LAUNCH_PROTOCOL, LaunchReply, LaunchSpec, SecretsFrame};
use super::proof::AttachKey;
use super::wire::{
    FromShim, PROTOCOL, Refusal, ToShim, WireError, hex, read_frame, unhex, write_frame,
};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("shim i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("shim frame: {0}")]
    Wire(#[from] WireError),
    #[error("shim protocol: {0}")]
    Protocol(String),
    #[error("the shim did not answer in time")]
    Timeout,
    /// The shim did not prove its executable (DL-194): no token was sent.
    #[error("the shim could not be verified: {0}")]
    Untrusted(String),
}

/// A started shim process. The station waits on it so it leaves no
/// zombie; dropping it never stops the shim or the provider.
#[derive(Debug)]
pub struct Launched {
    pub child: Child,
    pub reply: LaunchReply,
}

/// Starts `<exe> shim --id <shim_id>` with a cleared environment and sends
/// the launch frame. The journal (`created`) and the key file must exist.
pub async fn launch(
    exe: &Path,
    spec: &LaunchSpec,
    key: &AttachKey,
    wait: Duration,
) -> Result<Launched, ClientError> {
    launch_with(exe, spec, key, wait, None).await
}

/// [`launch`] with the two-step exchange of DL-194. `secrets` is `Some`
/// (the expected SHA-256 of the shim executable, the session token) iff
/// `spec.secrets` is true. The station writes the token only after a `hello`
/// with the expected hash; in every other case it closes stdin without a
/// second frame, kills the shim and returns [`ClientError::Untrusted`].
pub async fn launch_with(
    exe: &Path,
    spec: &LaunchSpec,
    key: &AttachKey,
    wait: Duration,
    secrets: Option<(&str, &Secret<String>)>,
) -> Result<Launched, ClientError> {
    if secrets.is_some() != spec.secrets {
        return Err(ClientError::Protocol(
            "secrets and spec.secrets differ".to_owned(),
        ));
    }
    let deadline = tokio::time::Instant::now() + wait;
    let mut child = tokio::process::Command::new(exe)
        .arg("shim")
        .arg("--id")
        .arg(&spec.shim_id)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(false)
        .spawn()?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(ClientError::Protocol("the shim has no pipes".to_owned()));
    };
    write_frame(&mut stdin, spec, key.expose()).await?;
    let mut stdout = BufReader::new(stdout);
    if let Some((expected, token)) = secrets {
        let mut hello = String::new();
        let first = tokio::time::timeout_at(deadline, stdout.read_line(&mut hello)).await;
        let reason = match first {
            Err(_) => Some("timeout"),
            Ok(Err(_)) => Some("no hello"),
            Ok(Ok(_)) => match serde_json::from_str::<LaunchReply>(hello.trim_end()) {
                Ok(LaunchReply::Hello {
                    exe_sha256,
                    protocol,
                }) => {
                    if protocol != LAUNCH_PROTOCOL {
                        Some("wrong protocol")
                    } else if exe_sha256 != expected {
                        Some("wrong executable")
                    } else {
                        None
                    }
                }
                _ => Some("no hello"),
            },
        };
        if let Some(reason) = reason {
            // No second frame: the token bytes are never written (DL-194).
            drop(stdin);
            reap_untrusted(&mut child).await;
            return Err(ClientError::Untrusted(reason.to_owned()));
        }
        let frame = SecretsFrame {
            token: token.expose().clone(),
        };
        write_frame(&mut stdin, &frame, &[]).await?;
    }
    stdin.shutdown().await?;
    drop(stdin);
    let mut line = String::new();
    match tokio::time::timeout_at(deadline, stdout.read_line(&mut line)).await {
        Err(_) => return Err(ClientError::Timeout),
        Ok(Err(e)) => return Err(e.into()),
        Ok(Ok(_)) => {}
    }
    let reply = serde_json::from_str(line.trim_end())
        .map_err(|e| ClientError::Protocol(format!("launch reply {line:?}: {e}")))?;
    Ok(Launched { child, reply })
}

/// Kills an unverified shim and waits for it, at most 2 seconds (DL-194).
async fn reap_untrusted(child: &mut Child) {
    if let Err(e) = child.start_kill() {
        tracing::warn!(error = %e, "cannot kill the unverified shim");
    }
    match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "cannot wait for the unverified shim"),
        Err(_) => tracing::warn!("the unverified shim did not end in time"),
    }
}

/// The `hello` of a shim.
#[derive(Debug, Clone)]
pub struct Hello {
    pub protocols: Vec<u32>,
    pub shim_build: String,
    pub shim_id: String,
    pub nonce: Vec<u8>,
}

/// A connection after `hello`, before `attach`.
#[derive(Debug)]
pub struct Connection {
    r: OwnedReadHalf,
    w: OwnedWriteHalf,
    pub hello: Hello,
}

/// The result of an attach.
#[derive(Debug)]
pub enum AttachResult {
    Attached {
        attachment: Attachment,
        frame: FromShim,
    },
    Refused(Refusal),
}

impl Connection {
    /// Connects and reads `hello`.
    pub async fn open(socket: &Path, wait: Duration) -> Result<Self, ClientError> {
        let stream = tokio::time::timeout(wait, UnixStream::connect(socket))
            .await
            .map_err(|_| ClientError::Timeout)??;
        let (mut r, w) = stream.into_split();
        let frame = tokio::time::timeout(wait, read_frame::<_, FromShim>(&mut r))
            .await
            .map_err(|_| ClientError::Timeout)??;
        let Some((
            FromShim::Hello {
                protocols,
                shim_build,
                shim_id,
                nonce,
            },
            _,
        )) = frame
        else {
            return Err(ClientError::Protocol("no hello".to_owned()));
        };
        let nonce = unhex(&nonce).ok_or_else(|| ClientError::Protocol("bad nonce".to_owned()))?;
        Ok(Self {
            r,
            w,
            hello: Hello {
                protocols,
                shim_build,
                shim_id,
                nonce,
            },
        })
    }

    /// Sends `attach` with the proof of `key` and reads the answer.
    pub async fn attach(
        self,
        key: &AttachKey,
        incarnation: &str,
        generation: u64,
        wait: Duration,
    ) -> Result<AttachResult, ClientError> {
        let proof = hex(&key.proof(&self.hello.nonce, incarnation, generation));
        self.attach_with_proof(incarnation, generation, proof, wait)
            .await
    }

    /// `attach` with a given proof (tests of a bad proof).
    pub async fn attach_with_proof(
        self,
        incarnation: &str,
        generation: u64,
        proof: String,
        wait: Duration,
    ) -> Result<AttachResult, ClientError> {
        // The highest version that both sides speak (ADR-005 §5.3); a shim
        // with no common version refuses the attach.
        let protocol = super::wire::pick_protocol(&self.hello.protocols).unwrap_or(PROTOCOL);
        self.attach_picking(incarnation, generation, proof, protocol, wait)
            .await
    }

    /// `attach` as a station of another build: it names `protocol`, which
    /// the shim keeps for this connection (tests of the N−1 encoding,
    /// ADR-005 §5.3).
    pub async fn attach_with_protocol(
        self,
        key: &AttachKey,
        incarnation: &str,
        generation: u64,
        protocol: u32,
        wait: Duration,
    ) -> Result<AttachResult, ClientError> {
        let proof = hex(&key.proof(&self.hello.nonce, incarnation, generation));
        self.attach_picking(incarnation, generation, proof, protocol, wait)
            .await
    }

    async fn attach_picking(
        mut self,
        incarnation: &str,
        generation: u64,
        proof: String,
        protocol: u32,
        wait: Duration,
    ) -> Result<AttachResult, ClientError> {
        let frame = ToShim::Attach {
            protocol,
            incarnation: incarnation.to_owned(),
            generation,
            proof,
        };
        write_frame(&mut self.w, &frame, &[]).await?;
        let answer = tokio::time::timeout(wait, read_frame::<_, FromShim>(&mut self.r))
            .await
            .map_err(|_| ClientError::Timeout)??;
        match answer {
            Some((FromShim::Refused { reason }, _)) => Ok(AttachResult::Refused(reason)),
            Some((frame @ FromShim::Attached { .. }, _)) => Ok(AttachResult::Attached {
                attachment: Attachment {
                    r: self.r,
                    w: self.w,
                    generation,
                },
                frame,
            }),
            Some((other, _)) => Err(ClientError::Protocol(format!("unexpected {other:?}"))),
            None => Err(ClientError::Protocol(
                "the shim closed the connection".to_owned(),
            )),
        }
    }
}

/// An attached connection of one generation.
#[derive(Debug)]
pub struct Attachment {
    r: OwnedReadHalf,
    w: OwnedWriteHalf,
    pub generation: u64,
}

impl Attachment {
    /// Sends one frame.
    pub async fn send(&mut self, frame: &ToShim, payload: &[u8]) -> Result<(), ClientError> {
        write_frame(&mut self.w, frame, payload).await?;
        Ok(())
    }

    /// The next frame; `None` when the shim closed the connection.
    pub async fn recv(
        &mut self,
        wait: Duration,
    ) -> Result<Option<(FromShim, Vec<u8>)>, ClientError> {
        tokio::time::timeout(wait, read_frame::<_, FromShim>(&mut self.r))
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(Into::into)
    }
}

impl Attachment {
    /// A crash-boundary fault tool (ADR-005 test 27): writes only the first
    /// `cut` bytes of the encoded frame and closes the connection, as a
    /// station that dies in the middle of a frame.
    ///
    /// # Errors
    /// An encoding or socket error.
    pub async fn send_torn(
        mut self,
        frame: &ToShim,
        payload: &[u8],
        cut: usize,
    ) -> Result<(), ClientError> {
        let bytes = super::wire::encode(frame, payload)?;
        let cut = cut.min(bytes.len().saturating_sub(1));
        self.w
            .write_all(&bytes[..cut])
            .await
            .map_err(ClientError::Io)?;
        self.w.flush().await.map_err(ClientError::Io)?;
        drop(self);
        Ok(())
    }

    /// Splits the attachment into its two directions, so that one task can
    /// read frames while another sends them.
    #[must_use]
    pub fn into_split(self) -> (FrameReader, FrameWriter) {
        (FrameReader { r: self.r }, FrameWriter { w: self.w })
    }
}

/// The read direction of an [`Attachment`].
#[derive(Debug)]
pub struct FrameReader {
    r: OwnedReadHalf,
}

impl FrameReader {
    /// The next frame, without a time limit; `None` when the shim closed
    /// the connection.
    pub async fn next(&mut self) -> Result<Option<(FromShim, Vec<u8>)>, ClientError> {
        Ok(read_frame::<_, FromShim>(&mut self.r).await?)
    }
}

/// The write direction of an [`Attachment`].
#[derive(Debug)]
pub struct FrameWriter {
    w: OwnedWriteHalf,
}

impl FrameWriter {
    /// Sends one frame.
    pub async fn send(&mut self, frame: &ToShim, payload: &[u8]) -> Result<(), ClientError> {
        write_frame(&mut self.w, frame, payload).await?;
        Ok(())
    }
}

/// The socket path of a spec, for callers that only hold the spec.
#[must_use]
pub fn socket_of(spec: &LaunchSpec) -> PathBuf {
    spec.socket_path.clone()
}
