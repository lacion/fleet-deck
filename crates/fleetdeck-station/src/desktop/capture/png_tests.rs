//! The structural PNG check (design §5.3 items 1 and 2).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Seek, SeekFrom, Write};

use fleetdeck_proto::capture::{code, text};

use super::{PngInfo, check_png};

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// A bitwise CRC-32, independent of the table in the module under test.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for b in bytes {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                0xedb8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    let mut out = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
    out
}

fn ihdr(width: u32, height: u32) -> Vec<u8> {
    let mut data = width.to_be_bytes().to_vec();
    data.extend_from_slice(&height.to_be_bytes());
    data.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &data)
}

fn idat() -> Vec<u8> {
    chunk(b"IDAT", &[1, 2, 3, 4, 5, 6, 7])
}

fn iend() -> Vec<u8> {
    chunk(b"IEND", &[])
}

fn png(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut out = SIGNATURE.to_vec();
    for part in parts {
        out.extend_from_slice(part);
    }
    out
}

fn check(bytes: &[u8]) -> Result<PngInfo, (&'static str, String)> {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    // The check reads from offset 0 whatever the offset is.
    file.seek(SeekFrom::End(0)).unwrap();
    check_png(&mut file, bytes.len() as u64).map_err(|e| (e.code, e.message))
}

fn failed() -> (&'static str, String) {
    (code::CAPTURE_FAILED, text::CAPTURE_FAILED.to_owned())
}

fn too_big() -> (&'static str, String) {
    (
        code::CAPTURE_DIMENSIONS,
        text::CAPTURE_DIMENSIONS.to_owned(),
    )
}

#[test]
fn a_valid_minimal_png_passes() {
    let bytes = png(&[ihdr(640, 480), idat(), iend()]);
    assert_eq!(
        check(&bytes),
        Ok(PngInfo {
            width: 640,
            height: 480
        })
    );
}

#[test]
fn a_png_with_other_chunks_and_two_idat_passes() {
    let bytes = png(&[ihdr(2, 2), chunk(b"tEXt", b"k\0v"), idat(), idat(), iend()]);
    assert!(check(&bytes).is_ok());
}

#[test]
fn a_chunk_larger_than_the_copy_buffer_passes() {
    let big = chunk(b"IDAT", &vec![7_u8; 200_000]);
    assert!(check(&png(&[ihdr(2, 2), big, iend()])).is_ok());
}

#[test]
fn a_bad_signature_fails() {
    let mut bytes = png(&[ihdr(2, 2), idat(), iend()]);
    bytes[1] = b'X';
    assert_eq!(check(&bytes), Err(failed()));
    assert_eq!(check(b""), Err(failed()));
}

#[test]
fn ihdr_must_be_first_and_13_bytes() {
    assert_eq!(check(&png(&[idat(), ihdr(2, 2), iend()])), Err(failed()));
    assert_eq!(
        check(&png(&[chunk(b"IHDR", &[0; 12]), idat(), iend()])),
        Err(failed())
    );
    assert_eq!(
        check(&png(&[ihdr(2, 2), ihdr(2, 2), idat(), iend()])),
        Err(failed())
    );
}

#[test]
fn a_bad_crc_fails() {
    let mut bytes = png(&[ihdr(2, 2), idat(), iend()]);
    // Flip one data byte of the IDAT chunk.
    let at = SIGNATURE.len() + ihdr(2, 2).len() + 8;
    bytes[at] ^= 0xff;
    assert_eq!(check(&bytes), Err(failed()));
}

#[test]
fn a_bad_ihdr_crc_fails() {
    let mut bytes = png(&[ihdr(2, 2), idat(), iend()]);
    bytes[8 + 8] ^= 0x01;
    assert_eq!(check(&bytes), Err(failed()));
}

#[test]
fn a_missing_idat_fails() {
    assert_eq!(check(&png(&[ihdr(2, 2), iend()])), Err(failed()));
}

#[test]
fn a_missing_iend_fails() {
    assert_eq!(check(&png(&[ihdr(2, 2), idat()])), Err(failed()));
}

#[test]
fn iend_must_be_empty_and_last() {
    let mut bytes = png(&[ihdr(2, 2), idat(), iend()]);
    bytes.push(0);
    assert_eq!(check(&bytes), Err(failed()));
    assert_eq!(
        check(&png(&[ihdr(2, 2), idat(), chunk(b"IEND", &[1])])),
        Err(failed())
    );
    assert_eq!(
        check(&png(&[ihdr(2, 2), idat(), iend(), idat()])),
        Err(failed())
    );
}

#[test]
fn a_chunk_length_past_the_end_fails() {
    let mut bytes = png(&[ihdr(2, 2), idat(), iend()]);
    let at = SIGNATURE.len() + ihdr(2, 2).len();
    bytes[at..at + 4].copy_from_slice(&1_000_000_u32.to_be_bytes());
    assert_eq!(check(&bytes), Err(failed()));
    bytes[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(check(&bytes), Err(failed()));
}

#[test]
fn a_truncated_file_fails() {
    let bytes = png(&[ihdr(2, 2), idat(), iend()]);
    for cut in [4, 8, 20, 33, bytes.len() - 1] {
        assert_eq!(check(&bytes[..cut]), Err(failed()), "cut at {cut}");
    }
}

#[test]
fn the_dimension_limits() {
    assert!(check(&png(&[ihdr(8192, 2048), idat(), iend()])).is_ok());
    assert!(check(&png(&[ihdr(1, 8192), idat(), iend()])).is_ok());
    for (w, h) in [(8193, 1), (1, 8193), (8192, 2049), (0, 5), (5, 0)] {
        assert_eq!(
            check(&png(&[ihdr(w, h), idat(), iend()])),
            Err(too_big()),
            "{w}x{h}"
        );
    }
}
