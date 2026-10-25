//! The shim socket protocol (ADR-005 §3, §4, §5.3).
//!
//! One frame is `u32 length (big endian) ‖ u32 header length ‖ header ‖
//! payload`. The header is one JSON object with a `type` tag; the payload is
//! raw bytes (provider stdout for [`FromShim::Output`], stdin bytes for
//! [`ToShim::Write`], empty otherwise). A frame is processed only when it is
//! complete: [`read_frame`] returns [`WireError::Truncated`] for a frame that
//! the peer did not finish, and the caller discards it (ADR-005 §4.3).
//!
//! Every station frame after `attach` carries the attachment generation
//! (ADR-005 §3 "Stale writers"); [`ToShim::generation`] gives it.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The protocol version of this build (N). Version 2 reports an
/// unavailable descendant count as `null` and tags the exit report with
/// its inspection version (DL-182).
pub const PROTOCOL: u32 = 2;
/// The versions this build speaks: N and N−1 (ADR-005 §5.3).
pub const PROTOCOLS: &[u32] = &[PROTOCOL, 1];
/// The largest payload of one frame (ADR-005 §4.3: 1 MiB).
pub const MAX_PAYLOAD: usize = 1024 * 1024;
/// The largest header of one frame.
pub const MAX_HEADER: usize = 64 * 1024;

/// The highest version that both sides speak.
#[must_use]
pub fn pick_protocol(offered: &[u32]) -> Option<u32> {
    pick_common(PROTOCOLS, offered)
}

/// The highest protocol in both `ours` and `offered`: a station and a shim
/// of builds N and N−1 agree on the highest version they share (ADR-005
/// §5.3, DL-128).
#[must_use]
pub fn pick_common(ours: &[u32], offered: &[u32]) -> Option<u32> {
    ours.iter().copied().filter(|v| offered.contains(v)).max()
}

/// The identity of a provider process (ADR-005 §3.1). A PID alone is never
/// identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderIdentity {
    pub pid: u32,
    /// Linux: `/proc/<pid>/stat` field 22 (clock ticks after boot).
    pub start_time: u64,
    /// Linux: `/proc/sys/kernel/random/boot_id`.
    pub boot_id: String,
}

/// Why a shim refused an attach (ADR-005 §3 step 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    BadProof,
    ForeignStation,
    StaleOwner,
    /// No common protocol version (ADR-005 §5.3).
    Incompatible,
}

/// How the provider ended, as the shim reports it (ADR-005 §4.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitState {
    /// `code:<n>`, `signal:<n>` or `unknown`.
    pub termination: String,
    /// `exited`, `stopped`, `stop_forced` or `killed`.
    pub cause: String,
    pub stderr_tail: String,
    /// The members of the provider's process group or session that were
    /// still alive after the exit, before the shim signalled them. `None`:
    /// the inspection was incomplete (an unreadable `/proc`, or a platform
    /// without a reader), so the count proves nothing (DL-182 §1a).
    pub descendants_remaining: Option<u32>,
    /// The stdout offset after the last byte.
    pub final_offset: u64,
    /// The inspection version of the report. 1: the count was taken by the
    /// complete inspection of DL-182. Absent (0): an older shim; its count
    /// is not a proof of an ended execution.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub inspection: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// The inspection version that the shim of this build writes.
pub const INSPECTION: u32 = 1;

impl ExitState {
    /// The report as a peer that negotiated `protocol` reads it. The older
    /// protocol has no `null` count and no inspection field: an unavailable
    /// count becomes `u32::MAX`, which an older station reads as
    /// descendants remaining (ADR-005 §5.3, DL-182 §1a).
    #[must_use]
    pub fn for_protocol(&self, protocol: u32) -> Self {
        if protocol >= 2 {
            return self.clone();
        }
        Self {
            descendants_remaining: Some(self.descendants_remaining.unwrap_or(u32::MAX)),
            inspection: 0,
            ..self.clone()
        }
    }
}
/// Why the shim reports a range of stdout as lost (ADR-005 §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapReason {
    BelowFloor,
    OutputFrameTooLarge,
}

/// The state of one stdin write record (ADR-005 §4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WriteState {
    Received,
    Writing { bytes_done: u64 },
    Written,
    Partial { bytes_done: u64, error: String },
    FailedUnwritten { error: String },
    ClosedUnwritten,
}

impl WriteState {
    /// True for a state that never changes again.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Received | Self::Writing { .. })
    }
}

/// The answer to a `write_status` frame (ADR-005 §4.3 table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum StatusAnswer {
    /// The shim has no record, the sequence is above the eviction watermark
    /// and no record holds it. The shim stored a tombstone in the same step.
    NotReceived,
    /// The shim knows the write; this is its state.
    Known { state: WriteState },
    /// The record was evicted or the sequence is at or below the watermark.
    Forgotten,
    /// The identity and the sequence do not belong together.
    Mismatch,
}

/// Frames from the station to the shim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToShim {
    /// Step 3 of the handshake. `proof` is lower-case hex.
    Attach {
        protocol: u32,
        incarnation: String,
        generation: u64,
        proof: String,
    },
    /// The station committed everything below `cursor` (ADR-005 §4.1).
    Ack {
        generation: u64,
        cursor: u64,
    },
    /// Send stdout again from `from`.
    Replay {
        generation: u64,
        from: u64,
    },
    /// Write the payload to the provider's stdin.
    Write {
        generation: u64,
        write_id: String,
        write_seq: u64,
        /// The station's wall-clock time in ms (diagnostic only).
        sent_at: u64,
    },
    WriteStatus {
        generation: u64,
        write_id: String,
        write_seq: u64,
    },
    Stop {
        generation: u64,
        grace_ms: u64,
        term_grace_ms: u64,
    },
    Kill {
        generation: u64,
    },
    CloseStdin {
        generation: u64,
    },
    Interrupt {
        generation: u64,
    },
    Status {
        generation: u64,
    },
    /// The station committed the exit state and the final cursor.
    AckExit {
        generation: u64,
    },
}

impl ToShim {
    /// The generation of the frame.
    #[must_use]
    pub fn generation(&self) -> u64 {
        match self {
            Self::Attach { generation, .. }
            | Self::Ack { generation, .. }
            | Self::Replay { generation, .. }
            | Self::Write { generation, .. }
            | Self::WriteStatus { generation, .. }
            | Self::Stop { generation, .. }
            | Self::Kill { generation }
            | Self::CloseStdin { generation }
            | Self::Interrupt { generation }
            | Self::Status { generation }
            | Self::AckExit { generation } => *generation,
        }
    }

    /// True for a control frame (ADR-005 §4.4: at most 64 queue).
    #[must_use]
    pub fn is_control(&self) -> bool {
        matches!(
            self,
            Self::Stop { .. }
                | Self::Kill { .. }
                | Self::CloseStdin { .. }
                | Self::Interrupt { .. }
                | Self::Status { .. }
        )
    }
}

/// Frames from the shim to the station.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FromShim {
    /// Step 2 of the handshake. `nonce` is lower-case hex.
    Hello {
        protocols: Vec<u32>,
        shim_build: String,
        shim_id: String,
        nonce: String,
    },
    Refused {
        reason: Refusal,
    },
    Attached {
        generation: u64,
        provider_identity: Option<ProviderIdentity>,
        /// The oldest stdout byte that the shim still holds.
        cursor_floor: u64,
        exit: Option<ExitState>,
    },
    /// The payload holds stdout bytes from `offset` on.
    Output {
        offset: u64,
        read_at: u64,
    },
    Gap {
        from: u64,
        to: u64,
        reason: GapReason,
    },
    WriteDone {
        write_id: String,
        write_seq: u64,
        state: WriteState,
    },
    WriteStatus {
        write_id: String,
        write_seq: u64,
        answer: StatusAnswer,
    },
    Exited {
        exit: ExitState,
    },
    Status {
        provider_identity: Option<ProviderIdentity>,
        acked: u64,
        end: u64,
        pending_writes: u32,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("the frame ended before its length was reached")]
    Truncated,
    #[error("the frame is too large ({0} bytes)")]
    TooLarge(usize),
    #[error("the frame is malformed: {0}")]
    Malformed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// One frame.
pub fn encode<H: Serialize>(header: &H, payload: &[u8]) -> Result<Vec<u8>, WireError> {
    let head = serde_json::to_vec(header).map_err(|e| WireError::Malformed(e.to_string()))?;
    if head.len() > MAX_HEADER {
        return Err(WireError::TooLarge(head.len()));
    }
    if payload.len() > MAX_PAYLOAD {
        return Err(WireError::TooLarge(payload.len()));
    }
    let body = 4 + head.len() + payload.len();
    let mut out = Vec::with_capacity(4 + body);
    out.extend_from_slice(&len32(body)?.to_be_bytes());
    out.extend_from_slice(&len32(head.len())?.to_be_bytes());
    out.extend_from_slice(&head);
    out.extend_from_slice(payload);
    Ok(out)
}

fn len32(n: usize) -> Result<u32, WireError> {
    u32::try_from(n).map_err(|_| WireError::TooLarge(n))
}

/// Writes one frame.
pub async fn write_frame<W, H>(w: &mut W, header: &H, payload: &[u8]) -> Result<(), WireError>
where
    W: AsyncWrite + Unpin,
    H: Serialize,
{
    let frame = encode(header, payload)?;
    w.write_all(&frame).await?;
    w.flush().await?;
    Ok(())
}

/// Reads one frame. `Ok(None)` is a clean end at a frame boundary; an end
/// inside a frame is [`WireError::Truncated`], and the caller discards it.
/// The sizes are checked before any allocation.
pub async fn read_frame<R, H>(r: &mut R) -> Result<Option<(H, Vec<u8>)>, WireError>
where
    R: AsyncRead + Unpin,
    H: DeserializeOwned,
{
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        let n = r.read(&mut len[got..]).await?;
        if n == 0 {
            return if got == 0 {
                Ok(None)
            } else {
                Err(WireError::Truncated)
            };
        }
        got += n;
    }
    let body = u32::from_be_bytes(len) as usize;
    if !(4..=4 + MAX_HEADER + MAX_PAYLOAD).contains(&body) {
        return Err(WireError::TooLarge(body));
    }
    let mut buf = vec![0u8; body];
    r.read_exact(&mut buf).await.map_err(|e| match e.kind() {
        std::io::ErrorKind::UnexpectedEof => WireError::Truncated,
        _ => WireError::Io(e),
    })?;
    let (head_len, rest) = buf.split_at(4);
    let head_len =
        u32::from_be_bytes([head_len[0], head_len[1], head_len[2], head_len[3]]) as usize;
    if head_len > MAX_HEADER || head_len > rest.len() {
        return Err(WireError::Malformed("bad header length".to_owned()));
    }
    let (head, payload) = rest.split_at(head_len);
    if payload.len() > MAX_PAYLOAD {
        return Err(WireError::TooLarge(payload.len()));
    }
    let header = serde_json::from_slice(head).map_err(|e| WireError::Malformed(e.to_string()))?;
    Ok(Some((header, payload.to_vec())))
}

/// Lower-case hex.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The bytes of lower-case or upper-case hex, or `None`.
#[must_use]
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod exit_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn report(count: Option<u32>) -> ExitState {
        ExitState {
            termination: "code:0".to_owned(),
            cause: "exited".to_owned(),
            stderr_tail: String::new(),
            descendants_remaining: count,
            final_offset: 7,
            inspection: INSPECTION,
        }
    }

    #[test]
    fn the_protocol_is_two_and_one_stays_spoken() {
        assert_eq!(PROTOCOL, 2);
        assert_eq!(PROTOCOLS, &[2, 1]);
        assert_eq!(pick_protocol(&[1]), Some(1));
        assert_eq!(pick_protocol(&[1, 2]), Some(2));
        assert_eq!(pick_protocol(&[3]), None);
    }

    #[test]
    fn an_unavailable_count_is_null_on_the_wire_of_version_two() {
        let text = serde_json::to_string(&report(None)).unwrap();
        assert!(text.contains(r#""descendants_remaining":null"#), "{text}");
        assert!(text.contains(r#""inspection":1"#), "{text}");
        let back: ExitState = serde_json::from_str(&text).unwrap();
        assert_eq!(back, report(None));
    }

    #[test]
    fn version_one_says_unknown_as_the_largest_count_and_has_no_inspection_field() {
        let old = report(None).for_protocol(1);
        assert_eq!(old.descendants_remaining, Some(u32::MAX));
        assert_eq!(old.inspection, 0);
        let text = serde_json::to_string(&old).unwrap();
        assert!(!text.contains("inspection"), "{text}");
        // A known count passes as it is.
        assert_eq!(
            report(Some(3)).for_protocol(1).descendants_remaining,
            Some(3)
        );
        // Version two keeps the report.
        assert_eq!(report(None).for_protocol(2), report(None));
    }

    #[test]
    fn a_report_of_an_older_shim_or_an_old_exit_file_reads_as_unverified() {
        let old = r#"{"termination":"code:0","cause":"exited","stderr_tail":"","descendants_remaining":0,"final_offset":5}"#;
        let parsed: ExitState = serde_json::from_str(old).unwrap();
        assert_eq!(parsed.descendants_remaining, Some(0));
        assert_eq!(parsed.inspection, 0, "no inspection: not a proof");
    }
}
