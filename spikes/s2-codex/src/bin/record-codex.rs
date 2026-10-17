//! `record-codex`: records live Codex app-server fixtures (PLAN §13).
//!
//! Live use only (owner or recorder). It runs the real `codex app-server` in a
//! temporary directory with a cheap model and low reasoning effort.
//!
//! ```sh
//! cargo run -p s2-codex --bin record-codex -- --scenario simple
//! cargo run -p s2-codex --bin record-codex -- --scenario all
//! ```

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Parser;
use s2_codex::record::{Config, SCENARIOS, record};

#[derive(Parser)]
#[command(about = "Record live Codex app-server fixtures")]
struct Args {
    /// Scenario name, or `all`.
    #[arg(long, default_value = "all")]
    scenario: String,
    /// Model (keep it cheap).
    #[arg(long, default_value = "gpt-5.6-luna")]
    model: String,
    /// Reasoning effort.
    #[arg(long, default_value = "low")]
    effort: String,
    /// The `codex` binary.
    #[arg(long, default_value = "codex")]
    codex: String,
    /// Output directory for the fixture files.
    #[arg(long, default_value = "spikes/s2-codex/fixtures")]
    out: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let version = std::process::Command::new(&args.codex)
        .arg("--version")
        .output()?;
    // "codex-cli 0.160.0"
    let cli_version = String::from_utf8_lossy(&version.stdout)
        .split_whitespace()
        .last()
        .unwrap_or("unknown")
        .to_owned();
    let cfg = Config {
        bin: args.codex.clone(),
        model: args.model.clone(),
        effort: args.effort.clone(),
        cli_version,
        out_dir: args.out.clone(),
    };
    std::fs::create_dir_all(&cfg.out_dir)?;
    let selected: Vec<_> = SCENARIOS
        .iter()
        .filter(|s| args.scenario == "all" || args.scenario.split(',').any(|n| n == s.name))
        .collect();
    if selected.is_empty() {
        let names: Vec<_> = SCENARIOS.iter().map(|s| s.name).collect();
        bail!(
            "unknown scenario {}; known: {}",
            args.scenario,
            names.join(", ")
        );
    }
    let mut failed = 0;
    for sc in selected {
        match record(&cfg, sc).await {
            Ok(path) => println!("ok     {:<28} {}", sc.name, path.display()),
            Err(e) => {
                failed += 1;
                println!("FAILED {:<28} {e:#}", sc.name);
            }
        }
    }
    if failed > 0 {
        bail!("{failed} scenario(s) failed");
    }
    Ok(())
}
