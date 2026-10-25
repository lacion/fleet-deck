//! `fd-link-node`: one deck or station process for the multi-process link
//! tests (`tests/multiprocess.rs`). Each process has its own data directory
//! (identity key + `peers.json`) and its own port, so several nodes run side
//! by side on loopback.
//!
//! ```sh
//! fd-link-node --role deck --data /tmp/deck --listen 127.0.0.1:7700 --invites 1
//! fd-link-node --role station --data /tmp/st1 --join 'fdinv1:wss://127.0.0.1:7700#...' --dial 127.0.0.1:7700
//! ```
//!
//! The process prints one line per event (`node`, `listening`, `invite`,
//! `paired`, `joined`, `linked`, `reply`, `served`, `closed`, `lost`,
//! `retry`, `error`) and runs until it is killed. A station that only dials
//! and never listens is the NAT case of PLAN §8.1.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use s4_link::frames::{Outcome, Scope};
use s4_link::identity::{Identity, PeerKey};
use s4_link::link::{self, Inbound, Link, NodeConfig};
use s4_link::pairing::{DEFAULT_TTL, Invitation, hex, unhex};
use s4_link::reconnect::{BackoffPolicy, DialerEvent, spawn_dialer};
use s4_link::tls::Role;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Copy, ValueEnum)]
enum RoleArg {
    Deck,
    Station,
}

#[derive(Parser)]
#[command(about = "Run one Fleet Deck link node (S4 spike)")]
struct Args {
    #[arg(long, value_enum)]
    role: RoleArg,
    /// Data directory: identity key and pinned peers.
    #[arg(long)]
    data: PathBuf,
    /// Listen address (omit for a station that only dials).
    #[arg(long)]
    listen: Option<SocketAddr>,
    /// Number of invitations to print (needs --listen).
    #[arg(long, default_value_t = 0)]
    invites: usize,
    /// Pair with this invitation before anything else.
    #[arg(long)]
    join: Option<String>,
    /// Keep a link to this address (reconnects with backoff).
    #[arg(long)]
    dial: Option<SocketAddr>,
    /// First backoff ceiling in milliseconds.
    #[arg(long, default_value_t = 500)]
    backoff_ms: u64,
    /// Largest backoff ceiling in milliseconds.
    #[arg(long, default_value_t = 300_000)]
    backoff_cap_ms: u64,
}

/// One pinned peer in `peers.json`.
#[derive(Serialize, Deserialize)]
struct PeerRecord {
    key: String,
    role: Role,
    scopes: BTreeSet<Scope>,
}

const PEERS_FILE: &str = "peers.json";

/// What this node lets its peers do: a station lets the deck read, drive and
/// approve; the deck lets a station coordinate.
fn scopes_for_peers(role: Role) -> Vec<Scope> {
    match role {
        Role::Station => vec![Scope::Read, Scope::Drive, Scope::Approve],
        Role::Deck => vec![Scope::Coordinate],
    }
}

fn load_peers(dir: &Path, node: &NodeConfig) -> Result<()> {
    let path = dir.join(PEERS_FILE);
    if !path.exists() {
        return Ok(());
    }
    let peers: Vec<PeerRecord> = serde_json::from_slice(&std::fs::read(&path)?)?;
    for p in peers {
        let bytes = unhex::<32>(&p.key).context("bad key in peers.json")?;
        let key = PeerKey::from_bytes(bytes)?;
        node.pins.pin(key, p.role);
        node.grants.grant(key, p.scopes);
    }
    Ok(())
}

fn save_peer(dir: &Path, key: PeerKey, role: Role, scopes: BTreeSet<Scope>) -> Result<()> {
    let path = dir.join(PEERS_FILE);
    let mut peers: Vec<PeerRecord> = if path.exists() {
        serde_json::from_slice(&std::fs::read(&path)?)?
    } else {
        Vec::new()
    };
    let key = hex(key.as_bytes());
    peers.retain(|p| p.key != key);
    peers.push(PeerRecord { key, role, scopes });
    std::fs::write(&path, serde_json::to_vec_pretty(&peers)?)?;
    Ok(())
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Deck => "deck",
        Role::Station => "station",
    }
}

/// Serves the peer's requests and, on the deck, asks the station who it is.
fn run_link(node: &NodeConfig, side: &'static str, mut link: Link) {
    println!(
        "linked {side} peer={} epoch={} fp={}",
        role_name(link.peer_role),
        link.epoch,
        link.peer.fingerprint().short()
    );
    let me = node.identity.fingerprint().to_string();
    let role = node.local.role;
    if role == Role::Deck {
        let control = link.control.clone();
        tokio::spawn(async move {
            match control.request("station.info", None, json!({})).await {
                Ok(Outcome::Ok(v)) => println!("reply {v}"),
                Ok(Outcome::Err(e)) => println!("error {}", e.code),
                Err(e) => println!("error {e}"),
            }
        });
    }
    tokio::spawn(async move {
        while let Some(inbound) = link.inbound.recv().await {
            match inbound {
                Inbound::Request { id, method, .. } => {
                    let body = json!({"method": method, "node": me, "role": role_name(role)});
                    let _ = link.control.respond(id, Outcome::Ok(body)).await;
                    println!("served {method}");
                }
                Inbound::Closed { reason, .. } => {
                    println!("closed {reason}");
                    break;
                }
                Inbound::Frame(_) => {}
            }
        }
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let role = match args.role {
        RoleArg::Deck => Role::Deck,
        RoleArg::Station => Role::Station,
    };
    std::fs::create_dir_all(&args.data)?;
    let identity = Arc::new(Identity::load_or_create(&args.data)?);
    let node = NodeConfig::new(identity, role);
    load_peers(&args.data, &node)?;
    println!("node {} {}", role_name(role), node.identity.fingerprint());
    let grants: BTreeSet<Scope> = scopes_for_peers(role).into_iter().collect();

    if let Some(text) = &args.join {
        let inv: Invitation = match text.parse() {
            Ok(inv) => inv,
            Err(e) => {
                println!("error {}", s4_link::pairing::PairError::reason(&e));
                std::process::exit(2);
            }
        };
        match link::pair(&node, &inv, grants.iter().copied()).await {
            Ok(key) => {
                let peer_role = node.pins.role_of(&key).unwrap_or(Role::Deck);
                save_peer(&args.data, key, peer_role, grants.clone())?;
                println!("joined {} {}", role_name(peer_role), key.fingerprint());
            }
            Err(e) => {
                println!("error {}", e.reason().unwrap_or("pair_failed"));
                std::process::exit(1);
            }
        }
    }

    if let Some(addr) = args.listen {
        let mut listener = link::listen(node.clone(), addr).await?;
        println!("listening {}", listener.local_addr);
        for _ in 0..args.invites {
            let inv = node.invites.create(
                listener.local_addr,
                node.identity.fingerprint(),
                DEFAULT_TTL,
                grants.iter().copied(),
            )?;
            println!("invite {}", inv.to_uri());
        }
        let watcher = node.clone();
        let dir = args.data.clone();
        tokio::spawn(async move {
            while let Some(p) = watcher.invites.next_paired().await {
                if let Err(e) = save_peer(&dir, p.key, p.role, p.scopes.clone()) {
                    println!("error {e}");
                }
                println!("paired {} {}", role_name(p.role), p.key.fingerprint());
            }
        });
        let n = node.clone();
        tokio::spawn(async move {
            while let Some(next) = listener.next().await {
                match next {
                    Ok(link) => run_link(&n, "listener", link),
                    Err(e) => println!("refused {}", e.reason().unwrap_or("error")),
                }
            }
        });
    }

    if let Some(addr) = args.dial {
        let policy = BackoffPolicy {
            base: Duration::from_millis(args.backoff_ms),
            cap: Duration::from_millis(args.backoff_cap_ms),
            stable: BackoffPolicy::default().stable,
        };
        let (mut events, _task) = spawn_dialer(node.clone(), addr, policy);
        let n = node.clone();
        tokio::spawn(async move {
            while let Some(ev) = events.recv().await {
                match ev {
                    DialerEvent::Linked(link) => run_link(&n, "dialer", link),
                    DialerEvent::Failed {
                        attempt,
                        reason,
                        retry_in,
                    } => println!(
                        "retry attempt={attempt} in_ms={} reason={reason}",
                        retry_in.as_millis()
                    ),
                    DialerEvent::Lost { uptime, retry_in } => println!(
                        "lost uptime_ms={} retry_in_ms={}",
                        uptime.as_millis(),
                        retry_in.as_millis()
                    ),
                }
            }
        });
    }

    if args.listen.is_none() && args.dial.is_none() {
        if args.join.is_some() {
            return Ok(());
        }
        bail!("nothing to do: pass --listen, --dial or --join");
    }
    std::future::pending::<()>().await;
    Ok(())
}
