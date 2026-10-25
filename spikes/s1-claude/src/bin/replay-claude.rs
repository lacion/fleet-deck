//! `replay-claude`: replays recorded Claude fixtures offline (no network, no `claude`).
//!
//! ```sh
//! cargo run -p s1-claude --bin replay-claude
//! cargo run -p s1-claude --bin replay-claude -- --events spikes/s1-claude/fixtures/rust/simple.jsonl
//! cargo run -p s1-claude --bin replay-claude -- --keys spikes/s1-claude/fixtures/sdk
//! ```
//!
//! Fixtures of our recorder (`fixtures/rust`) must replay with identical host
//! writes. Fixtures of the TypeScript SDK recorder (`fixtures/sdk`) replay with
//! host commands inferred from the SDK's writes and must match semantically.
//! Exit code 0 when every fixture replays and no frame is unknown.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use s1_claude::reference::replay_reference;
use s1_claude::replay::replay;
use spike_canon::fixture::Fixture;

#[derive(Parser)]
#[command(about = "Replay recorded Claude fixtures through the adapter core")]
struct Args {
    /// Fixture files or directories (default: the Rust fixture directory).
    paths: Vec<PathBuf>,
    /// Print the canonical events of each fixture.
    #[arg(long)]
    events: bool,
    /// Print the wire keys that only the SDK or only the core sends (SDK fixtures).
    #[arg(long)]
    keys: bool,
}

/// One replay result in a form that both replay kinds share.
struct Outcome {
    kind: &'static str,
    processes: usize,
    writes: usize,
    events: Vec<spike_canon::CanonEvent>,
    unknown: usize,
    keys: Vec<String>,
}

fn run(fx: &Fixture) -> Result<Outcome, String> {
    if fx.header.recorder.starts_with("sdk-recorder") {
        let r = replay_reference(fx).map_err(|e| e.to_string())?;
        Ok(Outcome {
            kind: "sdk",
            processes: r.processes,
            writes: r.writes_checked,
            events: r.events,
            unknown: r.unknown_frames,
            keys: r.key_differences.into_iter().collect(),
        })
    } else {
        let r = replay(fx).map_err(|e| e.to_string())?;
        Ok(Outcome {
            kind: "rust",
            processes: r.processes,
            writes: r.writes_checked,
            events: r.events,
            unknown: r.unknown_frames,
            keys: Vec::new(),
        })
    }
}

fn fixture_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(p)
                .with_context(|| p.display().to_string())?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|f| {
                    let n = f.to_string_lossy();
                    n.ends_with(".jsonl") && !n.ends_with(".failed.jsonl")
                })
                .collect();
            entries.sort();
            out.extend(entries);
        } else {
            out.push(p.clone());
        }
    }
    Ok(out)
}

fn main() -> Result<ExitCode> {
    let args = Args::parse();
    let paths = if args.paths.is_empty() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        vec![root.join("fixtures/rust"), root.join("fixtures/sdk")]
    } else {
        args.paths
    };
    let mut failed = 0;
    for file in fixture_files(&paths)? {
        let fx = Fixture::load(&file).with_context(|| file.display().to_string())?;
        match run(&fx) {
            Ok(r) if r.unknown > 0 => {
                failed += 1;
                println!(
                    "FAILED {:<4} {:<24} {} unknown frame(s)",
                    r.kind, fx.header.scenario, r.unknown
                );
            }
            Ok(r) => {
                println!(
                    "ok     {:<4} {:<24} processes={} writes={} events={} unknown={}",
                    r.kind,
                    fx.header.scenario,
                    r.processes,
                    r.writes,
                    r.events.len(),
                    r.unknown
                );
                if args.events {
                    for e in &r.events {
                        println!("       {}", serde_json::to_string(e)?);
                    }
                }
                if args.keys {
                    for k in &r.keys {
                        println!("       key {k}");
                    }
                }
            }
            Err(e) => {
                failed += 1;
                println!("FAILED {:<24} {e}", fx.header.scenario);
            }
        }
    }
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
