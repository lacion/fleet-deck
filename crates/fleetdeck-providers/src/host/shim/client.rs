//! The station side of a shim: launch, connect, attach, frames (ADR-005
//! §3). The station layer (`ShimHost`, the `shims` table, recovery) builds
//! on this.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::process::Child;

use super::launch::{LaunchReply, LaunchSpec};
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
    stdin.shutdown().await?;
    drop(stdin);
    let mut line = String::new();
    let mut stdout = BufReader::new(stdout);
    match tokio::time::timeout(wait, stdout.read_line(&mut line)).await {
        Err(_) => return Err(ClientError::Timeout),
        Ok(Err(e)) => return Err(e.into()),
        Ok(Ok(_)) => {}
    }
    let reply = serde_json::from_str(line.trim_end())
        .map_err(|e| ClientError::Protocol(format!("launch reply {line:?}: {e}")))?;
    Ok(Launched { child, reply })
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
        mut self,
        incarnation: &str,
        generation: u64,
        proof: String,
        wait: Duration,
    ) -> Result<AttachResult, ClientError> {
        let frame = ToShim::Attach {
            protocol: PROTOCOL,
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
