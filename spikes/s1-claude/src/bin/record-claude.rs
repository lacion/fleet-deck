//! `record-claude`: records live Claude fixtures (PLAN §13).
//!
//! Live use only (owner or recorder). It runs the real `claude` CLI in a
//! temporary directory with a cheap model and `--safe-mode`.
//!
//! ```sh
//! cargo run -p s1-claude --bin record-claude -- --scenario simple
//! cargo run -p s1-claude --bin record-claude -- --scenario all
//! ```

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Parser;
use s1_claude::record::{Config, SCENARIOS, record};

#[derive(Parser)]
#[command(about = "Record live Claude stream-json fixtures")]
struct Args {
    /// Scenario name, or `all`.
    #[arg(long, default_value = "all")]
    scenario: String,
    /// Model alias (keep it cheap).
    #[arg(long, default_value = "haiku")]
    model: String,
    /// The `claude` binary.
    #[arg(long, default_value = "claude")]
    claude: String,
    /// Output directory for the fixture files.
    #[arg(long, default_value = "spikes/s1-claude/fixtures/rust")]
    out: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let version = std::process::Command::new(&args.claude)
        .arg("--version")
        .output()?;
    let cli_version = String::from_utf8_lossy(&version.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("unknown")
        .to_owned();
    let cfg = Config {
        bin: args.claude.clone(),
        model: args.model.clone(),
        cli_version,
        out_dir: args.out.clone(),
    };
    let selected: Vec<_> = SCENARIOS
        .iter()
        .filter(|s| args.scenario == "all" || s.name == args.scenario)
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
            Ok(path) => println!("ok     {:<24} {}", sc.name, path.display()),
            Err(e) => {
                failed += 1;
                println!("FAILED {:<24} {e:#}", sc.name);
            }
        }
    }
    if failed > 0 {
        bail!("{failed} scenario(s) failed");
    }
    Ok(())
}
