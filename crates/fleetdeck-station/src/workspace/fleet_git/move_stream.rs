//! The export stream of a move (FG-9, design §12.3 step 2, ADR-009 §9): the
//! station writes the manifest files back to back, in manifest order, and
//! closes the stream. It never writes more than `manifest.total` bytes.
//!
//! The deck opens the stream with the token of `session.move_export`
//! (purpose `move-export:<move_id>`). The files are opened again when the
//! stream opens; the target checks each size and SHA-256, so a file that
//! changed in between is refused there.

use std::os::unix::fs::FileExt;

use fleetdeck_link::data::Data;

use super::move_export::Exported;
use crate::manager::SessionManager;

/// The size of one write on the stream.
const CHUNK: usize = 64 * 1024;

/// The reset reason of a stream for a move that does not hold the session.
pub const MOVE_ABORTED: &str = "move_aborted";
/// The reset reason of a read error or a file that became shorter.
pub const MOVE_IO: &str = "move_io";
/// The reset reason of a move without files (a branch-only move).
pub const NOT_NATIVE: &str = "not_native";
/// The reset reason of a stream or a channel that ended.
const CLOSED: &str = "closed";

/// The bytes of the files of a move, one chunk at a time, in manifest order:
/// the files back to back, never more than `manifest.total` bytes.
pub struct Chunks<'a> {
    exported: &'a Exported,
    file: usize,
    at: u64,
    sent: u64,
}

impl<'a> Chunks<'a> {
    #[must_use]
    pub fn new(exported: &'a Exported) -> Self {
        Self {
            exported,
            file: 0,
            at: 0,
            sent: 0,
        }
    }

    /// Reads the next chunk into `buf`; `Ok(None)` after the last byte.
    ///
    /// # Errors
    /// A read error, a file that became shorter, or more bytes than the
    /// manifest total.
    pub fn next_chunk(&mut self, buf: &mut [u8]) -> std::io::Result<Option<usize>> {
        loop {
            let Some(open) = self.exported.files.get(self.file) else {
                return Ok(None);
            };
            let size = open.entry.size;
            if self.at >= size {
                self.file += 1;
                self.at = 0;
                continue;
            }
            let want = usize::try_from((size - self.at).min(buf.len() as u64)).unwrap_or(buf.len());
            let n = open.file.read_at(&mut buf[..want], self.at)?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            self.at += n as u64;
            self.sent += n as u64;
            if self.sent > self.exported.manifest.total {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            return Ok(Some(n));
        }
    }
}

/// Writes `exported` on `stream` and closes it.
///
/// # Errors
/// The reset reason: [`MOVE_IO`] for a read error or a short file, or
/// `closed` when the stream or the channel ended.
pub async fn write_export(handle: &Data, stream: u32, exported: &Exported) -> Result<(), String> {
    let mut buf = vec![0_u8; CHUNK];
    let mut chunks = Chunks::new(exported);
    while let Some(n) = chunks
        .next_chunk(&mut buf)
        .map_err(|_| MOVE_IO.to_owned())?
    {
        handle
            .write(stream, buf[..n].to_vec())
            .await
            .map_err(|_| CLOSED.to_owned())?;
    }
    handle.close(stream).await.map_err(|_| CLOSED.to_owned())
}

/// Serves one `move-export:` stream: opens the files of the move and writes
/// them. A failure resets the stream with its reason.
pub async fn serve_export(manager: &SessionManager, handle: Data, stream: u32, move_id: &str) {
    let reason = match manager.move_export_open(move_id).await {
        Ok(Some(exported)) => match write_export(&handle, stream, &exported).await {
            Ok(()) => return,
            Err(reason) => reason,
        },
        Ok(None) => NOT_NATIVE.to_owned(),
        Err(e) if e.code == "invalid_state" => MOVE_ABORTED.to_owned(),
        Err(_) => MOVE_IO.to_owned(),
    };
    let _ = handle.reset(stream, &reason).await;
}

#[cfg(test)]
mod tests {
    use fleetdeck_proto::event::ProviderKind;

    use super::Chunks;
    use crate::workspace::fleet_git::move_layout::tests::{NATIVE, Rig, claude_line};

    #[test]
    fn the_stream_carries_the_files_back_to_back_in_manifest_order_and_no_more() {
        let rig = Rig::new();
        let body = claude_line(NATIVE, 1).repeat(5000);
        rig.write(&rig.claude_main(), body.as_bytes());
        let sub = vec![b'x'; 100_000];
        rig.write(&rig.claude_sub("agent-a1.jsonl"), &sub);
        let exported = rig.export(ProviderKind::Claude).unwrap();
        assert_eq!(exported.manifest.files.len(), 2);
        let mut got = Vec::new();
        let mut buf = vec![0_u8; 4096];
        let mut chunks = Chunks::new(&exported);
        while let Some(n) = chunks.next_chunk(&mut buf).unwrap() {
            assert!(n <= buf.len());
            got.extend_from_slice(&buf[..n]);
        }
        let mut want = body.into_bytes();
        want.extend_from_slice(&sub);
        assert_eq!(got.len() as u64, exported.manifest.total);
        assert_eq!(got, want);
        // After the last byte there is nothing more.
        assert_eq!(chunks.next_chunk(&mut buf).unwrap(), None);
    }

    #[test]
    fn a_file_that_became_shorter_ends_the_stream_with_an_error() {
        let rig = Rig::new();
        rig.write(
            &rig.claude_main(),
            claude_line(NATIVE, 1).repeat(100).as_bytes(),
        );
        let exported = rig.export(ProviderKind::Claude).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(rig.claude_main())
            .unwrap()
            .set_len(10)
            .unwrap();
        let mut buf = vec![0_u8; 4096];
        let mut chunks = Chunks::new(&exported);
        let mut result = Ok(Some(0));
        while let Ok(Some(_)) = result {
            result = chunks.next_chunk(&mut buf);
        }
        assert!(result.is_err());
    }
}
