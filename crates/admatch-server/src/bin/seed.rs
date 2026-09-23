//! Seed data generator.
//!
//! Writes `data/seed.json` (campaigns plus the synthetic audience table) and
//! `data/requests.jsonl` (complete `/v1/match` bodies for load testing).
//! The same `--seed` always produces the same files.
//!
//! ```text
//! cargo run --release --bin seed -- --seed 42
//! ```

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use admatch_core::engine::EngineConfig;
use admatch_server::seedgen::{SeedOptions, generate};
use anyhow::Context;
use clap::Parser;

/// Command-line flags. `clap` generates `--help` from these doc comments.
#[derive(Debug, Parser)]
#[command(about = "Generate deterministic AdMatch seed data")]
struct Args {
    /// RNG seed; the same seed always gives the same data.
    #[arg(long, default_value_t = 42)]
    seed: u64,
    /// Number of campaigns to generate before validation.
    #[arg(long, default_value_t = 10_000)]
    campaigns: usize,
    /// Where to write the campaign seed file.
    #[arg(long, default_value = "data/seed.json")]
    out_file: PathBuf,
    /// Where to write the request lines for the load generator.
    #[arg(long, default_value = "data/requests.jsonl")]
    requests_out: PathBuf,
    /// Number of request lines.
    #[arg(long, default_value_t = 100_000)]
    requests: usize,
    /// Privacy threshold for age-targeted campaigns (same default as the
    /// server's K_TARGETING).
    #[arg(long, default_value_t = EngineConfig::default().k_targeting)]
    k_targeting: i64,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let generated = generate(SeedOptions {
        seed: args.seed,
        campaigns: args.campaigns,
        requests: args.requests,
        k_targeting: args.k_targeting,
    });

    let mut file = create(&args.out_file)?;
    serde_json::to_writer(&mut file, &generated.file)?;
    // Flush explicitly: a BufWriter that fails while flushing on drop would
    // lose the error silently.
    file.flush()
        .with_context(|| format!("cannot write {}", args.out_file.display()))?;

    let mut out = create(&args.requests_out)?;
    for line in &generated.requests {
        serde_json::to_writer(&mut out, line)?;
        out.write_all(b"\n")?;
    }
    out.flush()
        .with_context(|| format!("cannot write {}", args.requests_out.display()))?;

    let keywords: usize = generated
        .file
        .campaigns
        .iter()
        .map(|c| c.keywords.len())
        .sum();
    let rejected: usize = generated.rejected.values().sum();
    println!("seed {}", args.seed);
    println!(
        "campaigns: {} generated, {} accepted, {} rejected",
        args.campaigns,
        generated.file.campaigns.len(),
        rejected
    );
    for (reason, count) in &generated.rejected {
        println!("  rejected {reason}: {count}");
    }
    println!("keywords: {keywords}");
    println!("wrote {}", args.out_file.display());
    println!("requests: {}", generated.requests.len());
    for (kind, count) in &generated.requests_by_kind {
        println!("  {kind}: {count}");
    }
    println!("wrote {}", args.requests_out.display());
    Ok(())
}

/// Creates a file for buffered writing, creating its directory if needed.
fn create(path: &Path) -> anyhow::Result<BufWriter<File>> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let file = File::create(path).with_context(|| format!("cannot create {}", path.display()))?;
    Ok(BufWriter::new(file))
}
