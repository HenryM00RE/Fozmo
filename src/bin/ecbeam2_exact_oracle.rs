use std::path::PathBuf;

use clap::Parser;
use fozmo::audio::dsd::ecbeam2_oracle_tool::run_frozen_exact_oracle;

#[derive(Debug, Parser)]
#[command(
    name = "ecbeam2_exact_oracle",
    about = "Run frozen EcBeam2 exact N8/N12/N16 difficult-window oracles"
)]
struct Args {
    /// Frozen ecbeam2-corpus-v1 manifest.
    #[arg(long)]
    corpus_manifest: PathBuf,
    /// Exact-oracle request JSON.
    #[arg(long)]
    request: PathBuf,
    /// Frozen calibration budget document required by a budget-bound request.
    #[arg(long)]
    budgets: Option<PathBuf>,
    /// Destination ecbeam2-exact-oracle-v1 JSON file.
    #[arg(long)]
    out: PathBuf,
}

fn main() {
    let args = Args::parse();
    match run_frozen_exact_oracle(
        &args.corpus_manifest,
        &args.request,
        args.budgets.as_deref(),
        &args.out,
    ) {
        Ok(results) => {
            println!(
                "wrote {} exact-oracle rows to {}",
                results.result_count(),
                args.out.display()
            );
        }
        Err(error) => {
            eprintln!("ecbeam2_exact_oracle: {error}");
            std::process::exit(2);
        }
    }
}
