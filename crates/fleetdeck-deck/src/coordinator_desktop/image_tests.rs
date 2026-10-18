//! The checks of the image read (R13 P6 §4.9, §5.7): the bytes of the one
//! chunk and the size of the whole reply.

use serde_json::json;

use super::*;

/// A PNG-like file of `size` bytes: the signature and filler.
fn png(size: usize) -> Vec<u8> {
    let mut bytes = PNG_SIGNATURE.to_vec();
    bytes.resize(size, 7);
    bytes
}

fn chunk(bytes: &[u8]) -> Value {
    json!({
        "path": "capture.png", "size": bytes.len(), "offset": 0, "length": bytes.len(),
        "data_base64": STANDARD.encode(bytes), "eof": true,
    })
}

fn meta(size: u64, sha256: &str) -> CaptureImageMeta {
    CaptureImageMeta {
        artifact: ArtifactRef {
            station_id: StationId::from("st"),
            artifact_id: "ART1".into(),
            version: 1,
        },
        capture_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        monitor: "DP-1".into(),
        at: 7,
        size,
        sha256: sha256.into(),
    }
}

#[test]
fn a_file_that_matches_the_version_passes_and_keeps_its_base64() {
    let bytes = png(100);
    let hash = sha256_hex(&bytes);
    let got = check_file(chunk(&bytes), 100, &hash).expect("a valid file");
    assert_eq!(got, STANDARD.encode(&bytes));
}

#[test]
fn the_exact_cap_passes() {
    let bytes = png(716_800);
    let hash = sha256_hex(&bytes);
    assert!(check_file(chunk(&bytes), 716_800, &hash).is_ok());
}

#[test]
fn every_mismatch_is_image_invalid() {
    let bytes = png(100);
    let hash = sha256_hex(&bytes);
    let mut other_hash = hash.clone();
    other_hash.replace_range(0..1, if hash.starts_with('0') { "1" } else { "0" });
    // Not a PNG: the right size and hash, the wrong signature.
    let mut plain = bytes.clone();
    plain[0] = 0;
    let plain_hash = sha256_hex(&plain);
    let mut longer = chunk(&png(101));
    longer["size"] = json!(100);
    let mut with_offset = chunk(&bytes);
    with_offset["offset"] = json!(1);
    let mut not_eof = chunk(&bytes);
    not_eof["eof"] = json!(false);
    let mut not_base64 = chunk(&bytes);
    not_base64["data_base64"] = json!("!!!!");
    let cases = [
        ("size", check_file(chunk(&bytes), 99, &hash)),
        ("hash", check_file(chunk(&bytes), 100, &other_hash)),
        ("signature", check_file(chunk(&plain), 100, &plain_hash)),
        ("longer", check_file(longer, 100, &hash)),
        ("offset", check_file(with_offset, 100, &hash)),
        ("eof", check_file(not_eof, 100, &hash)),
        ("base64", check_file(not_base64, 100, &hash)),
        ("shape", check_file(json!({"nonsense": 1}), 100, &hash)),
        ("empty", check_file(chunk(&[]), 0, &sha256_hex(&[]))),
    ];
    for (name, got) in cases {
        assert_eq!(got, Err("image_invalid"), "{name}");
    }
}

#[test]
fn a_maximum_image_fits_the_control_frame_with_room_for_the_envelope() {
    let bytes = png(716_800);
    let b64 = STANDARD.encode(&bytes);
    assert_eq!(b64.len(), 955_736);
    let outcome = answer(meta(716_800, &sha256_hex(&bytes)), b64).expect("fits");
    let Outcome::Ok(value) = outcome else {
        panic!("a reply");
    };
    assert_eq!(value["status"], "ok");
    let frame = serde_json::to_vec(&value).expect("bytes").len();
    assert!(frame <= MAX_CONTROL_FRAME_BYTES - FRAME_ROOM, "{frame}");
}

#[test]
fn a_reply_over_the_frame_budget_is_image_too_large() {
    // Not possible for 716,800 bytes: the check still refuses a larger text.
    let huge = "A".repeat(MAX_CONTROL_FRAME_BYTES);
    assert_eq!(answer(meta(1, "x"), huge).err(), Some("image_too_large"));
    let edge = "A".repeat(MAX_CONTROL_FRAME_BYTES - FRAME_ROOM);
    assert_eq!(answer(meta(1, "x"), edge).err(), Some("image_too_large"));
}

#[test]
fn the_png_signature_is_the_eight_bytes_of_the_format() {
    assert_eq!(&PNG_SIGNATURE, b"\x89PNG\r\n\x1a\n");
}
