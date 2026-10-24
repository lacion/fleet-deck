//! Fixtures must not leak local paths, also when a path is split over several
//! streamed deltas (`record::coalesce`).

use std::path::Path;

use serde_json::Value;
use spike_canon::fixture::{Fixture, Record};

const FORBIDDEN: &[&str] = &["/home/", "/Users/", "/tmp/fd-s1-", "/tmp/tmp."];

fn streamed_text(fx: &Fixture) -> String {
    let mut joined = String::new();
    for r in &fx.records {
        if let Record::Out { frame } = r
            && frame["type"] == "stream_event"
        {
            let d = &frame["event"]["delta"];
            for k in ["text", "thinking", "partial_json"] {
                joined.push_str(d[k].as_str().unwrap_or_default());
            }
        }
    }
    joined
}

#[test]
fn no_fixture_leaks_a_local_path() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut checked = 0;
    for dir in ["rust", "sdk"] {
        for entry in std::fs::read_dir(root.join(dir)).expect("fixture dir") {
            let path = entry.expect("entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let raw = std::fs::read_to_string(&path).expect("read");
            let fx = Fixture::load(&path).expect("load");
            let joined = streamed_text(&fx);
            for bad in FORBIDDEN {
                assert!(!raw.contains(bad), "{} contains {bad}", path.display());
                assert!(
                    !joined.contains(bad),
                    "{} streams {bad} across deltas",
                    path.display()
                );
            }
            let _: Value = serde_json::to_value(&fx.header).expect("header");
            checked += 1;
        }
    }
    assert!(checked >= 22, "only {checked} fixtures");
}
