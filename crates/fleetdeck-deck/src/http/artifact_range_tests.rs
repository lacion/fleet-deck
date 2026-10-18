//! The range parser, the validators and the preconditions of the MP4 range
//! behavior (R13 P5b E1, contract 11.3).

use axum::http::{HeaderMap, HeaderValue, header};

use super::*;

const N: u64 = 1000;

fn range(v: &str) -> Result<RangeReq, InvalidRange> {
    parse_range(Some(&HeaderValue::from_str(v).unwrap()), N)
}

fn single(start: u64, end: u64) -> Result<RangeReq, InvalidRange> {
    Ok(RangeReq::Single { start, end })
}

#[test]
fn no_header_asks_for_the_whole_file() {
    assert_eq!(parse_range(None, N), Ok(RangeReq::Full));
}

#[test]
fn a_satisfiable_single_range_is_the_selected_bytes() {
    assert_eq!(range("bytes=0-0"), single(0, 0));
    assert_eq!(range("bytes=10-19"), single(10, 19));
    assert_eq!(range("bytes=999-999"), single(999, 999));
    assert_eq!(range("bytes=0-999"), single(0, 999));
}

#[test]
fn an_open_end_runs_to_the_end_of_the_file() {
    assert_eq!(range("bytes=0-"), single(0, 999));
    assert_eq!(range("bytes=1000-"), Ok(RangeReq::Unsatisfiable));
    assert_eq!(range("bytes=999-"), single(999, 999));
}

#[test]
fn a_suffix_is_the_last_bytes_and_a_long_suffix_is_the_whole_file() {
    assert_eq!(range("bytes=-500"), single(500, 999));
    assert_eq!(range("bytes=-1"), single(999, 999));
    assert_eq!(range("bytes=-1000"), single(0, 999));
    assert_eq!(range("bytes=-5000"), single(0, 999));
}

#[test]
fn an_end_past_the_end_of_the_file_is_clamped() {
    assert_eq!(range("bytes=990-5000"), single(990, 999));
    assert_eq!(range("bytes=0-18446744073709551615"), single(0, 999));
}

#[test]
fn a_zero_suffix_and_a_start_at_or_after_the_end_are_unsatisfiable() {
    assert_eq!(range("bytes=-0"), Ok(RangeReq::Unsatisfiable));
    assert_eq!(range("bytes=1000-1000"), Ok(RangeReq::Unsatisfiable));
    assert_eq!(range("bytes=5000-6000"), Ok(RangeReq::Unsatisfiable));
    assert_eq!(
        parse_range(Some(&HeaderValue::from_static("bytes=0-")), 0),
        Ok(RangeReq::Unsatisfiable)
    );
    assert_eq!(
        parse_range(Some(&HeaderValue::from_static("bytes=-5")), 0),
        Ok(RangeReq::Unsatisfiable)
    );
}

#[test]
fn several_valid_ranges_and_an_unknown_unit_select_the_whole_file() {
    assert_eq!(range("bytes=0-1,5-6"), Ok(RangeReq::Full));
    assert_eq!(range("bytes=0-1, 5-6, -3"), Ok(RangeReq::Full));
    assert_eq!(range("items=0-1"), Ok(RangeReq::Full));
    assert_eq!(range("lines=banana"), Ok(RangeReq::Full));
}

#[test]
fn the_unit_and_the_spaces_around_a_range_follow_the_http_grammar() {
    assert_eq!(range("BYTES=0-1"), single(0, 1));
    assert_eq!(range(" bytes=0-1 "), single(0, 1));
    assert_eq!(range("bytes=0-1,"), single(0, 1));
    assert_eq!(range("bytes= 0-1"), single(0, 1));
}

#[test]
fn a_malformed_range_is_invalid() {
    for bad in [
        "",
        "bytes",
        "bytes=",
        "bytes=,",
        "bytes=-",
        "bytes=a-b",
        "bytes=1",
        "bytes=1-2-3",
        "bytes=+1-2",
        "bytes=1- 2",
        "bytes=-+5",
        "bytes=--5",
        "bytes=5-2",
        "bytes=0-1,x",
        "bytes=0x1-5",
        "=0-1",
        "by tes=0-1",
    ] {
        assert_eq!(range(bad), Err(InvalidRange), "{bad:?}");
    }
}

#[test]
fn a_number_that_overflows_is_invalid() {
    assert_eq!(range("bytes=18446744073709551616-"), Err(InvalidRange));
    assert_eq!(range("bytes=0-18446744073709551616"), Err(InvalidRange));
    assert_eq!(range("bytes=-18446744073709551616"), Err(InvalidRange));
    assert_eq!(
        range("bytes=18446744073709551615-"),
        Ok(RangeReq::Unsatisfiable)
    );
}

#[test]
fn a_header_over_256_bytes_is_invalid_and_one_of_256_bytes_is_read() {
    // The leading zeros make the header long; the number is 5.
    let fits = format!("bytes=0-{}5", "0".repeat(256 - "bytes=0-".len() - 1));
    assert_eq!(fits.len(), 256);
    assert_eq!(range(&fits), single(0, 5));
    let long = format!("bytes=0-{}5", "0".repeat(256 - "bytes=0-".len()));
    assert_eq!(long.len(), 257);
    assert_eq!(range(&long), Err(InvalidRange));
    let many = format!("bytes=0-1{}", ",2-3".repeat(80));
    assert!(many.len() > 256);
    assert_eq!(range(&many), Err(InvalidRange));
}

#[test]
fn a_header_with_bytes_that_are_not_text_is_invalid() {
    let v = HeaderValue::from_bytes(b"bytes=0-\xff").unwrap();
    assert_eq!(parse_range(Some(&v), N), Err(InvalidRange));
}

#[test]
fn the_etag_is_the_quoted_sha256() {
    assert_eq!(etag_of("ab12"), "\"sha256-ab12\"");
}

#[test]
fn if_range_lets_the_range_apply_only_for_an_absent_or_a_strong_exact_tag() {
    let etag = etag_of("ab12");
    let v = |s: &str| HeaderValue::from_str(s).unwrap();
    assert!(if_range_matches(None, &etag));
    assert!(if_range_matches(Some(&v("\"sha256-ab12\"")), &etag));
    assert!(if_range_matches(Some(&v(" \"sha256-ab12\" ")), &etag));
    assert!(!if_range_matches(Some(&v("W/\"sha256-ab12\"")), &etag));
    assert!(!if_range_matches(Some(&v("\"sha256-ab13\"")), &etag));
    assert!(!if_range_matches(Some(&v("sha256-ab12")), &etag));
    assert!(!if_range_matches(
        Some(&v("Wed, 21 Oct 2015 07:28:00 GMT")),
        &etag
    ));
    assert!(!if_range_matches(Some(&v("*")), &etag));
}

fn with(name: header::HeaderName, value: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(name, HeaderValue::from_str(value).unwrap());
    h
}

#[test]
fn without_a_precondition_the_request_proceeds() {
    assert_eq!(conditions(&HeaderMap::new(), "\"x\""), Cond::Proceed);
}

#[test]
fn if_match_needs_a_strong_match_or_a_star() {
    let etag = etag_of("ab12");
    let check = |v: &str| conditions(&with(header::IF_MATCH, v), &etag);
    assert_eq!(check("\"sha256-ab12\""), Cond::Proceed);
    assert_eq!(check("*"), Cond::Proceed);
    assert_eq!(check("\"other\", \"sha256-ab12\""), Cond::Proceed);
    assert_eq!(check("\"other\""), Cond::Failed);
    assert_eq!(check("W/\"sha256-ab12\""), Cond::Failed);
    assert_eq!(check(""), Cond::Failed);
}

#[test]
fn if_match_in_several_header_lines_is_one_list() {
    let etag = etag_of("ab12");
    let mut h = HeaderMap::new();
    h.append(header::IF_MATCH, HeaderValue::from_static("\"other\""));
    h.append(
        header::IF_MATCH,
        HeaderValue::from_static("\"sha256-ab12\""),
    );
    assert_eq!(conditions(&h, &etag), Cond::Proceed);
}

#[test]
fn if_none_match_that_matches_is_not_modified_with_a_weak_comparison() {
    let etag = etag_of("ab12");
    let check = |v: &str| conditions(&with(header::IF_NONE_MATCH, v), &etag);
    assert_eq!(check("\"sha256-ab12\""), Cond::NotModified);
    assert_eq!(check("W/\"sha256-ab12\""), Cond::NotModified);
    assert_eq!(check("*"), Cond::NotModified);
    assert_eq!(check("\"other\", \"sha256-ab12\""), Cond::NotModified);
    assert_eq!(check("\"other\""), Cond::Proceed);
}

#[test]
fn if_match_is_decided_before_if_none_match() {
    let etag = etag_of("ab12");
    let mut h = HeaderMap::new();
    h.insert(header::IF_MATCH, HeaderValue::from_static("\"other\""));
    h.insert(header::IF_NONE_MATCH, HeaderValue::from_static("*"));
    assert_eq!(conditions(&h, &etag), Cond::Failed);
}

#[test]
fn only_a_single_mp4_file_is_in_the_range_scope() {
    let version = |files: &[&str]| ArtifactVersion {
        artifact_id: "a".to_owned(),
        version: 1,
        size: 1,
        sha256: String::new(),
        files: files.iter().map(|f| (*f).to_owned()).collect(),
        created_at: 1,
        capture: None,
    };
    assert!(in_scope(&version(&["capture.mp4"]), "capture.mp4"));
    assert!(in_scope(&version(&["v/Clip.MP4"]), "v/Clip.MP4"));
    assert!(!in_scope(&version(&["capture.mp4"]), "other.mp4"));
    assert!(!in_scope(
        &version(&["capture.mp4", "a.png"]),
        "capture.mp4"
    ));
    assert!(!in_scope(&version(&["capture.png"]), "capture.png"));
    assert!(!in_scope(&version(&["capture.webm"]), "capture.webm"));
    assert!(!in_scope(&version(&[]), "capture.mp4"));
}

fn answer(offset: u64, bytes: &[u8], size: u64) -> ArtifactFile {
    ArtifactFile {
        path: "capture.mp4".to_owned(),
        size,
        offset,
        length: bytes.len() as u64,
        data_base64: STANDARD.encode(bytes),
        eof: offset + bytes.len() as u64 == size,
    }
}

#[test]
fn a_station_answer_is_valid_only_when_every_field_agrees_with_the_request() {
    let bytes = [7u8; 10];
    let good = answer(20, &bytes, 100);
    assert_eq!(
        checked(&good, "capture.mp4", 20, 10, 100).unwrap(),
        bytes.to_vec()
    );
    // The last chunk: eof is true.
    let last = answer(90, &bytes, 100);
    assert!(last.eof);
    assert!(checked(&last, "capture.mp4", 90, 10, 100).is_ok());
    let fails = |a: &ArtifactFile| checked(a, "capture.mp4", 20, 10, 100).is_err();
    let mut wrong = good.clone();
    wrong.path = "other.mp4".to_owned();
    assert!(fails(&wrong), "path");
    let mut wrong = good.clone();
    wrong.offset = 21;
    assert!(fails(&wrong), "offset");
    let mut wrong = good.clone();
    wrong.size = 101;
    assert!(fails(&wrong), "size");
    let mut wrong = good.clone();
    wrong.length = 9;
    assert!(fails(&wrong), "length that differs from the decoded one");
    let mut wrong = good.clone();
    wrong.eof = true;
    assert!(fails(&wrong), "eof before the end");
    let mut wrong = good.clone();
    wrong.data_base64 = "!!!!".to_owned();
    assert!(fails(&wrong), "base64");
    let mut wrong = good.clone();
    wrong.data_base64 = STANDARD.encode([7u8; 400]);
    assert!(fails(&wrong), "base64 beyond the bound");
    assert!(fails(&answer(20, &bytes[..9], 100)), "a short answer");
    assert!(fails(&answer(20, &[7u8; 11], 100)), "a long answer");
    assert!(fails(&answer(20, &[], 100)), "an empty answer");
    let mut last_wrong = last.clone();
    last_wrong.eof = false;
    assert!(checked(&last_wrong, "capture.mp4", 90, 10, 100).is_err());
}

#[test]
fn the_fixed_texts_of_the_video_errors_are_the_contract_texts() {
    let e = invalid_range();
    assert_eq!((e.status.as_u16(), e.code), (400, "invalid_range"));
    assert_eq!(e.message, "The byte range is not valid.");
    let e = bad_data();
    assert_eq!((e.status.as_u16(), e.code), (502, "station_error"));
    assert_eq!(e.message, "The station returned invalid artifact data.");
    let e = video_timeout();
    assert_eq!((e.status.as_u16(), e.code), (504, "timeout"));
    assert_eq!(e.message, "The video data did not finish loading in time.");
    let e = video_unreachable();
    assert_eq!((e.status.as_u16(), e.code), (409, "station_unreachable"));
    assert_eq!(e.message, "The station is not connected.");
}

#[test]
fn the_416_answer_names_the_size_and_the_json_body_keeps_its_own_headers() {
    let r = unsatisfiable(1234);
    assert_eq!(r.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(r.headers()[header::CONTENT_RANGE], "bytes */1234");
    assert_eq!(r.headers()[header::CONTENT_TYPE], "application/json");
}
