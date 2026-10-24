//! `replay-codex`: replays recorded Codex fixtures offline (no network, no `codex`).
//!
//! ```sh
//! cargo run -p s2-codex --bin replay-codex
//! cargo run -p s2-codex --bin replay-codex -- --events spikes/s2-codex/fixtures/simple.jsonl
//! ```
//!
//! Every fixture must replay with identical host writes, and no frame may be
//! unknown to the core. Exit code 0 when every fixture passes.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use s2_codex::replay::replay;
use spike_canon::fixture::Fixture;

#[derive(Parser)]
#[command(about = "Replay recorded Codex fixtures through the adapter core")]
struct Args {
    /// Fixture files or directories (default: `spikes/s2-codex/fixtures`).
    paths: Vec<PathBuf>,
    /// Print the canonical events of each fixture.
    #[arg(long)]
    events: bool,
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
        vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")]
    } else {
        args.paths
    };
    let mut failed = 0;
    for file in fixture_files(&paths)? {
        let fx = Fixture::load(&file).with_context(|| file.display().to_string())?;
        match replay(&fx) {
            Ok(r) if r.unknown_frames > 0 => {
                failed += 1;
                println!(
                    "FAILED {:<28} {} unknown frame(s)",
                    fx.header.scenario, r.unknown_frames
                );
            }
            Ok(r) => {
                println!(
                    "ok     {:<28} processes={} writes={} events={} unknown={}",
                    fx.header.scenario,
                    r.processes,
                    r.writes_checked,
                    r.events.len(),
                    r.unknown_frames
                );
                if args.events {
                    for e in &r.events {
                        println!("       {}", serde_json::to_string(e)?);
                    }
                }
            }
            Err(e) => {
                failed += 1;
                println!("FAILED {:<28} {e}", fx.header.scenario);
            }
        }
    }
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
