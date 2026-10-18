use std::io::{self, Cursor};

use super::*;
use crate::media::mp4_fixture::{Mp4Fixture, SAMPLE, bx};

/// The offset where `mdat` ends in the default layout: `ftyp` (24), the
/// `mdat` header (8) and 60 samples.
const MDAT_END: usize = 32 + 60 * SAMPLE;

fn lim() -> Mp4Limits {
    Mp4Limits::for_seconds(2)
}

fn run(bytes: &[u8], l: &Mp4Limits) -> Result<Mp4Info, Mp4Error> {
    validate(&mut Cursor::new(bytes), bytes.len() as u64, l)
}

fn check(f: &Mp4Fixture) -> Result<Mp4Info, Mp4Error> {
    run(&f.build(), &lim())
}

fn info(ms: u64, samples: u64) -> Result<Mp4Info, Mp4Error> {
    Ok(Mp4Info {
        width: 1920,
        height: 1080,
        duration_ms: ms,
        samples,
    })
}

fn find(b: &[u8], tag: &[u8; 4]) -> usize {
    b.windows(4).position(|w| w == tag).expect("tag in file")
}

/// The default file with the 32-bit word at `delta` bytes after `tag` changed.
fn patched(tag: &[u8; 4], delta: usize, v: u32) -> Result<Mp4Info, Mp4Error> {
    let mut b = Mp4Fixture::default().build();
    let p = find(&b, tag) + delta;
    b[p..p + 4].copy_from_slice(&v.to_be_bytes());
    run(&b, &lim())
}

/// A reader that fails on every call.
struct Broken;

impl io::Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("read"))
    }
}

impl io::Seek for Broken {
    fn seek(&mut self, _: io::SeekFrom) -> io::Result<u64> {
        Err(io::Error::other("seek"))
    }
}

#[test]
fn the_valid_fixture_is_accepted() {
    assert_eq!(check(&Mp4Fixture::default()), info(2000, 60));
}

#[test]
fn valid_variants_are_accepted() {
    let cases = [
        Mp4Fixture::default().co64(),
        Mp4Fixture::default().moov_first(),
        Mp4Fixture::default().moov_first().open_mdat(),
        Mp4Fixture::default().wide_mdat(),
        Mp4Fixture::default().size_table(),
        Mp4Fixture::default().junk(),
        Mp4Fixture::default().v1(),
        Mp4Fixture::default().free_boxes(1000),
        Mp4Fixture::default().brands(*b"qt  ", &[*b"qt  ", *b"mp42"]),
        Mp4Fixture::default().brands(*b"avc1", &[]),
    ];
    for f in cases {
        assert_eq!(check(&f), info(2000, 60));
    }
    assert_eq!(check(&Mp4Fixture::default().samples(55)), info(1833, 55));
    let table = Mp4Fixture::default().samples(55).size_table();
    assert_eq!(check(&table), info(1833, 55));
}

#[test]
fn extra_chunks_are_allowed_and_too_few_are_not() {
    // 20 samples in each chunk: 3 chunks place all samples, 3 are extra.
    assert_eq!(patched(b"stsc", 16, 20), info(2000, 60));
    // Only 5 chunks of 10 samples.
    assert_eq!(patched(b"stco", 8, 5), Err(Mp4Error::Structure));
}

#[test]
fn skippable_top_level_boxes_are_skipped() {
    for kind in [b"free", b"skip", b"uuid", b"udta", b"meta", b"wide"] {
        let mut b = Mp4Fixture::default().build();
        b.extend(bx(kind, &[1, 2, 3, 4]));
        assert_eq!(run(&b, &lim()), info(2000, 60), "{kind:?}");
    }
}

#[test]
fn box_header_rules() {
    // A size smaller than the header.
    let mut b = Mp4Fixture::default().build();
    b[0..4].copy_from_slice(&4u32.to_be_bytes());
    assert_eq!(run(&b, &lim()), Err(Mp4Error::Truncated));
    // A box that ends past the file.
    let mut b = Mp4Fixture::default().build();
    b[24..28].copy_from_slice(&0x7fff_ffffu32.to_be_bytes());
    assert_eq!(run(&b, &lim()), Err(Mp4Error::Truncated));
    // A 64-bit size smaller than its header.
    let mut b = Mp4Fixture::default().wide_mdat().build();
    b[32..40].copy_from_slice(&12u64.to_be_bytes());
    assert_eq!(run(&b, &lim()), Err(Mp4Error::Truncated));
    // A 64-bit size that overflows the offset.
    let mut b = Mp4Fixture::default().wide_mdat().build();
    b[32..40].copy_from_slice(&u64::MAX.to_be_bytes());
    assert_eq!(run(&b, &lim()), Err(Mp4Error::Structure));
    // Trailing bytes under a header.
    let mut b = Mp4Fixture::default().build();
    b.extend([0, 0, 0, 8]);
    assert_eq!(run(&b, &lim()), Err(Mp4Error::Truncated));
}

#[test]
fn a_size_zero_box_is_only_the_last_mdat() {
    assert_eq!(
        check(&Mp4Fixture::default().zero_size_box()),
        Err(Mp4Error::Structure)
    );
    // The open `mdat` swallows `moov`, which then is missing.
    assert_eq!(
        check(&Mp4Fixture::default().open_mdat()),
        Err(Mp4Error::Structure)
    );
}

#[test]
fn top_level_rules() {
    let bad_brand = Mp4Fixture::default().brands(*b"qt  ", &[*b"qt  "]);
    assert_eq!(check(&bad_brand), Err(Mp4Error::Structure));
    let many = vec![*b"isom"; 65];
    assert_eq!(
        check(&Mp4Fixture::default().brands(*b"isom", &many)),
        Err(Mp4Error::Limits)
    );
    let ok = vec![*b"isom"; 64];
    assert_eq!(
        check(&Mp4Fixture::default().brands(*b"isom", &ok)),
        info(2000, 60)
    );
    assert_eq!(
        run(&bx(b"ftyp", &[1, 2, 3, 4]), &lim()),
        Err(Mp4Error::Structure)
    );
    assert_eq!(
        check(&Mp4Fixture::default().omit(b"ftyp")),
        Err(Mp4Error::Structure)
    );
    assert_eq!(
        check(&Mp4Fixture::default().omit(b"moov")),
        Err(Mp4Error::Structure)
    );
    assert_eq!(
        check(&Mp4Fixture::default().omit(b"mdat")),
        Err(Mp4Error::Structure)
    );
    let mut b = Mp4Fixture::default().build();
    b.extend(bx(b"abcd", &[]));
    assert_eq!(run(&b, &lim()), Err(Mp4Error::Structure));
    let mut b = Mp4Fixture::default().build();
    let moov = b[MDAT_END..].to_vec();
    b.extend(moov);
    assert_eq!(run(&b, &lim()), Err(Mp4Error::Structure));
}

#[test]
fn fragmented_files_are_rejected() {
    assert_eq!(
        check(&Mp4Fixture::default().moof()),
        Err(Mp4Error::Fragmented)
    );
    assert_eq!(
        check(&Mp4Fixture::default().mvex()),
        Err(Mp4Error::Fragmented)
    );
    assert_eq!(
        check(&Mp4Fixture::default().styp_first()),
        Err(Mp4Error::Fragmented)
    );
    for kind in [b"mfra", b"sidx", b"styp"] {
        let mut b = Mp4Fixture::default().build();
        b.extend(bx(kind, &[]));
        assert_eq!(run(&b, &lim()), Err(Mp4Error::Fragmented));
    }
}

#[test]
fn track_rules() {
    assert_eq!(
        check(&Mp4Fixture::default().extra_track()),
        Err(Mp4Error::Tracks)
    );
    assert_eq!(
        check(&Mp4Fixture::default().audio_handler()),
        Err(Mp4Error::Tracks)
    );
    assert_eq!(
        check(&Mp4Fixture::default().omit(b"trak")),
        Err(Mp4Error::Tracks)
    );
    assert_eq!(
        check(&Mp4Fixture::default().omit(b"vmhd")),
        Err(Mp4Error::Tracks)
    );
}

#[test]
fn missing_boxes_are_structure_errors() {
    for kind in [
        b"mvhd", b"tkhd", b"mdia", b"mdhd", b"hdlr", b"minf", b"dinf", b"dref", b"stbl", b"stsd",
        b"stts", b"stsc", b"stsz", b"stco",
    ] {
        assert_eq!(
            check(&Mp4Fixture::default().omit(kind)),
            Err(Mp4Error::Structure),
            "{kind:?}"
        );
    }
    let mut b = Mp4Fixture::default().build();
    let p = find(&b, b"stco");
    b[p..p + 4].copy_from_slice(b"co64");
    // `co64` with the `stco` payload has too small a table for 8-byte entries.
    assert_eq!(run(&b, &lim()), Err(Mp4Error::Truncated));
}

#[test]
fn versions_and_short_payloads() {
    for tag in [b"mvhd", b"mdhd"] {
        let mut b = Mp4Fixture::default().build();
        let p = find(&b, tag);
        b[p + 4] = 2;
        assert_eq!(run(&b, &lim()), Err(Mp4Error::Structure));
    }
    // A `mvhd` with a payload that is too short for its fields.
    let mut b = Mp4Fixture::default().build();
    let p = find(&b, b"mvhd");
    b[p - 4..p].copy_from_slice(&16u32.to_be_bytes());
    assert_eq!(run(&b, &lim()), Err(Mp4Error::Structure));
}

#[test]
fn data_reference_and_codec_rules() {
    assert_eq!(
        check(&Mp4Fixture::default().url_flags(0)),
        Err(Mp4Error::DataRef)
    );
    assert_eq!(patched(b"dref", 8, 2), Err(Mp4Error::DataRef));
    let mut b = Mp4Fixture::default().build();
    let p = find(&b, b"url ");
    b[p..p + 4].copy_from_slice(b"urn ");
    assert_eq!(run(&b, &lim()), Err(Mp4Error::DataRef));
    assert_eq!(check(&Mp4Fixture::default().avc3()), Err(Mp4Error::Codec));
    assert_eq!(
        check(&Mp4Fixture::default().short_avc1()),
        Err(Mp4Error::Codec)
    );
    assert_eq!(
        check(&Mp4Fixture::default().omit(b"avcC")),
        Err(Mp4Error::Codec)
    );
    assert_eq!(patched(b"stsd", 8, 2), Err(Mp4Error::Codec));
}

#[test]
fn dimension_rules() {
    for (w, h) in [
        (1919, 1080),
        (1920, 1079),
        (0, 1080),
        (1920, 0),
        (8194, 2),
        (4098, 4098),
    ] {
        let f = Mp4Fixture::default().size(w, h);
        assert_eq!(check(&f), Err(Mp4Error::Dimensions), "{w}x{h}");
    }
    let f = Mp4Fixture::default().size(2, 2);
    assert_eq!(
        check(&f),
        Ok(Mp4Info {
            width: 2,
            height: 2,
            duration_ms: 2000,
            samples: 60
        })
    );
}

#[test]
fn sample_extent_rules() {
    assert_eq!(
        check(&Mp4Fixture::default().offset_past_mdat()),
        Err(Mp4Error::Extents)
    );
    assert_eq!(
        check(&Mp4Fixture::default().zero_sample()),
        Err(Mp4Error::Extents)
    );
    // The sample size is 1,001 bytes: the last sample ends past `mdat`.
    assert_eq!(patched(b"stsz", 8, 1001), Err(Mp4Error::Extents));
    assert_eq!(patched(b"stsz", 12, 59), Err(Mp4Error::Structure));
    assert_eq!(patched(b"stsc", 12, 2), Err(Mp4Error::Structure));
    assert_eq!(patched(b"stsc", 16, 0), Err(Mp4Error::Structure));
}

#[test]
fn table_count_rules() {
    assert_eq!(patched(b"stts", 8, 5), Err(Mp4Error::Truncated));
    assert_eq!(patched(b"stts", 8, 1000), Err(Mp4Error::Limits));
    assert_eq!(patched(b"stco", 8, 1000), Err(Mp4Error::Limits));
    assert_eq!(patched(b"stts", 12, 0), Err(Mp4Error::Structure));
}

#[test]
fn timing_rules() {
    assert_eq!(
        check(&Mp4Fixture::default().zero_delta()),
        Err(Mp4Error::Timing)
    );
    assert_eq!(patched(b"mvhd", 16, 0), Err(Mp4Error::Timing));
    assert_eq!(patched(b"mdhd", 16, 0), Err(Mp4Error::Timing));
    assert_eq!(patched(b"mdhd", 20, 0), Err(Mp4Error::Timing));
    // One delta of difference is allowed; ten are not.
    assert_eq!(patched(b"mdhd", 20, 60_500), info(2000, 60));
    assert_eq!(patched(b"mdhd", 20, 70_000), Err(Mp4Error::Timing));
    // A duration under 1 ms.
    assert_eq!(patched(b"mdhd", 16, 100_000_000), Err(Mp4Error::Timing));
    // 31 fps over 2 seconds.
    let l = Mp4Limits {
        max_samples: 100,
        ..lim()
    };
    let b = Mp4Fixture::default().fps(31).build();
    assert_eq!(run(&b, &l), Err(Mp4Error::Timing));
    let b = Mp4Fixture::default().fps(30).build();
    assert_eq!(run(&b, &l), info(2000, 60));
}

#[test]
fn limit_rules() {
    assert_eq!(
        check(&Mp4Fixture::default().samples(61)),
        Err(Mp4Error::Limits)
    );
    // The valid file has 22 boxes.
    let exact = Mp4Fixture::default().free_boxes(100_000 - 22).build();
    assert_eq!(run(&exact, &lim()), info(2000, 60));
    let over = Mp4Fixture::default().free_boxes(100_001).build();
    assert_eq!(run(&over, &lim()), Err(Mp4Error::Limits));
    let small = Mp4Limits {
        max_bytes: 1000,
        ..lim()
    };
    assert_eq!(
        run(&Mp4Fixture::default().build(), &small),
        Err(Mp4Error::Limits)
    );
}

#[test]
fn depth_rules() {
    let b = Mp4Fixture::default().build();
    // `avcC` is the deepest box, at depth 8.
    for (depth, want) in [
        (8, info(2000, 60)),
        (7, Err(Mp4Error::Limits)),
        (4, Err(Mp4Error::Limits)),
    ] {
        let l = Mp4Limits {
            max_depth: depth,
            ..lim()
        };
        assert_eq!(run(&b, &l), want, "depth {depth}");
    }
}

#[test]
fn a_file_over_the_byte_limit_is_not_read() {
    let l = lim();
    assert_eq!(
        validate(&mut Broken, l.max_bytes + 1, &l),
        Err(Mp4Error::Limits)
    );
}

#[test]
fn a_tiny_file_is_truncated_and_a_broken_reader_is_io() {
    for size in [0, 7] {
        assert_eq!(
            validate(&mut Broken, size, &lim()),
            Err(Mp4Error::Truncated)
        );
    }
    assert_eq!(validate(&mut Broken, 1000, &lim()), Err(Mp4Error::Io));
}

#[test]
fn a_truncated_copy_is_truncated() {
    let b = Mp4Fixture::default().build();
    let n = b.len();
    for len in [
        0,
        3,
        7,
        12,
        23,
        100,
        30_000,
        MDAT_END - 1,
        MDAT_END + 3,
        n - 1,
    ] {
        assert_eq!(
            run(&b[..len], &lim()),
            Err(Mp4Error::Truncated),
            "length {len}"
        );
    }
}

#[test]
fn a_short_read_inside_the_declared_size_is_truncated() {
    let b = Mp4Fixture::default().build();
    let short = &b[..MDAT_END + 20];
    let got = validate(&mut Cursor::new(short), b.len() as u64, &lim());
    assert_eq!(got, Err(Mp4Error::Truncated));
}

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[test]
fn random_mutations_never_panic() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let originals = [
        Mp4Fixture::default().build(),
        Mp4Fixture::default().co64().build(),
    ];
    for i in 0..200 {
        let mut b = originals[i % 2].clone();
        let tail = b.len() - MDAT_END;
        for _ in 0..1 + xorshift(&mut seed) % 4 {
            let r = (xorshift(&mut seed) % (32 + tail as u64)) as usize;
            let at = if r < 32 { r } else { MDAT_END + r - 32 };
            b[at] = xorshift(&mut seed) as u8;
        }
        let _ = run(&b, &lim());
    }
}
