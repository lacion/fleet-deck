//! The usage that a Codex rollout file records (B20, design `usage-cost.md`
//! §4.2 "Codex record").
//!
//! The rollout has one `event_msg` / `token_count` record per turn with the
//! running `total_token_usage` of the thread, and a `turn_context` record
//! with the model of the turn that follows. The reader scans the whole file
//! forward and prices each interval of the total at the model of the
//! `turn_context` before it, so a model switch does not re-price earlier
//! turns.

use std::fs::File;
use std::io;
use std::time::Instant;

use fleetdeck_proto::event::{USAGE_SCHEME, Usage};
use fleetdeck_proto::fold::add_usage;
use fleetdeck_providers::codex::thread_usage::TokenTotals;
use serde_json::Value;

use super::Limits;
use super::lines::{Line, Lines, ReadError};

/// The total usage of a Codex rollout, scheme 2, with `tokens_partial` and
/// `cost_partial` set (a sub-agent thread has its own rollout, which this
/// file does not hold). `Ok(None)`: no `token_count` record with `info`.
///
/// `fallback_model` prices the intervals before the first `turn_context`.
///
/// # Errors
/// A read error, or [`io::ErrorKind::TimedOut`] when `deadline` passed.
pub fn codex_usage(
    file: &File,
    fallback_model: Option<&str>,
    limits: &Limits,
    deadline: Instant,
) -> io::Result<Option<Usage>> {
    let size = file.metadata()?.len();
    let mut lines = Lines::new(file, 0, size, limits.line_max).with_deadline(deadline);
    let mut model: Option<String> = fallback_model.map(str::to_owned);
    let mut prev = TokenTotals::default();
    let mut sum = Usage {
        scheme: Some(USAGE_SCHEME),
        tokens_partial: true,
        cost_partial: true,
        ..Usage::default()
    };
    let mut seen = false;
    loop {
        let next = lines.next_line().map_err(|e| match e {
            ReadError::Deadline => io::Error::new(io::ErrorKind::TimedOut, "the scan timed out"),
            ReadError::Io(e) => e,
        })?;
        let Some((_, line)) = next else { break };
        let Line::Record(bytes) = line else { continue };
        let is_context = contains(bytes, b"\"turn_context\"");
        let is_count = contains(bytes, b"\"token_count\"");
        if !is_context && !is_count {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Value>(bytes) else {
            continue;
        };
        let payload = &record["payload"];
        match record.get("type").and_then(Value::as_str) {
            Some("turn_context") => {
                if let Some(m) = payload.get("model").and_then(Value::as_str) {
                    model = Some(m.to_owned());
                }
            }
            Some("event_msg")
                if payload.get("type").and_then(Value::as_str) == Some("token_count") =>
            {
                let Some(total) = payload
                    .get("info")
                    .and_then(|info| info.get("total_token_usage"))
                    .filter(|t| t.is_object())
                    .and_then(TokenTotals::from_rollout)
                else {
                    continue;
                };
                let delta = total.since(&prev);
                add_usage(&mut sum, &delta.usage(model.as_deref()));
                prev = total;
                seen = true;
            }
            _ => {}
        }
    }
    Ok(seen.then_some(sum))
}

/// True when `needle` occurs in `hay`.
fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::Duration;

    use super::*;

    fn context(model: &str) -> String {
        format!(r#"{{"type":"turn_context","payload":{{"model":"{model}"}}}}"#)
    }

    /// A `token_count` record with the given running totals.
    fn count(input: u64, cached: u64, output: u64) -> String {
        format!(
            r#"{{"type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output}}}}}}}}}"#
        )
    }

    fn rollout(lines: &[String]) -> File {
        let mut f = tempfile::tempfile().unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
        f
    }

    fn read(lines: &[String], fallback: Option<&str>) -> Option<Usage> {
        let f = rollout(lines);
        codex_usage(
            &f,
            fallback,
            &Limits::default(),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap()
    }

    fn cost_of(model: &str, input: u64, cached: u64, output: u64) -> f64 {
        TokenTotals {
            input,
            cached,
            cache_write: 0,
            output,
        }
        .usage(Some(model))
        .cost_usd
        .unwrap()
    }

    #[test]
    fn a_codex_session_prices_each_interval_by_its_model() {
        let usage = read(
            &[
                context("gpt-5"),
                count(1000, 400, 50),
                context("gpt-5-mini"),
                count(3000, 1400, 150),
            ],
            None,
        )
        .unwrap();
        let first = cost_of("gpt-5", 1000, 400, 50);
        let growth = cost_of("gpt-5-mini", 2000, 1000, 100);
        let want = first + growth;
        assert!((usage.cost_usd.unwrap() - want).abs() < 1e-12, "{usage:?}");
        // New input is the input without the cached part; the parts add.
        assert_eq!(usage.input_tokens, Some(3000 - 1400));
        assert_eq!(usage.cache_read_tokens, Some(1400));
        assert_eq!(usage.output_tokens, Some(150));
        assert_eq!(usage.scheme, Some(USAGE_SCHEME));
        assert!(usage.tokens_partial && usage.cost_partial);
        assert!(usage.cost_inferred);
    }

    #[test]
    fn an_unlisted_interval_keeps_the_known_cost_partial() {
        let usage = read(
            &[
                context("gpt-5"),
                count(1000, 400, 50),
                context("gpt-daybreak-blue-latest"),
                count(3000, 1400, 150),
            ],
            None,
        )
        .unwrap();
        let want = cost_of("gpt-5", 1000, 400, 50);
        assert!((usage.cost_usd.unwrap() - want).abs() < 1e-12, "{usage:?}");
        assert!(usage.cost_partial);
        assert_eq!(usage.output_tokens, Some(150));
    }

    #[test]
    fn an_unlisted_model_gives_an_unknown_cost() {
        let usage = read(
            &[context("gpt-daybreak-blue-latest"), count(1000, 400, 50)],
            None,
        )
        .unwrap();
        assert_eq!(usage.cost_usd, None);
        assert!(usage.cost_partial);
        assert_eq!(usage.output_tokens, Some(50));
        assert_eq!(usage.prices, None);
    }

    #[test]
    fn the_fallback_model_prices_the_turns_before_a_context() {
        let usage = read(&[count(1000, 0, 50)], Some("gpt-5")).unwrap();
        let want = cost_of("gpt-5", 1000, 0, 50);
        assert!((usage.cost_usd.unwrap() - want).abs() < 1e-12, "{usage:?}");
    }

    #[test]
    fn a_token_count_without_info_is_skipped() {
        let empty =
            r#"{"type":"event_msg","payload":{"type":"token_count","info":null}}"#.to_owned();
        assert_eq!(read(std::slice::from_ref(&empty), Some("gpt-5")), None);
        let usage = read(&[empty, count(1000, 0, 50)], Some("gpt-5")).unwrap();
        assert_eq!(usage.output_tokens, Some(50));
    }

    #[test]
    fn a_lower_total_is_a_new_count() {
        let usage = read(&[count(1000, 0, 50), count(300, 0, 20)], Some("gpt-5")).unwrap();
        assert_eq!(usage.output_tokens, Some(70));
        assert_eq!(usage.input_tokens, Some(1300));
    }

    #[test]
    fn a_passed_deadline_is_a_timeout() {
        let f = rollout(&[count(1000, 0, 50)]);
        let err = codex_usage(&f, None, &Limits::default(), Instant::now()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }
}
