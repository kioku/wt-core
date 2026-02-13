mod cli;
mod domain;
mod error;
mod git;
mod output;

use clap::Parser;

fn main() {
    let cli = cli::Cli::parse();
    eprintln!("wt-core: parsed command: {cli:?}");
}
