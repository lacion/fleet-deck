//! `fleetdeck desktop feed [--notify]` (R13 P4 task B2, `omarchy-p4.md`
//! §3.3): prints the fleet summary of the local station as one compact JSON
//! line on each change, for a desktop bar.
//!
//! The command is a client of the local socket op `fleet_summary`: it calls
//! with the last `seq`, and the station holds the call until `seq` grows or
//! 25 seconds pass. So a line comes at least every 30 seconds. The command
//! ends with exit 0 when stdout is closed and never ends for another reason.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use fleetdeck_core::paths::NodeDirs;
use fleetdeck_proto::fleet_summary::{DeckState, FleetSummary};
use fleetdeck_station::local::{LocalClient, LocalRequest, LocalResponse, socket_path};

use crate::notify::Notifier;

/// The read limit of one call: the 25 s hold of the station and 5 s margin.
const CALL_LIMIT: Duration = Duration::from_secs(30);
/// The wait after an answer `FleetSummaryOff`.
const OFF_WAIT: Duration = Duration::from_secs(5);
/// The longest wait after a failure.
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// A summary older than this many seconds is stale: `deck` is `unknown`.
const STALE_AFTER_S: u64 = 150;
/// The line for no summary, a stopped station or the setting off.
const UNKNOWN: &str = r#"{"v":1,"deck":"unknown"}"#;

/// What one call to the station gave.
#[derive(Debug)]
pub(crate) enum Answer {
    Summary {
        seq: u64,
        link_up: bool,
        held_s: u64,
        summary: Option<FleetSummary>,
    },
    Off,
    Failed,
}

pub(crate) async fn ask(path: &Path, after: u64, limit: Duration) -> Answer {
    let call = async {
        let mut client = LocalClient::connect(path).await?;
        client.call(&LocalRequest::FleetSummary { after }).await
    };
    match tokio::time::timeout(limit, call).await {
        Ok(Ok(LocalResponse::FleetSummary {
            seq,
            link,
            held_s,
            summary,
        })) => match link.as_str() {
            "up" | "down" => Answer::Summary {
                seq,
                link_up: link == "up",
                held_s,
                summary,
            },
            _ => {
                tracing::debug!("the station gave a link state that is not up or down");
                Answer::Failed
            }
        },
        Ok(Ok(LocalResponse::FleetSummaryOff {})) => Answer::Off,
        Ok(Ok(_)) => {
            tracing::debug!("the station gave an answer that is not a fleet summary");
            Answer::Failed
        }
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "the call to the station failed");
            Answer::Failed
        }
        Err(_) => {
            tracing::debug!("the station gave no answer in time");
            Answer::Failed
        }
    }
}

/// The summary with the corrections of §3.3: `deck` is `down` when the link
/// is down, `unknown` when the summary is stale, else `up`; `age_s` and
/// `resets_in_s` move by the seconds that the station held the summary.
pub(crate) fn corrected(mut summary: FleetSummary, link_up: bool, held_s: u64) -> FleetSummary {
    summary.deck = if !link_up {
        DeckState::Down
    } else if held_s > STALE_AFTER_S {
        DeckState::Unknown
    } else {
        DeckState::Up
    };
    let held = u32::try_from(held_s).unwrap_or(u32::MAX);
    for item in &mut summary.needs_you {
        item.age_s = item.age_s.saturating_add(held);
    }
    if let Some(usage) = &mut summary.usage {
        usage.resets_in_s = usage.resets_in_s.map(|s| s.saturating_sub(held));
    }
    summary
}

fn render(summary: &FleetSummary) -> String {
    serde_json::to_string(summary).unwrap_or_else(|e| {
        tracing::error!(error = %e, "the summary could not be written as JSON");
        UNKNOWN.to_owned()
    })
}

/// Waits of the failures: 1, 2, 4 ... 30 seconds; a good answer resets it.
#[derive(Debug)]
struct Backoff {
    next: Duration,
}

impl Backoff {
    const START: Duration = Duration::from_secs(1);

    fn new() -> Self {
        Self { next: Self::START }
    }

    fn wait(&mut self) -> Duration {
        let wait = self.next;
        self.next = (wait * 2).min(BACKOFF_MAX);
        wait
    }

    fn reset(&mut self) {
        self.next = Self::START;
    }
}

/// Writes one line and flushes. `false` means that stdout is closed.
fn emit(out: &mut impl Write, line: &str) -> bool {
    match writeln!(out, "{line}").and_then(|()| out.flush()) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => false,
        Err(e) => {
            eprintln!("fleetdeck: cannot write to stdout: {e}");
            false
        }
    }
}

/// Runs the feed. Returns `Ok` when stdout is closed.
pub async fn run(dirs: &NodeDirs, notify: bool) -> anyhow::Result<()> {
    let path = socket_path(&dirs.station());
    let mut out = std::io::stdout();
    let mut notifier = notify.then(Notifier::default);
    let mut backoff = Backoff::new();
    // After a failure the station may have restarted, so its `seq` too.
    let mut after = 0;
    loop {
        match ask(&path, after, CALL_LIMIT).await {
            Answer::Summary {
                seq,
                link_up,
                held_s,
                summary,
            } => {
                backoff.reset();
                after = seq;
                let Some(summary) = summary else {
                    if !emit(&mut out, UNKNOWN) {
                        return Ok(());
                    }
                    continue;
                };
                let summary = corrected(summary, link_up, held_s);
                if !emit(&mut out, &render(&summary)) {
                    return Ok(());
                }
                if let Some(notifier) = &mut notifier {
                    notifier.on_summary(&summary).await;
                }
            }
            Answer::Off => {
                if !emit(&mut out, UNKNOWN) {
                    return Ok(());
                }
                tokio::time::sleep(OFF_WAIT).await;
            }
            Answer::Failed => {
                after = 0;
                if !emit(&mut out, UNKNOWN) {
                    return Ok(());
                }
                tokio::time::sleep(backoff.wait()).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary() -> FleetSummary {
        serde_json::from_str(
            r#"{"v":1,"at":"2026-10-18T12:04:00Z","board":"https://deck.example:7420","deck":"up",
 "counts":{"working":1,"idle":0,"needs_you":1,"blocked":0,"lost":0},
 "needs_you":[{"station":"srv","station_id":"st1","session":"s1","request":"r1",
               "callsign":"lynx-12","kind":"tool","age_s":95},
              {"station":"srv","station_id":"st1","session":"s2","request":"r2",
               "callsign":null,"kind":"question","age_s":4294967290}],
 "usage":{"label":"claude · 5 h","pct":63,"resets_in_s":100},
 "stations":[]}"#,
        )
        .unwrap()
    }

    #[test]
    fn desktop_feed_deck_state_follows_link_and_age() {
        assert_eq!(corrected(summary(), true, 0).deck, DeckState::Up);
        assert_eq!(corrected(summary(), true, 150).deck, DeckState::Up);
        assert_eq!(corrected(summary(), true, 151).deck, DeckState::Unknown);
        assert_eq!(corrected(summary(), false, 0).deck, DeckState::Down);
        assert_eq!(corrected(summary(), false, 900).deck, DeckState::Down);
    }

    #[test]
    fn desktop_feed_age_and_reset_move_by_the_held_seconds() {
        let got = corrected(summary(), true, 40);
        assert_eq!(got.needs_you[0].age_s, 135);
        assert_eq!(got.needs_you[1].age_s, u32::MAX, "no overflow");
        assert_eq!(got.usage.as_ref().unwrap().resets_in_s, Some(60));
        let got = corrected(summary(), true, 140);
        assert_eq!(got.usage.unwrap().resets_in_s, Some(0), "not below 0");
    }

    #[test]
    fn desktop_feed_line_is_compact_json() {
        let line = render(&corrected(summary(), true, 0));
        assert!(!line.contains('\n'));
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["deck"], "up");
        assert_eq!(value["v"], 1);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(UNKNOWN).unwrap(),
            serde_json::json!({"v": 1, "deck": "unknown"})
        );
    }

    #[test]
    fn desktop_feed_backoff_doubles_to_30_seconds_and_resets() {
        let mut backoff = Backoff::new();
        let waits: Vec<u64> = (0..8).map(|_| backoff.wait().as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30]);
        backoff.reset();
        assert_eq!(backoff.wait(), Duration::from_secs(1));
    }

    #[test]
    fn desktop_feed_emit_reports_a_closed_pipe() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(!emit(&mut Closed, "x"));
        let mut open = Vec::new();
        assert!(emit(&mut open, "x"));
        assert_eq!(open, b"x\n");
    }
}
