//! Logging (PLAN §12: `tracing`, `tracing-subscriber` with a JSON option).
//!
//! Logs go to stderr. The filter comes from `FLEETDECK_LOG`, then `RUST_LOG`,
//! then the default (`info`). Secrets reach a log line only as
//! `Secret(<redacted>)` (see [`crate::secret`]).

use std::str::FromStr;

use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

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

/// A subscriber that writes to `writer`. Tests pass a buffer. `ansi` adds
/// colors to text lines (only for a terminal).
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
        LogFormat::Text => Box::new(builder.finish()),
        LogFormat::Json => Box::new(
            builder
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .finish(),
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
    fn formats_parse() {
        assert_eq!("json".parse(), Ok(LogFormat::Json));
        assert_eq!("text".parse(), Ok(LogFormat::Text));
        assert!("xml".parse::<LogFormat>().is_err());
    }
}
