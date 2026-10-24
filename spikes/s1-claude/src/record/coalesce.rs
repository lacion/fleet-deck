//! Scrubbing across streamed deltas.
//!
//! A home path or a temporary directory name can be split over several
//! `stream_event` deltas (`text_delta`, `thinking_delta`, `input_json_delta`), so
//! a per-frame scrub misses it. [`scrub_records`] joins each run of consecutive
//! deltas of one content block; when the joined text needs a replacement, the run
//! becomes one delta frame with the scrubbed text. Runs that need no replacement
//! stay unchanged, so the fixture keeps the real delta granularity.

use serde_json::Value;
use spike_canon::fixture::{Record, Scrubber};

/// The field that carries the streamed text of a delta type.
fn delta_field(delta_type: &str) -> Option<&'static str> {
    match delta_type {
        "text_delta" => Some("text"),
        "thinking_delta" => Some("thinking"),
        "input_json_delta" => Some("partial_json"),
        _ => None,
    }
}

/// `(index, delta type, text)` of a streamed delta frame.
fn delta_of(r: &Record) -> Option<(u64, String, String)> {
    let Record::Out { frame } = r else {
        return None;
    };
    if frame["type"] != "stream_event" || frame["event"]["type"] != "content_block_delta" {
        return None;
    }
    let delta = &frame["event"]["delta"];
    let ty = delta["type"].as_str()?;
    let field = delta_field(ty)?;
    Some((
        frame["event"]["index"].as_u64().unwrap_or(0),
        ty.to_owned(),
        delta[field].as_str()?.to_owned(),
    ))
}

/// Scrubs every record, and scrubs streamed text across delta boundaries.
#[must_use]
pub fn scrub_records(scrub: &Scrubber, records: &[Record]) -> Vec<Record> {
    let mut out = Vec::with_capacity(records.len());
    let mut i = 0;
    while i < records.len() {
        let Some((index, ty, first)) = delta_of(&records[i]) else {
            out.push(scrub.scrub_record(&records[i]));
            i += 1;
            continue;
        };
        let mut joined = first;
        let mut end = i + 1;
        while let Some((ix, t, text)) = records.get(end).and_then(delta_of) {
            if ix != index || t != ty {
                break;
            }
            joined.push_str(&text);
            end += 1;
        }
        let scrubbed = scrub.scrub_str(&joined);
        if scrubbed == joined {
            out.extend(records[i..end].iter().map(|r| scrub.scrub_record(r)));
        } else if let Record::Out { frame } = &records[i] {
            let mut merged = scrub.scrub(frame);
            if let Some(field) = delta_field(&ty) {
                merged["event"]["delta"][field] = Value::String(scrubbed);
            }
            out.push(Record::Out { frame: merged });
        }
        i = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn delta(text: &str) -> Record {
        Record::Out {
            frame: json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}}),
        }
    }

    #[test]
    fn a_secret_split_over_deltas_is_scrubbed() {
        let s = Scrubber::new().replace("/home/alice", "<home>");
        let recs = vec![
            delta("see /ho"),
            delta("me/ali"),
            delta("ce/x"),
            Record::Note { text: "n".into() },
        ];
        let out = scrub_records(&s, &recs);
        assert_eq!(out.len(), 2);
        let Record::Out { frame } = &out[0] else {
            panic!("{out:?}")
        };
        assert_eq!(frame["event"]["delta"]["text"], "see <home>/x");
    }

    #[test]
    fn clean_runs_keep_their_granularity() {
        let s = Scrubber::new().replace("/home/alice", "<home>");
        let recs = vec![delta("a"), delta("b"), delta("c")];
        assert_eq!(scrub_records(&s, &recs), recs);
    }
}
