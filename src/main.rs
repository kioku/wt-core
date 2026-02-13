mod cli;
mod domain;
mod error;

use clap::Parser;

fn main() {
    let cli = cli::Cli::parse();
    eprintln!("wt-core: parsed command: {cli:?}");
}
