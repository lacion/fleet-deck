//! The structural check of the screenshot (design §5.3 items 1 and 2): the
//! signature, the chunk framing, every chunk CRC, the first IHDR, an IDAT
//! and the last IEND, then the size limits. There is no decoder: the pixels
//! are never read.

use std::fs::File;
use std::os::unix::fs::FileExt;

use fleetdeck_proto::capture::{code, text};

use super::budget::check_dimensions;
use super::{COPY_BUFFER, MAX_PNG_BYTES};
use crate::desktop::Fail;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
/// The length, the type and the CRC of one chunk, without its data.
const FRAME: u64 = 12;

/// The size that the IHDR of a checked PNG reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PngInfo {
    pub width: u32,
    pub height: u32,
}

fn failed() -> Fail {
    Fail::new(code::CAPTURE_FAILED, text::CAPTURE_FAILED)
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0_u32; 256];
    let mut n = 0;
    while n < 256 {
        #[allow(clippy::cast_possible_truncation, reason = "n < 256")]
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 == 1 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = crc_table();

fn crc_update(crc: u32, bytes: &[u8]) -> u32 {
    bytes.iter().fold(crc, |c, b| {
        CRC_TABLE[usize::from((c ^ u32::from(*b)).to_le_bytes()[0])] ^ (c >> 8)
    })
}

fn read_at(file: &File, buf: &mut [u8], offset: u64) -> Result<(), Fail> {
    file.read_exact_at(buf, offset).map_err(|_| failed())
}

fn be32(bytes: &[u8]) -> u32 {
    let mut b = [0_u8; 4];
    b.copy_from_slice(&bytes[..4]);
    u32::from_be_bytes(b)
}

/// Checks the PNG in `file`, which is `len` bytes long. It reads through the
/// descriptor from offset 0 and leaves the offset unspecified.
///
/// # Errors
/// `capture_dimensions` for a valid PNG over the size limits; `capture_failed`
/// for any other failure.
pub(crate) fn check_png(file: &mut File, len: u64) -> Result<PngInfo, Fail> {
    if len > MAX_PNG_BYTES {
        return Err(failed());
    }
    let mut signature = [0_u8; 8];
    read_at(file, &mut signature, 0)?;
    if signature != SIGNATURE {
        return Err(failed());
    }
    let mut buf = vec![0_u8; COPY_BUFFER];
    let mut offset = 8_u64;
    let mut info: Option<PngInfo> = None;
    let mut idat = false;
    loop {
        let remaining = len.checked_sub(offset).ok_or_else(failed)?;
        if remaining < FRAME {
            return Err(failed());
        }
        let mut head = [0_u8; 8];
        read_at(file, &mut head, offset)?;
        let length = u64::from(be32(&head[..4]));
        let kind = [head[4], head[5], head[6], head[7]];
        if length > MAX_PNG_BYTES || length > remaining - FRAME {
            return Err(failed());
        }
        if info.is_none() && (kind != *b"IHDR" || length != 13) {
            return Err(failed());
        }
        let mut crc = crc_update(u32::MAX, &kind);
        let mut data_at = offset + 8;
        let mut left = length;
        let mut first = [0_u8; 8];
        while left > 0 {
            let want = usize::try_from(left).map_or(COPY_BUFFER, |l| l.min(COPY_BUFFER));
            read_at(file, &mut buf[..want], data_at)?;
            if info.is_none() {
                first.copy_from_slice(&buf[..8]);
            }
            crc = crc_update(crc, &buf[..want]);
            data_at += want as u64;
            left -= want as u64;
        }
        let mut stored = [0_u8; 4];
        read_at(file, &mut stored, data_at)?;
        if !crc != u32::from_be_bytes(stored) {
            return Err(failed());
        }
        offset = data_at + 4;
        match &kind {
            b"IHDR" if info.is_none() => {
                info = Some(PngInfo {
                    width: be32(&first[..4]),
                    height: be32(&first[4..8]),
                });
            }
            b"IHDR" => return Err(failed()),
            b"IDAT" => idat = true,
            b"IEND" => {
                if length != 0 || offset != len || !idat {
                    return Err(failed());
                }
                break;
            }
            _ => {}
        }
    }
    let info = info.ok_or_else(failed)?;
    check_dimensions(info.width, info.height)?;
    Ok(info)
}
