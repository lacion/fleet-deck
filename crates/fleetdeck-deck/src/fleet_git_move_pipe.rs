//! The stream pipe of a native move (design §12.3 step 3, ADR-009 §9; FG-9,
//! T4.4b Q10). The deck opens the export stream on the source and the import
//! stream on the target, and copies each data event from the first to the
//! second. It keeps no copy on disk and holds at most the two stream windows
//! in memory: a chunk is written to the target before its credit goes back
//! to the source.
//!
//! The station closes its half of the import stream when the files arrived
//! and checked; it resets the stream with a code on a failure. The deck waits
//! for that end before it sends `MoveIn`.

use std::time::Duration;

use fleetdeck_link::data::DataEvent;
use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::code;

use crate::fleet_git_move::Deadlines;
use crate::fleet_git_move_send::Fail;
use crate::link::DeckLink;
use crate::link::streams::DeckStream;

const CHECKSUM: &str = "The conversation files did not arrive complete. Try again.";
const TOO_LARGE: &str = "The conversation files are larger than 64 MiB. Use a branch-only move.";
const DISCARDED: &str = "This move was cancelled; it does not run again.";

/// The failure of a stream reset with `reason`. `export`: the reset came from
/// the source.
fn reset_fail(reason: &str, export: bool) -> Fail {
    match reason {
        code::MOVE_TOO_LARGE => Fail::new(code::MOVE_TOO_LARGE, TOO_LARGE),
        code::MOVE_DISCARDED => Fail::new(code::MOVE_DISCARDED, DISCARDED),
        "move_aborted" => Fail::new(
            code::MOVE_NOT_READY,
            "The session is no longer held for the move.",
        ),
        "not_native" => Fail::new(code::MOVE_CHECKSUM, CHECKSUM),
        other if export => Fail::new(
            other,
            format!("The conversation files could not be read ({other})."),
        ),
        _ => Fail::new(code::MOVE_CHECKSUM, CHECKSUM),
    }
}

/// Pipes the files of `tokens.0` (the export token, on `stations.0`) into
/// the upload with `tokens.1` (on `stations.1`). `total` is the byte count of
/// the manifest.
///
/// # Errors
/// The failure of the pipe: a station code, `move_too_large`,
/// `move_checksum`, or `station_unreachable`.
pub(crate) async fn run(
    links: &DeckLink,
    stations: (&StationId, &StationId),
    tokens: (&str, &str),
    total: u64,
    d: &Deadlines,
) -> Result<(), Fail> {
    let unreachable = |s: &StationId| {
        Fail::new(
            "station_unreachable",
            format!("Fleet Deck lost contact with {s} during the move."),
        )
    };
    let (_, src_hub) = links
        .data_for(stations.0)
        .ok_or_else(|| unreachable(stations.0))?;
    let (_, dst_hub) = links
        .data_for(stations.1)
        .ok_or_else(|| unreachable(stations.1))?;
    // The upload opens first: its task waits for the first byte.
    let mut import = dst_hub
        .open(tokens.1)
        .await
        .map_err(|_| unreachable(stations.1))?;
    let mut export = src_hub
        .open(tokens.0)
        .await
        .map_err(|_| unreachable(stations.0))?;
    let work = copy(&mut export, &mut import, total, d);
    let result = match tokio::time::timeout(d.pipe_total, work).await {
        Ok(r) => r,
        Err(_) => Err(Fail::new(code::MOVE_CHECKSUM, CHECKSUM)),
    };
    if let Err(f) = &result {
        // A failed pipe ends both streams, so no task of a station waits.
        let _ = export.handle.reset(export.id, &f.code).await;
        let _ = import.handle.reset(import.id, &f.code).await;
    }
    result
}

async fn next(stream: &mut DeckStream, idle: Duration) -> Option<DataEvent> {
    match tokio::time::timeout(idle, stream.events.recv()).await {
        Ok(event) => event,
        Err(_) => Some(DataEvent::Reset {
            stream: stream.id,
            reason: "timeout".to_owned(),
        }),
    }
}

async fn copy(
    export: &mut DeckStream,
    import: &mut DeckStream,
    total: u64,
    d: &Deadlines,
) -> Result<(), Fail> {
    let mut sent: u64 = 0;
    loop {
        tokio::select! {
            event = next(export, d.pipe_idle) => match event {
                Some(DataEvent::Data { bytes, .. }) => {
                    let n = bytes.len();
                    sent = sent.saturating_add(n as u64);
                    if sent > total {
                        return Err(reset_fail(code::MOVE_TOO_LARGE, true));
                    }
                    import
                        .handle
                        .write(import.id, bytes)
                        .await
                        .map_err(|_| reset_fail("closed", false))?;
                    export
                        .handle
                        .consumed(export.id, n)
                        .await
                        .map_err(|_| reset_fail("closed", true))?;
                }
                Some(DataEvent::Closed { .. }) => {
                    if sent != total {
                        return Err(reset_fail(code::MOVE_CHECKSUM, false));
                    }
                    import
                        .handle
                        .close(import.id)
                        .await
                        .map_err(|_| reset_fail("closed", false))?;
                    return end(import, d.upload_end).await;
                }
                Some(DataEvent::Reset { reason, .. }) => return Err(reset_fail(&reason, true)),
                Some(DataEvent::Opened { .. }) => {}
                None => return Err(reset_fail("closed", true)),
            },
            event = import.events.recv() => match event {
                // The station ends an upload that cannot go on.
                Some(DataEvent::Reset { reason, .. }) => return Err(reset_fail(&reason, false)),
                Some(DataEvent::Closed { .. }) => return Err(reset_fail(code::MOVE_CHECKSUM, false)),
                Some(_) => {}
                None => return Err(reset_fail("closed", false)),
            },
        }
    }
}

/// Waits for the end of the upload: the station closes its half (success) or
/// resets the stream.
async fn end(import: &mut DeckStream, wait: Duration) -> Result<(), Fail> {
    loop {
        match next(import, wait).await {
            Some(DataEvent::Closed { .. }) => return Ok(()),
            Some(DataEvent::Reset { reason, .. }) => return Err(reset_fail(&reason, false)),
            Some(_) => {}
            None => return Err(reset_fail("closed", false)),
        }
    }
}
