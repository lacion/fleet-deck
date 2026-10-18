//! `fleetdeck desktop wall next | mode next | show` (R13 P4 task E, design
//! `omarchy-p4.md` §3.6): the commands of the wall hotkeys.
//!
//! - `next` and `mode next` call the local station (`LocalRequest::
//!   WallDirective`). The station sends one `display.directive` for each wall
//!   bound to it. Success prints nothing. The first refused result prints
//!   `<code>: <text>` on stderr and exits with 1.
//! - `show` runs in this process, with `hyprctl` from the `PATH` of the
//!   desktop session. It changes no Fleet Deck state: it focuses the wall
//!   window, or goes back to the previous workspace when the wall is already
//!   on the active workspace.

use std::process::Stdio;
use std::time::Duration;

use anyhow::bail;
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_station::local::{LocalClient, LocalRequest, LocalResponse, socket_path};
use fleetdeck_station::local_wall::WallResult;
use serde_json::Value;
use tokio::process::Command;

use crate::tailnet_join::Refused;

/// The intent of `desktop wall next`.
pub const NEXT_NEEDS_YOU: &str = "next_needs_you";
/// The intent of `desktop wall mode next`.
pub const NEXT_MODE: &str = "next_mode";

/// The limit of one `hyprctl` call.
const HYPRCTL_LIMIT: Duration = Duration::from_secs(5);

const NO_STATION: &str = "The local station is not running.";
const NOT_BOUND: &str = "This station has no wall display bound to it.";
const NO_WALL: &str = "No Fleet Deck wall window is open.";
const NO_HYPRCTL: &str = "hyprctl is not installed.";
const HYPRCTL_FAILED: &str = "hyprctl did not answer.";
const HYPRCTL_UNKNOWN: &str = "hyprctl gave an answer that this program does not know.";

fn refuse(code: &str, detail: &str) -> anyhow::Error {
    Refused {
        code: code.to_owned(),
        detail: detail.to_owned(),
    }
    .into()
}

/// The exit of `next` and `mode next` from the results of the station: no
/// wall is `not_bound`, the first refused result is its own code and text.
fn outcome(results: &[WallResult]) -> anyhow::Result<()> {
    if results.is_empty() {
        return Err(refuse("not_bound", NOT_BOUND));
    }
    match results.iter().find(|r| r.code.is_some()) {
        Some(r) => Err(refuse(
            r.code.as_deref().unwrap_or_default(),
            r.text.as_deref().unwrap_or_default(),
        )),
        None => Ok(()),
    }
}

/// `desktop wall next` (`intent` is [`NEXT_NEEDS_YOU`]) and `desktop wall
/// mode next` (`intent` is [`NEXT_MODE`]).
pub async fn directive(dirs: &NodeDirs, intent: &str) -> anyhow::Result<()> {
    let path = socket_path(&dirs.station());
    let Ok(mut client) = LocalClient::connect(&path).await else {
        return Err(refuse("unavailable", NO_STATION));
    };
    let request = LocalRequest::WallDirective {
        intent: intent.to_owned(),
    };
    let Ok(answer) = client.call(&request).await else {
        return Err(refuse("unavailable", NO_STATION));
    };
    match answer {
        LocalResponse::WallDirective { results } => outcome(&results),
        LocalResponse::Error { message } => Err(refuse("error", &message)),
        _ => bail!("the station gave an answer that this program does not know"),
    }
}

/// `^fleetdeck-wall-[A-Za-z0-9_-]+$`.
fn class_ok(class: &str) -> bool {
    class.strip_prefix("fleetdeck-wall-").is_some_and(|id| {
        !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

/// `^0x[0-9a-f]{1,16}$`.
fn address_ok(address: &str) -> bool {
    address.strip_prefix("0x").is_some_and(|hex| {
        (1..=16).contains(&hex.len()) && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// The first wall window of `hyprctl clients -j`: its address and the id of
/// its workspace.
fn first_wall(clients: &Value) -> Option<(String, Option<i64>)> {
    clients.as_array()?.iter().find_map(|client| {
        let class = client.get("class")?.as_str()?;
        let address = client.get("address")?.as_str()?;
        (class_ok(class) && address_ok(address)).then(|| {
            let workspace = client.get("workspace").and_then(|w| w.get("id")?.as_i64());
            (address.to_owned(), workspace)
        })
    })
}

/// What `show` dispatches: `workspace previous` when the wall is on the
/// active workspace, else `focuswindow address:<address>`.
fn dispatch_args(address: &str, wall_workspace: Option<i64>, active: Option<i64>) -> Vec<String> {
    let shown = wall_workspace.is_some() && wall_workspace == active;
    if shown {
        vec![
            "dispatch".to_owned(),
            "workspace".to_owned(),
            "previous".to_owned(),
        ]
    } else {
        vec![
            "dispatch".to_owned(),
            "focuswindow".to_owned(),
            format!("address:{address}"),
        ]
    }
}

/// One `hyprctl` call, at most [`HYPRCTL_LIMIT`]. The child is killed when
/// the limit passes.
async fn hyprctl(args: &[&str]) -> anyhow::Result<String> {
    let call = Command::new("hyprctl")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(HYPRCTL_LIMIT, call).await {
        Ok(Ok(out)) if out.status.success() => {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        }
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => bail!(NO_HYPRCTL),
        Ok(_) | Err(_) => bail!(HYPRCTL_FAILED),
    }
}

fn json(text: &str) -> anyhow::Result<Value> {
    serde_json::from_str(text).map_err(|_| anyhow::anyhow!(HYPRCTL_UNKNOWN))
}

/// `desktop wall show`.
pub async fn show() -> anyhow::Result<()> {
    let clients = json(&hyprctl(&["clients", "-j"]).await?)?;
    let Some((address, workspace)) = first_wall(&clients) else {
        bail!(NO_WALL);
    };
    let active = json(&hyprctl(&["activeworkspace", "-j"]).await?)?;
    let active = active.get("id").and_then(Value::as_i64);
    let args = dispatch_args(&address, workspace, active);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    hyprctl(&args).await.map(drop)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn result(id: &str, code: Option<&str>, text: Option<&str>) -> WallResult {
        WallResult {
            display_id: id.to_owned(),
            code: code.map(str::to_owned),
            text: text.map(str::to_owned),
        }
    }

    #[test]
    fn wall_class_and_address_grammar() {
        for ok in ["fleetdeck-wall-a", "fleetdeck-wall-A_b-9"] {
            assert!(class_ok(ok), "{ok}");
        }
        for bad in [
            "fleetdeck-wall-",
            "fleetdeck-wall",
            "fleetdeck-preview-a",
            "xfleetdeck-wall-a",
            "fleetdeck-wall-a b",
            "fleetdeck-wall-a.b",
            "fleetdeck-wall-a\n",
            "",
        ] {
            assert!(!class_ok(bad), "{bad:?}");
        }
        for ok in ["0x1", "0xabc", "0x0123456789abcdef"] {
            assert!(address_ok(ok), "{ok}");
        }
        for bad in [
            "0x",
            "0X1",
            "0xABC",
            "0x0123456789abcdef0",
            "abc",
            "0xg",
            "0x1\n",
            "",
        ] {
            assert!(!address_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_first_wall_window_is_picked_and_a_bad_address_is_skipped() {
        let clients = json!([
            {"class": "chromium", "address": "0x1", "workspace": {"id": 1}},
            {"class": "fleetdeck-wall-a", "address": "0xBAD", "workspace": {"id": 2}},
            {"class": "fleetdeck-preview-a", "address": "0x2", "workspace": {"id": 3}},
            {"class": "fleetdeck-wall-b", "address": "0x3f", "workspace": {"id": 5}},
            {"class": "fleetdeck-wall-c", "address": "0x4", "workspace": {"id": 6}}
        ]);
        assert_eq!(first_wall(&clients), Some(("0x3f".to_owned(), Some(5))));
        assert_eq!(first_wall(&json!([])), None);
        assert_eq!(first_wall(&json!({"class": "fleetdeck-wall-a"})), None);
        let no_workspace = json!([{"class": "fleetdeck-wall-a", "address": "0x1"}]);
        assert_eq!(first_wall(&no_workspace), Some(("0x1".to_owned(), None)));
    }

    #[test]
    fn show_goes_back_when_the_wall_is_shown_and_focuses_it_otherwise() {
        assert_eq!(
            dispatch_args("0x3f", Some(5), Some(5)),
            ["dispatch", "workspace", "previous"]
        );
        assert_eq!(
            dispatch_args("0x3f", Some(5), Some(1)),
            ["dispatch", "focuswindow", "address:0x3f"]
        );
        // Without a known workspace the wall is focused.
        assert_eq!(
            dispatch_args("0x3f", None, None),
            ["dispatch", "focuswindow", "address:0x3f"]
        );
    }

    #[test]
    fn next_prints_the_first_refusal_and_not_bound_without_a_wall() {
        assert_eq!(
            outcome(&[]).unwrap_err().to_string(),
            "not_bound: This station has no wall display bound to it."
        );
        let ok = result("d1", None, None);
        assert!(outcome(std::slice::from_ref(&ok)).is_ok());
        let first = result("d2", Some("nothing_to_show"), Some("No session."));
        let second = result("d3", Some("not_bound"), Some("No wall."));
        let err = outcome(&[ok, first, second]).unwrap_err();
        assert_eq!(err.to_string(), "nothing_to_show: No session.");
        assert!(err.downcast_ref::<Refused>().is_some());
    }
}
