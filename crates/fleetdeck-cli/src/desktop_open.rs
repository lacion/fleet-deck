//! `fleetdeck desktop open [<station_id>/<session>] [--request <id>]`
//! (R13 P4 task B2, `omarchy-p4.md` §3.3): opens the board in the browser.
//!
//! The command asks the local station once for the board address of the
//! last fleet summary, builds the URL (no token in it) and starts
//! `xdg-open` detached. An error exits with 1 and the text on stderr.

use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::bail;
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_station::local::{LocalClient, LocalRequest, LocalResponse, socket_path};

use crate::notify::id_ok;

/// The limit of the one call to the station.
const CALL_LIMIT: Duration = Duration::from_secs(5);
const NO_BOARD: &str = "No board address is known. Run the feed first.";
const NO_ORIGIN: &str = "The deck did not tell its board address.";
const USE: &str = "Use <station_id>/<session>.";
const NO_XDG_OPEN: &str = "xdg-open is not installed.";

/// The session part of the URL.
#[derive(Debug, PartialEq, Eq)]
struct Target<'a> {
    station_id: &'a str,
    session: &'a str,
    request: Option<&'a str>,
}

/// `Ok(None)` without an argument. `--request` needs a session.
fn parse<'a>(
    target: Option<&'a str>,
    request: Option<&'a str>,
) -> anyhow::Result<Option<Target<'a>>> {
    let Some(target) = target else {
        if request.is_some() {
            bail!(USE);
        }
        return Ok(None);
    };
    let Some((station_id, session)) = target.split_once('/') else {
        bail!(USE);
    };
    if !id_ok(station_id) || !id_ok(session) || request.is_some_and(|r| !id_ok(r)) {
        bail!(USE);
    }
    Ok(Some(Target {
        station_id,
        session,
        request,
    }))
}

/// `<board>/`, or `<board>/#/s/<station_id>/<session>` with
/// `?request=<id>` inside the fragment.
fn url(board: &str, target: Option<&Target<'_>>) -> String {
    let Some(target) = target else {
        return format!("{board}/");
    };
    let mut url = format!("{board}/#/s/{}/{}", target.station_id, target.session);
    if let Some(request) = target.request {
        url.push_str("?request=");
        url.push_str(request);
    }
    url
}

/// The board origin of the last summary of the station.
async fn board(dirs: &NodeDirs) -> anyhow::Result<String> {
    let path = socket_path(&dirs.station());
    let call = async {
        let mut client = LocalClient::connect(&path).await?;
        client.call(&LocalRequest::FleetSummary { after: 0 }).await
    };
    match tokio::time::timeout(CALL_LIMIT, call).await {
        Ok(answer) => match answer? {
            LocalResponse::FleetSummary {
                summary: Some(summary),
                ..
            } => summary.board.ok_or_else(|| anyhow::anyhow!(NO_ORIGIN)),
            LocalResponse::FleetSummary { summary: None, .. }
            | LocalResponse::FleetSummaryOff {} => bail!(NO_BOARD),
            LocalResponse::Error { message } => bail!("the station refused the call: {message}"),
            _ => bail!("the station gave an answer that this program does not know"),
        },
        // With no summary the station holds the call: a timeout means that
        // no summary exists.
        Err(_) => bail!(NO_BOARD),
    }
}

/// Starts `xdg-open` detached (its own process group, no standard streams).
fn open(url: &str) -> anyhow::Result<()> {
    let spawned = Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    match spawned {
        Ok(_child) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(NO_XDG_OPEN),
        Err(e) => bail!("cannot start xdg-open: {e}"),
    }
}

pub async fn run(
    dirs: &NodeDirs,
    target: Option<&str>,
    request: Option<&str>,
) -> anyhow::Result<()> {
    let target = parse(target, request)?;
    let board = board(dirs).await?;
    open(&url(&board, target.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(result: anyhow::Result<Option<Target<'_>>>) -> String {
        result.unwrap_err().to_string()
    }

    #[test]
    fn desktop_open_parses_the_session_and_the_request() {
        assert_eq!(parse(None, None).unwrap(), None);
        let got = parse(Some("st-1/s_2"), Some("r3")).unwrap().unwrap();
        assert_eq!(
            got,
            Target {
                station_id: "st-1",
                session: "s_2",
                request: Some("r3")
            }
        );
        assert_eq!(parse(Some("st1/s2"), None).unwrap().unwrap().request, None);
    }

    #[test]
    fn desktop_open_refuses_a_bad_argument_with_one_text() {
        for (target, request) in [
            (None, Some("r1")),
            (Some("st1"), None),
            (Some("st1/"), None),
            (Some("/s1"), None),
            (Some("st1/s1/x"), None),
            (Some("st 1/s1"), None),
            (Some("st1/s.1"), None),
            (Some("st1/s1"), Some("r 1")),
            (Some("st1/s1"), Some("r?x=1")),
            (Some("st1/s1"), Some("")),
            (Some("st1/s1"), Some(&"r".repeat(65))),
        ] {
            assert_eq!(
                text(parse(target, request)),
                "Use <station_id>/<session>.",
                "{target:?} {request:?}"
            );
        }
    }

    #[test]
    fn desktop_open_url_forms() {
        let board = "https://deck.example.ts.net:7420";
        assert_eq!(url(board, None), "https://deck.example.ts.net:7420/");
        let session = parse(Some("st1/s2"), None).unwrap();
        assert_eq!(
            url(board, session.as_ref()),
            "https://deck.example.ts.net:7420/#/s/st1/s2"
        );
        let request = parse(Some("st1/s2"), Some("r3")).unwrap();
        assert_eq!(
            url(board, request.as_ref()),
            "https://deck.example.ts.net:7420/#/s/st1/s2?request=r3"
        );
    }
}
