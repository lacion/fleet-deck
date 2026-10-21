//! Logging (PLAN §12: `tracing`, `tracing-subscriber` with a JSON option).
//!
//! Logs go to stderr. The filter comes from `FLEETDECK_LOG`, then `RUST_LOG`,
//! then the default (`info`). Secrets reach a log line only as
//! `Secret(<redacted>)` (see [`crate::secret`]).

use std::str::FromStr;

use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;

pub const FILTER_ENV: &str = "FLEETDECK_LOG";
pub const DEFAULT_FILTER: &str = "info";

/// The output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFormat {
    /// Human-readable lines.
    #[default]
    Text,
    /// One JSON object per line (for log collectors).
    Json,
}

impl FromStr for LogFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            other => Err(format!("unknown log format `{other}` (use text or json)")),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("bad log filter: {0}")]
    Filter(String),
    #[error("a global logger is already set")]
    AlreadySet,
}

/// The filter of the environment, or `default`.
pub fn env_filter(default: &str) -> Result<EnvFilter, LogError> {
    let text = [FILTER_ENV, "RUST_LOG"]
        .into_iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| default.to_owned());
    EnvFilter::try_new(&text).map_err(|e| LogError::Filter(format!("`{text}`: {e}")))
}

/// Whether `target` belongs to the MCP SDK; its events never reach a log
/// (MCP P1 §3.5). The WebSocket crates can log whole frames, which can carry
/// secret files (Fleet Sync R5), so their targets are denied too.
pub fn is_sdk_target(target: &str) -> bool {
    ["rmcp", "tungstenite", "tokio_tungstenite"]
        .iter()
        .any(|name| target == *name || target.starts_with(&format!("{name}::")))
}

/// A subscriber that writes to `writer`. Tests pass a buffer. `ansi` adds
/// colors to text lines (only for a terminal).
///
/// The MCP SDK logs whole requests and results; no filter text may turn them
/// on (MCP P1 §3.5). The deny is a global filter layer outside `EnvFilter`,
/// because `EnvFilter` lets a more specific directive such as
/// `rmcp::service=trace` win over a general one.
pub fn subscriber<W>(
    format: LogFormat,
    filter: EnvFilter,
    writer: W,
    ansi: bool,
) -> Box<dyn Subscriber + Send + Sync>
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(ansi && format == LogFormat::Text)
        .with_target(true);
    match format {
        LogFormat::Text => Box::new(
            builder
                .finish()
                .with(filter_fn(|meta| !is_sdk_target(meta.target()))),
        ),
        LogFormat::Json => Box::new(
            builder
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .finish()
                .with(filter_fn(|meta| !is_sdk_target(meta.target()))),
        ),
    }
}

/// Installs the global subscriber (stderr). Call it once in `main`.
pub fn init(format: LogFormat) -> Result<(), LogError> {
    use std::io::IsTerminal;
    let ansi = std::io::stderr().is_terminal();
    let sub = subscriber(format, env_filter(DEFAULT_FILTER)?, std::io::stderr, ansi);
    tracing::subscriber::set_global_default(sub).map_err(|_| LogError::AlreadySet)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::secret::Secret;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl Write for Buf {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'w> MakeWriter<'w> for Buf {
        type Writer = Buf;
        fn make_writer(&'w self) -> Self::Writer {
            self.clone()
        }
    }

    fn capture(format: LogFormat, f: impl FnOnce()) -> String {
        let buf = Buf::default();
        let sub = subscriber(format, EnvFilter::new("debug"), buf.clone(), false);
        tracing::subscriber::with_default(sub, f);
        String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn json_lines_parse_and_redact_secrets() {
        let token = Secret::new("tok-123".to_owned());
        let out = capture(LogFormat::Json, || {
            tracing::info!(station = "laptop", ?token, "paired");
            tracing::trace!("filtered out");
        });
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1, "{out}");
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["message"], "paired");
        assert_eq!(v["station"], "laptop");
        assert_eq!(v["level"], "INFO");
        assert_eq!(v["token"], "Secret(<redacted>)");
        assert!(!out.contains("tok-123"));
    }

    #[test]
    fn text_lines_are_human_readable() {
        let out = capture(LogFormat::Text, || tracing::warn!(n = 3, "slow peer"));
        assert!(out.contains("WARN") && out.contains("slow peer") && out.contains("n=3"));
    }

    #[test]
    fn sdk_targets_are_the_rmcp_crate_and_its_modules() {
        for t in [
            "rmcp",
            "rmcp::service",
            "rmcp::transport::streamable_http_server::tower",
        ] {
            assert!(is_sdk_target(t), "{t}");
        }
        for t in ["rmcpx", "fleetdeck_station::rmcp", "rmc"] {
            assert!(!is_sdk_target(t), "{t}");
        }
    }

    #[test]
    fn the_websocket_crates_are_denied_like_the_sdk() {
        for t in [
            "tungstenite",
            "tungstenite::protocol",
            "tokio_tungstenite",
            "tokio_tungstenite::compat",
        ] {
            assert!(is_sdk_target(t), "{t}");
        }
        for t in [
            "tungstenitex",
            "fleetdeck_link::tungstenite",
            "tokio_tungstenitex",
        ] {
            assert!(!is_sdk_target(t), "{t}");
        }
    }

    #[test]
    fn no_filter_text_turns_on_the_sdk_logs() {
        for format in [LogFormat::Text, LogFormat::Json] {
            for text in [
                "trace",
                "rmcp=trace",
                "rmcp::service=trace",
                "rmcp::transport=trace",
                "[s]=trace",
                "[{secret}]=trace",
                "[s{secret}]=trace",
            ] {
                let buf = Buf::default();
                let filter = EnvFilter::try_new(text).unwrap_or_else(|e| panic!("{text}: {e}"));
                let sub = subscriber(format, filter, buf.clone(), false);
                tracing::subscriber::with_default(sub, || {
                    tracing::debug!(target: "rmcp::service", secret = "SENTINEL-1", "received request");
                    let span =
                        tracing::info_span!(target: "rmcp::service", "s", secret = "SENTINEL-2");
                    let _entered = span.enter();
                    tracing::debug!(target: "rmcp::service", "inside the span");
                    tracing::debug!(target: "fleetdeck_station", "visible");
                });
                let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
                assert!(!out.contains("SENTINEL"), "{format:?} {text}: {out}");
                if text == "trace" {
                    assert!(out.contains("visible"), "{format:?} {text}: {out}");
                }
            }
        }
    }

    #[test]
    fn formats_parse() {
        assert_eq!("json".parse(), Ok(LogFormat::Json));
        assert_eq!("text".parse(), Ok(LogFormat::Text));
        assert!("xml".parse::<LogFormat>().is_err());
    }
}
