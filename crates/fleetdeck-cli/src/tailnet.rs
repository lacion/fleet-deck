//! `fleetdeck tailnet status` (DL-178, tailnet design section 3): what
//! Tailscale tells the deck and the local station, from any machine.
//!
//! The command reads `deck.toml` and `station.toml` from disk (the defaults
//! when a file is missing) and runs the `tailscale` program itself. It talks
//! to no running process and changes nothing.

use std::net::IpAddr;
use std::path::Path;

use fleetdeck_core::netscope::{self, AddrClass};
use fleetdeck_core::paths::{Env, NodeDirs};
use fleetdeck_core::tailscale::{self, TailscaleCli};
use serde_json::{Map, Value, json};

use crate::cli::TailnetCommand;

pub async fn run(dirs: &NodeDirs, env: &Env, command: &TailnetCommand) -> anyhow::Result<()> {
    match command {
        TailnetCommand::Status { json } => status(dirs, env, *json).await,
    }
}

/// What one scope (the deck or the station) sees of Tailscale.
struct View {
    scope: &'static str,
    available: bool,
    reason: Option<String>,
    dns_name: Option<String>,
    ips: Vec<IpAddr>,
    userspace: bool,
    /// The deck only: the tailnet entries of `listen`, then of `link_listen`.
    tailnet_listen: Vec<String>,
    tailnet_link_listen: Vec<String>,
}

async fn view(scope: &'static str, bin: Option<&Path>) -> View {
    let mut view = View {
        scope,
        available: false,
        reason: None,
        dns_name: None,
        ips: Vec::new(),
        userspace: false,
        tailnet_listen: Vec::new(),
        tailnet_link_listen: Vec::new(),
    };
    match TailscaleCli::locate(bin).status().await {
        Ok(status) => {
            view.available = true;
            view.userspace = !status
                .this
                .ips
                .iter()
                .any(|ip| tailscale::is_assigned_locally(*ip));
            view.dns_name = Some(status.this.dns_name);
            view.ips = status.this.ips;
        }
        Err(e) => view.reason = Some(e.reason()),
    }
    view
}

fn tailnet_entries(list: &[std::net::SocketAddr]) -> Vec<String> {
    list.iter()
        .filter(|a| netscope::classify(a.ip()) == AddrClass::Tailnet)
        .map(ToString::to_string)
        .collect()
}

async fn status(dirs: &NodeDirs, env: &Env, json: bool) -> anyhow::Result<()> {
    let deck = fleetdeck_core::config::DeckConfig::load(&dirs.deck_config())?;
    let station = fleetdeck_core::config::StationConfig::load_with_store(
        &dirs.station_config(),
        env,
        &dirs.station().join("secrets"),
    )?;
    let mut deck_view = view("deck", deck.tailscale_bin.as_deref()).await;
    deck_view.tailnet_listen = tailnet_entries(&deck.listen);
    deck_view.tailnet_link_listen = tailnet_entries(&deck.link_listen);
    let station_view = view("station", station.tailscale_bin.as_deref()).await;
    let views = [deck_view, station_view];
    if json {
        println!("{}", serde_json::to_string_pretty(&render_json(&views))?);
    } else {
        print!("{}", render_table(&views));
    }
    Ok(())
}

fn scope_json(view: &View, deck: bool) -> Value {
    let mut m = Map::new();
    m.insert("available".to_owned(), json!(view.available));
    if let Some(reason) = &view.reason {
        m.insert("reason".to_owned(), json!(reason));
    }
    if let Some(name) = &view.dns_name {
        m.insert("dns_name".to_owned(), json!(name));
    }
    m.insert(
        "ips".to_owned(),
        json!(view.ips.iter().map(ToString::to_string).collect::<Vec<_>>()),
    );
    m.insert("userspace".to_owned(), json!(view.userspace));
    if deck {
        m.insert("tailnet_listen".to_owned(), json!(view.tailnet_listen));
        m.insert(
            "tailnet_link_listen".to_owned(),
            json!(view.tailnet_link_listen),
        );
    }
    Value::Object(m)
}

/// `{ "deck": {...}, "station": {...} }`.
fn render_json(views: &[View; 2]) -> Value {
    json!({
        "deck": scope_json(&views[0], true),
        "station": scope_json(&views[1], false),
    })
}

fn dash(text: String) -> String {
    if text.is_empty() {
        "-".to_owned()
    } else {
        text
    }
}

/// The columns `scope | available | dns name | ips | userspace | tailnet
/// listen`, padded to the widest cell of each column.
fn render_table(views: &[View; 2]) -> String {
    let yes_no = |b: bool| if b { "yes" } else { "no" }.to_owned();
    let mut rows: Vec<[String; 6]> = vec![[
        "scope".to_owned(),
        "available".to_owned(),
        "dns name".to_owned(),
        "ips".to_owned(),
        "userspace".to_owned(),
        "tailnet listen".to_owned(),
    ]];
    for (i, v) in views.iter().enumerate() {
        let available = match &v.reason {
            None => yes_no(v.available),
            Some(reason) => format!("no ({reason})"),
        };
        let ips = v
            .ips
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let listen = if i == 0 {
            v.tailnet_listen
                .iter()
                .chain(&v.tailnet_link_listen)
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
        } else {
            String::new()
        };
        rows.push([
            v.scope.to_owned(),
            available,
            dash(v.dns_name.clone().unwrap_or_default()),
            dash(ips),
            yes_no(v.userspace),
            dash(listen),
        ]);
    }
    let widths: Vec<usize> = (0..6)
        .map(|c| rows.iter().map(|r| r[c].chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in &rows {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, w)| format!("{cell:<w$}"))
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn views() -> [View; 2] {
        [
            View {
                scope: "deck",
                available: true,
                reason: None,
                dns_name: Some("deck.t.ts.net".to_owned()),
                ips: vec![
                    "100.64.0.10".parse().unwrap(),
                    "fd7a:115c:a1e0::a".parse().unwrap(),
                ],
                userspace: false,
                tailnet_listen: vec!["100.64.0.10:7420".to_owned()],
                tailnet_link_listen: vec!["[fd7a:115c:a1e0::a]:7421".to_owned()],
            },
            View {
                scope: "station",
                available: false,
                reason: Some("not installed".to_owned()),
                dns_name: None,
                ips: Vec::new(),
                userspace: false,
                tailnet_listen: Vec::new(),
                tailnet_link_listen: Vec::new(),
            },
        ]
    }

    #[test]
    fn the_json_has_the_documented_shape() {
        let v = render_json(&views());
        assert_eq!(
            v,
            json!({
                "deck": {
                    "available": true,
                    "dns_name": "deck.t.ts.net",
                    "ips": ["100.64.0.10", "fd7a:115c:a1e0::a"],
                    "userspace": false,
                    "tailnet_listen": ["100.64.0.10:7420"],
                    "tailnet_link_listen": ["[fd7a:115c:a1e0::a]:7421"]
                },
                "station": {
                    "available": false,
                    "reason": "not installed",
                    "ips": [],
                    "userspace": false
                }
            })
        );
    }

    #[test]
    fn the_table_has_a_header_and_one_row_for_each_scope() {
        let table = render_table(&views());
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3, "{table}");
        let columns: Vec<&str> = lines[0].split("  ").filter(|c| !c.is_empty()).collect();
        assert_eq!(
            columns.iter().map(|c| c.trim()).collect::<Vec<_>>(),
            [
                "scope",
                "available",
                "dns name",
                "ips",
                "userspace",
                "tailnet listen"
            ]
        );
        assert!(lines[1].starts_with("deck"), "{table}");
        assert!(
            lines[1].contains("100.64.0.10,fd7a:115c:a1e0::a"),
            "{table}"
        );
        assert!(
            lines[1].contains("100.64.0.10:7420,[fd7a:115c:a1e0::a]:7421"),
            "{table}"
        );
        assert!(lines[2].starts_with("station"), "{table}");
        assert!(lines[2].contains("no (not installed)"), "{table}");
        // The columns line up.
        assert_eq!(lines[0].find("available"), lines[1].find("yes"));
    }
}
