use std::path::PathBuf;

use clap::Parser;
use fozmo::audio::dsd::seventh_order_search_oracle_tool::run_frozen_exact_oracle;

#[derive(Debug, Parser)]
#[command(
    name = "seventh_order_search_exact_oracle",
    about = "Run frozen 7th Order Search exact N8/N12/N16 difficult-window oracles"
)]
struct Args {
    /// Frozen v1 corpus manifest.
    #[arg(long)]
    corpus_manifest: PathBuf,
    /// Exact-oracle request JSON.
    #[arg(long)]
    request: PathBuf,
    /// Frozen calibration budget document required by a budget-bound request.
    #[arg(long)]
    budgets: Option<PathBuf>,
    /// Destination exact-oracle JSON file.
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
            eprintln!("seventh_order_search_exact_oracle: {error}");
            std::process::exit(2);
        }
    }
}
